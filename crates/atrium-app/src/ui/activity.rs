//! The side panel's other views: what the shown conversation is doing in
//! the background, and other conversations watched side by side.

use ely_gpui_component::{
    buttons::{Button, ButtonVariant, IconButton},
    primitives::{Icon, IconName},
    theme::{ActiveTheme, ControlSize, IconSize, Radius, TextSize},
};
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::*,
};

use super::{
    RootView,
    project::{clip, state_color, state_dot, state_label},
};
use crate::i18n::{t, tf};
use crate::workspace::{Activity, ActivityKind, StreamState, activities};

/// At most this many conversations side by side.
const MAX_WATCHED: usize = 4;

fn activity_row(activity: &Activity, cx: &Context<RootView>) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let icon = match activity.kind {
        ActivityKind::Command => IconName::Terminal,
        ActivityKind::Tool => IconName::SlidersHorizontal,
        ActivityKind::SubAgent => IconName::Bot,
        ActivityKind::Search => IconName::Search,
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .py_0p5()
        .text_size(theme.text_size(TextSize::Sm))
        .child(Icon::new(icon).size(IconSize::Xs).color(if activity.live {
            colors.accent
        } else if activity.failed {
            colors.danger
        } else {
            colors.fg_subtle
        }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .when(activity.kind == ActivityKind::Command, |el| {
                    el.font_family(theme.mono_family.clone())
                })
                .text_color(if activity.live {
                    colors.fg
                } else {
                    colors.fg_muted
                })
                .child(activity.label.clone()),
        )
        .children(activity.detail.clone().map(|d| {
            div()
                .flex_none()
                .text_size(theme.text_size(TextSize::Xs))
                .text_color(if activity.failed {
                    colors.danger
                } else {
                    colors.fg_subtle
                })
                .child(d)
        }))
        .when(activity.live, |el| {
            el.child(
                div()
                    .flex_none()
                    .text_size(theme.text_size(TextSize::Xs))
                    .text_color(colors.accent)
                    .child(t("running")),
            )
        })
        .into_any_element()
}

fn heading(text: String, cx: &Context<RootView>) -> AnyElement {
    let theme = cx.theme();
    div()
        .pt_1()
        .text_size(theme.text_size(TextSize::Xs))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.colors.fg_subtle)
        .child(text)
        .into_any_element()
}

