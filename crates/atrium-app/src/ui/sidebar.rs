//! Left column: new chat, search, conversations by day, and the backend's
//! state. Folds into an icon rail, its width animated.

use atrium_core::{Account, plan_label};
use ely_gpui_component::{
    buttons::{Button, ButtonVariant, IconButton, SegmentedControl},
    chat::new_chat_button,
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

use super::chat_list::{ChatList, ChatRow};
use super::{NewChat, OpenPalette, OpenSettings, RootView, TITLE_BAR_HEIGHT, ToggleSidebar};
use crate::i18n::{t, tf};
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
        Status::Connecting => (colors.warning, t("Starting Codex…").to_string()),
        Status::Ready => match ws.signed_in {
            Some(false) => (
                colors.warning,
                tf(
                    "{backend} · not signed in",
                    &[("backend", &ws.backend_label)],
                ),
            ),
            _ => (colors.success, ws.backend_label.clone()),
        },
        Status::Failed(_) => (colors.danger, t("Codex is not running").to_string()),
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
                        rail_button("rail-expand", IconName::PanelLeft, t("Show sidebar (⌘B)"))
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(ToggleSidebar), cx)
                            }),
                    )
                    .child(
                        rail_button("rail-new", IconName::SquarePen, t("New chat (⌘N)")).on_click(
                            |_, window, cx| window.dispatch_action(Box::new(NewChat), cx),
                        ),
                    )
                    .child(
                        rail_button("rail-search", IconName::Search, t("Search (⌘K)")).on_click(
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
                        rail_button("rail-settings", IconName::Settings, t("Settings (⌘,)"))
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(OpenSettings), cx)
                            }),
                    )
                    .child(div().size(px(8.)).rounded_full().bg(dot)),
            )
            .into_any_element();
    }

    let now = Timestamp::now();
    let in_project = ws.project_root().is_some();
    let in_workflows = matches!(ws.room, crate::workspace::Room::Workflows { .. });
    // Workstreams' sessions live in their project, not among the chats.
    let project_threads = ws.project_threads();
    let conversations: Vec<ChatRow> = ws
        .threads
        .iter()
        .filter(|thread| !project_threads.contains(&thread.id) && ws.chat_shown(&thread.id))
        .map(|thread| ChatRow {
            key: thread.id.clone().into(),
            title: thread.title(),
            at: Timestamp::from_second(thread.updated_at).unwrap_or(now),
            pinned: ws.settings.is_pinned(&thread.id),
            status: ws.chat_status(&thread.id),
            agent: crate::agents::split_backend(&thread.backend).0.to_string(),
            // Chats on other devices carry the device's name.
            device: ws
                .chat_device(&thread.id)
                .map(|device| ws.device_label(&device)),
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
    let extra = {
        // A conversation another device owns can be moved here.
        let remote: std::collections::HashSet<String> = ws
            .threads
            .iter()
            .filter(|thread| ws.sync_is_remote(&thread.id))
            .map(|thread| thread.id.clone())
            .collect();
        let mover = workspace.clone();
        move |key: &SharedString| {
            if !remote.contains(key.as_ref()) {
                return Vec::new();
            }
            let mover = mover.clone();
            vec![(
                t("Move to this computer").into(),
                IconName::Monitor,
                std::rc::Rc::new(move |key: &SharedString, _: &mut Window, cx: &mut App| {
                    let id = key.to_string();
                    mover.update(cx, |ws, cx| ws.move_to_this_computer(id, cx));
                }) as std::rc::Rc<dyn Fn(&SharedString, &mut Window, &mut App)>,
            )]
        }
    };
    let list = ChatList::new(
        conversations,
        active,
        query,
        today,
        move |key, _, cx| {
            let id = key.to_string();
            open.update(cx, |ws, cx| ws.open_thread(id, cx));
        },
        {
            let root = root.clone();
            move |key, window, cx| {
                let id = key.to_string();
                root.update(cx, |root, cx| root.start_rename(id, window, cx));
            }
        },
        move |key, _, cx| {
            let id = key.to_string();
            pin.update(cx, |ws, cx| ws.toggle_pin(&id, cx));
        },
        move |key, _, cx| {
            let id = key.to_string();
            archive.update(cx, |ws, cx| ws.archive(id, cx));
        },
        extra,
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
                        .tooltip(t("Search (⌘K)"))
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(OpenPalette), cx)
                        }),
                )
                .child(
                    IconButton::new("hide-sidebar", IconName::PanelLeft)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip(t("Hide sidebar (⌘B)"))
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(ToggleSidebar), cx)
                        }),
                ),
        )
        .child({
            let switch = workspace.clone();
            div().flex_none().px_3().pb_2().child(
                SegmentedControl::new(
                    "room",
                    if in_project {
                        "projects"
                    } else if in_workflows {
                        "workflows"
                    } else {
                        "chats"
                    },
                )
                .segment("chats", t("Chats"), Some(IconName::MessageSquare))
                .segment("projects", t("Projects"), Some(IconName::Layers))
                .segment("workflows", t("Workflows"), Some(IconName::Workflow))
                .size(ControlSize::Sm)
                .on_change(move |value, _, cx| {
                    let room = value.as_ref();
                    switch.update(cx, |ws, cx| {
                        if room == "projects" {
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
                        } else if room == "workflows" {
                            ws.enter_workflows(None, cx);
                        } else {
                            ws.enter_chats(cx);
                        }
                    })
                }),
            )
        })
        .when(in_workflows, |sidebar| {
            let start = workspace.clone();
            sidebar
                .child(
                    div().flex_none().px_3().pb_2().child(
                        Button::new("new-workflow", t("Start a workflow"))
                            .icon(IconName::Workflow)
                            .variant(ButtonVariant::Outline)
                            .full_width()
                            .on_click(move |_, _, cx| {
                                start.update(cx, |ws, cx| ws.enter_workflows(None, cx))
                            }),
                    ),
                )
                .child(super::motion::enter(
                    div().flex_1().min_h_0().flex().flex_col().child(
                        div()
                            .id("workflow-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .px_2()
                            .pb_2()
                            .child(super::workflow::sidebar_list(workspace, cx)),
                    ),
                    "sidebar-workflows",
                    0,
                    super::motion::Enter::Slide,
                    cx,
                ))
        })
        .when(in_project, |sidebar| {
            let add = workspace.clone();
            sidebar
                .child(
                    div().flex_none().px_3().pb_2().child(
                        Button::new("open-project", t("Open a folder as a project"))
                            .icon(IconName::FolderPlus)
                            .variant(ButtonVariant::Outline)
                            .full_width()
                            .on_click(move |_, _, cx| {
                                super::project::choose_project(add.clone(), cx)
                            }),
                    ),
                )
                .child(super::motion::enter(
                    div().flex_1().min_h_0().flex().flex_col().child(
                        div()
                            .id("project-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .px_2()
                            .pb_2()
                            .child(super::project::sidebar_list(workspace, cx)),
                    ),
                    "sidebar-projects",
                    0,
                    super::motion::Enter::Slide,
                    cx,
                ))
        })
        .when(!in_project && !in_workflows, |sidebar| {
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
                .children(device_chips(workspace, &account_root, cx))
                .child(super::motion::enter(
                    div().flex_1().min_h_0().flex().flex_col().child(
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
                                            t("Loading chats…")
                                        } else {
                                            t("No chats yet.")
                                        })
                                        .child(
                                            div()
                                                .text_size(theme.text_size(TextSize::Xs))
                                                .text_color(colors.fg_subtle)
                                                .child(t("Your Codex conversations show here.")),
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
                                            .child(t("In messages")),
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
                    ),
                    "sidebar-chats",
                    0,
                    super::motion::Enter::Slide,
                    cx,
                ))
        })
        .children(ws.update.clone().map(|update| {
            let url = update.url.clone();
            div()
                .id("update-row")
                .flex_none()
                .mx_2()
                .mb_1()
                .px_2()
                .py_1p5()
                .flex()
                .items_center()
                .gap_2()
                .rounded_md()
                .cursor_pointer()
                .bg(colors.accent.opacity(0.12))
                .text_color(colors.accent)
                .text_size(theme.text_size(TextSize::Sm))
                .on_click(move |_, _, cx| cx.open_url(&url))
                .child(Icon::new(IconName::Download).size(IconSize::Xs))
                .child(div().flex_1().truncate().child(tf(
                    "Atrium {version} is out",
                    &[("version", &update.version)],
                )))
                .child(
                    div()
                        .text_size(theme.text_size(TextSize::Xs))
                        .child(t("Download")),
                )
        }))
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
                    .child(t("API key"))
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
                        t("Finish signing in in your browser…")
                    } else {
                        t("Sign in with ChatGPT")
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
                            .tooltip(t("Restart Codex"))
                            .on_click(move |_, _, cx| retry.update(cx, |ws, cx| ws.connect(cx))),
                    )
                })
                .child({
                    let memory = workspace.clone();
                    IconButton::new("open-memory", IconName::Lightbulb)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip(t("Memory"))
                        .on_click(move |_, _, cx| memory.update(cx, |ws, cx| ws.open_memory(cx)))
                })
                .child(
                    IconButton::new("open-settings", IconName::Settings)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip(t("Settings (⌘,)"))
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(OpenSettings), cx)
                        }),
                ),
        )
        .into_any_element()
}

