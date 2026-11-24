//! The changes pane: every file the agent changed in this conversation, as
//! diffs you can comment on line by line, then send back all at once.

use atrium_core::{ChangeKind, EntryKind, Item, Transcript};
use ely_gpui_component::{
    buttons::{Button, ButtonVariant, IconButton},
    forms::Input,
    primitives::{Icon, IconName},
    theme::{ActiveTheme, ControlSize, IconSize, Radius, TextSize},
};
use gpui::{
    AnyElement, Context, Focusable, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::*, px,
};

use super::RootView;
use crate::i18n::{t, tf};
use crate::workspace::Composed;

/// Lines of one file drawn before the rest is folded away.
const MAX_LINES: usize = 600;

pub use crate::engine_view::DiffKind as LineKind;
pub use crate::engine_view::DiffLine;

/// One changed file, with every diff the agent made to it, in order.
#[derive(Clone, Debug, PartialEq)]
pub struct ChangedFile {
    pub path: String,
    pub kind: ChangeKind,
    pub lines: Vec<DiffLine>,
    pub added: usize,
    pub removed: usize,
}

/// A comment on one line of a changed file.
#[derive(Clone, Debug, PartialEq)]
pub struct LineComment {
    pub path: String,
    /// Index into the file's `lines`, to draw it under its line.
    pub row: usize,
    pub line: Option<u32>,
    /// The line's text, so the agent finds it even when numbers moved.
    pub quote: String,
    pub text: String,
}

/// Review state kept by the root view.
#[derive(Default)]
pub struct Review {
    pub open: bool,
    /// Which of the side panel's views shows.
    pub tab: SideTab,
    /// Conversations watched side by side, by thread id.
    pub watched: Vec<String>,
    pub comments: Vec<LineComment>,
    /// The line being commented on: `(path, row)`.
    pub target: Option<(String, usize)>,
    /// Two columns of removed and added lines instead of one unified column.
    pub diff_side_by_side: bool,
}

/// The side panel's views.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SideTab {
    #[default]
    Changes,
    Overview,
    Activity,
    Watch,
    Terminal,
    Run,
    Preview,
    /// The template an authoring chat is writing.
    Draft,
}

/// Reads a unified diff leniently: hunk headers with or without numbers, and
/// a new file's plain text as all added.
pub fn parse_diff(diff: &str, kind: &ChangeKind) -> Vec<DiffLine> {
    let unified = diff.lines().any(|l| l.starts_with("@@"));
    let mut out = Vec::new();
    // git's header for a binary file: nothing to show line by line.
    if !unified && diff.starts_with("diff --git") {
        if diff.lines().any(|l| l.starts_with("Binary files")) {
            out.push(DiffLine {
                kind: LineKind::Context,
                old: None,
                new: None,
                text: crate::i18n::t("Binary file").to_string(),
            });
        }
        return out;
    }
    if !unified {
        let added = !matches!(kind, ChangeKind::Delete);
        for (i, text) in diff.lines().enumerate() {
            let n = i as u32 + 1;
            out.push(DiffLine {
                kind: if added {
                    LineKind::Added
                } else {
                    LineKind::Removed
                },
                old: (!added).then_some(n),
                new: added.then_some(n),
                text: text.to_string(),
            });
        }
        return out;
    }
    let (mut old, mut new) = (1u32, 1u32);
    for line in diff.lines() {
        if line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("diff ")
            || line.starts_with("index ")
        {
            continue;
        }
        if let Some(rest) = line.strip_prefix("@@") {
            // `@@ -12,5 +12,7 @@ fn name`
            let mut parts = rest.split_whitespace();
            let start = |p: Option<&str>, sign: char| {
                p.and_then(|p| p.strip_prefix(sign))
                    .and_then(|p| p.split(',').next())
                    .and_then(|n| n.parse::<u32>().ok())
            };
            if let Some(o) = start(parts.next(), '-') {
                old = o.max(1);
            }
            if let Some(n) = start(parts.next(), '+') {
                new = n.max(1);
            }
            out.push(DiffLine {
                kind: LineKind::Hunk,
                old: None,
                new: None,
                text: line.to_string(),
            });
            continue;
        }
        let (kind, text) = match line.chars().next() {
            Some('+') => (LineKind::Added, &line[1..]),
            Some('-') => (LineKind::Removed, &line[1..]),
            Some(' ') => (LineKind::Context, &line[1..]),
            Some('\\') => continue,
            // Header lines git adds (`new file mode`, `similarity index`…).
            Some(_) => continue,
            None => (LineKind::Context, line),
        };
        let (o, n) = match kind {
            LineKind::Added => (None, Some(new)),
            LineKind::Removed => (Some(old), None),
            _ => (Some(old), Some(new)),
        };
        if o.is_some() {
            old += 1;
        }
        if n.is_some() {
            new += 1;
        }
        out.push(DiffLine {
            kind,
            old: o,
            new: n,
            text: text.to_string(),
        });
    }
    out
}

