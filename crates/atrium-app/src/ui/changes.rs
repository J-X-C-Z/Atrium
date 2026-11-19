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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Hunk,
    Context,
    Added,
    Removed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

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
    pub comments: Vec<LineComment>,
    /// The line being commented on: `(path, row)`.
    pub target: Option<(String, usize)>,
}

/// Reads a unified diff leniently: hunk headers with or without numbers, and
/// a new file's plain text as all added.
pub fn parse_diff(diff: &str, kind: &ChangeKind) -> Vec<DiffLine> {
    let unified = diff.lines().any(|l| l.starts_with("@@"));
    let mut out = Vec::new();
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
            _ => (LineKind::Context, line),
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
        self.review.open = !self.review.open;
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
        let files = ws.transcript().map(changed_files).unwrap_or_default();
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

        let close = root.clone();
        let header = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(colors.border)
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(t("Changes")),
            )
            .child(
                div()
                    .text_size(tiny)
                    .text_color(colors.fg_subtle)
                    .child(tf("{n} files", &[("n", &files.len())])),
            )
            .child(
                div()
                    .text_size(tiny)
                    .text_color(colors.success)
                    .child(format!("+{added}")),
            )
            .child(
                div()
                    .text_size(tiny)
                    .text_color(colors.danger)
                    .child(format!("−{removed}")),
            )
            .child(div().flex_1())
            .child(
                IconButton::new("changes-close", IconName::X)
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .tooltip(t("Close"))
                    .on_click(move |_, _, cx| close.update(cx, |r, cx| r.toggle_changes(cx))),
            );

        let mut body = div().flex().flex_col().gap_3().p_3();
        if files.is_empty() {
            body = body.child(
                div()
                    .text_size(small)
                    .text_color(colors.fg_muted)
                    .child(t("No file changes in this conversation yet.")),
            );
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
                        ),
                );
            for (row, line) in file.lines.iter().enumerate().take(MAX_LINES) {
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
                        ),
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
                                .on_click(move |_, _, cx| {
                                    remove.update(cx, |r, cx| {
                                        if ix < r.review.comments.len() {
                                            r.review.comments.remove(ix);
                                        }
                                        cx.notify();
                                    })
                                }),
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

        Some(
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
                        .id("changes-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(body),
                )
                .children(footer)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
