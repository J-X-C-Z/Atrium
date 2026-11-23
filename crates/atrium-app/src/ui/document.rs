//! An office document open for editing in the side panel: Word and
//! PowerPoint as blocks of text, Excel as a sheet of cells.

use std::collections::HashMap;

use atrium_office::{Document, Office, Op, SlideOp, Snapshot, WordOp};
use ely_gpui_component::documents::{BlockData, BlockEditor, BlockEvent, BlockKind};
use gpui::{
    Context, Entity, EventEmitter, IntoElement, ParentElement, Render, Styled, Subscription,
    Window, div, prelude::*,
};

use crate::docmap::{self, Refusal, Target};
use crate::i18n::{t, tf};

/// What the document tells the panel.
pub enum DocEvent {
    /// An edit landed; the document is now dirty.
    Edited,
    /// An edit was refused and put back, with why.
    Refused(String),
}

pub struct DocView {
    pub doc: Document,
    editor: Option<Entity<BlockEditor>>,
    targets: HashMap<u64, Target>,
    /// The editor's blocks as last reconciled.
    shown: Vec<(u64, BlockData)>,
    /// The sheet showing, for workbooks.
    pub sheet: usize,
    /// The document as the agent last saw it: when opened, or when your
    /// changes were last handed over.
    pub baseline: Snapshot,
    _editor_events: Option<Subscription>,
    /// Word and PowerPoint on their pages.
    pub(super) pages: Option<super::pages::Pages>,
    /// Workbooks as a grid.
    pub(super) grid: Option<super::grid::Grid>,
    /// Shows the blocks outline instead of the pages.
    pub(super) outline: bool,
}

impl EventEmitter<DocEvent> for DocView {}

fn refusal_text(why: &Refusal) -> String {
    match why {
        Refusal::Locked => t("This paragraph holds tracked changes, comments or fields. Edit it in the original app.").into(),
        Refusal::TableShape => t("Rows, columns and new tables can't be changed here yet; edit the cells' text.").into(),
        Refusal::Fixed => t("This part is shown as it is and can't be edited here.").into(),
    }
}

impl DocView {
    pub fn new(doc: Document, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let baseline = doc.office.snapshot();
        let mut view = Self {
            doc,
            editor: None,
            targets: HashMap::new(),
            shown: Vec::new(),
            sheet: 0,
            baseline,
            _editor_events: None,
            pages: None,
            grid: None,
            outline: false,
        };
        if matches!(view.doc.office, Office::Word(_) | Office::Slides(_)) {
            view.pages = Some(super::pages::Pages::new(&view.doc.office, window, cx));
        }
        if matches!(view.doc.office, Office::Sheet(_)) {
            view.grid = Some(super::grid::Grid::new(window, cx));
        }
        view
    }

    /// Switches between the pages and the outline of blocks.
    pub(super) fn set_outline(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.outline = on;
        if on {
            self.build_editor(window, cx);
        } else {
            self.editor = None;
            self._editor_events = None;
            if self.pages.is_some() {
                self.pages = Some(super::pages::Pages::new(&self.doc.office, window, cx));
            }
        }
        cx.notify()
    }

    pub fn is_dirty(&self) -> bool {
        self.doc.is_dirty()
    }

    /// The blocks the editor shows, each with what it stands for.
    fn blocks(&self) -> Vec<(BlockData, Target)> {
        match &self.doc.office {
            Office::Word(doc) => docmap::word_blocks(doc.blocks()),
            Office::Slides(deck) => {
                let mut out = Vec::new();
                for (i, slide) in deck.slides().iter().enumerate() {
                    let title = tf("Slide {n}", &[("n", &(i + 1))]);
                    let title = if slide.hidden {
                        format!("{title} · {}", t("hidden"))
                    } else {
                        title
                    };
                    out.push((
                        BlockData::new(BlockKind::Heading(3), [title]),
                        Target::Fixed,
                    ));
                    for p in slide.shapes.iter().flat_map(|s| s.paras.iter()) {
                        let (text, atoms) =
                            docmap::inlines_to_text(&p.inlines, &docmap::atom_label);
                        out.push((
                            BlockData::new(BlockKind::Paragraph, [text]),
                            Target::Para {
                                id: p.id,
                                locked: p.locked,
                                atoms,
                                style: p.style.clone(),
                            },
                        ));
                    }
                    out.push((
                        BlockData::new(BlockKind::Divider, Vec::<String>::new()),
                        Target::Fixed,
                    ));
                }
                out
            }
            Office::Sheet(_) => Vec::new(),
        }
    }

