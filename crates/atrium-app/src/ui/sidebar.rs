//! Left column: new chat, search, conversations by day, and the backend's
//! state. Folds into an icon rail, its width animated.

use atrium_core::{Account, plan_label};
use ely_gpui_component::{
    buttons::{Button, ButtonVariant, IconButton, SegmentedControl},
    chat::{Conversation, ConversationList, new_chat_button},
    data_display::{Avatar, Badge, Tone},
    forms::{SearchInput, TextInput},
    layout::Sidebar,
    motion::{Spinner, SpinnerStyle},
    primitives::{Icon, IconName},
    shell::drag_region,
    theme::{ActiveTheme, AvatarSize, ControlSize, IconSize, TextSize},
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
    let in_project = ws.project_root().is_some();
    // Workstreams' sessions live in their project, not among the chats.
    let project_threads = ws.project_threads();
    let conversations: Vec<Conversation> = ws
        .threads
        .iter()
        .filter(|thread| !project_threads.contains(&thread.id))
        .map(|thread| Conversation {
            key: thread.id.clone().into(),
            // Other agents' chats say whose they are.
            title: match (
                ws.agent(&thread.backend),
                crate::agents::split_backend(&thread.backend),
            ) {
                (Some(agent), (_, Some(device))) => {
                    format!("{}@{device} · {}", agent.profile.short, thread.title()).into()
                }
                (Some(agent), _) => format!("{} · {}", agent.profile.short, thread.title()).into(),
                (None, (_, Some(device))) => format!("Codex@{device} · {}", thread.title()).into(),
                (None, _) => thread.title().into(),
            },
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
    let account_root = root.clone();
    // Chats whose messages (not only titles) match the search.
    let hits: Vec<(String, String, String)> = match &ws.search {
        Some((searched, hits)) if searched.trim() == query.trim() => hits
            .iter()
            .map(|(meta, snippet)| (meta.id.clone(), meta.title(), snippet.clone()))
            .collect(),
        _ => Vec::new(),
    };
    // Whether any title matches, as the list filters: every word, any case.
    let titles_match = query.trim().is_empty()
        || ws.threads.iter().any(|thread| {
            let title = thread.title().to_lowercase();
            query
                .split_whitespace()
                .all(|word| title.contains(&word.to_lowercase()))
        });
    let account = ws.account.clone();
    let signing_in = ws.login.is_some();
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
        .child({
            let switch = workspace.clone();
            div().flex_none().px_3().pb_2().child(
                SegmentedControl::new("room", if in_project { "projects" } else { "chats" })
                    .segment("chats", "Chats", Some(IconName::MessageSquare))
                    .segment("projects", "Projects", Some(IconName::Layers))
                    .size(ControlSize::Sm)
                    .on_change(move |value, _, cx| {
                        let projects = value.as_ref() == "projects";
                        switch.update(cx, |ws, cx| {
                            if projects {
                                match ws
                                    .settings
                                    .active_project
                                    .clone()
                                    .or_else(|| ws.projects.first().map(|p| p.root.clone()))
                                {
                                    Some(root) => ws.enter_project(root, cx),
                                    None => {
                                        ws.room = crate::workspace::Room::Project {
                                            root: std::path::PathBuf::new(),
                                            stream: None,
                                        };
                                        cx.notify();
                                    }
                                }
                            } else {
                                ws.enter_chats(cx);
                            }
                        })
                    }),
            )
        })
        .when(in_project, |sidebar| {
            let add = workspace.clone();
            sidebar
                .child(
                    div().flex_none().px_3().pb_2().child(
                        Button::new("open-project", "Open a folder as a project")
                            .icon(IconName::FolderPlus)
                            .variant(ButtonVariant::Outline)
                            .full_width()
                            .on_click(move |_, _, cx| {
                                super::project::choose_project(add.clone(), cx)
                            }),
                    ),
                )
                .child(
                    div()
                        .id("project-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px_2()
                        .pb_2()
                        .child(super::project::sidebar_list(workspace, cx)),
                )
        })
        .when(!in_project, |sidebar| {
            sidebar
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
                        // With only message hits, the list's "no match" would push them away.
                        .when(!empty && (titles_match || hits.is_empty()), |column| {
                            column.child(list)
                        })
                        .when(!hits.is_empty(), |column| {
                            column
                                .child(
                                    div()
                                        .px_2()
                                        .pt_3()
                                        .pb_1()
                                        .text_size(theme.text_size(TextSize::Xs))
                                        .text_color(colors.fg_subtle)
                                        .child("In messages"),
                                )
                                .children(hits.into_iter().map(|(id, title, snippet)| {
                                    let open = workspace.clone();
                                    div()
                                        .id(SharedString::from(format!("hit-{id}")))
                                        .flex()
                                        .flex_col()
                                        .gap_0p5()
                                        .px_2()
                                        .py_1p5()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .hover(|row| row.bg(colors.hover))
                                        .on_click(move |_, _, cx| {
                                            let id = id.clone();
                                            open.update(cx, |ws, cx| ws.open_thread(id, cx))
                                        })
                                        .child(
                                            div()
                                                .truncate()
                                                .text_size(theme.text_size(TextSize::Sm))
                                                .child(title),
                                        )
                                        .child(
                                            div()
                                                .text_size(theme.text_size(TextSize::Xs))
                                                .text_color(colors.fg_muted)
                                                .line_clamp(2)
                                                .child(snippet),
                                        )
                                }))
                        }),
                )
        })
        .child({
            let open = account_root.clone();
            let row = div()
                .id("account-row")
                .flex_none()
                .mx_2()
                .mb_1()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1p5()
                .rounded_md()
                .cursor_pointer()
                .hover(|row| row.bg(colors.hover))
                .on_click(move |_, _, cx| open.update(cx, |root, cx| root.open_account(cx)));
            match &account {
                Some(Account::ChatGpt { email, plan, .. }) => {
                    let who = email.clone().unwrap_or_else(|| "ChatGPT".into());
                    row.child(Avatar::new("me", who.clone()).size(AvatarSize::Sm))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(theme.text_size(TextSize::Sm))
                                .child(who),
                        )
                        .child(Badge::new(plan_label(plan)).tone(Tone::Accent))
                        .into_any_element()
                }
                Some(Account::ApiKey) => row
                    .text_size(theme.text_size(TextSize::Sm))
                    .text_color(colors.fg_muted)
                    .child(Icon::new(IconName::Lock).size(IconSize::Sm))
                    .child("API key")
                    .into_any_element(),
                Some(Account::Other(kind)) => row
                    .text_size(theme.text_size(TextSize::Sm))
                    .child(kind.clone())
                    .into_any_element(),
                None => row
                    .text_size(theme.text_size(TextSize::Sm))
                    .text_color(colors.accent)
                    .child(
                        Icon::new(IconName::Sparkles)
                            .size(IconSize::Sm)
                            .color(colors.accent),
                    )
                    .child(if signing_in {
                        "Finish signing in in your browser…"
                    } else {
                        "Sign in with ChatGPT"
                    })
                    .into_any_element(),
            }
        })
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
