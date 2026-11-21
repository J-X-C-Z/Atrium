//! The side panel's Preview view: a file an agent made or changed, shown
//! without leaving Atrium. Word, Excel and PowerPoint open for editing;
//! saving hands your changes to the agent with your next message.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use atrium_office::{Format, OpenError, SaveError};
use atrium_preview::{Ask, Fonts, Layout, Preview, Previewer, Refused};
use ely_gpui_component::{
    buttons::{Button, ButtonVariant, IconButton},
    documents::{DocPage, DocumentViewer, MarkdownRenderer},
    primitives::{Icon, IconName},
    tables::Spreadsheet,
    theme::{ActiveTheme, ControlSize, IconSize, Radius, TextSize},
};
use gpui::{
    AnyElement, Context, Entity, IntoElement, ParentElement, SharedString, Styled, Subscription,
    Window, div, img, prelude::*, px,
};

use super::RootView;
use super::changes::SideTab;
use super::document::{DocEvent, DocView};
use crate::i18n::{t, tf};
use crate::opener;
use crate::workspace::Notice;

/// How often an open file is checked for changes on disk.
const WATCH: Duration = Duration::from_secs(2);
/// Text files larger than this are not shown.
const TEXT_LIMIT: u64 = 2 * 1024 * 1024;

/// Which of a document's two views shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// The reflowing view you can edit.
    #[default]
    Edit,
    /// The pages as the original app lays them out.
    Pages,
}

pub enum Look {
    Empty,
    Loading,
    Image(PathBuf),
    Markdown(SharedString),
    Text(SharedString),
    Doc(Entity<DocView>),
    Rendered(Preview),
    Refused(String),
}

/// A save that found the file changed on disk, waiting for your choice.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Clash {
    None,
    ChangedOnDisk,
}

pub struct PreviewPane {
    pub path: Option<PathBuf>,
    pub look: Look,
    pub mode: Mode,
    /// Whether the file opens as an editable document at all.
    pub editable: bool,
    pub zoom: f32,
    pub sheet: usize,
    previewer: Arc<Previewer>,
    /// The file's modification time when it was read.
    stamp: Option<SystemTime>,
    checked: Option<Instant>,
    /// The file changed on disk while it had unsaved edits.
    pub stale: bool,
    pub clash: Clash,
    /// Save as soon as the agent stops.
    pub save_when_idle: bool,
    /// The file as it was before the last save, to undo it.
    pub backup: Option<(PathBuf, PathBuf)>,
    _doc_events: Option<Subscription>,
}

impl PreviewPane {
    pub fn new() -> Self {
        let cache = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("Atrium")
            .join("previews");
        Self {
            path: None,
            look: Look::Empty,
            mode: Mode::Edit,
            editable: false,
            zoom: 1.0,
            sheet: 0,
            previewer: Arc::new(Previewer::new(cache, Fonts::system())),
            stamp: None,
            checked: None,
            stale: false,
            clash: Clash::None,
            save_when_idle: false,
            backup: None,
            _doc_events: None,
        }
    }

    fn doc(&self) -> Option<&Entity<DocView>> {
        match &self.look {
            Look::Doc(doc) => Some(doc),
            _ => None,
        }
    }
}

