//! Main column: title bar (model, chat title, status, chat menu), the
//! conversation or the welcome screen, and the composer.

use ely_gpui_component::{
    buttons::{ButtonVariant, IconButton},
    chat::{ChatContainer, ErrorMessage, MessageList, SuggestionChips, WelcomeScreen},
    menus::{DropdownMenu, Menu, MenuItem, OverflowMenu},
    motion::{Skeleton, SkeletonText, Spinner, SpinnerStyle},
    primitives::IconName,
    shell::drag_region,
    theme::{ActiveTheme, ControlSize, IconSize, Radius, TextSize},
};
use gpui::{
    AnyElement, App, Entity, IntoElement, ParentElement, Styled, Window, div, prelude::*, px,
};
use jiff::{Timestamp, tz::TimeZone};

use super::{
    NewChat, RootView, TITLE_BAR_HEIGHT,
    composer::{Composer, short_path},
    rows::{self, COLUMN},
};
use crate::workspace::{Status, View, Workspace};

const STARTERS: [(&str, IconName); 4] = [
    ("Explain this project", IconName::FileText),
    ("Fix a failing test", IconName::Terminal),
    ("Review my changes", IconName::GitBranch),
    ("Plan a feature", IconName::ListChecks),
];

pub fn render(
    workspace: &Entity<Workspace>,
    composer: &Entity<Composer>,
    root: Entity<RootView>,
    sidebar_collapsed: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let colors = cx.theme().colors.clone();
    let header = header(workspace, root, sidebar_collapsed, window, cx);
    let body = body(workspace, composer, window, cx);
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(colors.bg)
        .child(header)
        .child(div().flex_1().min_h_0().child(body))
        .into_any_element()
}

fn header(
    workspace: &Entity<Workspace>,
    root: Entity<RootView>,
    sidebar_collapsed: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let bar = drag_region("chat-title", window, cx);
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let ws = workspace.read(cx);
    let title = ws.active_thread().map(|thread| thread.title());
    let thread_id = ws.active_thread_id().map(str::to_owned);
    let usage = ws.transcript().and_then(|t| t.usage()).map(|usage| {
        let tokens = format_tokens(usage.total_tokens);
        match usage.context_window {
            Some(window) if window > 0 => {
                let share = usage.last_turn_tokens as f64 * 100. / window as f64;
                if share < 1. {
                    format!("{tokens} tokens · <1% context")
                } else {
                    format!("{tokens} tokens · {share:.0}% context")
                }
            }
            _ => format!("{tokens} tokens"),
        }
    });
    let running = ws.running_for();

    let mut models = Menu::new().item(
        MenuItem::radio("Codex default", ws.settings.model.is_none()).on_click({
            let workspace = workspace.clone();
            move |_, cx| workspace.update(cx, |ws, cx| ws.set_model(None, cx))
        }),
    );
    if !ws.models.is_empty() {
        models = models.separator();
    }
    for model in &ws.models {
        let id = model.id.clone();
        let workspace = workspace.clone();
        let mut item = MenuItem::radio(
            model.display_name.clone(),
            ws.settings.model.as_deref() == Some(&model.id),
        )
        .on_click(move |_, cx| workspace.update(cx, |ws, cx| ws.set_model(Some(id.clone()), cx)));
        if !model.description.is_empty() {
            item = item.keys(truncate(&model.description, 36));
        }
        models = models.item(item);
    }

    // Chat menu: rename, pin, compact, review, archive.
    let chat_menu =
        thread_id.as_ref().map(|id| {
            let pinned = ws.settings.is_pinned(id);
            let (rename, pin, compact, review, archive) = (
                (root.clone(), id.clone()),
                (workspace.clone(), id.clone()),
                workspace.clone(),
                workspace.clone(),
                (workspace.clone(), id.clone()),
            );
            let menu =
                Menu::new()
                    .item(MenuItem::new("Rename…").icon(IconName::Pencil).on_click(
                        move |window, cx| {
                            let (root, id) = rename.clone();
                            root.update(cx, |root, cx| root.start_rename(id, window, cx));
                        },
                    ))
                    .item(
                        MenuItem::new(if pinned { "Unpin" } else { "Pin to top" })
                            .icon(if pinned {
                                IconName::PinOff
                            } else {
                                IconName::Pin
                            })
                            .on_click(move |_, cx| {
                                let (ws, id) = pin.clone();
                                ws.update(cx, |ws, cx| ws.toggle_pin(&id, cx));
                            }),
                    )
                    .separator()
                    .item(
                        MenuItem::new("Review uncommitted changes")
                            .icon(IconName::Eye)
                            .on_click(move |_, cx| review.update(cx, |ws, cx| ws.review(None, cx))),
                    )
                    .item(
                        MenuItem::new("Compact conversation")
                            .icon(IconName::Minimize2)
                            .on_click(move |_, cx| compact.update(cx, |ws, cx| ws.compact(cx))),
                    )
                    .separator()
                    .item(MenuItem::new("Archive").icon(IconName::Archive).on_click(
                        move |_, cx| {
                            let (ws, id) = archive.clone();
                            ws.update(cx, |ws, cx| ws.archive(id, cx));
                        },
                    ));
            OverflowMenu::new("chat-menu", menu)
        });

    let left_room = if sidebar_collapsed && cfg!(target_os = "macos") {
        px(28.)
    } else {
        px(12.)
    };
    bar.flex_none()
        .h(px(TITLE_BAR_HEIGHT))
        .pl(left_room)
        .pr_2()
        .flex()
        .items_center()
        .gap_2()
        .child(
            DropdownMenu::new("model", ws.model_label(), models)
                .icon(IconName::Sparkles)
                .variant(ButtonVariant::Ghost),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .justify_center()
                .text_size(theme.text_size(TextSize::Sm))
                .text_color(colors.fg_muted)
                .children(title.map(|title| div().truncate().child(title))),
        )
        .children(running.map(|took| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1p5()
                .px_2()
                .py_0p5()
                .rounded_full()
                .bg(colors.sunken)
                .text_size(theme.text_size(TextSize::Xs))
                .text_color(colors.fg_muted)
                .child(
                    Spinner::new("working")
                        .style(SpinnerStyle::Ring)
                        .size(IconSize::Xs)
                        .color(colors.accent),
                )
                .child(format!("Working · {}s", took.as_secs()))
        }))
        .children(usage.filter(|_| running.is_none()).map(|usage| {
            div()
                .flex_none()
                .text_size(theme.text_size(TextSize::Xs))
                .text_color(colors.fg_subtle)
                .child(usage)
        }))
        .children(chat_menu)
        .when(thread_id.is_some(), |bar| {
            bar.child(
                IconButton::new("header-new-chat", IconName::SquarePen)
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .tooltip("New chat (⌘N)")
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(NewChat), cx)),
            )
        })
        .into_any_element()
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max).collect::<String>())
    }
}