impl RootView {
    /// Background work of the shown conversation: its running commands,
    /// tools and sub-agents, its workstream's sub-agents, and recent ones.
    pub(super) fn activity_body(&self, cx: &Context<Self>) -> AnyElement {
        let ws = self.workspace.read(cx);
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let mut body = div().flex().flex_col().gap_1().p_3();
        let Some(thread) = ws.active_thread_id().map(str::to_owned) else {
            return body
                .child(
                    div()
                        .text_color(colors.fg_muted)
                        .child(t("Open a conversation to see its activity.")),
                )
                .into_any_element();
        };
        let all = ws
            .transcript_of(&thread)
            .map(activities)
            .unwrap_or_default();
        let (live, done): (Vec<&Activity>, Vec<&Activity>) = all.iter().partition(|a| a.live);

        // A workstream's own sub-agents.
        let stream = ws
            .stream_views()
            .into_iter()
            .find(|v| v.stream.thread_id.as_deref() == Some(thread.as_str()));
        let children: Vec<_> = stream
            .as_ref()
            .map(|me| {
                ws.stream_views()
                    .into_iter()
                    .filter(|v| v.stream.parent.as_deref() == Some(me.stream.id.as_str()))
                    .filter(|v| !v.stream.retired)
                    .collect()
            })
            .unwrap_or_default();

        body = body.child(heading(tf("Running now · {n}", &[("n", &live.len())]), cx));
        if live.is_empty() {
            body = body.child(
                div()
                    .text_size(theme.text_size(TextSize::Sm))
                    .text_color(colors.fg_muted)
                    .child(t("Nothing in the background.")),
            );
        }
        for a in &live {
            body = body.child(activity_row(a, cx));
        }
        if !live.is_empty() {
            let stop = self.workspace.clone();
            body = body.child(
                div().pt_1().child(
                    Button::new("activity-stop", t("Stop this turn"))
                        .icon(IconName::Square)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| stop.update(cx, |ws, cx| ws.stop(cx))),
                ),
            );
        }
        if !children.is_empty() {
            body = body.child(heading(
                tf("Sub-agents · {n}", &[("n", &children.len())]),
                cx,
            ));
            for child in children {
                let id = child.stream.id.clone();
                let open = self.workspace.clone();
                body = body.child(
                    div()
                        .id(SharedString::from(format!("activity-child-{id}")))
                        .flex()
                        .items_center()
                        .gap_2()
                        .py_0p5()
                        .px_1()
                        .rounded(theme.radius(Radius::Sm))
                        .cursor_pointer()
                        .hover(|el| el.bg(colors.hover))
                        .text_size(theme.text_size(TextSize::Sm))
                        .on_click(move |_, _, cx| {
                            let id = id.clone();
                            open.update(cx, |ws, cx| ws.show_stream(&id, cx))
                        })
                        .child(state_dot(&child.state, cx))
                        .child(div().flex_none().child(child.stream.name.clone()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(colors.fg_subtle)
                                .child(
                                    child
                                        .task
                                        .as_ref()
                                        .map(|t| t.title.clone())
                                        .unwrap_or_default(),
                                ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(theme.text_size(TextSize::Xs))
                                .text_color(state_color(&child.state, cx))
                                .child(state_label(&child.state)),
                        ),
                );
            }
        }
        if !done.is_empty() {
            body = body.child(heading(t("Recently").to_string(), cx));
            for a in done.iter().take(15) {
                body = body.child(activity_row(a, cx));
            }
        }
        body.into_any_element()
    }

    /// Conversations watched side by side: each one's state, what it runs
    /// and what it said last, with a way in.
    pub(super) fn watch_body(&self, cx: &Context<Self>) -> AnyElement {
        let ws = self.workspace.read(cx);
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let root = cx.entity();
        let mut body = div().flex().flex_col().gap_3().p_3();
        let current = ws.active_thread_id().map(str::to_owned);

        if let Some(current) = current.clone().filter(|c| !self.review.watched.contains(c))
            && self.review.watched.len() < MAX_WATCHED
        {
            let add = root.clone();
            body = body.child(
                Button::new("watch-current", t("Watch this conversation"))
                    .icon(IconName::Eye)
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .on_click(move |_, _, cx| {
                        let current = current.clone();
                        add.update(cx, |r, cx| {
                            r.review.watched.push(current);
                            cx.notify();
                        })
                    }),
            );
        }
        if self.review.watched.is_empty() {
            body = body.child(
                div()
                    .text_size(theme.text_size(TextSize::Sm))
                    .text_color(colors.fg_muted)
                    .child(t("Watch up to four conversations here while you work in another: each shows its state, what it is running and what it said last.")),
            );
        }
        for (ix, thread) in self.review.watched.iter().enumerate() {
            let Some(glance) = ws.glance(thread) else {
                continue;
            };
            let border = match glance.state {
                StreamState::NeedsYou(_) => colors.warning,
                StreamState::Failed(_) => colors.danger,
                _ => colors.border,
            };
            let (open, remove) = (self.workspace.clone(), root.clone());
            let open_id = thread.clone();
            let shown = current.as_deref() == Some(thread.as_str());
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .rounded(theme.radius(Radius::Md))
                    .border_1()
                    .border_color(border)
                    .bg(colors.surface)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(state_dot(&glance.state, cx))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(glance.title.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(theme.text_size(TextSize::Xs))
                                    .text_color(state_color(&glance.state, cx))
                                    .child(state_label(&glance.state)),
                            )
                            .when(!shown, |el| {
                                el.child(
                                    IconButton::new(
                                        SharedString::from(format!("watch-open-{ix}")),
                                        IconName::ChevronRight,
                                    )
                                    .variant(ButtonVariant::Ghost)
                                    .size(ControlSize::Sm)
                                    .tooltip(t("Open"))
                                    .on_click(
                                        move |_, _, cx| {
                                            let id = open_id.clone();
                                            open.update(cx, |ws, cx| ws.open_thread(id, cx))
                                        },
                                    ),
                                )
                            })
                            .child(
                                IconButton::new(
                                    SharedString::from(format!("watch-remove-{ix}")),
                                    IconName::X,
                                )
                                .variant(ButtonVariant::Ghost)
                                .size(ControlSize::Sm)
                                .tooltip(t("Stop watching"))
                                .on_click(move |_, _, cx| {
                                    remove.update(cx, |r, cx| {
                                        if ix < r.review.watched.len() {
                                            r.review.watched.remove(ix);
                                        }
                                        cx.notify();
                                    })
                                }),
                            ),
                    )
                    .children(glance.running.iter().take(3).map(|a| activity_row(a, cx)))
                    .children(glance.said.as_ref().map(|said| {
                        div()
                            .text_size(theme.text_size(TextSize::Sm))
                            .text_color(colors.fg_muted)
                            .line_clamp(4)
                            .child(clip(said, 400))
                    })),
            );
        }

        // Others worth watching: the shown project's workstreams, then recent chats.
        let mut candidates: Vec<String> = ws
            .stream_views()
            .into_iter()
            .filter_map(|v| v.stream.thread_id)
            .collect();
        candidates.extend(ws.threads.iter().take(8).map(|t| t.id.clone()));
        candidates.dedup();
        let candidates: Vec<String> = candidates
            .into_iter()
            .filter(|c| !self.review.watched.contains(c) && Some(c) != current.as_ref())
            .take(8)
            .collect();
        if !candidates.is_empty() && self.review.watched.len() < MAX_WATCHED {
            let mut list = div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(heading(t("Add").to_string(), cx));
            for (ix, thread) in candidates.into_iter().enumerate() {
                let Some(glance) = ws.glance(&thread) else {
                    continue;
                };
                let add = root.clone();
                list = list.child(
                    div()
                        .id(SharedString::from(format!("watch-add-{ix}")))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_1()
                        .py_0p5()
                        .rounded(theme.radius(Radius::Sm))
                        .cursor_pointer()
                        .hover(|el| el.bg(colors.hover))
                        .text_size(theme.text_size(TextSize::Sm))
                        .on_click(move |_, _, cx| {
                            let thread = thread.clone();
                            add.update(cx, |r, cx| {
                                if r.review.watched.len() < MAX_WATCHED {
                                    r.review.watched.push(thread);
                                }
                                cx.notify();
                            })
                        })
                        .child(
                            Icon::new(IconName::Plus)
                                .size(IconSize::Xs)
                                .color(colors.fg_subtle),
                        )
                        .child(state_dot(&glance.state, cx))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(glance.title.clone()),
                        ),
                );
            }
            body = body.child(list);
        }
        body.into_any_element()
    }
}
