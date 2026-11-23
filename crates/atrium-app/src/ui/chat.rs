//! Main column: title bar (model, chat title, status, chat menu), the
//! conversation or the welcome screen, and the composer.

use ely_gpui_component::{
    buttons::{Button, ButtonVariant, IconButton},
    chat::{ChatContainer, ErrorMessage, MessageList, SuggestionChips, WelcomeScreen},
    menus::{Menu, MenuItem, OverflowMenu},
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

use crate::i18n::{t, tf};
use crate::workspace::{AgentStatus, CODEX, Composed, Status, View, Workspace};

const STARTERS: [(&str, IconName); 4] = [
    ("Explain this project", IconName::FileText),
    ("Fix a failing test", IconName::Terminal),
    ("Review my changes", IconName::GitBranch),
    ("Plan a feature", IconName::ListChecks),
];

#[allow(clippy::too_many_arguments)]
pub fn render(
    workspace: &Entity<Workspace>,
    composer: &Entity<Composer>,
    root: Entity<RootView>,
    sidebar_collapsed: bool,
    changes_open: bool,
    editor: Option<&'static str>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let colors = cx.theme().colors.clone();
    let header = header(
        workspace,
        root.clone(),
        sidebar_collapsed,
        changes_open,
        editor,
        window,
        cx,
    );
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
    open: bool,
    editor: Option<&'static str>,
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
                .tooltip(ely_gpui_component::primitives::Tooltip::text(tf(
                    "Plan usage: {used}% of the closest limit",
                    &[("used", &format!("{used:.0}"))],
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
            menu = menu.item(MenuItem::new(t("Rename…")).icon(IconName::Pencil).on_click(
                move |window, cx| {
                    let (root, id) = rename.clone();
                    root.update(cx, |root, cx| root.start_rename(id, window, cx));
                },
            ));
        }
        let pin = (workspace.clone(), id.clone());
        menu = menu.item(
            MenuItem::new(if pinned { t("Unpin") } else { t("Pin to top") })
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
        if !ws.sync_is_remote(id) {
            let (workspace, id) = (workspace.clone(), id.clone());
            menu = menu.item(
                MenuItem::new(t("Learn from this conversation"))
                    .icon(IconName::Lightbulb)
                    .on_click(move |_, cx| {
                        workspace.update(cx, |ws, cx| ws.learn_from_chat(&id, cx));
                    }),
            );
        }
        if ws.sync_is_remote(id) {
            let (workspace, id) = (workspace.clone(), id.clone());
            menu = menu.item(
                MenuItem::new(t("Move to this computer"))
                    .icon(IconName::Monitor)
                    .on_click(move |_, cx| {
                        workspace.update(cx, |ws, cx| ws.move_to_this_computer(id.clone(), cx));
                    }),
            );
        }
        if codex {
            let (review, compact) = (workspace.clone(), workspace.clone());
            menu = menu
                .separator()
                .item(
                    MenuItem::new(t("Review uncommitted changes"))
                        .icon(IconName::Eye)
                        .on_click(move |_, cx| review.update(cx, |ws, cx| ws.review(None, cx))),
                )
                .item(
                    MenuItem::new(t("Compact conversation"))
                        .icon(IconName::Minimize2)
                        .on_click(move |_, cx| compact.update(cx, |ws, cx| ws.compact(cx))),
                )
                .separator()
                .item(
                    MenuItem::new(t("Set a goal…"))
                        .icon(IconName::ListChecks)
                        .on_click({
                            let root = root.clone();
                            move |window, cx| root.update(cx, |root, cx| root.open_goal(window, cx))
                        }),
                )
                .item(
                    MenuItem::new(t("Branch into a new chat"))
                        .icon(IconName::GitBranch)
                        .on_click({
                            let workspace = workspace.clone();
                            move |_, cx| workspace.update(cx, |ws, cx| ws.branch(None, cx))
                        }),
                )
                .item(MenuItem::check(t("Use memory"), memory).on_click({
                    let workspace = workspace.clone();
                    move |_, cx| workspace.update(cx, |ws, cx| ws.toggle_memory(cx))
                }));
        }
        if ws.chat_tree().is_some() {
            let (merge, remove) = (workspace.clone(), workspace.clone());
            menu = menu
                .separator()
                .item(
                    MenuItem::new(t("Merge this worktree into the main branch"))
                        .icon(IconName::GitMerge)
                        .on_click(move |_, cx| merge.update(cx, |ws, cx| ws.merge_chat_tree(cx))),
                )
                .item(
                    MenuItem::new(t("Remove this worktree"))
                        .icon(IconName::Trash2)
                        .on_click(move |_, cx| remove.update(cx, |ws, cx| ws.remove_chat_tree(cx))),
                );
        }
        if ws.can_undo_restore() {
            let undo = workspace.clone();
            menu = menu.separator().item(
                MenuItem::new(t("Undo the last file restore"))
                    .icon(IconName::Redo2)
                    .on_click(move |_, cx| undo.update(cx, |ws, cx| ws.undo_restore(cx))),
            );
        }
        // The folder elsewhere, and the conversation as a file.
        let local = ws.local_dir(id).is_some();
        if local {
            if let Some(editor) = editor {
                let open = root.clone();
                menu = menu.separator().item(
                    MenuItem::new(tf("Open in {editor}", &[("editor", &editor)]))
                        .icon(IconName::Code)
                        .on_click(move |_, cx| {
                            open.update(cx, |r, cx| {
                                if let Some(dir) = r.chat_dir(cx) {
                                    r.open_in_editor(&dir, None, cx)
                                }
                            })
                        }),
                );
            } else {
                menu = menu.separator();
            }
            let reveal = root.clone();
            menu = menu.item(
                MenuItem::new(t("Show in file manager"))
                    .icon(IconName::FolderOpen)
                    .on_click(move |_, cx| reveal.update(cx, |r, cx| r.reveal_chat_dir(cx))),
            );
        }
        let (export, copy) = (root.clone(), root.clone());
        menu = menu
            .separator()
            .item(
                MenuItem::new(t("Export as Markdown…"))
                    .icon(IconName::Download)
                    .on_click(move |_, cx| export.update(cx, |r, cx| r.export_markdown(cx))),
            )
            .item(
                MenuItem::new(t("Copy as Markdown"))
                    .icon(IconName::Copy)
                    .on_click(move |_, cx| copy.update(cx, |r, cx| r.copy_markdown(cx))),
            );
        let archive = (workspace.clone(), id.clone());
        let details = root.clone();
        menu = menu
            .separator()
            .item(
                MenuItem::new(t("Context & speed"))
                    .icon(IconName::Zap)
                    .keys("secondary-i")
                    .on_click(move |_, cx| details.update(cx, |root, cx| root.open_context(cx))),
            )
            .separator()
            .item(
                MenuItem::new(t("Archive"))
                    .icon(IconName::Archive)
                    .on_click(move |_, cx| {
                        let (ws, id) = archive.clone();
                        ws.update(cx, |ws, cx| ws.archive(id, cx));
                    }),
            );
        OverflowMenu::new("chat-menu", menu).tooltip(t("More"))
    });

    // The link to the device a remote chat runs on, when it is not well.
    let link = ws
        .link_state(ws.view_backend())
        .and_then(|state| {
            let device = crate::agents::split_backend(ws.view_backend())
                .1
                .unwrap_or_default();
            let device = ws.device_label(device);
            match state {
                agent_rpc::node::LinkState::Reconnecting { attempt, .. } => Some((
                    colors.warning,
                    tf(
                        "Reconnecting to {device}… ({attempt})",
                        &[("device", &device), ("attempt", &attempt)],
                    ),
                )),
                agent_rpc::node::LinkState::Gone(why) => {
                    Some((colors.danger, format!("{device}: {why}")))
                }
                agent_rpc::node::LinkState::Ended { .. } => Some((
                    colors.danger,
                    tf("The agent on {device} stopped", &[("device", &device)]),
                )),
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
        .children(link)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .justify_center()
                .text_size(theme.text_size(TextSize::Sm))
                .text_color(colors.fg_muted)
                .gap_2()
                .items_center()
                .children(title.map(|title| div().truncate().child(title)))
                .children(ws.chat_tree().and_then(|dir| {
                    let name = dir.file_name()?.to_string_lossy().to_string();
                    Some(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_1p5()
                            .rounded_md()
                            .bg(colors.sunken)
                            .text_size(theme.text_size(TextSize::Xs))
                            .child(Icon::new(IconName::GitBranch).size(IconSize::Xs))
                            .child(format!("atrium/{name}")),
                    )
                })),
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
                .child(tf("Working · {secs}s", &[("secs", &took.as_secs())]))
        }))
        .children(quick_view)
        .children(usage_meter)
        .children({
            let files = workspace
                .read(cx)
                .transcript()
                .map(|t| super::changes::changed_files(t).len())
                .unwrap_or(0);
            (files > 0 || open).then(|| {
                let toggle = root.clone();
                Button::new("header-changes", files.to_string())
                    .icon(IconName::GitBranch)
                    .variant(if open {
                        ButtonVariant::Secondary
                    } else {
                        ButtonVariant::Ghost
                    })
                    .size(ControlSize::Sm)
                    .on_click(move |_, _, cx| toggle.update(cx, |r, cx| r.toggle_changes(cx)))
            })
        })
        .when(thread_id.is_some(), |bar| {
            let toggle = root.clone();
            bar.child(
                IconButton::new("header-side", IconName::Columns2)
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .tooltip(t("Activity and side by side"))
                    .on_click(move |_, _, cx| {
                        toggle.update(cx, |r, cx| {
                            r.toggle_side(super::changes::SideTab::Activity, cx)
                        })
                    }),
            )
        })
        .children(chat_menu)
        .when(thread_id.is_some(), |bar| {
            bar.child(
                IconButton::new("header-new-chat", IconName::SquarePen)
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .tooltip(t("New chat (⌘N)"))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(NewChat), cx)),
            )
        })
        .into_any_element()
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
                    ErrorMessage::new("codex-failed", t("Could not start Codex"))
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
                STARTERS.map(|(text, icon)| (t(text), icon)),
                move |ix, window, cx| {
                    fill.update(cx, |composer, cx| {
                        composer.set_text(t(STARTERS[ix].0), window, cx)
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
    let next = next_steps_strip(workspace, cx);
    let reports = doc_reports_strip(workspace, cx);
    let offline = offline_strip(workspace, cx);
    ChatContainer::new(messages)
        .composer(
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap_2()
                .children(offline)
                .children(next)
                .children(reports)
                .child(composer_column(composer)),
        )
        .into_any_element()
}

/// The shown chat lives on a device that is offline: what shows is the last
/// copy, and nothing can be sent until it is back.
fn offline_strip(workspace: &Entity<Workspace>, cx: &App) -> Option<AnyElement> {
    let device = workspace.read(cx).offline_owner()?;
    let theme = cx.theme();
    let colors = theme.colors.clone();
    Some(
        div()
            .w_full()
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(COLUMN))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1p5()
                    .rounded(theme.radius(Radius::Md))
                    .bg(colors.warning.opacity(0.12))
                    .text_size(theme.text_size(TextSize::Sm))
                    .child(
                        Icon::new(IconName::WifiOff)
                            .size(IconSize::Xs)
                            .color(colors.warning),
                    )
                    .child(div().flex_1().min_w_0().child(tf(
                        "{device} is offline. This is the copy from when it was last seen, read-only; you can continue once it is back.",
                        &[("device", &device)],
                    ))),
            )
            .into_any_element(),
    )
}

/// Documents you edited and saved, whose changes go with your next message.
fn doc_reports_strip(workspace: &Entity<Workspace>, cx: &App) -> Option<AnyElement> {
    let files = workspace.read(cx).pending_doc_reports();
    if files.is_empty() {
        return None;
    }
    let names: Vec<String> = files
        .iter()
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
        .collect();
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let paths: Vec<std::path::PathBuf> = files.iter().map(|p| p.to_path_buf()).collect();
    let drop = workspace.clone();
    Some(
        div()
            .w_full()
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(COLUMN))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1p5()
                    .rounded(theme.radius(Radius::Md))
                    .bg(colors.accent.opacity(0.08))
                    .text_size(theme.text_size(TextSize::Sm))
                    .child(
                        Icon::new(IconName::FileText)
                            .size(IconSize::Xs)
                            .color(colors.accent),
                    )
                    .child(div().flex_1().min_w_0().child(tf(
                        "Your changes to {files} go to the agent with your next message.",
                        &[("files", &names.join("、"))],
                    )))
                    .child(
                        IconButton::new("doc-reports-drop", IconName::X)
                            .variant(ButtonVariant::Ghost)
                            .size(ControlSize::Sm)
                            .tooltip(t("Don't send"))
                            .on_click(move |_, _, cx| {
                                drop.update(cx, |ws, cx| {
                                    for p in &paths {
                                        ws.drop_doc_report(p, cx);
                                    }
                                })
                            }),
                    ),
            )
            .into_any_element(),
    )
}

