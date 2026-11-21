//! The side panel's working tools for a conversation's folder: its git
//! changes (commit, push, pull request, undo a file or a hunk) and a
//! terminal.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use atrium_core::ChangeKind;
use atrium_project::repo::{self, ChangeState};
use ely_gpui_component::{
    buttons::{Button, ButtonVariant, IconButton},
    forms::{Input, TextInput},
    primitives::{Icon, IconName},
    terminal::{Launch, Terminal},
    theme::{ActiveTheme, ControlSize, IconSize, Radius, TextSize},
};
use gpui::{
    AnyElement, Context, Entity, Focusable, IntoElement, ParentElement, SharedString, Styled,
    Window, div, prelude::*,
};

use super::RootView;
use super::changes::{ChangedFile, LineKind, SideTab, parse_diff};
use crate::i18n::{t, tf};
use crate::opener;
use crate::workspace::Notice;

/// How often the open changes view re-reads git while things happen.
const REFRESH: Duration = Duration::from_secs(3);

/// Git as last read for one folder.
pub struct GitView {
    pub dir: PathBuf,
    /// `None` when the folder is not in a repository.
    pub status: Option<repo::Status>,
    pub files: Vec<(ChangedFile, repo::Change)>,
}

pub struct Tools {
    pub git: Option<GitView>,
    loading: bool,
    loaded_at: Option<Instant>,
    /// What a running git action is doing, such as "Pushing…".
    pub busy: Option<String>,
    pub commit_input: Entity<TextInput>,
    terminals: HashMap<PathBuf, Entity<Terminal>>,
    /// The Run view: its command box, the folder it was filled for, and
    /// what runs, by folder.
    pub run_input: Entity<TextInput>,
    pub run_dir: Option<PathBuf>,
    pub runs: HashMap<PathBuf, Entity<Terminal>>,
    /// Editors found when Atrium started.
    pub editors: Vec<opener::Editor>,
}

impl Tools {
    pub fn new(window: &mut Window, cx: &mut Context<RootView>) -> Self {
        Self {
            git: None,
            loading: false,
            loaded_at: None,
            busy: None,
            commit_input: cx.new(|cx| {
                TextInput::new(window, cx)
                    .multi_line(1, 4)
                    .placeholder(t("Commit message"))
            }),
            terminals: HashMap::new(),
            run_input: cx.new(|cx| {
                TextInput::new(window, cx).placeholder(t("Command, such as npm run dev"))
            }),
            run_dir: None,
            runs: HashMap::new(),
            editors: opener::installed(),
        }
    }
}

fn change_kind(state: ChangeState) -> ChangeKind {
    match state {
        ChangeState::Added | ChangeState::Untracked => ChangeKind::Add,
        ChangeState::Deleted => ChangeKind::Delete,
        _ => ChangeKind::Update { move_to: None },
    }
}

/// Reads git for `dir`: the status, and each changed file's diff.
fn read_git(dir: PathBuf) -> GitView {
    let Ok(status) = repo::status(&dir) else {
        return GitView {
            dir,
            status: None,
            files: Vec::new(),
        };
    };
    let files = status
        .changes
        .iter()
        .take(200)
        .map(|change| {
            let diff = repo::diff(&status.root, change).unwrap_or_default();
            let kind = change_kind(change.state);
            let lines = parse_diff(&diff, &kind);
            let added = lines.iter().filter(|l| l.kind == LineKind::Added).count();
            let removed = lines.iter().filter(|l| l.kind == LineKind::Removed).count();
            let file = ChangedFile {
                path: status.root.join(&change.path).display().to_string(),
                kind,
                lines,
                added,
                removed,
            };
            (file, change.clone())
        })
        .collect();
    GitView {
        dir,
        status: Some(status),
        files,
    }
}

