//! Main column: title bar (model, chat title, status, chat menu), the
//! conversation or the welcome screen, and the composer.

use ely_gpui_component::{
    buttons::{Button, ButtonVariant, IconButton},
    chat::{ChatContainer, ErrorMessage, MessageList, SuggestionChips, WelcomeScreen},
    menus::{DropdownMenu, Menu, MenuItem, OverflowMenu},
    motion::{ProgressBar, ProgressRing, Skeleton, SkeletonText, Spinner, SpinnerStyle},
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
    rows::{self, COLUMN},
};
use agentdesk_core::GoalStatus;

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

    let memory = ws.memory_enabled();
    // How much of the plan's usage is spent: a ring that opens the details.
    let usage_meter = ws
        .rate_limits
        .as_ref()
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
                .child(ProgressRing::new("usage-ring", (used / 100.) as f32).size(gpui::rems(0.9)))
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
        menu = menu
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

    // Which agent: chosen for a new chat, fixed once the chat exists.
    let agent_picker = {
        let backend = ws.view_backend().to_string();
        let label = crate::agents::backend_name(&backend);
        if thread_id.is_none() {
            let mut menu = Menu::new().item(
                MenuItem::radio(agent_label("Codex", codex_status(ws)), backend == CODEX).on_click(
                    {
                        let workspace = workspace.clone();
                        move |_, cx| workspace.update(cx, |ws, cx| ws.set_draft_backend(CODEX, cx))
                    },
                ),
            );
            for agent in &ws.agents {
                let id = agent.profile.id;
                let installed = agent.status != AgentStatus::NotInstalled;
                let workspace = workspace.clone();
                menu = menu.item(
                    MenuItem::radio(
                        agent_label(agent.profile.name, agent_status_text(&agent.status)),
                        backend == id,
                    )
                    .disabled(!installed)
                    .on_click(move |_, cx| {
                        workspace.update(cx, |ws, cx| ws.set_draft_backend(id, cx))
                    }),
                );
            }
            DropdownMenu::new("agent", label, menu)
                .icon(IconName::Bot)
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
                    Icon::new(IconName::Bot)
                        .size(IconSize::Sm)
                        .color(colors.fg_muted),
                )
                .child(label)
                .into_any_element()
        }
    };

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
        .when(codex, |bar| {
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
        .children(usage.filter(|_| running.is_none()).map(|usage| {
            div()
                .flex_none()
                .text_size(theme.text_size(TextSize::Xs))
                .text_color(colors.fg_subtle)
                .child(usage)
        }))
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

fn format_tokens(tokens: u64) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..1_000_000 => format!("{:.1}k", tokens as f64 / 1_000.),
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.),
    }
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

/// "Claude Code is ready in ~/app", or why it is not.
fn readiness(ws: &Workspace, folder: &str) -> String {
    let backend = ws.view_backend();
    let name = crate::agents::backend_name(backend);
    let state = if backend == CODEX {
        codex_status(ws)
    } else {
        ws.agent(backend).and_then(|a| agent_status_text(&a.status))
    };
    match state {
        None => format!("{name} is ready in {folder}"),
        Some("starting") => format!("Starting {name}…"),
        Some("not installed") => {
            let install = ws
                .agent(backend)
                .map(|a| a.profile.install)
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