    /// (Re)builds the editor from the model, as after opening or a refused edit.
    fn build_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.doc.office, Office::Sheet(_)) {
            return;
        }
        let blocks = self.blocks();
        let data: Vec<BlockData> = blocks.iter().map(|(d, _)| d.clone()).collect();
        let editor = cx.new(|cx| BlockEditor::new(data, window, cx));
        let keyed = editor.read(cx).keyed_blocks(cx);
        self.targets.clear();
        for ((key, _), (_, target)) in keyed.iter().zip(blocks) {
            let locked = matches!(target, Target::Fixed | Target::Para { locked: true, .. });
            if locked {
                editor.update(cx, |e, cx| e.set_locked(*key, true, cx));
            }
            self.targets.insert(*key, target);
        }
        self.shown = keyed;
        self._editor_events =
            Some(
                cx.subscribe_in(&editor, window, |this, _, event, window, cx| {
                    if matches!(event, BlockEvent::Changed) {
                        this.editor_changed(window, cx);
                    }
                }),
            );
        self.editor = Some(editor);
        cx.notify();
    }

    fn editor_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &self.editor else { return };
        let now = editor.read(cx).keyed_blocks(cx);
        if now == self.shown {
            return;
        }
        let slides = matches!(self.doc.office, Office::Slides(_));
        // Slides take text edits only: the same blocks, in the same order.
        if slides {
            let same_shape = now.len() == self.shown.len()
                && now
                    .iter()
                    .zip(&self.shown)
                    .all(|((a, x), (b, y))| a == b && x.kind == y.kind);
            if !same_shape {
                cx.emit(DocEvent::Refused(
                    t("Slides take text edits here; add or remove boxes in the original app.")
                        .into(),
                ));
                self.build_editor(window, cx);
                return;
            }
        }
        let before = std::mem::take(&mut self.shown);
        let doc = &mut self.doc;
        let mut failure = None;
        let result = docmap::reconcile(&before, &now, &mut self.targets, &mut |op| {
            let op = if slides {
                match op {
                    WordOp::SetContent { para, inlines } => {
                        Op::Slides(SlideOp::SetContent { para, inlines })
                    }
                    _ => return None,
                }
            } else {
                Op::Word(op)
            };
            match doc.apply(op) {
                Ok(Op::Word(WordOp::DeletePara { para })) => Some(para),
                Ok(_) => None,
                Err(e) => {
                    failure = Some(e.to_string());
                    None
                }
            }
        });
        match result {
            Ok(_) if failure.is_none() => {
                self.shown = now;
                cx.emit(DocEvent::Edited);
            }
            Ok(_) => {
                cx.emit(DocEvent::Refused(failure.unwrap_or_default()));
                self.build_editor(window, cx);
            }
            Err(why) => {
                cx.emit(DocEvent::Refused(refusal_text(&why)));
                self.shown = before;
                self.build_editor(window, cx);
            }
        }
        cx.notify();
    }
}

impl Render for DocView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if matches!(self.doc.office, Office::Sheet(_)) {
            return self.grid_body(window, cx);
        }
        if !self.outline && self.pages.is_some() {
            return self.pages_body(window, cx);
        }
        let back = self.pages.is_some().then(|| {
            let me = cx.entity();
            div().px_2().pt_1().child(
                ely_gpui_component::buttons::Button::new("doc-back-to-pages", t("Back to pages"))
                    .variant(ely_gpui_component::buttons::ButtonVariant::Ghost)
                    .size(ely_gpui_component::theme::ControlSize::Sm)
                    .icon(ely_gpui_component::primitives::IconName::FileText)
                    .on_click(move |_, window, cx| {
                        me.update(cx, |d, cx| d.set_outline(false, window, cx))
                    }),
            )
        });
        match &self.editor {
            Some(editor) => div()
                .size_full()
                .flex()
                .flex_col()
                .children(back)
                .child(div().flex_1().min_h_0().px_3().py_2().child(editor.clone()))
                .into_any_element(),
            None => div().into_any_element(),
        }
    }
}