/// Chips over the chat list to show one device's chats, when chats from
/// other devices are in it. Each device's dot says whether it is online.
/// A chip: its id, label, filter, whether its device is online, and the device.
type Chip = (
    SharedString,
    String,
    crate::workspace::DeviceFilter,
    Option<bool>,
    Option<String>,
);

fn device_chips(
    workspace: &Entity<Workspace>,
    root: &Entity<RootView>,
    cx: &App,
) -> Option<AnyElement> {
    use crate::workspace::DeviceFilter;
    let ws = workspace.read(cx);
    let devices = ws.chat_devices();
    if devices.is_empty() {
        return None;
    }
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let current = ws.device_filter.clone();
    let mut chips: Vec<Chip> = vec![
        (
            "chip-all".into(),
            t("All").into(),
            DeviceFilter::All,
            None,
            None,
        ),
        (
            "chip-here".into(),
            t("This computer").into(),
            DeviceFilter::Here,
            None,
            None,
        ),
    ];
    for device in devices {
        let online = ws.device_online(&device);
        chips.push((
            format!("chip-{device}").into(),
            ws.device_label(&device),
            DeviceFilter::Device(device.clone()),
            Some(online),
            Some(device),
        ));
    }
    let row = div()
        .flex_none()
        .flex()
        .flex_wrap()
        .gap_1()
        .px_3()
        .pb_2()
        .children(
            chips
                .into_iter()
                .map(|(id, label, filter, online, device)| {
                    let root = root.clone();
                    let on = filter == current;
                    let pick = workspace.clone();
                    div()
                        .id(id)
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .py_0p5()
                        .rounded_full()
                        .cursor_pointer()
                        .text_size(theme.text_size(TextSize::Xs))
                        .border_1()
                        .border_color(if on {
                            colors.border_strong
                        } else {
                            colors.border
                        })
                        .when(on, |c| c.bg(colors.active).text_color(colors.fg))
                        .when(!on, |c| {
                            c.text_color(colors.fg_muted).hover(|c| c.bg(colors.hover))
                        })
                        .children(online.map(|online| {
                            div().size(px(6.)).rounded_full().bg(if online {
                                colors.success
                            } else {
                                colors.fg_subtle
                            })
                        }))
                        .child(label)
                        // Right-click a device to give it a name of your own.
                        .when_some(device, |chip, device| {
                            chip.tooltip(ely_gpui_component::primitives::Tooltip::text(t(
                                "Right-click to rename",
                            )))
                            .on_mouse_down(
                                gpui::MouseButton::Right,
                                move |_, window, cx| {
                                    let device = device.clone();
                                    root.update(cx, |root, cx| {
                                        root.start_rename_device(device, window, cx)
                                    })
                                },
                            )
                        })
                        .on_click(move |_, _, cx| {
                            let filter = filter.clone();
                            pick.update(cx, |ws, cx| ws.set_device_filter(filter, cx))
                        })
                }),
        );
    Some(row.into_any_element())
}
