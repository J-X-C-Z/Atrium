//! Main column: title bar (model, chat title, status, chat menu), the
//! conversation or the welcome screen, and the composer.

use ely_gpui_component::{
    buttons::{Button, ButtonVariant, IconButton},
    chat::{ChatContainer, ErrorMessage, MessageList, SuggestionChips, WelcomeScreen},
    menus::{DropdownMenu, Menu, MenuItem, OverflowMenu},
    motion::{ProgressBar, Skeleton, SkeletonText, Spinner, SpinnerStyle},
    primitives::{Icon, IconName},
    shell::drag_region,
    theme::{ActiveTheme, ControlSize, IconSize, Radius, TextSize},
};
use gpui::{
    AnyElement, App, Entity, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use jiff::{Timestamp, tz::TimeZone};

use super::{
    NewChat, RootView, TITLE_BAR_HEIGHT,
    composer::{Composer, short_path},
    context::format_tokens,
    rows::{self, COLUMN},
};
use atrium_core::GoalStatus;

use crate::workspace::{AgentStatus, CODEX, Status, View, Workspace};

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
    let header = header(workspace, root.clone(), sidebar_collapsed, window, cx);
    let goal = goal_banner(workspace, root, cx);
    let body = body(workspace, composer, window, cx);
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(colors.bg)
        .child(header)
        .children(super::project::stream_bar(workspace, cx))
        .children(goal)
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
    let quick_view = super::context::quick_view(ws, root.clone(), cx);
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

    let memory = ws.memory_enabled();
    // How much of the plan's usage is spent: a ring that opens the details.
    // Codex's plan usage, shown in Codex chats only.
    let usage_meter = ws
        .rate_limits
        .as_ref()
        .filter(|_| ws.is_codex_view())
        .and_then(|l| l.highest_used())
        .map(|used| {
            let open = root.clone();
            let reached = ws.rate_limits.as_ref().is_some_and(|l| l.reached);
            div()
                .id("usage-meter")
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .px_1p5()
                .py_0p5()
                .rounded_full()
                .cursor_pointer()
                .hover(|meter| meter.bg(colors.hover))
                .on_click(move |_, _, cx| open.update(cx, |root, cx| root.open_account(cx)))
                .tooltip(ely_gpui_component::primitives::Tooltip::text(format!(
                    "Plan usage: {used:.0}% of the closest limit"
                )))
                .child(
                    Icon::new(IconName::Gauge)
                        .size(IconSize::Xs)
                        .color(colors.fg_subtle),
                )
                .child(
                    div()
                        .text_size(theme.text_size(TextSize::Xs))
                        .text_color(if reached || used >= 90. {
                            colors.danger
                        } else {
                            colors.fg_muted
                        })
                        .child(format!("{used:.0}%")),
                )
        });

    // Chat menu. Rename, review, compact, goals, branches and memory are Codex's.
    let codex = ws.is_codex_view();
    let chat_menu = thread_id.as_ref().map(|id| {
        let pinned = ws.settings.is_pinned(id);
        let mut menu = Menu::new();
        if codex {
            let rename = (root.clone(), id.clone());
            menu = menu.item(MenuItem::new("Rename…").icon(IconName::Pencil).on_click(
                move |window, cx| {
                    let (root, id) = rename.clone();
                    root.update(cx, |root, cx| root.start_rename(id, window, cx));
                },
            ));
        }
        let pin = (workspace.clone(), id.clone());
        menu = menu.item(
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
        );
        if codex {
            let (review, compact) = (workspace.clone(), workspace.clone());
            menu = menu
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
                .item(
                    MenuItem::new("Set a goal…")
                        .icon(IconName::ListChecks)
                        .on_click({
                            let root = root.clone();
                            move |window, cx| root.update(cx, |root, cx| root.open_goal(window, cx))
                        }),
                )
                .item(
                    MenuItem::new("Branch into a new chat")
                        .icon(IconName::GitBranch)
                        .on_click({
                            let workspace = workspace.clone();
                            move |_, cx| workspace.update(cx, |ws, cx| ws.branch(None, cx))
                        }),
                )
                .item(MenuItem::check("Use memory", memory).on_click({
                    let workspace = workspace.clone();
                    move |_, cx| workspace.update(cx, |ws, cx| ws.toggle_memory(cx))
                }));
        }
        let archive = (workspace.clone(), id.clone());
        let details = root.clone();
        menu = menu
            .separator()
            .item(
                MenuItem::new("Context & speed")
                    .icon(IconName::Zap)
                    .keys("⌘I")
                    .on_click(move |_, cx| details.update(cx, |root, cx| root.open_context(cx))),
            )
            .separator()
            .item(
                MenuItem::new("Archive")
                    .icon(IconName::Archive)
                    .on_click(move |_, cx| {
                        let (ws, id) = archive.clone();
                        ws.update(cx, |ws, cx| ws.archive(id, cx));
                    }),
            );
        OverflowMenu::new("chat-menu", menu)
    });

    // Which agent, and on which device: chosen for a new chat, fixed once
    // the chat exists.
    let agent_picker = {
        let backend = ws.view_backend().to_string();
        let label = crate::agents::backend_label(&backend);
        if thread_id.is_none() {
            let item = |id: String, name: String| {
                let workspace = workspace.clone();
                let state = backend_status(ws, &id);
                MenuItem::radio(agent_label(&name, state), backend == id)
                    .disabled(state == Some("not installed"))
                    .on_click(move |_, cx| {
                        workspace.update(cx, |ws, cx| ws.set_draft_backend(&id, cx))
                    })
            };
            let here: Vec<MenuItem> = std::iter::once(item(CODEX.into(), "Codex".into()))
                .chain(
                    ws.agents
                        .iter()
                        .filter(|a| a.remote.is_none())
                        .map(|a| item(a.profile.id.clone(), a.profile.name.clone())),
                )
                .collect();
            let mut menu = if ws.devices.is_empty() {
                here.into_iter().fold(Menu::new(), |menu, i| menu.item(i))
            } else {
                Menu::new().group("This computer", here)
            };
            for device in &ws.devices {
                let mut items = Vec::new();
                let codex = crate::agents::backend_id(CODEX, Some(&device.id));
                if device.codex_status != AgentStatus::NotInstalled {
                    items.push(item(codex, "Codex".into()));
                }
                items.extend(
                    ws.agents
                        .iter()
                        .filter(|a| a.remote.as_ref().is_some_and(|r| r.device == device.id))
                        .map(|a| item(a.profile.id.clone(), a.profile.name.clone())),
                );
                let title = match &device.status {
                    crate::workspace::DeviceStatus::Online => device.id.clone(),
                    crate::workspace::DeviceStatus::Offline(_) => {
                        format!("{} · offline", device.id)
                    }
                };
                if items.is_empty() {
                    items.push(MenuItem::new("No agents there").disabled(true));
                }
                menu = menu.group(title, items);
            }
            DropdownMenu::new("agent", label, menu)
                .icon(if ws.is_remote_view() {
                    IconName::Monitor
                } else {
                    IconName::Bot
                })
                .variant(ButtonVariant::Ghost)
                .into_any_element()
        } else {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1p5()
                .px_2()
                .text_size(theme.text_size(TextSize::Sm))
                .child(
                    Icon::new(if ws.is_remote_view() {
                        IconName::Monitor
                    } else {
                        IconName::Bot
                    })
                    .size(IconSize::Sm)
                    .color(colors.fg_muted),
                )
                .child(label)
                .into_any_element()
        }
    };

    // The link to the device a remote chat runs on, when it is not well.
    let link = ws
        .link_state(ws.view_backend())
        .and_then(|state| {
            let device = crate::agents::split_backend(ws.view_backend())
                .1
                .unwrap_or_default()
                .to_string();
            match state {
                agent_rpc::node::LinkState::Reconnecting { attempt, .. } => Some((
                    colors.warning,
                    format!("Reconnecting to {device}… ({attempt})"),
                )),
                agent_rpc::node::LinkState::Gone(why) => {
                    Some((colors.danger, format!("{device}: {why}")))
                }
                agent_rpc::node::LinkState::Ended { .. } => {
                    Some((colors.danger, format!("The agent on {device} stopped")))
                }
                _ => None,
            }
        })
        .map(|(color, text)| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .py_0p5()
                .rounded_full()
                .bg(colors.sunken)
                .text_size(theme.text_size(TextSize::Xs))
                .text_color(color)
                .child(Icon::new(IconName::Monitor).size(IconSize::Xs).color(color))
                .child(text)
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
        .child(agent_picker)
        .children(link)
        .when(codex && !ws.is_remote_view(), |bar| {
            bar.child(
                DropdownMenu::new("model", ws.model_label(), models)
                    .icon(IconName::Sparkles)
                    .variant(ButtonVariant::Ghost),
            )
        })
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
        .children(quick_view)
        .children(usage_meter)
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
        let folder = short_path(&ws.folder());
        return WelcomeScreen::new("welcome", hour)
            .child(
                div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .mt_neg_4()
                    .text_size(theme.text_size(TextSize::Sm))
                    .text_color(colors.fg_muted)
                    .child(readiness(ws, &folder)),
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

/// The chat's goal under the title bar: what, how far, and its controls.
fn goal_banner(
    workspace: &Entity<Workspace>,
    root: Entity<RootView>,
    cx: &mut App,
) -> Option<AnyElement> {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let goal = workspace.read(cx).goal()?.clone();
    let active = goal.status == GoalStatus::Active;
    let tint = match goal.status {
        GoalStatus::Active => colors.accent,
        GoalStatus::Complete => colors.success,
        GoalStatus::Paused => colors.fg_muted,
        _ => colors.warning,
    };
    let spent = match goal.token_budget {
        Some(budget) if budget > 0 => format!(
            "{} of {} tokens",
            format_tokens(goal.tokens_used),
            format_tokens(budget)
        ),
        _ => format!("{} tokens", format_tokens(goal.tokens_used)),
    };
    let share = goal
        .token_budget
        .filter(|b| *b > 0)
        .map(|b| (goal.tokens_used as f32 / b as f32).min(1.));
    let (toggle, clear) = (workspace.clone(), workspace.clone());
    Some(
        div()
            .flex_none()
            .w_full()
            .flex()
            .justify_center()
            .px_6()
            .pt_2()
            .child(
                div()
                    .w_full()
                    .max_w(px(COLUMN))
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius(Radius::Lg))
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.surface)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(theme.text_size(TextSize::Sm))
                            .child(
                                Icon::new(IconName::ListChecks)
                                    .size(IconSize::Sm)
                                    .color(tint),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(goal.objective.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(theme.text_size(TextSize::Xs))
                                    .text_color(tint)
                                    .child(goal.status.label()),
                            )
                            .when(goal.status != GoalStatus::Complete, |row| {
                                row.child(
                                    Button::new(
                                        "goal-toggle",
                                        if active { "Pause" } else { "Resume" },
                                    )
                                    .variant(ButtonVariant::Ghost)
                                    .size(ControlSize::Sm)
                                    .on_click(
                                        move |_, _, cx| {
                                            let next = if active {
                                                GoalStatus::Paused
                                            } else {
                                                GoalStatus::Active
                                            };
                                            toggle.update(cx, |ws, cx| ws.set_goal_status(next, cx))
                                        },
                                    ),
                                )
                            })
                            .child(
                                IconButton::new("goal-edit", IconName::Pencil)
                                    .variant(ButtonVariant::Ghost)
                                    .size(ControlSize::Sm)
                                    .tooltip("Change the goal")
                                    .on_click(move |_, window, cx| {
                                        root.update(cx, |root, cx| root.open_goal(window, cx))
                                    }),
                            )
                            .child(
                                IconButton::new("goal-clear", IconName::X)
                                    .variant(ButtonVariant::Ghost)
                                    .size(ControlSize::Sm)
                                    .tooltip("Clear the goal")
                                    .on_click(move |_, _, cx| {
                                        clear.update(cx, |ws, cx| ws.clear_goal(cx))
                                    }),
                            ),
                    )
                    .when_some(share, |column, share| {
                        column.child(ProgressBar::new("goal-budget", share))
                    })
                    .child(
                        div()
                            .text_size(theme.text_size(TextSize::Xs))
                            .text_color(colors.fg_muted)
                            .child(format!(
                                "{spent} · {}",
                                super::rows::format_duration(std::time::Duration::from_secs(
                                    goal.time_used_secs
                                ),)
                            )),
                    ),
            )
            .into_any_element(),
    )
}

fn agent_label(name: &str, status: Option<&'static str>) -> String {
    match status {
        Some(status) => format!("{name} · {status}"),
        None => name.to_string(),
    }
}

fn codex_status(ws: &Workspace) -> Option<&'static str> {
    match ws.status {
        Status::Ready => None,
        Status::Connecting => Some("starting"),
        Status::Failed(_) => Some("not running"),
    }
}