/// Every file changed in `transcript`, in the order first changed.
pub fn changed_files(transcript: &Transcript) -> Vec<ChangedFile> {
    let mut files: Vec<ChangedFile> = Vec::new();
    for entry in transcript.entries() {
        let EntryKind::Item(Item::FileChange { changes, .. }) = &entry.kind else {
            continue;
        };
        for change in changes {
            let lines = parse_diff(&change.diff, &change.kind);
            let added = lines.iter().filter(|l| l.kind == LineKind::Added).count();
            let removed = lines.iter().filter(|l| l.kind == LineKind::Removed).count();
            match files.iter_mut().find(|f| f.path == change.path) {
                Some(file) => {
                    file.kind = change.kind.clone();
                    file.lines.extend(lines);
                    file.added += added;
                    file.removed += removed;
                }
                None => files.push(ChangedFile {
                    path: change.path.clone(),
                    kind: change.kind.clone(),
                    lines,
                    added,
                    removed,
                }),
            }
        }
    }
    files
}

/// The message that carries the comments back to the agent.
pub fn compose(comments: &[LineComment]) -> String {
    let mut text = String::from(
        "Review comments on your changes. Address each one, then say what you changed:\n",
    );
    for c in comments {
        let at = match c.line {
            Some(n) => format!("{}:{n}", c.path),
            None => c.path.clone(),
        };
        text.push_str(&format!("\n- `{at}`"));
        if !c.quote.trim().is_empty() {
            text.push_str(&format!(" (`{}`)", c.quote.trim()));
        }
        text.push_str(&format!(": {}", c.text.trim()));
    }
    text
}

/// The path relative to the conversation's folder, when inside it.
fn short(path: &str, cwd: &str) -> String {
    path.strip_prefix(cwd)
        .map(|p| p.trim_start_matches('/').to_string())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| path.to_string())
}

impl RootView {
    pub(super) fn toggle_changes(&mut self, cx: &mut Context<Self>) {
        self.toggle_side(SideTab::Changes, cx);
    }

    /// Opens the side panel on `tab`, or closes it when `tab` already shows.
    pub(super) fn toggle_side(&mut self, tab: SideTab, cx: &mut Context<Self>) {
        if self.review.open && self.review.tab == tab {
            self.review.open = false;
        } else {
            self.review.open = true;
            self.review.tab = tab;
        }
        cx.notify();
    }

    fn add_comment(&mut self, file: &ChangedFile, row: usize, cx: &mut Context<Self>) {
        let text = self.comment_input.read(cx).text().to_string();
        if text.trim().is_empty() {
            return;
        }
        let line = &file.lines[row];
        self.review.comments.push(LineComment {
            path: file.path.clone(),
            row,
            line: line.new.or(line.old),
            quote: line.text.chars().take(120).collect(),
            text,
        });
        self.review.target = None;
        self.comment_input
            .update(cx, |input, cx| input.set_text("", cx));
        cx.notify();
    }

    fn send_review(&mut self, cx: &mut Context<Self>) {
        if self.review.comments.is_empty() {
            return;
        }
        let text = compose(&self.review.comments);
        self.review.comments.clear();
        self.review.target = None;
        self.workspace
            .update(cx, |ws, cx| ws.send(Composed::text(text), cx));
        cx.notify();
    }