fn ext(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn is_image(path: &Path) -> bool {
    matches!(
        ext(path).as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp"
    )
}

/// Whether the Preview view can show `path` at all.
pub fn previewable(path: &Path) -> bool {
    is_image(path)
        || matches!(
            ext(path).as_str(),
            "docx" | "xlsx" | "pptx" | "pdf" | "csv" | "tsv" | "xls" | "ods" | "md" | "markdown"
        )
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// The file as text, when it is text and not too big.
fn read_text(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > TEXT_LIMIT {
        return Err(t("The file is too big to show here.").into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.contains(&0) {
        return Err(t("There is no preview for this kind of file.").into());
    }
    String::from_utf8(bytes)
        .map_err(|_| t("There is no preview for this kind of file.").to_string())
}

fn refused_text(why: &Refused) -> String {
    match why {
        Refused::Unsupported => t("Previews of this kind of file are not available yet.").into(),
        Refused::Password => t("The file is password protected.").into(),
        Refused::Damaged => t("The file looks damaged.").into(),
        Refused::TooBig => t("The file is too big to preview.").into(),
        Refused::Timeout => t("The preview took too long.").into(),
        Refused::Failed(why) => why.clone(),
    }
}

fn open_error_text(why: &OpenError) -> String {
    match why {
        OpenError::Encrypted => t("The file is password protected.").into(),
        OpenError::Legacy => {
            t("This is an old Office format; open it in its app, or save it as a newer one.").into()
        }
        OpenError::Damaged(_) => t("The file looks damaged.").into(),
        OpenError::TooBig => t("The file is too big to open here.").into(),
        OpenError::Unsupported => t("Editing this kind of file is not available yet.").into(),
        OpenError::Io(why) => why.clone(),
    }
}

impl RootView {
    /// "Preview" on a changed file, for kinds the view can show.
    pub(super) fn preview_button(
        &self,
        ix: usize,
        path: &str,
        cwd: &str,
        _cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let path = Path::new(cwd).join(path);
        if !previewable(&path) || !path.is_file() {
            return None;
        }
        let root = _cx.entity();
        Some(
            IconButton::new(
                SharedString::from(format!("file-preview-{ix}")),
                IconName::Eye,
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Sm)
            .tooltip(t("Preview"))
            .on_click(move |_, window, cx| {
                let path = path.clone();
                root.update(cx, |r, cx| r.open_preview(path, window, cx))
            })
            .into_any_element(),
        )
    }

    /// Shows `path` in the side panel's Preview view.
    pub(super) fn open_preview(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.preview.doc().is_some_and(|d| d.read(cx).is_dirty())
            && self.preview.path.as_deref() != Some(&path)
        {
            self.toast(
                &Notice::Info(t("Save or discard your edits to this document first.").into()),
                cx,
            );
            self.review.open = true;
            self.review.tab = SideTab::Preview;
            cx.notify();
            return;
        }
        self.review.open = true;
        self.review.tab = SideTab::Preview;
        if self.preview.path.as_deref() != Some(&path) {
            self.preview.mode = Mode::Edit;
        }
        self.preview.path = Some(path);
        self.preview.sheet = 0;
        self.preview.backup = None;
        self.load_preview(window, cx);
    }

    /// Reads the open file again, as it is on disk.
    pub(super) fn load_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.preview.path.clone() else {
            return;
        };
        self.preview.stamp = mtime(&path);
        self.preview.checked = Some(Instant::now());
        self.preview.stale = false;
        self.preview.clash = Clash::None;
        self.preview._doc_events = None;
        self.preview.editable = Format::of(&path).is_some();
        if is_image(&path) {
            self.preview.look = Look::Image(path);
            cx.notify();
            return;
        }
        if matches!(ext(&path).as_str(), "md" | "markdown") {
            self.preview.look = match read_text(&path) {
                Ok(text) => Look::Markdown(text.into()),
                Err(why) => Look::Refused(why),
            };
            cx.notify();
            return;
        }
        self.preview.look = Look::Loading;
        cx.notify();
        let editable = self.preview.editable && self.preview.mode == Mode::Edit;
        let previewer = self.preview.previewer.clone();
        let ask = Ask {
            width_px: (560.0 * window.scale_factor()) as u32,
            pages: 0..20,
            sheet: Some(self.preview.sheet),
            layout: if self.preview.mode == Mode::Pages {
                Layout::Pages
            } else {
                Layout::Grid
            },
        };
        let job = cx.background_spawn(async move {
            if editable {
                match atrium_office::open(&path) {
                    Ok(doc) => return Ok(Ok(doc)),
                    // No editor for it yet: show its pages instead.
                    Err(OpenError::Unsupported) => {}
                    Err(why) => return Err(open_error_text(&why)),
                }
            }
            if atrium_preview::kind(&path) == atrium_preview::Kind::Other {
                return read_text(&path).map(|text| Err(Look::Text(text.into())));
            }
            match previewer.preview(&path, &ask) {
                Ok(preview) => Ok(Err(Look::Rendered(preview))),
                Err(why) => Err(refused_text(&why)),
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            let _ = this.update_in(cx, |root, window, cx| {
                root.preview.look = match result {
                    Ok(Ok(doc)) => {
                        let view = cx.new(|cx| DocView::new(doc, window, cx));
                        root.preview._doc_events =
                            Some(cx.subscribe(&view, |root, _, event, cx| match event {
                                DocEvent::Edited => cx.notify(),
                                DocEvent::Refused(why) => {
                                    root.toast(&Notice::Info(why.clone()), cx)
                                }
                            }));
                        Look::Doc(view)
                    }
                    Ok(Err(look)) => {
                        if root.preview.mode == Mode::Edit && root.preview.editable {
                            // Opened as pages after all.
                            root.preview.mode = Mode::Pages;
                        }
                        look
                    }
                    Err(why) => Look::Refused(why),
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// Called each render while the Preview view shows: notices the file
    /// changing on disk, and saves once the agent stops when asked to.
    pub(super) fn prepare_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.preview.save_when_idle && !self.workspace.read(cx).is_running() {
            self.preview.save_when_idle = false;
            self.save_document(false, window, cx);
        }
        let Some(path) = self.preview.path.clone() else {
            return;
        };
        if self.preview.checked.is_some_and(|at| at.elapsed() < WATCH) {
            return;
        }
        self.preview.checked = Some(Instant::now());
        let now = mtime(&path);
        if now == self.preview.stamp || self.preview.stale {
            return;
        }
        if self.preview.doc().is_some_and(|d| d.read(cx).is_dirty()) {
            self.preview.stale = true;
            cx.notify();
        } else {
            self.load_preview(window, cx);
        }
    }

    /// Saves the open document. `over` writes even when the file changed on disk.
    pub(super) fn save_document(
        &mut self,
        over: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(doc) = self.preview.doc().cloned() else {
            return;
        };
        let Some(path) = self.preview.path.clone() else {
            return;
        };
        if self.workspace.read(cx).is_running() {
            self.preview.save_when_idle = true;
            self.toast(
                &Notice::Info(
                    t("The agent is working. Your edits will be saved when it stops.").into(),
                ),
                cx,
            );
            cx.notify();
            return;
        }
        // A copy to undo the save with.
        let backup = dirs::data_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("Atrium")
            .join("doc-backups");
        let copy = backup.join(format!(
            "{}-{}",
            jiff::Timestamp::now().as_millisecond(),
            path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
        ));
        let backed_up =
            std::fs::create_dir_all(&backup).is_ok() && std::fs::copy(&path, &copy).is_ok();
        let result = doc.update(cx, |d, _| {
            if over {
                d.doc.save_over()
            } else {
                d.doc.save()
            }
        });
        match result {
            Ok(()) => {
                self.preview.clash = Clash::None;
                self.preview.stale = false;
                self.preview.stamp = mtime(&path);
                self.preview.backup = backed_up.then(|| (path.clone(), copy));
                self.hand_over_changes(&doc, &path, cx);
                self.toast(&Notice::Success(t("Saved.").into()), cx);
            }
            Err(SaveError::ChangedOnDisk) => {
                let _ = std::fs::remove_file(&copy);
                self.preview.clash = Clash::ChangedOnDisk;
            }
            Err(SaveError::Unsupported) => {
                let _ = std::fs::remove_file(&copy);
                self.toast(&Notice::Info(t("Saving Word, Excel and PowerPoint files is not available yet. Your edits stay here until you close the file.").into()), cx);
            }
            Err(SaveError::Io(why)) => {
                let _ = std::fs::remove_file(&copy);
                self.toast(&Notice::Error(why), cx);
            }
        }
        let _ = window;
        cx.notify();
    }

    /// Queues what you changed since the agent last saw the file, for your
    /// next message in this conversation.
    fn hand_over_changes(&mut self, doc: &Entity<DocView>, path: &Path, cx: &mut Context<Self>) {
        let (before, after) = {
            let d = doc.read(cx);
            (d.baseline.clone(), d.doc.office.snapshot())
        };
        let changes = atrium_office::changes(&before, &after);
        if changes.is_empty() {
            return;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let lang = if crate::i18n::chinese() {
            atrium_office::Lang::Zh
        } else {
            atrium_office::Lang::En
        };
        let report = atrium_office::describe(&name, &changes, lang);
        doc.update(cx, |d, _| d.baseline = after);
        self.workspace
            .update(cx, |ws, cx| ws.queue_doc_report(path, report, cx));
    }

    fn undo_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, copy)) = self.preview.backup.take() else {
            return;
        };
        match std::fs::copy(&copy, &path) {
            Ok(_) => {
                self.workspace
                    .update(cx, |ws, cx| ws.drop_doc_report(&path, cx));
                self.toast(
                    &Notice::Success(t("The file is back as it was before you saved.").into()),
                    cx,
                );
                self.load_preview(window, cx);
            }
            Err(e) => self.toast(&Notice::Error(e.to_string()), cx),
        }
    }

    fn save_copy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.preview.path.clone() else {
            return;
        };
        let Some(doc) = self.preview.doc().cloned() else {
            return;
        };
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("copy")
            .to_string();
        let picked = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(to))) = picked.await else {
                return;
            };
            let _ = this.update_in(cx, |root, window, cx| {
                match doc.update(cx, |d, _| d.doc.save_as(&to)) {
                    Ok(()) => {
                        root.preview.clash = Clash::None;
                        root.open_preview(to, window, cx);
                    }
                    Err(e) => root.toast(&Notice::Error(e.to_string()), cx),
                }
            });
        })
        .detach();
    }

    pub(super) fn pick_preview_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |root, window, cx| root.open_preview(path, window, cx));
        })
        .detach();
    }

    fn preview_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let path = self.preview.path.clone()?;
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let small = theme.text_size(TextSize::Sm);
        let root = cx.entity();
        let dirty = self.preview.doc().is_some_and(|d| d.read(cx).is_dirty());
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let mut bar = div()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(colors.border)
            .child(
                Icon::new(IconName::FileText)
                    .size(IconSize::Xs)
                    .color(colors.fg_muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(small)
                    .child(if dirty { format!("{name} ●") } else { name }),
            );
        if self.preview.editable {
            let mode = self.preview.mode;
            let switch = |id: &'static str, label: &'static str, which: Mode| {
                let go = root.clone();
                let on = mode == which;
                div()
                    .id(id)
                    .px_2()
                    .py_0p5()
                    .rounded(theme.radius(Radius::Sm))
                    .text_size(theme.text_size(TextSize::Xs))
                    .cursor_pointer()
                    .when(on, |el| el.bg(colors.selection))
                    .when(!on, |el| {
                        el.text_color(colors.fg_muted)
                            .hover(|el| el.bg(colors.hover))
                    })
                    .on_click(move |_, window, cx| {
                        go.update(cx, |r, cx| {
                            if r.preview.mode == which {
                                return;
                            }
                            if r.preview.doc().is_some_and(|d| d.read(cx).is_dirty()) {
                                r.toast(
                                    &Notice::Info(
                                        t("Save or discard your edits to this document first.")
                                            .into(),
                                    ),
                                    cx,
                                );
                                return;
                            }
                            r.preview.mode = which;
                            r.load_preview(window, cx);
                        })
                    })
                    .child(t(label).to_string())
            };
            bar = bar
                .child(switch("preview-edit", "Edit", Mode::Edit))
                .child(switch("preview-pages", "Pages", Mode::Pages));
        }
        if self.preview.backup.is_some() && !dirty {
            let undo = root.clone();
            bar = bar.child(
                Button::new("preview-undo-save", t("Undo save"))
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .icon(IconName::Undo2)
                    .on_click(move |_, window, cx| {
                        undo.update(cx, |r, cx| r.undo_save(window, cx))
                    }),
            );
        }
        if dirty {
            let (discard, save) = (root.clone(), root.clone());
            bar = bar
                .child(
                    Button::new("preview-discard", t("Discard"))
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .on_click(move |_, window, cx| {
                            discard.update(cx, |r, cx| r.load_preview(window, cx))
                        }),
                )
                .child(
                    Button::new(
                        "preview-save",
                        if self.preview.save_when_idle {
                            t("Saving when the agent stops")
                        } else {
                            t("Save")
                        },
                    )
                    .primary()
                    .size(ControlSize::Sm)
                    .icon(IconName::Save)
                    .on_click(move |_, window, cx| {
                        save.update(cx, |r, cx| r.save_document(false, window, cx))
                    }),
                );
        }
        let (reload, open, editor) = (root.clone(), path.clone(), (root.clone(), path.clone()));
        bar = bar
            .child(
                IconButton::new("preview-reload", IconName::RotateCw)
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .tooltip(t("Reload"))
                    .disabled(dirty)
                    .on_click(move |_, window, cx| {
                        reload.update(cx, |r, cx| r.load_preview(window, cx))
                    }),
            )
            .child(
                IconButton::new("preview-open-app", IconName::ExternalLink)
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .tooltip(t("Open with its app"))
                    .on_click(move |_, _, cx| {
                        if let Err(e) = opener::open_default(&open) {
                            log::warn!("open {}: {e:#}", open.display());
                        }
                        let _ = cx;
                    }),
            )
            .child(
                IconButton::new("preview-open-editor", IconName::Code)
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .tooltip(t("Open in editor"))
                    .on_click(move |_, _, cx| {
                        let (root, path) = editor.clone();
                        root.update(cx, |r, cx| r.open_in_editor(&path, None, cx))
                    }),
            );
        Some(bar.into_any_element())
    }

    /// A strip under the bar: the file changed on disk, or a save clashed.
    fn preview_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let small = theme.text_size(TextSize::Sm);
        let root = cx.entity();
        let (text, clash) = if self.preview.clash == Clash::ChangedOnDisk {
            (
                t("The file changed on disk since you opened it, probably by the agent."),
                true,
            )
        } else if self.preview.stale {
            (
                t("The file changed on disk while you were editing it."),
                false,
            )
        } else {
            return None;
        };
        let (reload, over, copy) = (root.clone(), root.clone(), root.clone());
        let mut row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .bg(colors.warning.opacity(0.12))
            .text_size(small)
            .child(div().flex_1().min_w(px(160.)).child(text.to_string()))
            .child(
                Button::new("preview-take-disk", t("Load theirs"))
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .on_click(move |_, window, cx| {
                        reload.update(cx, |r, cx| r.load_preview(window, cx))
                    }),
            );
        if clash || self.preview.stale {
            row = row
                .child(
                    Button::new("preview-overwrite", t("Keep mine"))
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .on_click(move |_, window, cx| {
                            over.update(cx, |r, cx| r.save_document(true, window, cx))
                        }),
                )
                .child(
                    Button::new("preview-save-copy", t("Save a copy…"))
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .on_click(move |_, window, cx| {
                            copy.update(cx, |r, cx| r.save_copy(window, cx))
                        }),
                );
        }
        Some(row.into_any_element())
    }

    fn rendered_body(&self, preview: &Preview, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let tiny = theme.text_size(TextSize::Xs);
        let small = theme.text_size(TextSize::Sm);
        let root = cx.entity();
        match preview {
            Preview::Pages {
                pages,
                total,
                warnings,
            } => {
                let title = self
                    .preview
                    .path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string();
                let shown: Vec<DocPage> = pages
                    .iter()
                    .filter(|p| p.width_pt > 0.0 && p.height_pt > 0.0)
                    .map(|p| DocPage {
                        source: p.png.display().to_string().into(),
                        width: p.width_pt,
                        height: p.height_pt,
                    })
                    .collect();
                let zoom = root.clone();
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .when(!warnings.is_empty(), |el| {
                        el.child(
                            div()
                                .px_3()
                                .py_1()
                                .text_size(tiny)
                                .text_color(colors.fg_muted)
                                .child(
                                    t("Some parts may look different from the original app.")
                                        .to_string(),
                                ),
                        )
                    })
                    .when(*total > pages.len(), |el| {
                        el.child(
                            div()
                                .px_3()
                                .py_1()
                                .text_size(tiny)
                                .text_color(colors.fg_muted)
                                .child(tf(
                                    "Showing {n} of {total} pages",
                                    &[("n", &pages.len()), ("total", total)],
                                )),
                        )
                    })
                    .child(div().flex_1().min_h_0().child(
                        DocumentViewer::new("preview-pages-view", title, shown).zoom(
                            self.preview.zoom,
                            move |z, _, cx| {
                                zoom.update(cx, |r, cx| {
                                    r.preview.zoom = z;
                                    cx.notify();
                                })
                            },
                        ),
                    ))
                    .into_any_element()
            }
            Preview::Table {
                sheets,
                current,
                rows,
                more_rows,
            } => {
                let mut tabs = div().flex().flex_wrap().gap_1().px_2().py_1();
                for (i, name) in sheets.iter().enumerate() {
                    let on = i == *current;
                    let pick = root.clone();
                    tabs = tabs.child(
                        div()
                            .id(SharedString::from(format!("preview-sheet-{i}")))
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
                            .on_click(move |_, window, cx| {
                                pick.update(cx, |r, cx| {
                                    r.preview.sheet = i;
                                    r.load_preview(window, cx);
                                })
                            })
                            .child(name.clone()),
                    );
                }
                let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
                let mut cells = std::collections::HashMap::new();
                let mut merges = Vec::new();
                for (r, row) in rows.iter().enumerate() {
                    for (c, cell) in row.iter().enumerate() {
                        if !cell.text.is_empty() {
                            cells.insert((r, c), SharedString::from(cell.text.clone()));
                        }
                        let (dr, dc) = (cell.span.0.max(1) as usize, cell.span.1.max(1) as usize);
                        if dr > 1 || dc > 1 {
                            merges.push(((r, c), (r + dr - 1, c + dc - 1)));
                        }
                    }
                }
                let mut grid = Spreadsheet::new(
                    SharedString::from(format!("preview-grid-{current}")),
                    rows.len().max(1),
                    cols.max(1),
                )
                .cells(std::rc::Rc::new(cells))
                .freeze(1, 0)
                .size_full();
                for (from, to) in merges {
                    grid = grid.merge(from, to);
                }
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .when(sheets.len() > 1, |el| el.child(tabs))
                    .child(div().flex_1().min_h_0().child(grid))
                    .when(*more_rows, |el| {
                        el.child(
                            div()
                                .px_3()
                                .py_1()
                                .text_size(tiny)
                                .text_color(colors.fg_muted)
                                .child(tf("Showing the first {n} rows", &[("n", &rows.len())])),
                        )
                    })
                    .into_any_element()
            }
        }
    }

    /// The whole Preview view, below the side panel's tabs.
    pub(super) fn preview_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let small = theme.text_size(TextSize::Sm);
        let mono = theme.mono_family.clone();
        let root = cx.entity();
        let pick = root.clone();
        let choose = Button::new("preview-pick", t("Open a file…"))
            .size(ControlSize::Sm)
            .icon(IconName::FolderOpen)
            .on_click(move |_, window, cx| {
                pick.update(cx, |r, cx| r.pick_preview_file(window, cx))
            });
        let body: AnyElement = match &self.preview.look {
            Look::Empty => div()
                .p_4()
                .flex()
                .flex_col()
                .gap_3()
                .text_size(small)
                .text_color(colors.fg_muted)
                .child(t("Open a document, spreadsheet, slides, PDF, picture or Markdown file the agent made. Word, Excel and PowerPoint files can be edited here, and your changes go to the agent with your next message.").to_string())
                .child(div().child(choose))
                .into_any_element(),
            Look::Loading => div()
                .p_4()
                .text_size(small)
                .text_color(colors.fg_muted)
                .child(t("Opening…").to_string())
                .into_any_element(),
            Look::Image(path) => div()
                .id("preview-image")
                .size_full()
                .overflow_y_scroll()
                .p_3()
                .child(img(path.clone()).max_w_full())
                .into_any_element(),
            Look::Markdown(text) => div()
                .id("preview-markdown")
                .size_full()
                .overflow_y_scroll()
                .px_4()
                .py_3()
                .child(MarkdownRenderer::new("preview-md", text.clone()))
                .into_any_element(),
            Look::Text(text) => div()
                .id("preview-text")
                .size_full()
                .overflow_scroll()
                .p_3()
                .font_family(mono)
                .text_size(theme.text_size(TextSize::Xs))
                .whitespace_nowrap()
                .children(text.lines().take(20_000).map(|l| div().child(l.to_string())))
                .into_any_element(),
            Look::Doc(doc) => doc.clone().into_any_element(),
            Look::Rendered(preview) => self.rendered_body(preview, cx),
            Look::Refused(why) => {
                let open = self.preview.path.clone();
                div()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .text_size(small)
                    .child(div().text_color(colors.fg_muted).child(why.clone()))
                    .when_some(open, |el, path| {
                        el.child(
                            div().child(
                                Button::new("preview-open-instead", t("Open with its app"))
                                    .size(ControlSize::Sm)
                                    .icon(IconName::ExternalLink)
                                    .on_click(move |_, _, _| {
                                        if let Err(e) = opener::open_default(&path) {
                                            log::warn!("open {}: {e:#}", path.display());
                                        }
                                    }),
                            ),
                        )
                    })
                    .into_any_element()
            }
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .children(self.preview_bar(cx))
            .children(self.preview_notice(cx))
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }
}