/// The follow-ups the agent proposed, each to send here or start in a new
/// conversation, or all at once.
fn next_steps_strip(workspace: &Entity<Workspace>, cx: &App) -> Option<AnyElement> {
    let steps = workspace.read(cx).suggested_next();
    if steps.is_empty() {
        return None;
    }
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let small = theme.text_size(TextSize::Sm);
    let mut list = div()
        .w_full()
        .max_w(px(COLUMN))
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded(theme.radius(Radius::Lg))
        .border_1()
        .border_color(colors.border)
        .bg(colors.surface)
        .child({
            let (all, all_steps, dismiss) = (workspace.clone(), steps.clone(), workspace.clone());
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_1()
                .child(
                    Icon::new(IconName::Sparkles)
                        .size(IconSize::Xs)
                        .color(colors.accent),
                )
                .child(
                    div()
                        .flex_1()
                        .text_size(theme.text_size(TextSize::Xs))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(colors.fg_subtle)
                        .child(t("Suggested next")),
                )
                .child(
                    Button::new("next-all", tf("Start all {n}", &[("n", &steps.len())]))
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| {
                            let steps = all_steps.clone();
                            all.update(cx, |ws, cx| {
                                for step in steps {
                                    ws.start_chat(step, cx);
                                }
                                ws.dismiss_next(cx);
                            })
                        }),
                )
                .child(
                    IconButton::new("next-dismiss", IconName::X)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip(t("Dismiss"))
                        .on_click(move |_, _, cx| dismiss.update(cx, |ws, cx| ws.dismiss_next(cx))),
                )
        });
    for (ix, step) in steps.into_iter().enumerate() {
        let (here, new, text_here, text_new) = (
            workspace.clone(),
            workspace.clone(),
            step.clone(),
            step.clone(),
        );
        list = list.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_1()
                .py_0p5()
                .rounded(theme.radius(Radius::Md))
                .hover(|el| el.bg(colors.hover))
                .text_size(small)
                .child(div().flex_1().min_w_0().truncate().child(step))
                .child(
                    IconButton::new(
                        gpui::SharedString::from(format!("next-here-{ix}")),
                        IconName::Send,
                    )
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .tooltip(t("Send here"))
                    .on_click(move |_, _, cx| {
                        let text = text_here.clone();
                        here.update(cx, |ws, cx| {
                            ws.dismiss_next(cx);
                            ws.send(Composed::text(text), cx);
                        })
                    }),
                )
                .child(
                    IconButton::new(
                        gpui::SharedString::from(format!("next-new-{ix}")),
                        IconName::SquarePen,
                    )
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .tooltip(t("Start in a new conversation"))
                    .on_click(move |_, _, cx| {
                        let text = text_new.clone();
                        new.update(cx, |ws, cx| ws.start_chat(text, cx))
                    }),
                ),
        );
    }
    Some(
        div()
            .w_full()
            .flex()
            .justify_center()
            .child(list)
            .into_any_element(),
    )
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
        Some(budget) if budget > 0 => tf(
            "{used} of {budget} tokens",
            &[
                ("used", &format_tokens(goal.tokens_used)),
                ("budget", &format_tokens(budget)),
            ],
        ),
        _ => tf(
            "{used} tokens",
            &[("used", &format_tokens(goal.tokens_used))],
        ),
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
                                    .child(t(goal.status.label())),
                            )
                            .when(goal.status != GoalStatus::Complete, |row| {
                                row.child(
                                    Button::new(
                                        "goal-toggle",
                                        if active { t("Pause") } else { t("Resume") },
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
                                    .tooltip(t("Change the goal"))
                                    .on_click(move |_, window, cx| {
                                        root.update(cx, |root, cx| root.open_goal(window, cx))
                                    }),
                            )
                            .child(
                                IconButton::new("goal-clear", IconName::X)
                                    .variant(ButtonVariant::Ghost)
                                    .size(ControlSize::Sm)
                                    .tooltip(t("Clear the goal"))
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

pub(super) fn agent_label(name: &str, status: Option<&'static str>) -> String {
    match status {
        Some(status) => format!("{name} · {}", t(status)),
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
pub(super) fn backend_status(ws: &Workspace, backend: &str) -> Option<&'static str> {
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
    if let Some(device) = ws.offline_owner() {
        return tf(
            "{device} is offline. This conversation has not been copied here yet; it shows once {device} is back.",
            &[("device", &device)],
        );
    }
    if let Some(id) = ws.active_thread_id()
        && ws.sync_is_remote(id)
    {
        let device = ws.device_label(&ws.chat_device(id).unwrap_or_default());
        return tf("Runs on {device}", &[("device", &device)]);
    }
    let backend = ws.view_backend();
    let name = ws.backend_label(backend);
    match backend_status(ws, backend) {
        None => tf(
            "{name} is ready in {folder}",
            &[("name", &name), ("folder", &folder)],
        ),
        Some("starting") => tf("Starting {name}…", &[("name", &name)]),
        Some("offline") => tf("{name}: the device is offline.", &[("name", &name)]),
        Some("not installed") => {
            let install = ws
                .agent(backend)
                .map(|a| a.profile.install.clone())
                .unwrap_or_default();
            tf(
                "{name} is not installed. {install}",
                &[("name", &name), ("install", &install)],
            )
        }
        Some(_) => tf(
            "{name} is not running. Pick it again to restart it.",
            &[("name", &name)],
        ),
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