    /// The pane beside the chat; `None` while closed or with no conversation.
    pub(super) fn changes_pane(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.review.open {
            return None;
        }
        let ws = self.workspace.read(cx);
        // In a repository the view is git's: everything uncommitted, which
        // can be committed or undone. Elsewhere, the files the agent changed.
        let git_files = self
            .git_view(cx)
            .filter(|g| g.status.is_some())
            .map(|g| g.files.iter().map(|(f, _)| f.clone()).collect::<Vec<_>>());
        let in_git = git_files.is_some();
        let files =
            git_files.unwrap_or_else(|| ws.transcript().map(changed_files).unwrap_or_default());
        let cwd = ws
            .active_thread()
            .map(|t| t.cwd.clone())
            .unwrap_or_default();
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let tiny = theme.text_size(TextSize::Xs);
        let small = theme.text_size(TextSize::Sm);
        let mono = theme.mono_family.clone();
        let root = cx.entity();
        let (added, removed) = files
            .iter()
            .fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));

        let header = self.side_tabs(
            Some(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(tiny)
                    .child(
                        div()
                            .text_color(colors.fg_subtle)
                            .child(tf("{n} files", &[("n", &files.len())])),
                    )
                    .child(div().text_color(colors.success).child(format!("+{added}")))
                    .child(div().text_color(colors.danger).child(format!("−{removed}")))
                    .child({
                        let root = root.clone();
                        let side = self.review.diff_side_by_side;
                        IconButton::new(
                            "diff-layout",
                            if side {
                                IconName::List
                            } else {
                                IconName::Columns2
                            },
                        )
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip(if side {
                            t("Unified")
                        } else {
                            t("Side by side")
                        })
                        .on_click(move |_, _, cx| {
                            root.update(cx, |root, cx| {
                                root.review.diff_side_by_side = !root.review.diff_side_by_side;
                                cx.notify();
                            })
                        })
                    })
                    .into_any_element(),
            ),
            cx,
        );
        if self.review.tab == SideTab::Preview {
            return Some(
                div()
                    .flex_none()
                    .w(px(600.))
                    .h_full()
                    .flex()
                    .flex_col()
                    .border_l_1()
                    .border_color(colors.border)
                    .bg(colors.bg)
                    .child(header)
                    .child(div().flex_1().min_h_0().child(self.preview_body(cx)))
                    .into_any_element(),
            );
        }
        if matches!(self.review.tab, SideTab::Terminal | SideTab::Run) {
            let body = if self.review.tab == SideTab::Run {
                self.run_body(cx)
            } else {
                div()
                    .size_full()
                    .p_1()
                    .child(self.terminal_body(cx))
                    .into_any_element()
            };
            return Some(
                div()
                    .flex_none()
                    .w(px(480.))
                    .h_full()
                    .flex()
                    .flex_col()
                    .border_l_1()
                    .border_color(colors.border)
                    .bg(colors.bg)
                    .child(header)
                    .child(div().flex_1().min_h_0().child(body))
                    .into_any_element(),
            );
        }
        if self.review.tab != SideTab::Changes {
            let body = match self.review.tab {
                SideTab::Activity => self.activity_body(cx),
                SideTab::Overview => self.overview_body(cx),
                SideTab::Draft => self.draft_body(cx),
                _ => self.watch_body(cx),
            };
            return Some(Self::side_frame(header, body, None, cx));
        }

        let mut body = div().flex().flex_col().gap_3().p_3();
        if files.is_empty() {
            body = body.child(div().text_size(small).text_color(colors.fg_muted).child(
                if in_git {
                    t("Nothing uncommitted.")
                } else {
                    t("No file changes in this conversation yet.")
                },
            ));
        }
        for (file_ix, file) in files.iter().enumerate() {
            let mut card = div()
                .flex()
                .flex_col()
                .rounded(theme.radius(Radius::Md))
                .border_1()
                .border_color(colors.border)
                .overflow_hidden()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .bg(colors.sunken)
                        .border_b_1()
                        .border_color(colors.border)
                        .text_size(small)
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
                                .child(short(&file.path, &cwd)),
                        )
                        .child(div().text_size(tiny).text_color(colors.fg_subtle).child(
                            match file.kind {
                                ChangeKind::Add => t("new").to_string(),
                                ChangeKind::Delete => t("deleted").to_string(),
                                ChangeKind::Update { .. } => String::new(),
                            },
                        ))
                        .child(
                            div()
                                .text_size(tiny)
                                .text_color(colors.success)
                                .child(format!("+{}", file.added)),
                        )
                        .child(
                            div()
                                .text_size(tiny)
                                .text_color(colors.danger)
                                .child(format!("−{}", file.removed)),
                        )
                        .children(self.preview_button(file_ix, &file.path, &cwd, cx))
                        .children(self.open_file_button(file_ix, &file.path, &cwd, cx))
                        .children(self.file_buttons(file_ix, file, cx)),
                );
            if self.review.diff_side_by_side {
                for (side_ix, side) in crate::engine_view::side_by_side(&file.lines)
                    .into_iter()
                    .enumerate()
                {
                    if let Some(hunk) = side.hunk {
                        card = card.child(
                            div()
                                .flex()
                                .items_start()
                                .font_family(mono.clone())
                                .text_size(tiny)
                                .bg(colors.selection.opacity(0.5))
                                .child(div().flex_1().px_2().child(hunk)),
                        );
                        continue;
                    }
                    let cell = |cell: Option<crate::engine_view::SideCell>| {
                        let Some(cell) = cell else {
                            return div().flex_1().min_w_0().into_any_element();
                        };
                        let (bg, sign) = match cell.kind {
                            crate::engine_view::DiffKind::Added => {
                                (Some(colors.success.opacity(0.10)), "+")
                            }
                            crate::engine_view::DiffKind::Removed => {
                                (Some(colors.danger.opacity(0.10)), "−")
                            }
                            _ => (None, " "),
                        };
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_start()
                            .when_some(bg, |el, bg| el.bg(bg))
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(30.))
                                    .text_right()
                                    .pr_1()
                                    .text_color(colors.fg_subtle)
                                    .child(cell.number.map(|n| n.to_string()).unwrap_or_default()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(10.))
                                    .text_color(colors.fg_subtle)
                                    .child(sign),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .whitespace_normal()
                                    .text_color(colors.fg)
                                    .child(if cell.text.is_empty() {
                                        " ".to_string()
                                    } else {
                                        cell.text
                                    }),
                            )
                            .into_any_element()
                    };
                    card = card.child(
                        div()
                            .id(SharedString::from(format!("side-{file_ix}-{side_ix}")))
                            .flex()
                            .items_start()
                            .font_family(mono.clone())
                            .text_size(tiny)
                            .border_l_1()
                            .border_color(colors.border)
                            .child(cell(side.left))
                            .child(cell(side.right)),
                    );
                }
            } else {
                let mut hunk = 0usize;
                for (row, line) in file.lines.iter().enumerate().take(MAX_LINES) {
                    let hunk_undo = if line.kind == LineKind::Hunk {
                        hunk += 1;
                        self.hunk_button(
                            file,
                            hunk - 1,
                            SharedString::from(format!("hunk-undo-{file_ix}-{row}")),
                            cx,
                        )
                    } else {
                        None
                    };
                    let (bg, sign) = match line.kind {
                        LineKind::Added => (Some(colors.success.opacity(0.10)), "+"),
                        LineKind::Removed => (Some(colors.danger.opacity(0.10)), "−"),
                        LineKind::Hunk => (Some(colors.selection.opacity(0.5)), ""),
                        LineKind::Context => (None, " "),
                    };
                    let number = |n: Option<u32>| {
                        div()
                            .flex_none()
                            .w(px(34.))
                            .text_right()
                            .pr_1()
                            .text_color(colors.fg_subtle)
                            .child(n.map(|n| n.to_string()).unwrap_or_default())
                    };
                    let targeted = self.review.target.as_ref() == Some(&(file.path.clone(), row));
                    let pick = (root.clone(), file.path.clone());
                    let commentable = line.kind != LineKind::Hunk;
                    card = card.child(
                        div()
                            .id(SharedString::from(format!("diff-{file_ix}-{row}")))
                            .flex()
                            .items_start()
                            .font_family(mono.clone())
                            .text_size(tiny)
                            .when_some(bg, |el, bg| el.bg(bg))
                            .when(targeted, |el| el.bg(colors.accent.opacity(0.15)))
                            .when(commentable, |el| {
                                el.cursor_pointer()
                                    .hover(|el| el.bg(colors.hover))
                                    .on_click(move |_, window, cx| {
                                        let (root, path) = pick.clone();
                                        root.update(cx, |r, cx| {
                                            r.review.target = Some((path, row));
                                            let focus = r.comment_input.read(cx).focus_handle(cx);
                                            focus.focus(window, cx);
                                            cx.notify();
                                        })
                                    })
                            })
                            .child(number(line.old))
                            .child(number(line.new))
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(12.))
                                    .text_color(colors.fg_subtle)
                                    .child(sign),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .whitespace_normal()
                                    .text_color(if line.kind == LineKind::Hunk {
                                        colors.fg_subtle
                                    } else {
                                        colors.fg
                                    })
                                    .child(if line.text.is_empty() {
                                        " ".to_string()
                                    } else {
                                        line.text.clone()
                                    }),
                            )
                            .children(hunk_undo),
                    );
                    for (ix, comment) in self
                        .review
                        .comments
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| c.path == file.path && c.row == row)
                    {
                        let remove = root.clone();
                        card = card.child(
                            div()
                                .flex()
                                .items_start()
                                .gap_2()
                                .mx_2()
                                .my_1()
                                .px_2()
                                .py_1()
                                .rounded(theme.radius(Radius::Md))
                                .bg(colors.surface)
                                .border_1()
                                .border_color(colors.accent.opacity(0.5))
                                .text_size(small)
                                .child(
                                    Icon::new(IconName::MessageSquare)
                                        .size(IconSize::Xs)
                                        .color(colors.accent),
                                )
                                .child(div().flex_1().min_w_0().child(comment.text.clone()))
                                .child(
                                    IconButton::new(
                                        SharedString::from(format!("uncomment-{ix}")),
                                        IconName::X,
                                    )
                                    .variant(ButtonVariant::Ghost)
                                    .size(ControlSize::Sm)
                                    .on_click(
                                        move |_, _, cx| {
                                            remove.update(cx, |r, cx| {
                                                if ix < r.review.comments.len() {
                                                    r.review.comments.remove(ix);
                                                }
                                                cx.notify();
                                            })
                                        },
                                    ),
                                ),
                        );
                    }
                    if targeted {
                        let add = (root.clone(), file.clone());
                        let cancel = root.clone();
                        card = card.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .m_2()
                                .child(Input::new(&self.comment_input))
                                .child(
                                    div()
                                        .flex()
                                        .justify_end()
                                        .gap_1()
                                        .child(
                                            Button::new("comment-cancel", t("Cancel"))
                                                .variant(ButtonVariant::Ghost)
                                                .size(ControlSize::Sm)
                                                .on_click(move |_, _, cx| {
                                                    cancel.update(cx, |r, cx| {
                                                        r.review.target = None;
                                                        cx.notify();
                                                    })
                                                }),
                                        )
                                        .child(
                                            Button::new("comment-add", t("Add comment"))
                                                .size(ControlSize::Sm)
                                                .on_click(move |_, _, cx| {
                                                    let (root, file) = add.clone();
                                                    root.update(cx, |r, cx| {
                                                        r.add_comment(&file, row, cx)
                                                    })
                                                }),
                                        ),
                                ),
                        );
                    }
                }
                if file.lines.len() > MAX_LINES {
                    card = card.child(
                        div()
                            .px_2()
                            .py_1()
                            .text_size(tiny)
                            .text_color(colors.fg_subtle)
                            .child(tf(
                                "{n} more lines not shown",
                                &[("n", &(file.lines.len() - MAX_LINES))],
                            )),
                    );
                }
            }
            body = body.child(card);
        }

        let count = self.review.comments.len();
        let footer = (count > 0).then(|| {
            let (send, clear) = (root.clone(), root.clone());
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .border_t_1()
                .border_color(colors.border)
                .child(
                    div()
                        .flex_1()
                        .text_size(small)
                        .text_color(colors.fg_muted)
                        .child(tf("{n} comments", &[("n", &count)])),
                )
                .child(
                    Button::new("review-clear", t("Discard"))
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| {
                            clear.update(cx, |r, cx| {
                                r.review.comments.clear();
                                r.review.target = None;
                                cx.notify();
                            })
                        }),
                )
                .child(
                    Button::new("review-send", t("Send comments"))
                        .variant(ButtonVariant::Primary)
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| send.update(cx, |r, cx| r.send_review(cx))),
                )
        });

        let body = div()
            .flex()
            .flex_col()
            .children(self.git_bar(cx))
            .child(body);
        Some(Self::side_frame(
            header,
            body.into_any_element(),
            footer.map(|f| f.into_any_element()),
            cx,
        ))
    }

    /// The side panel around one view's `body`.
    fn side_frame(
        header: AnyElement,
        body: AnyElement,
        footer: Option<AnyElement>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors.clone();
        div()
            .flex_none()
            .w(px(480.))
            .h_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(colors.border)
            .bg(colors.bg)
            .child(header)
            .child(
                div()
                    .id("side-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
            .children(footer)
            .into_any_element()
    }

    /// The side panel's tab bar, with `extra` after the tabs.
    fn side_tabs(&self, extra: Option<AnyElement>, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let root = cx.entity();
        let tab = |id: &'static str, label: &str, which: SideTab| {
            let on = self.review.tab == which;
            let go = root.clone();
            div()
                .id(id)
                .px_2()
                .py_0p5()
                .rounded(theme.radius(Radius::Md))
                .cursor_pointer()
                .text_size(theme.text_size(TextSize::Sm))
                .when(on, |el| {
                    el.bg(colors.selection)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                })
                .when(!on, |el| {
                    el.text_color(colors.fg_muted)
                        .hover(|el| el.bg(colors.hover))
                })
                .on_click(move |_, _, cx| {
                    go.update(cx, |r, cx| {
                        r.review.tab = which;
                        cx.notify();
                    })
                })
                .child(label.to_string())
        };
        let close = root.clone();
        let draft = super::workflow_author::shown_draft(self.workspace.read(cx)).is_some();
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1p5()
            .border_b_1()
            .border_color(colors.border)
            .when(draft, |d| {
                d.child(tab("side-draft", t("Workflow draft"), SideTab::Draft))
            })
            .child(tab("side-overview", t("Overview"), SideTab::Overview))
            .child(tab("side-changes", t("Changes"), SideTab::Changes))
            .child(tab("side-activity", t("Activity"), SideTab::Activity))
            .child(tab("side-watch", t("Side by side"), SideTab::Watch))
            .child(tab("side-terminal", t("Terminal"), SideTab::Terminal))
            .child(tab("side-run", t("Run"), SideTab::Run))
            .child(tab("side-preview", t("Preview"), SideTab::Preview))
            .child(div().flex_1())
            .children(extra.filter(|_| self.review.tab == SideTab::Changes))
            .child(
                IconButton::new("side-close", IconName::X)
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .tooltip(t("Close"))
                    .on_click(move |_, _, cx| {
                        close.update(cx, |r, cx| {
                            r.review.open = false;
                            cx.notify();
                        })
                    }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_files_show_one_line() {
        let diff = "diff --git a/x.png b/x.png\nnew file mode 100644\nindex 0000000..7988b5a\nBinary files /dev/null and b/x.png differ\n";
        let lines = parse_diff(diff, &ChangeKind::Add);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].kind, LineKind::Context);
    }

    #[test]
    fn git_header_lines_stay_out() {
        let diff = "diff --git a/n b/n\nnew file mode 100644\nindex 0000000..1\n--- /dev/null\n+++ b/n\n@@ -0,0 +1 @@\n+hi\n";
        let lines = parse_diff(diff, &ChangeKind::Add);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].text, "hi");
    }

    #[test]
    fn unified_diffs_number_their_lines() {
        let diff = "--- a/x\n+++ b/x\n@@ -10,3 +10,4 @@ fn main\n keep\n-old\n+new\n+more\n tail\n";
        let lines = parse_diff(diff, &ChangeKind::Update { move_to: None });
        assert_eq!(lines[0].kind, LineKind::Hunk);
        let keep = &lines[1];
        assert_eq!((keep.old, keep.new), (Some(10), Some(10)));
        assert_eq!(
            (lines[2].kind, lines[2].old, lines[2].new),
            (LineKind::Removed, Some(11), None)
        );
        assert_eq!((lines[3].kind, lines[3].new), (LineKind::Added, Some(11)));
        assert_eq!((lines[4].text.as_str(), lines[4].new), ("more", Some(12)));
        assert_eq!((lines[5].old, lines[5].new), (Some(12), Some(13)));
        // ACP's diffs carry no numbers; a new file may be plain text.
        let acp = parse_diff("@@ @@\n-a\n+A\n", &ChangeKind::Update { move_to: None });
        assert_eq!((acp[1].old, acp[2].new), (Some(1), Some(1)));
        let new = parse_diff("one\ntwo\n", &ChangeKind::Add);
        assert!(new.iter().all(|l| l.kind == LineKind::Added));
        assert_eq!(new[1].new, Some(2));
    }

    #[test]
    fn comments_become_one_message() {
        let text = compose(&[
            LineComment {
                path: "src/a.rs".into(),
                row: 3,
                line: Some(12),
                quote: "let x = 1;".into(),
                text: "Name this better.".into(),
            },
            LineComment {
                path: "README.md".into(),
                row: 0,
                line: None,
                quote: String::new(),
                text: "Mention the flag.".into(),
            },
        ]);
        assert!(text.contains("- `src/a.rs:12` (`let x = 1;`): Name this better."));
        assert!(text.contains("- `README.md`: Mention the flag."));
    }
}