impl RootView {
    /// The folder of the open conversation, when it is on this computer.
    pub(super) fn chat_dir(&self, cx: &gpui::App) -> Option<PathBuf> {
        let ws = self.workspace.read(cx);
        let thread = ws.active_thread()?;
        if crate::agents::split_backend(&thread.backend).1.is_some() {
            return None;
        }
        let dir = PathBuf::from(&thread.cwd);
        dir.is_dir().then_some(dir)
    }

    /// The git view for the open conversation, once read.
    pub(super) fn git_view(&self, cx: &gpui::App) -> Option<&GitView> {
        let dir = self.chat_dir(cx)?;
        self.tools.git.as_ref().filter(|g| g.dir == dir)
    }

    /// Called each render: keeps git fresh while the changes view shows, and
    /// starts the terminal when its view shows.
    pub(super) fn prepare_side(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.review.open {
            return;
        }
        if self.review.tab == SideTab::Preview {
            self.prepare_preview(window, cx);
            return;
        }
        let Some(dir) = self.chat_dir(cx) else {
            return;
        };
        match self.review.tab {
            SideTab::Changes => {
                let stale = self.tools.git.as_ref().is_none_or(|g| g.dir != dir)
                    || self.tools.loaded_at.is_none_or(|at| at.elapsed() > REFRESH);
                if stale && !self.tools.loading {
                    self.refresh_git(cx);
                }
            }
            SideTab::Terminal if !self.tools.terminals.contains_key(&dir) => {
                let launch = Launch {
                    cwd: Some(dir.clone()),
                    ..Launch::default()
                };
                let mut failed = None;
                let term = cx.new(|cx| match Terminal::spawn(launch, cx) {
                    Ok(term) => term,
                    Err(e) => {
                        failed = Some(e);
                        Terminal::replay(b"", 80, 24, cx)
                    }
                });
                match failed {
                    None => {
                        term.read(cx).focus_handle(cx).focus(window, cx);
                        self.tools.terminals.insert(dir, term);
                    }
                    Some(e) => {
                        self.review.tab = SideTab::Changes;
                        self.toast(&Notice::Error(format!("{e:#}")), cx);
                    }
                }
            }
            SideTab::Run => self.prepare_run(&dir, cx),
            _ => {}
        }
    }

    pub(super) fn refresh_git(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.chat_dir(cx) else {
            return;
        };
        self.tools.loading = true;
        let job = cx.background_spawn(async move { read_git(dir) });
        cx.spawn(async move |this, cx| {
            let view = job.await;
            let _ = this.update(cx, |root, cx| {
                root.tools.git = Some(view);
                root.tools.loading = false;
                root.tools.loaded_at = Some(Instant::now());
                cx.notify();
            });
        })
        .detach();
    }