/// The composer, at the reading column's width.
fn composer_column(composer: &Entity<Composer>) -> AnyElement {
    div()
        .w_full()
        .flex()
        .justify_center()
        .child(div().w_full().max_w(px(COLUMN)).child(composer.clone()))
        .into_any_element()
}

fn body(
    workspace: &Entity<Workspace>,
    composer: &Entity<Composer>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let ws = workspace.read(cx);

    if let Status::Failed(reason) = &ws.status
        && ws.transcript().is_none()
    {
        let retry = workspace.clone();
        return div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p_8()
            .child(
                div().max_w(px(560.)).child(
                    ErrorMessage::new("codex-failed", "Could not start Codex")
                        .detail(reason.clone())
                        .on_retry(move |_, cx| retry.update(cx, |ws, cx| ws.connect(cx))),
                ),
            )
            .into_any_element();
    }

    if ws.is_loading() {
        // The shape of a conversation while its history loads.
        let skeleton = (0..3).map(|ix| {
            rows::column(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div().flex().justify_end().child(
                            Skeleton::new(format!("skeleton-user-{ix}"))
                                .w(px(220. + 60. * ix as f32))
                                .h(px(40.))
                                .rounded(theme.radius(Radius::Xl)),
                        ),
                    )
                    .child(SkeletonText::new(format!("skeleton-agent-{ix}"), 3)),
                1.,
                window,
            )
        });
        return ChatContainer::new(
            div()
                .size_full()
                .flex()
                .flex_col()
                .justify_end()
                .pb_4()
                .children(skeleton.collect::<Vec<_>>()),
        )
        .composer(composer_column(composer))
        .into_any_element();
    }

    let Some(transcript) = ws.transcript() else {
        let hour = Timestamp::now()
            .to_zoned(TimeZone::try_system().unwrap_or(TimeZone::UTC))
            .hour() as u8;
        let fill = composer.clone();
        let folder = short_path(&ws.settings.working_dir());
        return WelcomeScreen::new("welcome", hour)
            .child(
                div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .mt_neg_4()
                    .text_size(theme.text_size(TextSize::Sm))
                    .text_color(colors.fg_muted)
                    .child(format!("Codex is ready in {folder}")),
            )
            .child(composer_column(composer))
            .child(SuggestionChips::new(
                "starters",
                STARTERS,
                move |ix, window, cx| {
                    fill.update(cx, |composer, cx| {
                        composer.set_text(STARTERS[ix].0, window, cx)
                    });
                },
            ))
            .into_any_element();
    };

    let typing = usize::from(ws.awaiting_first_output());
    let count = transcript.len() + typing;
    let list_id = match &ws.view {
        View::Thread(id) => format!("messages-{id}"),
        View::Draft => "messages-draft".to_string(),
    };
    let rows_from = workspace.clone();
    let messages = MessageList::new(list_id, count, move |ix, window, cx| {
        rows::row(&rows_from, ix, window, cx)
    });
    ChatContainer::new(messages)
        .composer(composer_column(composer))
        .into_any_element()
}

fn format_tokens(tokens: u64) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..1_000_000 => format!("{:.1}k", tokens as f64 / 1_000.),
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tokens_read_short() {
        assert_eq!(super::format_tokens(950), "950");
        assert_eq!(super::format_tokens(12_345), "12.3k");
        assert_eq!(super::format_tokens(2_500_000), "2.5M");
    }
}
