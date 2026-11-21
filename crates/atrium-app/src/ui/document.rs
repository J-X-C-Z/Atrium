//! An office document open for editing in the side panel: Word and
//! PowerPoint as blocks of text, Excel as a sheet of cells.

use std::collections::HashMap;
use std::rc::Rc;

use atrium_office::{Document, Office, Op, SheetOp, SlideOp, Snapshot, WordOp};
use ely_gpui_component::{
    documents::{BlockData, BlockEditor, BlockEvent, BlockKind},
    tables::Spreadsheet,
    theme::{ActiveTheme, Radius, TextSize},
};
use gpui::{
    Context, Entity, EventEmitter, IntoElement, ParentElement, Render, SharedString, Styled,
    Subscription, Window, div, prelude::*,
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
        };
        view.build_editor(window, cx);
        view
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

    fn set_cells(&mut self, edits: &[(usize, usize, SharedString)], cx: &mut Context<Self>) {
        let sheet = self.sheet;
        for (r, c, text) in edits {
            let op = Op::Sheet(SheetOp::SetCell {
                sheet,
                r: *r as u32,
                c: *c as u32,
                input: text.to_string(),
            });
            if let Err(e) = self.doc.apply(op) {
                cx.emit(DocEvent::Refused(e.to_string()));
                cx.notify();
                return;
            }
        }
        cx.emit(DocEvent::Edited);
        cx.notify();
    }

    fn sheet_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Office::Sheet(wb) = &self.doc.office else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let small = theme.text_size(TextSize::Sm);
        let me = cx.entity();
        let sheets = wb.sheets();
        let current = self.sheet.min(sheets.len().saturating_sub(1));
        let mut tabs = div().flex().flex_wrap().gap_1().px_2().py_1();
        for (i, s) in sheets.iter().enumerate() {
            if s.hidden {
                continue;
            }
            let on = i == current;
            let pick = me.clone();
            tabs = tabs.child(
                div()
                    .id(SharedString::from(format!("doc-sheet-{i}")))
                    .px_2()
                    .py_0p5()
                    .rounded(theme.radius(Radius::Sm))
                    .text_size(small)
                    .cursor_pointer()
                    .when(on, |el| el.bg(colors.selection))
                    .when(!on, |el| {
                        el.text_color(colors.fg_muted)
                            .hover(|el| el.bg(colors.hover))
                    })
                    .on_click(move |_, _, cx| {
                        pick.update(cx, |d, cx| {
                            d.sheet = i;
                            cx.notify();
                        })
                    })
                    .child(s.name.clone()),
            );
        }
        let Some(sheet) = sheets.get(current) else {
            return div().into_any_element();
        };
        let (rows, cols) = sheet.extent();
        let mut cells = HashMap::new();
        for ((r, c), cell) in &sheet.cells {
            // Formulas show their value as the file has it; Ely's own
            // calculation would differ from Excel's for many functions.
            let text = if cell.formula.is_some() && !cell.edited {
                cell.display.clone()
            } else {
                cell.input()
            };
            cells.insert((*r as usize, *c as usize), SharedString::from(text));
        }
        let mut grid = Spreadsheet::new(
            SharedString::from(format!("doc-grid-{current}")),
            (rows as usize + 20).max(30),
            (cols as usize + 4).max(8),
        )
        .cells(Rc::new(cells))
        .freeze(1, 0)
        .size_full();
        for (r0, c0, r1, c1) in &sheet.merged {
            grid = grid.merge((*r0 as usize, *c0 as usize), (*r1 as usize, *c1 as usize));
        }
        let edit = me.clone();
        let grid = grid.on_change(move |edits, _, cx| {
            let edits = edits.to_vec();
            edit.update(cx, |d, cx| d.set_cells(&edits, cx));
        });
        let note = self.doc.is_dirty().then(|| {
            div()
                .px_3()
                .pb_1()
                .text_size(theme.text_size(TextSize::Xs))
                .text_color(colors.fg_muted)
                .child(t("Formulas keep their last results here; Excel recalculates them when it opens the file.").to_string())
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(tabs)
            .children(note)
            .child(div().flex_1().min_h_0().child(grid))
            .into_any_element()
    }
}

impl Render for DocView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if matches!(self.doc.office, Office::Sheet(_)) {
            return self.sheet_body(cx);
        }
        match &self.editor {
            Some(editor) => div()
                .size_full()
                .px_3()
                .py_2()
                .child(editor.clone())
                .into_any_element(),
            None => div().into_any_element(),
        }
    }
}