    /// Runs a git action off the UI thread, then says how it went.
    fn git_action(
        &mut self,
        doing: &str,
        action: impl FnOnce(&Path) -> anyhow::Result<String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self
            .git_view(cx)
            .and_then(|g| g.status.as_ref())
            .map(|s| s.root.clone())
        else {
            return;
        };
        if self.tools.busy.is_some() {
            return;
        }
        self.tools.busy = Some(doing.to_string());
        cx.notify();
        let job = cx.background_spawn(async move { action(&root) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |root, cx| {
                root.tools.busy = None;
                match result {
                    Ok(done) if done.starts_with("https://") => {
                        cx.open_url(&done);
                        root.toast(
                            &Notice::Success(tf("Pull request opened: {url}", &[("url", &done)])),
                            cx,
                        );
                    }
                    Ok(done) if !done.is_empty() => root.toast(&Notice::Success(done), cx),
                    Ok(_) => {}
                    Err(e) => root.toast(&Notice::Error(format!("{e:#}")), cx),
                }
                root.refresh_git(cx);
            });
        })
        .detach();
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        let message = self.tools.commit_input.read(cx).text().to_string();
        if message.trim().is_empty() {
            self.toast(&Notice::Info(t("Write a commit message first.").into()), cx);
            return;
        }
        self.tools
            .commit_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.git_action(
            t("Committing…"),
            move |root| {
                let id = repo::commit_all(root, &message)?;
                Ok(tf("Committed {id}", &[("id", &id)]))
            },
            cx,
        );
    }

    pub(super) fn revert_file(&mut self, change: repo::Change, cx: &mut Context<Self>) {
        self.git_action(
            t("Undoing…"),
            move |root| {
                repo::revert_file(root, &change)?;
                Ok(tf("Undid the changes to {path}", &[("path", &change.path)]))
            },
            cx,
        );
    }

    pub(super) fn revert_hunk(
        &mut self,
        change: repo::Change,
        hunk: usize,
        cx: &mut Context<Self>,
    ) {
        self.git_action(
            t("Undoing…"),
            move |root| {
                repo::revert_hunk(root, &change, hunk)?;
                Ok(String::new())
            },
            cx,
        );
    }

    /// The editor files open in: the chosen one, else the first installed.
    pub(super) fn editor(&self, cx: &gpui::App) -> Option<opener::Editor> {
        let chosen = self.workspace.read(cx).settings.editor.clone();
        chosen
            .and_then(|c| self.tools.editors.iter().find(|e| e.command == c).cloned())
            .or_else(|| self.tools.editors.first().cloned())
    }

    /// Shows the conversation's folder in the file manager.
    pub(super) fn reveal_chat_dir(&mut self, cx: &mut Context<Self>) {
        if let Some(dir) = self.chat_dir(cx)
            && let Err(e) = opener::reveal(&dir)
        {
            self.toast(&Notice::Error(format!("{e:#}")), cx);
        }
    }

    /// The open conversation as Markdown, with its title.
    fn markdown(&self, cx: &gpui::App) -> Option<(String, String)> {
        let ws = self.workspace.read(cx);
        let thread = ws.active_thread()?;
        let title = thread.title();
        let text = crate::export::to_markdown(&title, &thread.cwd, ws.transcript()?);
        Some((title, text))
    }

    pub(super) fn copy_markdown(&mut self, cx: &mut Context<Self>) {
        if let Some((_, text)) = self.markdown(cx) {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
            self.toast(&Notice::Success(t("Copied as Markdown").into()), cx);
        }
    }

    /// Asks where to save the conversation as a Markdown file.
    pub(super) fn export_markdown(&mut self, cx: &mut Context<Self>) {
        let Some((title, text)) = self.markdown(cx) else {
            return;
        };
        let dir = self
            .chat_dir(cx)
            .or_else(dirs::download_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_default();
        let picked = cx.prompt_for_new_path(&dir, Some(&crate::export::file_name(&title)));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = picked.await else {
                return;
            };
            let result = std::fs::write(&path, text);
            let _ = this.update(cx, |root, cx| match result {
                Ok(()) => root.toast(
                    &Notice::Success(tf("Saved to {path}", &[("path", &path.display())])),
                    cx,
                ),
                Err(e) => root.toast(&Notice::Error(e.to_string()), cx),
            });
        })
        .detach();
    }

    /// Opens a file of the conversation, at `line`, in the preferred editor.
    pub(super) fn open_in_editor(
        &mut self,
        path: &Path,
        line: Option<u32>,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.editor(cx) else {
            if let Err(e) = opener::reveal(path) {
                self.toast(&Notice::Error(format!("{e:#}")), cx);
            }
            return;
        };
        if let Err(e) = opener::open_in(&editor, path, line) {
            self.toast(&Notice::Error(format!("{e:#}")), cx);
        }
    }

    /// The branch, the commit box and push / pull request, above the files.
    pub(super) fn git_bar(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let status = self.git_view(cx)?.status.as_ref()?;
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let tiny = theme.text_size(TextSize::Xs);
        let small = theme.text_size(TextSize::Sm);
        let root = cx.entity();
        let busy = self.tools.busy.clone();
        let dirty = !status.changes.is_empty();
        let unpushed = status.upstream.is_none() || status.ahead > 0;
        let mut where_line = div()
            .flex()
            .items_center()
            .gap_2()
            .text_size(small)
            .child(
                Icon::new(IconName::GitBranch)
                    .size(IconSize::Xs)
                    .color(colors.fg_muted),
            )
            .child(
                div().font_weight(gpui::FontWeight::MEDIUM).child(
                    status
                        .branch
                        .clone()
                        .unwrap_or_else(|| t("detached").into()),
                ),
            );
        if let Some(up) = &status.upstream {
            let mut sync = Vec::new();
            if status.ahead > 0 {
                sync.push(tf("{n} to push", &[("n", &status.ahead)]));
            }
            if status.behind > 0 {
                sync.push(tf("{n} behind", &[("n", &status.behind)]));
            }
            where_line =
                where_line.child(div().text_size(tiny).text_color(colors.fg_subtle).child(
                    if sync.is_empty() {
                        tf("up to date with {up}", &[("up", up)])
                    } else {
                        sync.join(" · ")
                    },
                ));
        } else if status.branch.is_some() {
            where_line = where_line.child(
                div()
                    .text_size(tiny)
                    .text_color(colors.fg_subtle)
                    .child(t("not pushed yet")),
            );
        }
        where_line = where_line
            .child(div().flex_1())
            .children(busy.clone().map(|b| {
                div()
                    .text_size(tiny)
                    .text_color(colors.accent)
                    .child(b)
                    .into_any_element()
            }));
        let refresh = root.clone();
        where_line = where_line.child(
            IconButton::new("git-refresh", IconName::RefreshCw)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Sm)
                .tooltip(t("Refresh"))
                .on_click(move |_, _, cx| refresh.update(cx, |r, cx| r.refresh_git(cx))),
        );

        let (commit, push, pr) = (root.clone(), root.clone(), root.clone());
        let enabled = busy.is_none();
        let actions = div()
            .flex()
            .items_center()
            .gap_1()
            .child(div().flex_1())
            .child(
                Button::new("git-push", t("Push"))
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .icon(IconName::Upload)
                    .disabled(!enabled || !unpushed || status.branch.is_none())
                    .on_click(move |_, _, cx| {
                        push.update(cx, |r, cx| {
                            r.git_action(
                                t("Pushing…"),
                                |root| {
                                    repo::push(root)?;
                                    Ok(t("Pushed").into())
                                },
                                cx,
                            )
                        })
                    }),
            )
            .child(
                Button::new("git-pr", t("Create PR"))
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .icon(IconName::GitPullRequest)
                    .disabled(!enabled || status.branch.is_none())
                    .on_click(move |_, _, cx| {
                        pr.update(cx, |r, cx| {
                            r.git_action(
                                t("Opening a pull request…"),
                                |root| {
                                    repo::push(root)?;
                                    repo::create_pr(root)
                                },
                                cx,
                            )
                        })
                    }),
            )
            .child(
                Button::new("git-commit", t("Commit all"))
                    .size(ControlSize::Sm)
                    .icon(IconName::GitCommitHorizontal)
                    .disabled(!enabled || !dirty)
                    .on_click(move |_, _, cx| commit.update(cx, |r, cx| r.commit(cx))),
            );
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .border_b_1()
                .border_color(colors.border)
                .child(where_line)
                .when(dirty, |el| el.child(Input::new(&self.tools.commit_input)))
                .child(actions)
                .into_any_element(),
        )
    }

    /// A file's undo and open buttons, for its card's header in git mode.
    pub(super) fn file_buttons(
        &self,
        ix: usize,
        file: &ChangedFile,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let (_, change) = self
            .git_view(cx)?
            .files
            .iter()
            .find(|(f, _)| f.path == file.path)?;
        let root = cx.entity();
        let (undo, open) = (
            (root.clone(), change.clone()),
            (root, PathBuf::from(&file.path)),
        );
        let enabled = self.tools.busy.is_none();
        Some(
            div()
                .flex()
                .items_center()
                .child(
                    IconButton::new(
                        SharedString::from(format!("file-open-{ix}")),
                        IconName::ExternalLink,
                    )
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .tooltip(t("Open in editor"))
                    .on_click(move |_, _, cx| {
                        let (root, path) = open.clone();
                        root.update(cx, |r, cx| r.open_in_editor(&path, None, cx))
                    }),
                )
                .child(
                    IconButton::new(
                        SharedString::from(format!("file-undo-{ix}")),
                        IconName::Undo2,
                    )
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .disabled(!enabled)
                    .tooltip(t("Undo this file's changes"))
                    .on_click(move |_, _, cx| {
                        let (root, change) = undo.clone();
                        root.update(cx, |r, cx| r.revert_file(change, cx))
                    }),
                )
                .into_any_element(),
        )
    }

    /// "Undo" on a hunk's header line, in git mode.
    pub(super) fn hunk_button(
        &self,
        file: &ChangedFile,
        hunk: usize,
        id: SharedString,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let (_, change) = self
            .git_view(cx)?
            .files
            .iter()
            .find(|(f, _)| f.path == file.path)?;
        let undo = (cx.entity(), change.clone());
        Some(
            Button::new(id, t("Undo"))
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Sm)
                .icon(IconName::Undo2)
                .disabled(self.tools.busy.is_some())
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    let (root, change) = undo.clone();
                    root.update(cx, |r, cx| r.revert_hunk(change, hunk, cx))
                })
                .into_any_element(),
        )
    }

    /// The terminal view: a shell in the conversation's folder.
    pub(super) fn terminal_body(&self, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors.clone();
        let small = cx.theme().text_size(TextSize::Sm);
        let Some(dir) = self.chat_dir(cx) else {
            return div()
                .p_3()
                .text_size(small)
                .text_color(colors.fg_muted)
                .child(t(
                    "A terminal opens in a conversation's folder on this computer.",
                ))
                .into_any_element();
        };
        match self.tools.terminals.get(&dir) {
            Some(term) => div()
                .size_full()
                .rounded(cx.theme().radius(Radius::Md))
                .child(term.clone())
                .into_any_element(),
            None => div().into_any_element(),
        }
    }
}

