//! The side panel's overview of the shown conversation: the files it made
//! or changed, the subagents it started, and the tools and searches it drew
//! on.

use std::path::PathBuf;

use atrium_core::{ChangeKind, EntryKind, Item, ItemStatus, Transcript};
use ely_gpui_component::{
    motion::{Spinner, SpinnerStyle},
    primitives::{Icon, IconName},
    theme::{ActiveTheme, IconSize, Radius, TextSize},
};
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::*,
};

use super::{RootView, changes::SideTab, steps::diff_counts};
use crate::i18n::{t, tf};

/// A file the conversation wrote, with its lines added and removed in all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub path: String,
    pub created: bool,
    pub deleted: bool,
    pub added: usize,
    pub removed: usize,
}

/// A subagent the conversation started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Helper {
    pub label: String,
    pub detail: Option<String>,
    pub working: bool,
    pub failed: bool,
}

/// What the conversation drew on: a tool server with how often, or a web search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Tools { server: String, calls: usize },
    Web(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overview {
    pub outputs: Vec<Output>,
    pub helpers: Vec<Helper>,
    pub sources: Vec<Source>,
}

impl Overview {
    pub fn of(transcript: &Transcript) -> Self {
        let mut out = Self::default();
        for entry in transcript.entries() {
            let EntryKind::Item(item) = &entry.kind else {
                continue;
            };
            match item {
                Item::FileChange {
                    changes, status, ..
                } if !matches!(status, ItemStatus::Failed | ItemStatus::Declined) => {
                    for change in changes {
                        let (added, removed) = diff_counts(change);
                        let path = match &change.kind {
                            ChangeKind::Update {
                                move_to: Some(to), ..
                            } => to.clone(),
                            _ => change.path.clone(),
                        };
                        let ix = match out.outputs.iter().position(|o| o.path == path) {
                            Some(ix) => ix,
                            None => {
                                out.outputs.push(Output {
                                    path: path.clone(),
                                    created: matches!(change.kind, ChangeKind::Add),
                                    deleted: false,
                                    added: 0,
                                    removed: 0,
                                });
                                out.outputs.len() - 1
                            }
                        };
                        let o = &mut out.outputs[ix];
                        o.added += added;
                        o.removed += removed;
                        o.deleted = matches!(change.kind, ChangeKind::Delete);
                    }
                }
                Item::SubAgent {
                    label,
                    status,
                    detail,
                    ..
                } => out.helpers.push(Helper {
                    label: label.clone(),
                    detail: detail.clone(),
                    working: *status == ItemStatus::InProgress,
                    failed: matches!(status, ItemStatus::Failed | ItemStatus::Declined),
                }),
                Item::Tool { name, .. } => {
                    let server = name.split_once('.').map_or(name.as_str(), |(s, _)| s);
                    match out
                        .sources
                        .iter_mut()
                        .find(|s| matches!(s, Source::Tools { server: had, .. } if had == server))
                    {
                        Some(Source::Tools { calls, .. }) => *calls += 1,
                        _ => out.sources.push(Source::Tools {
                            server: server.to_string(),
                            calls: 1,
                        }),
                    }
                }
                Item::WebSearch { query, .. } if !query.trim().is_empty() => {
                    let web = Source::Web(query.clone());
                    if !out.sources.contains(&web) {
                        out.sources.push(web);
                    }
                }
                _ => {}
            }
        }
        out
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

impl RootView {
    pub(super) fn overview_body(&self, cx: &Context<Self>) -> AnyElement {
        let ws = self.workspace.read(cx);
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let small = theme.text_size(TextSize::Sm);
        let body = div().flex().flex_col().gap_3().p_3();
        let Some(thread) = ws.active_thread_id().map(str::to_owned) else {
            return body
                .child(
                    div()
                        .text_size(small)
                        .text_color(colors.fg_muted)
                        .child(t("Open a conversation to see what it made.")),
                )
                .into_any_element();
        };
        let cwd = ws
            .active_thread()
            .map(|t| PathBuf::from(&t.cwd))
            .unwrap_or_default();
        let overview = ws
            .transcript_of(&thread)
            .map(Overview::of)
            .unwrap_or_default();

        let section = |title: String, count: Option<usize>| {
            div()
                .flex()
                .items_center()
                .gap_1()
                .text_size(small)
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(colors.fg)
                .child(title)
                .children(count.map(|n| {
                    div()
                        .text_color(colors.fg_muted)
                        .font_weight(gpui::FontWeight::NORMAL)
                        .child(format!("· {n}"))
                }))
        };
        let card = || {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .rounded(theme.radius(Radius::Lg))
                .border_1()
                .border_color(colors.border)
                .bg(colors.surface)
        };
        let muted = |text: String| {
            div()
                .text_size(small)
                .text_color(colors.fg_muted)
                .child(text)
        };

        // What it made or changed.
        let mut outputs = card().child(section(t("Outputs").into(), Some(overview.outputs.len())));
        if overview.outputs.is_empty() {
            outputs = outputs.child(muted(t("No files made or changed yet.").into()));
        }
        for (ix, output) in overview.outputs.iter().enumerate() {
            let full = cwd.join(&output.path);
            let can_preview = !output.deleted && super::preview::previewable(&full);
            let root = cx.entity();
            let (icon, what) = if output.deleted {
                (IconName::Trash2, t("Deleted"))
            } else if output.created {
                (IconName::FilePlus, t("Created"))
            } else {
                (IconName::Pencil, t("Edited"))
            };
            outputs = outputs.child(
                div()
                    .id(SharedString::from(format!("overview-out-{ix}")))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_1()
                    .py_0p5()
                    .rounded(theme.radius(Radius::Sm))
                    .cursor_pointer()
                    .hover(|s| s.bg(colors.hover))
                    .text_size(small)
                    .child(Icon::new(icon).size(IconSize::Sm).color(colors.fg_muted))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .text_color(colors.fg)
                                    .child(file_name(&output.path).to_string()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(colors.fg_subtle)
                                    .child(format!("{what} · {}", output.path)),
                            ),
                    )
                    .when(!output.deleted, |row| {
                        row.child(
                            div()
                                .flex()
                                .flex_none()
                                .gap_1()
                                .child(
                                    div()
                                        .text_color(colors.success)
                                        .child(format!("+{}", output.added)),
                                )
                                .child(
                                    div()
                                        .text_color(colors.danger)
                                        .child(format!("-{}", output.removed)),
                                ),
                        )
                    })
                    .child(
                        Icon::new(if can_preview {
                            IconName::Eye
                        } else {
                            IconName::ChevronRight
                        })
                        .size(IconSize::Xs)
                        .color(colors.fg_subtle),
                    )
                    .on_click(move |_, window, cx| {
                        let full = full.clone();
                        root.update(cx, |root, cx| {
                            if can_preview {
                                root.open_preview(full, window, cx);
                            } else {
                                root.review.tab = SideTab::Changes;
                                cx.notify();
                            }
                        })
                    }),
            );
        }

        // Subagents.
        let root = cx.entity();
        let mut helpers = card().child(
            div()
                .id("overview-helpers")
                .flex()
                .items_center()
                .justify_between()
                .cursor_pointer()
                .child(section(t("Subagents").into(), Some(overview.helpers.len())))
                .child(
                    Icon::new(IconName::ChevronRight)
                        .size(IconSize::Xs)
                        .color(colors.fg_subtle),
                )
                .on_click(move |_, _, cx| {
                    root.update(cx, |root, cx| {
                        root.review.tab = SideTab::Activity;
                        cx.notify();
                    })
                }),
        );
        if overview.helpers.is_empty() {
            helpers = helpers.child(muted(t("No subagents in this conversation.").into()));
        }
        for (ix, helper) in overview.helpers.iter().enumerate().take(8) {
            let mark = if helper.working {
                Spinner::new(SharedString::from(format!("overview-helper-{ix}")))
                    .style(SpinnerStyle::Ring)
                    .size(IconSize::Xs)
                    .into_any_element()
            } else {
                Icon::new(IconName::Bot)
                    .size(IconSize::Sm)
                    .color(if helper.failed {
                        colors.danger
                    } else {
                        colors.fg_muted
                    })
                    .into_any_element()
            };
            helpers = helpers.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_1()
                    .text_size(small)
                    .child(mark)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(colors.fg)
                            .child(helper.label.clone()),
                    )
                    .children(helper.detail.clone().map(|d| {
                        div()
                            .max_w(gpui::px(160.))
                            .truncate()
                            .text_color(colors.fg_subtle)
                            .child(d)
                    })),
            );
        }
        if overview.helpers.len() > 8 {
            helpers = helpers.child(muted(tf(
                "{n} more in Activity",
                &[("n", &(overview.helpers.len() - 8))],
            )));
        }

        // Sources.
        let mut sources = card().child(section(t("Sources").into(), None));
        if overview.sources.is_empty() {
            sources = sources.child(muted(t("No tools or web searches used yet.").into()));
        }
        for source in &overview.sources {
            let (icon, text, more) = match source {
                Source::Tools { server, calls } => (
                    IconName::Wrench,
                    server.clone(),
                    Some(tf("{n} calls", &[("n", calls)])),
                ),
                Source::Web(query) => (IconName::Globe, query.clone(), None),
            };
            sources = sources.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_1()
                    .text_size(small)
                    .child(Icon::new(icon).size(IconSize::Sm).color(colors.fg_muted))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(colors.fg)
                            .child(text),
                    )
                    .children(more.map(|m| div().text_color(colors.fg_subtle).child(m))),
            );
        }
        let manage = cx.entity();
        sources = sources.child(
            div()
                .id("overview-manage-mcp")
                .flex()
                .items_center()
                .gap_2()
                .px_1()
                .cursor_pointer()
                .text_size(small)
                .text_color(colors.fg_muted)
                .hover(|s| s.text_color(colors.fg))
                .child(Icon::new(IconName::Settings).size(IconSize::Sm))
                .child(t("Manage MCP servers"))
                .on_click(move |_, window, cx| {
                    manage.update(cx, |root, cx| root.open_settings(window, cx))
                }),
        );

        body.child(outputs)
            .child(helpers)
            .child(sources)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atrium_core::{AgentEvent, FileChange};

    #[test]
    fn outputs_add_up_per_file_and_sources_group_by_server() {
        let mut t = Transcript::default();
        let items = vec![
            Item::FileChange {
                id: "1".into(),
                changes: vec![FileChange {
                    path: "a.md".into(),
                    kind: ChangeKind::Add,
                    diff: "line one\nline two\n".into(),
                }],
                status: ItemStatus::Completed,
            },
            Item::FileChange {
                id: "2".into(),
                changes: vec![FileChange {
                    path: "a.md".into(),
                    kind: ChangeKind::Update { move_to: None },
                    diff: "@@ -1 +1 @@\n-line one\n+line 1\n".into(),
                }],
                status: ItemStatus::Completed,
            },
            Item::Tool {
                id: "3".into(),
                name: "docs.search".into(),
                status: ItemStatus::Completed,
                arguments: String::new(),
                result: String::new(),
                duration_ms: None,
            },
            Item::Tool {
                id: "4".into(),
                name: "docs.fetch".into(),
                status: ItemStatus::Completed,
                arguments: String::new(),
                result: String::new(),
                duration_ms: None,
            },
        ];
        for item in items {
            t.apply(&AgentEvent::ItemCompleted {
                thread_id: "t".into(),
                turn_id: "u".into(),
                item,
            });
        }
        let o = Overview::of(&t);
        assert_eq!(
            o.outputs,
            vec![Output {
                path: "a.md".into(),
                created: true,
                deleted: false,
                added: 3,
                removed: 1,
            }]
        );
        assert_eq!(
            o.sources,
            vec![Source::Tools {
                server: "docs".into(),
                calls: 2
            }]
        );
    }
}
