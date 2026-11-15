//! Left column: new chat, search, conversations by day, and the backend's
//! state. Folds into an icon rail, its width animated.

use ely_gpui_component::{
    buttons::{ButtonVariant, IconButton},
    chat::{Conversation, ConversationList, new_chat_button},
    forms::{SearchInput, TextInput},
    layout::Sidebar,
    motion::{Spinner, SpinnerStyle},
    primitives::IconName,
    shell::drag_region,
    theme::{ActiveTheme, ControlSize, IconSize, TextSize},
};
use gpui::{
    AnyElement, App, Entity, IntoElement, ParentElement, SharedString, Styled, Window, div,
    prelude::*, px,
};
use jiff::{Timestamp, tz::TimeZone};

use super::{NewChat, OpenPalette, OpenSettings, RootView, TITLE_BAR_HEIGHT, ToggleSidebar};
use crate::workspace::{Status, View, Workspace};

/// Room for the macOS traffic lights at the left of the title strip.
pub fn traffic_light_room() -> gpui::Pixels {
    if cfg!(target_os = "macos") {
        px(76.)
    } else {
        px(12.)
    }
}

fn rail_button(id: &'static str, icon: IconName, tip: &'static str) -> IconButton {
    IconButton::new(id, icon)
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Md)
        .tooltip(tip)
}

pub fn render(
    workspace: &Entity<Workspace>,
    search: &Entity<TextInput>,
    root: Entity<RootView>,
    collapsed: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let title_bar = drag_region("sidebar-title", window, cx);
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let ws = workspace.read(cx);
    let (dot, label) = match &ws.status {
        Status::Connecting => (colors.warning, "Starting Codex…".to_string()),
        Status::Ready => match ws.signed_in {
            Some(false) => (
                colors.warning,
                format!("{} · not signed in", ws.backend_label),
            ),
            _ => (colors.success, ws.backend_label.clone()),
        },
        Status::Failed(_) => (colors.danger, "Codex is not running".to_string()),
    };
    let connecting = ws.status == Status::Connecting;
    let failed = matches!(ws.status, Status::Failed(_));

    if collapsed {
        return Sidebar::new("sidebar", true)
            .child(title_bar.flex_none().h(px(TITLE_BAR_HEIGHT)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .child(
                        rail_button("rail-expand", IconName::PanelLeft, "Show sidebar (⌘B)")
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(ToggleSidebar), cx)
                            }),
                    )
                    .child(
                        rail_button("rail-new", IconName::SquarePen, "New chat (⌘N)").on_click(
                            |_, window, cx| window.dispatch_action(Box::new(NewChat), cx),
                        ),
                    )
                    .child(
                        rail_button("rail-search", IconName::Search, "Search (⌘K)").on_click(
                            |_, window, cx| window.dispatch_action(Box::new(OpenPalette), cx),
                        ),
                    ),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .pb_3()
                    .child(
                        rail_button("rail-settings", IconName::Settings, "Settings (⌘,)").on_click(
                            |_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx),
                        ),
                    )
                    .child(div().size(px(8.)).rounded_full().bg(dot)),
            )
            .into_any_element();
    }

    let now = Timestamp::now();
    let conversations: Vec<Conversation> = ws
        .threads
        .iter()
        .map(|thread| Conversation {
            key: thread.id.clone().into(),
            title: thread.title().into(),
            at: Timestamp::from_second(thread.updated_at).unwrap_or(now),
            pinned: ws.settings.is_pinned(&thread.id),
        })
        .collect();
    let active: Option<SharedString> = match &ws.view {
        View::Thread(id) => Some(id.clone().into()),
        View::Draft => None,
    };
    let empty = conversations.is_empty();
    let today = now
        .to_zoned(TimeZone::try_system().unwrap_or(TimeZone::UTC))
        .date();
    let query = search.read(cx).text().to_string();

    let open = workspace.clone();
    let fresh = workspace.clone();
    let retry = workspace.clone();
    let pin = workspace.clone();
    let archive = workspace.clone();
    let list = ConversationList::new("conversations", conversations, today)
        .query(query)
        .active(active, move |key, _, cx| {
            let id = key.to_string();
            open.update(cx, |ws, cx| ws.open_thread(id, cx));
        })
        .actions(
            move |key, window, cx| {
                let id = key.to_string();
                root.update(cx, |root, cx| root.start_rename(id, window, cx));
            },
            move |key, _, cx| {
                let id = key.to_string();
                pin.update(cx, |ws, cx| ws.toggle_pin(&id, cx));
            },
            move |key, _, cx| {
                let id = key.to_string();
                archive.update(cx, |ws, cx| ws.archive(id, cx));
            },
        );

    Sidebar::new("sidebar", false)
        .child(
            title_bar
                .flex_none()
                .h(px(TITLE_BAR_HEIGHT))
                .pl(traffic_light_room())
                .pr_2()
                .flex()
                .items_center()
                .justify_end()
                .gap_0p5()
                .child(
                    IconButton::new("open-palette", IconName::Search)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip("Search (⌘K)")
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(OpenPalette), cx)
                        }),
                )
                .child(
                    IconButton::new("hide-sidebar", IconName::PanelLeft)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip("Hide sidebar (⌘B)")
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(ToggleSidebar), cx)
                        }),
                ),
        )
        .child(
            div().flex_none().px_3().pb_2().child(
                new_chat_button("new-chat")
                    .on_click(move |_, _, cx| fresh.update(cx, |ws, cx| ws.new_chat(cx))),
            ),
        )
        .child(
            div()
                .flex_none()
                .px_3()
                .pb_2()
                .child(SearchInput::new("search", search).size(ControlSize::Sm)),
        )
        .child(
            div()
                .id("conversation-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px_2()
                .pb_2()
                .when(empty, |column| {
                    column.child(
                        div()
                            .px_2()
                            .py_6()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .text_size(theme.text_size(TextSize::Sm))
                            .text_color(colors.fg_muted)
                            .child(if connecting {
                                "Loading chats…"
                            } else {
                                "No chats yet."
                            })
                            .child(
                                div()
                                    .text_size(theme.text_size(TextSize::Xs))
                                    .text_color(colors.fg_subtle)
                                    .child("Your Codex conversations show here."),
                            ),
                    )
                })
                .when(!empty, |column| column.child(list)),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_2()
                .pl_4()
                .pr_2()
                .py_2()
                .border_t_1()
                .border_color(colors.border)
                .text_size(theme.text_size(TextSize::Sm))
                .text_color(colors.fg_muted)
                .child(if connecting {
                    Spinner::new("connecting")
                        .style(SpinnerStyle::Ring)
                        .size(IconSize::Xs)
                        .color(colors.warning)
                        .into_any_element()
                } else {
                    div().size(px(8.)).rounded_full().bg(dot).into_any_element()
                })
                .child(div().flex_1().min_w_0().truncate().child(label))
                .when(failed, |row| {
                    row.child(
                        IconButton::new("reconnect", IconName::RefreshCw)
                            .variant(ButtonVariant::Ghost)
                            .size(ControlSize::Sm)
                            .tooltip("Restart Codex")
                            .on_click(move |_, _, cx| retry.update(cx, |ws, cx| ws.connect(cx))),
                    )
                })
                .child(
                    IconButton::new("open-settings", IconName::Settings)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip("Settings (⌘,)")
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(OpenSettings), cx)
                        }),
                ),
        )
        .into_any_element()
}