impl RootView {
    /// Follows an `atrium://` link.
    pub fn open_link(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        use crate::links::{Link, parse};
        let Some(link) = parse(url) else {
            self.toast(
                &Notice::Error(tf("Atrium cannot open {url}", &[("url", &url)])),
                cx,
            );
            return;
        };
        match link {
            Link::New {
                prompt,
                folder,
                send,
            } => {
                self.workspace.update(cx, |ws, cx| {
                    if let Some(dir) = folder.filter(|d| d.is_dir()) {
                        ws.set_working_dir(dir, cx);
                    }
                    ws.enter_chats(cx);
                    if send && !prompt.trim().is_empty() {
                        ws.send(crate::workspace::Composed::text(prompt), cx);
                    } else if !prompt.is_empty() {
                        cx.emit(crate::workspace::Fill(crate::workspace::Composed::text(
                            prompt,
                        )));
                    }
                });
                super::composer::Composer::focus_entity(&self.composer, window, cx);
            }
            Link::Stream { project, stream } => self.workspace.update(cx, |ws, cx| {
                let Some(root) = ws.projects.get(project).map(|p| p.root.clone()) else {
                    cx.emit(Notice::Error(t("That project is no longer open.").into()));
                    return;
                };
                ws.enter_project(root, cx);
                ws.show_stream(&stream, cx);
            }),
            Link::Chat(id) => self.workspace.update(cx, |ws, cx| ws.open_thread(id, cx)),
        }
        cx.notify();
    }
}