fn agent_status_text(status: &AgentStatus) -> Option<&'static str> {
    match status {
        AgentStatus::Ready => None,
        AgentStatus::Starting => Some("starting"),
        AgentStatus::NotInstalled => Some("not installed"),
        AgentStatus::Failed(_) => Some("not running"),
    }
}

/// Why a backend cannot take a message, when it cannot.
fn backend_status(ws: &Workspace, backend: &str) -> Option<&'static str> {
    match crate::agents::split_backend(backend) {
        (CODEX, None) => codex_status(ws),
        (CODEX, Some(device)) => match ws.device(device) {
            Some(device) if device.status != crate::workspace::DeviceStatus::Online => {
                Some("offline")
            }
            Some(device) => agent_status_text(&device.codex_status),
            None => Some("not running"),
        },
        _ => match ws.agent(backend) {
            Some(agent) => agent_status_text(&agent.status),
            None => Some("not running"),
        },
    }
}

/// "Claude Code is ready in ~/app", or why it is not.
fn readiness(ws: &Workspace, folder: &str) -> String {
    let backend = ws.view_backend();
    let name = crate::agents::backend_label(backend);
    match backend_status(ws, backend) {
        None => format!("{name} is ready in {folder}"),
        Some("starting") => format!("Starting {name}…"),
        Some("offline") => format!("{name}: the device is offline."),
        Some("not installed") => {
            let install = ws
                .agent(backend)
                .map(|a| a.profile.install.clone())
                .unwrap_or_default();
            format!("{name} is not installed. {install}")
        }
        Some(_) => format!("{name} is not running. Pick it again to restart it."),
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
