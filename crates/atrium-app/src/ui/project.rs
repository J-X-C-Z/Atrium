//! The project room: an overview of the workstreams side by side, what needs
//! the user, and the atrium where the workstreams align.

use std::path::PathBuf;

use ely_gpui_component::{
    buttons::{Button, ButtonVariant, SegmentedControl},
    data_display::{Badge, Tone},
    forms::{Input, TextInput},
    menus::{DropdownMenu, Menu, MenuItem},
    motion::{ProgressBar, Spinner, SpinnerStyle},
    overlays::Dialog,
    primitives::{Icon, IconName},
    shell::drag_region,
    theme::{ActiveTheme, ControlSize, IconSize, Radius, TextSize},
};
use gpui::{
    AnyElement, App, AppContext, Context, Entity, Hsla, InteractiveElement, IntoElement,
    ParentElement, PathPromptOptions, PromptButton, PromptLevel, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};

use super::{Overlay, RootView, TITLE_BAR_HEIGHT, composer::short_path};
use crate::i18n::{t, tf};
use crate::{
    agents,
    project::{self, ATRIUM, Role, Task, TaskStatus, YOU},
    workspace::{AgentStatus, CODEX, StreamState, StreamView, Workspace},
};

/// Which part of the atrium the side panel shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AtriumTab {
    #[default]
    Notices,
    Plan,
    Brief,
    Contracts,
    Status,
}

impl AtriumTab {
    fn value(self) -> &'static str {
        match self {
            AtriumTab::Notices => "notices",
            AtriumTab::Plan => "plan",
            AtriumTab::Brief => "brief",
            AtriumTab::Contracts => "contracts",
            AtriumTab::Status => "status",
        }
    }

    fn from_value(value: &str) -> Self {
        match value {
            "plan" => AtriumTab::Plan,
            "brief" => AtriumTab::Brief,
            "contracts" => AtriumTab::Contracts,
            "status" => AtriumTab::Status,
            _ => AtriumTab::Notices,
        }
    }
}

fn state_color(state: &StreamState, cx: &App) -> Hsla {
    let colors = &cx.theme().colors;
    match state {
        StreamState::Working => colors.info,
        StreamState::NeedsYou(_) => colors.warning,
        StreamState::Failed(_) => colors.danger,
        StreamState::Idle => colors.success,
        StreamState::Starting | StreamState::Offline => colors.fg_disabled,
    }
}

fn state_label(state: &StreamState) -> String {
    match state {
        StreamState::Starting => t("Starting…").into(),
        StreamState::Offline => t("Agent not running").into(),
        StreamState::Working => t("Working").into(),
        StreamState::NeedsYou(1) => t("Needs you").into(),
        StreamState::NeedsYou(n) => tf("Needs you ({n})", &[("n", n)]),
        StreamState::Failed(_) => t("Hit an error").into(),
        StreamState::Idle => t("Idle").into(),
    }
}

/// A small status dot, pulsing-free: the spinner shows work.
pub fn state_dot(state: &StreamState, cx: &App) -> AnyElement {
    if *state == StreamState::Working {
        return Spinner::new(SharedString::from("dot-working"))
            .style(SpinnerStyle::Ring)
            .size(IconSize::Xs)
            .color(cx.theme().colors.info)
            .into_any_element();
    }
    div()
        .size(px(8.))
        .flex_none()
        .rounded_full()
        .bg(state_color(state, cx))
        .into_any_element()
}

fn agent_short(ws: &Workspace, backend: &str) -> String {
    match ws.agent(backend) {
        Some(agent) => agent.profile.short.to_string(),
        None if backend == CODEX => "Codex".into(),
        None => agents::backend_name(backend).into(),
    }
}

/// Asks for a folder and opens it as a project.
pub fn choose_project(workspace: Entity<Workspace>, cx: &mut App) {
    let picked = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some(t("Open as a project").into()),
    });
    cx.spawn(async move |cx| {
        if let Ok(Ok(Some(paths))) = picked.await
            && let Some(dir) = paths.into_iter().next()
        {
            workspace.update(cx, |ws, cx| ws.add_project(dir, cx));
        }
    })
    .detach();
}

/// The sidebar in the project room: projects, and the open one's workstreams.
pub fn sidebar_list(workspace: &Entity<Workspace>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let ws = workspace.read(cx);
    let active = ws.project_root();
    let open_stream = match &ws.room {
        crate::workspace::Room::Project { stream, .. } => stream.clone(),
        _ => None,
    };
    let views = ws.stream_views();
    let small = theme.text_size(TextSize::Sm);
    let mut list = div().flex().flex_col().gap_0p5();
    if ws.projects.is_empty() {
        list = list.child(
            div()
                .px_2()
                .py_6()
                .flex()
                .flex_col()
                .gap_1()
                .text_size(small)
                .text_color(colors.fg_muted)
                .child(t("No projects yet."))
                .child(
                    div()
                        .text_size(theme.text_size(TextSize::Xs))
                        .text_color(colors.fg_subtle)
                        .child(t("A project is a folder that several long-running sessions work on, each toward its own goal.")),
                ),
        );
    }
    for (ix, project) in ws.projects.iter().enumerate() {
        let is_active = active.as_ref() == Some(&project.root);
        let root = project.root.clone();
        let enter = workspace.clone();
        let needs = if is_active {
            views
                .iter()
                .filter(|v| matches!(v.state, StreamState::NeedsYou(_)))
                .count()
        } else {
            0
        };
        list = list.child(
            div()
                .id(("project", ix))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1p5()
                .rounded_md()
                .cursor_pointer()
                .text_size(small)
                .when(is_active && open_stream.is_none(), |row| {
                    row.bg(colors.active)
                })
                .hover(|row| row.bg(colors.hover))
                .on_click(move |_, _, cx| {
                    let root = root.clone();
                    enter.update(cx, |ws, cx| ws.enter_project(root, cx))
                })
                .child(
                    Icon::new(if is_active {
                        IconName::FolderOpen
                    } else {
                        IconName::Folder
                    })
                    .size(IconSize::Sm)
                    .color(colors.fg_muted),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(project.file.name.clone()),
                )
                .when(needs > 0, |row| {
                    row.child(Badge::new(needs.to_string()).tone(Tone::Warning))
                }),
        );
        if is_active {
            for (depth, view) in tree(&views) {
                let id = view.stream.id.clone();
                let open = workspace.clone();
                let selected = open_stream.as_deref() == Some(id.as_str());
                list = list.child(
                    div()
                        .id(SharedString::from(format!("stream-{id}")))
                        .ml(px(16. + 14. * depth as f32))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .text_size(small)
                        .when(selected, |row| row.bg(colors.active))
                        .hover(|row| row.bg(colors.hover))
                        .on_click(move |_, _, cx| {
                            let id = id.clone();
                            open.update(cx, |ws, cx| ws.show_stream(&id, cx))
                        })
                        .child(state_dot(&view.state, cx))
                        .when(view.stream.role == Role::Controller, |row| {
                            row.child(
                                Icon::new(IconName::Network)
                                    .size(IconSize::Xs)
                                    .color(colors.fg_muted),
                            )
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .when(view.stream.role == Role::Worker, |name| {
                                    name.text_color(colors.fg_muted)
                                })
                                .child(if view.stream.role == Role::Controller {
                                    t("Controller").to_string()
                                } else {
                                    view.stream.name.clone()
                                }),
                        )
                        .when_some(
                            view.task
                                .as_ref()
                                .filter(|_| view.stream.role == Role::Lead)
                                .and_then(Task::progress),
                            |row, (done, total)| {
                                row.child(
                                    div()
                                        .text_size(theme.text_size(TextSize::Xs))
                                        .text_color(colors.fg_subtle)
                                        .child(format!("{done}/{total}")),
                                )
                            },
                        )
                        .when(view.unread > 0, |row| {
                            row.child(
                                div()
                                    .text_size(theme.text_size(TextSize::Xs))
                                    .text_color(colors.accent)
                                    .child(tf("{n} new", &[("n", &view.unread)])),
                            )
                        }),
                );
            }
        }
    }
    list.into_any_element()
}

/// The overview: workstream cards beside the atrium.
/// What the atrium panel shows, held by the root view.
pub struct AtriumPanel {
    pub tab: AtriumTab,
    /// Cards instead of the flow of phases.
    pub cards: bool,
    pub notice_input: Entity<TextInput>,
    pub notice_to: String,
}

pub fn render_overview(
    workspace: &Entity<Workspace>,
    root: Entity<RootView>,
    panel: AtriumPanel,
    sidebar_collapsed: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let bar = drag_region("project-title", window, cx);
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let ws = workspace.read(cx);
    let Some(project) = ws.active_project() else {
        return div().flex_1().into_any_element();
    };
    let views = ws.stream_views();
    let needing: Vec<&StreamView> = views
        .iter()
        .filter(|v| matches!(v.state, StreamState::NeedsYou(_)))
        .collect();
    let working = views
        .iter()
        .filter(|v| v.state == StreamState::Working)
        .count();

    let left_room = if sidebar_collapsed && cfg!(target_os = "macos") {
        px(28.)
    } else {
        px(16.)
    };
    let add = root.clone();
    let header = bar
        .flex_none()
        .h(px(TITLE_BAR_HEIGHT))
        .pl(left_room)
        .pr_2()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .flex_1()
                .flex()
                .items_baseline()
                .gap_2()
                .min_w_0()
                .child(
                    div()
                        .flex_none()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(project.file.name.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(theme.text_size(TextSize::Xs))
                        .text_color(colors.fg_subtle)
                        .child(short_path(&project.root)),
                ),
        )
        .child(
            div()
                .flex_none()
                .text_size(theme.text_size(TextSize::Xs))
                .text_color(colors.fg_muted)
                .child(format!(
                    "{}{}",
                    tf(
                        "{n} workstreams · {working} working",
                        &[("n", &views.len()), ("working", &working)],
                    ),
                    if needing.is_empty() {
                        String::new()
                    } else {
                        tf(" · {n} need you", &[("n", &needing.len())])
                    }
                )),
        )
        .child(
            Button::new("new-stream", t("New lead"))
                .icon(IconName::Plus)
                .size(ControlSize::Sm)
                .on_click(move |_, window, cx| {
                    add.update(cx, |root, cx| {
                        root.open_new_stream_as(Role::Lead, window, cx)
                    })
                }),
        )
        .child({
            let remove = (workspace.clone(), project.root.clone());
            let open_dir = project.dir();
            let approve_leads = project.file.approve_leads;
            let toggle = workspace.clone();
            let menu = Menu::new()
                .item(
                    MenuItem::new(t("Approve the controller's leads before they start"))
                        .icon(if approve_leads {
                            IconName::SquareCheck
                        } else {
                            IconName::Square
                        })
                        .on_click(move |_, cx| {
                            toggle.update(cx, |ws, cx| ws.set_approve_leads(!approve_leads, cx))
                        }),
                )
                .separator()
                .item(
                    MenuItem::new(t("Open the atrium folder"))
                        .icon(IconName::FolderOpen)
                        .on_click(move |_, cx| cx.open_with_system(&open_dir)),
                )
                .separator()
                .item(
                    MenuItem::new(t("Remove from Atrium"))
                        .icon(IconName::X)
                        .on_click(move |_, cx| {
                            let (ws, root) = remove.clone();
                            ws.update(cx, |ws, cx| ws.remove_project(root, cx));
                        }),
                );
            ely_gpui_component::menus::OverflowMenu::new("project-menu", menu)
        });

    // Who is waiting on the user, across workstreams.
    let attention = (!needing.is_empty()).then(|| {
        let mut strip = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .rounded(theme.radius(Radius::Lg))
            .bg(colors.warning_subtle)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(theme.text_size(TextSize::Sm))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(colors.warning)
                    .child(
                        Icon::new(IconName::Bell)
                            .size(IconSize::Sm)
                            .color(colors.warning),
                    )
                    .child(t("Needs you")),
            );
        for view in &needing {
            let id = view.stream.id.clone();
            let open = workspace.clone();
            let n = match view.state {
                StreamState::NeedsYou(n) => n,
                _ => 0,
            };
            strip = strip.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(theme.text_size(TextSize::Sm))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(view.stream.name.clone()),
                    )
                    .child(div().flex_1().text_color(colors.fg_muted).child(if n == 1 {
                        t("is waiting for an approval or an answer").to_string()
                    } else {
                        tf("is waiting on {n} approvals or answers", &[("n", &n)])
                    }))
                    .child(
                        Button::new(SharedString::from(format!("attend-{id}")), "Open")
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| {
                                let id = id.clone();
                                open.update(cx, |ws, cx| ws.show_stream(&id, cx))
                            }),
                    ),
            );
        }
        strip
    });

    let controller = views
        .iter()
        .find(|v| v.stream.role == Role::Controller)
        .map(|view| controller_card(workspace, view, &views, cx));
    let proposals = proposals_strip(workspace, cx);
    let cards: Vec<AnyElement> = views
        .iter()
        .filter(|v| v.stream.role == Role::Lead)
        .map(|view| stream_card(workspace, view, &views, cx))
        .collect();
    let empty = cards.is_empty() && controller.is_none();
    let mut grid = div().flex().flex_col().gap_3();
    let mut cards = cards.into_iter();
    while let Some(first) = cards.next() {
        let second = cards.next();
        grid = grid.child(
            div()
                .flex()
                .gap_3()
                .child(div().flex_1().min_w_0().child(first))
                .child(div().flex_1().min_w_0().children(second)),
        );
    }

    let cards_view = panel.cards;
    let flow = (!cards_view && !empty).then(|| flow_view(workspace, &views, cx));
    let switch = root.clone();
    let view_switch = (!empty).then(|| {
        div().flex().justify_end().child(
            SegmentedControl::new("overview-mode", if cards_view { "cards" } else { "flow" })
                .size(ControlSize::Sm)
                .segment("flow", t("Progress"), None)
                .segment("cards", t("Cards"), None)
                .on_change(move |value, _, cx| {
                    let cards = value.as_ref() == "cards";
                    switch.update(cx, |root, cx| {
                        root.overview_cards = cards;
                        cx.notify();
                    })
                }),
        )
    });
    let add = root.clone();
    let body = div()
        .id("project-body")
        .flex_1()
        .min_w_0()
        .h_full()
        .overflow_y_scroll()
        .p_4()
        .flex()
        .flex_col()
        .gap_4()
        .children(view_switch)
        .children(attention)
        .children(controller)
        .children(proposals)
        .children(flow)
        .when(empty, |body| {
            body.child(
                div()
                    .mt_12()
                    .mx_auto()
                    .max_w(px(460.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .text_center()
                    .child(Icon::new(IconName::Layers).size(IconSize::Lg).color(colors.fg_muted))
                    .child(
                        div()
                            .text_size(theme.text_size(TextSize::Lg))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(t("Start with a controller")),
                    )
                    .child(
                        div()
                            .text_size(theme.text_size(TextSize::Sm))
                            .text_color(colors.fg_muted)
                            .child(t("Give the controller the project's goal. It splits the work into leads, each owning a part with its own roadmap, and leads hand self-contained pieces to sub-agents. Or add leads yourself.")),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("first-controller", t("New controller"))
                                    .primary()
                                    .icon(IconName::Network)
                                    .on_click(move |_, window, cx| {
                                        add.update(cx, |root, cx| {
                                            root.open_new_stream_as(Role::Controller, window, cx)
                                        })
                                    }),
                            )
                            .child({
                                let add = root.clone();
                                Button::new("first-lead", t("New lead"))
                                    .icon(IconName::Plus)
                                    .on_click(move |_, window, cx| {
                                        add.update(cx, |root, cx| {
                                            root.open_new_stream_as(Role::Lead, window, cx)
                                        })
                                    })
                            }),
                    ),
            )
        })
        .when(cards_view, |body| body.child(grid));

    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(colors.bg)
        .child(header)
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .border_t_1()
                .border_color(colors.border)
                .child(body)
                .child(atrium_panel(workspace, root, panel, cx)),
        )
        .into_any_element()
}

fn stream_card(
    workspace: &Entity<Workspace>,
    view: &StreamView,
    views: &[StreamView],
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let ws = workspace.read(cx);
    let id = view.stream.id.clone();
    let small = theme.text_size(TextSize::Sm);
    let tiny = theme.text_size(TextSize::Xs);
    let open = workspace.clone();

    let roadmap = view.task.as_ref().and_then(|task| {
        let (done, total) = task.progress()?;
        let next = task
            .roadmap
            .iter()
            .find(|m| !m.done)
            .map(|m| m.text.clone());
        Some(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .gap_2()
                        .text_size(tiny)
                        .text_color(colors.fg_muted)
                        .child(div().truncate().child(match next {
                            Some(next) => tf("Roadmap · next: {step}", &[("step", &next)]),
                            None => t("Roadmap done").to_string(),
                        }))
                        .child(div().flex_none().child(format!("{done}/{total}"))),
                )
                .child(ProgressBar::new(
                    SharedString::from(format!("roadmap-{id}")),
                    done as f32 / total.max(1) as f32,
                ))
                .into_any_element(),
        )
    });
    let progress = match (view.plan, &view.step, &view.focus) {
        _ if roadmap.is_some() => roadmap,
        (Some((done, total)), step, _) => Some(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .gap_2()
                        .text_size(tiny)
                        .text_color(colors.fg_muted)
                        .child(
                            div()
                                .truncate()
                                .child(step.clone().unwrap_or_else(|| t("Plan done").into())),
                        )
                        .child(div().flex_none().child(format!("{done}/{total}"))),
                )
                .child(ProgressBar::new(
                    SharedString::from(format!("plan-{id}")),
                    done as f32 / total.max(1) as f32,
                ))
                .into_any_element(),
        ),
        (None, _, Some(focus)) => Some(
            div()
                .flex()
                .gap_1()
                .text_size(tiny)
                .text_color(colors.fg_muted)
                .child(div().flex_none().child(t("Focus:")))
                .child(div().truncate().child(focus.clone()))
                .into_any_element(),
        ),
        _ => None,
    };

    let mut facts: Vec<AnyElement> = Vec::new();
    let fact = |icon: IconName, text: String, color: Hsla| {
        div()
            .flex()
            .items_center()
            .gap_1()
            .text_size(tiny)
            .text_color(color)
            .child(Icon::new(icon).size(IconSize::Xs).color(color))
            .child(text)
            .into_any_element()
    };
    if view.unread > 0 {
        facts.push(fact(
            IconName::Mail,
            if view.unread == 1 {
                t("1 new notice").to_string()
            } else {
                tf("{n} new notices", &[("n", &view.unread)])
            },
            colors.accent,
        ));
    }
    if view.crossings > 0 {
        facts.push(fact(
            IconName::TriangleAlert,
            if view.crossings == 1 {
                t("1 change outside its scope").to_string()
            } else {
                tf("{n} changes outside its scope", &[("n", &view.crossings)])
            },
            colors.warning,
        ));
    }
    if let Some(speed) = &view.speed {
        facts.push(fact(IconName::Zap, speed.clone(), colors.fg_subtle));
    }
    if let Some(share) = view.context {
        facts.push(fact(
            IconName::Gauge,
            tf(
                "{share}% context",
                &[("share", &format!("{:.0}", share * 100.))],
            ),
            if share >= 0.8 {
                colors.warning
            } else {
                colors.fg_subtle
            },
        ));
    }

    let workers = sub_agents(workspace, view, views, cx);
    let (deliver, carry_on) = (workspace.clone(), workspace.clone());
    let idle = matches!(view.state, StreamState::Idle | StreamState::Failed(_));
    let unread = view.unread > 0;
    let (deliver_id, continue_id) = (id.clone(), id.clone());
    div()
        .id(SharedString::from(format!("card-{id}")))
        .w_full()
        .overflow_hidden()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded(theme.radius(Radius::Lg))
        .border_1()
        .border_color(match view.state {
            StreamState::NeedsYou(_) => colors.warning,
            _ => colors.border,
        })
        .bg(colors.surface)
        .cursor_pointer()
        .hover(|card| card.border_color(colors.border_strong))
        .on_click(move |_, _, cx| {
            let id = id.clone();
            open.update(cx, |ws, cx| ws.show_stream(&id, cx))
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(state_dot(&view.state, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(view.stream.name.clone()),
                )
                .child(Badge::new(agent_short(ws, &view.stream.backend)))
                .when(
                    view.goal_status == Some(atrium_core::GoalStatus::Complete),
                    |row| row.child(Badge::new(t("Goal done")).tone(Tone::Success)),
                )
                .child(
                    div()
                        .text_size(tiny)
                        .text_color(state_color(&view.state, cx))
                        .child(state_label(&view.state)),
                ),
        )
        .child(
            div()
                .w_full()
                .min_w_0()
                .text_size(small)
                .text_color(colors.fg_muted)
                .child(if view.stream.goal.is_empty() {
                    t("No goal set").to_string()
                } else {
                    clip(&view.stream.goal, 160)
                }),
        )
        .child(
            div()
                .text_size(tiny)
                .text_color(colors.fg_subtle)
                .truncate()
                .child(tf(
                    "Scope: {scope}",
                    &[("scope", &scope_text(&view.stream.scope))],
                )),
        )
        .children(progress)
        .children(view.said.clone().map(|said| {
            div()
                .text_size(tiny)
                .text_color(colors.fg_subtle)
                .truncate()
                .child(format!("“{said}”"))
        }))
        .when_some(
            match &view.state {
                StreamState::Failed(error) => Some(error.clone()),
                _ => None,
            },
            |card, error| {
                card.child(
                    div()
                        .text_size(tiny)
                        .text_color(colors.danger)
                        .child(clip(&error, 200)),
                )
            },
        )
        .when(!facts.is_empty(), |card| {
            card.child(
                div()
                    .flex()
                    .items_center()
                    .gap_x_3()
                    .gap_y_1()
                    .flex_wrap()
                    .children(facts),
            )
        })
        .children(workers)
        .child(
            div()
                .flex()
                .items_center()
                .justify_end()
                .gap_2()
                .when(unread, |row| {
                    row.child(
                        Button::new(
                            SharedString::from(format!("deliver-{deliver_id}")),
                            t("Hand over notices"),
                        )
                        .size(ControlSize::Sm)
                        .variant(ButtonVariant::Ghost)
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            let id = deliver_id.clone();
                            deliver.update(cx, |ws, cx| ws.deliver_notices(&id, cx))
                        }),
                    )
                })
                .when(idle && !unread, |row| {
                    row.child(
                        Button::new(
                            SharedString::from(format!("continue-{continue_id}")),
                            t("Carry on"),
                        )
                        .size(ControlSize::Sm)
                        .variant(ButtonVariant::Ghost)
                        .icon(IconName::Play)
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            let id = continue_id.clone();
                            carry_on.update(cx, |ws, cx| ws.continue_stream(&id, cx))
                        }),
                    )
                }),
        )
        .into_any_element()
}

/// Streams in tree order with their depth: the controller, then each lead
/// followed by its workers, then anything whose parent is gone.
fn tree(views: &[StreamView]) -> Vec<(usize, &StreamView)> {
    let mut out: Vec<(usize, &StreamView)> = Vec::new();
    let mut placed = std::collections::HashSet::new();
    fn place<'a>(
        view: &'a StreamView,
        depth: usize,
        views: &'a [StreamView],
        out: &mut Vec<(usize, &'a StreamView)>,
        placed: &mut std::collections::HashSet<String>,
    ) {
        if !placed.insert(view.stream.id.clone()) {
            return;
        }
        out.push((depth, view));
        for child in views
            .iter()
            .filter(|v| v.stream.parent.as_deref() == Some(view.stream.id.as_str()))
        {
            place(child, depth + 1, views, out, placed);
        }
    }
    let roots = views.iter().filter(|v| {
        v.stream
            .parent
            .as_deref()
            .is_none_or(|p| !views.iter().any(|o| o.stream.id == p))
    });
    let mut roots: Vec<&StreamView> = roots.collect();
    roots.sort_by_key(|v| v.stream.role != Role::Controller);
    for view in roots {
        // Leads hang under the controller only in the sidebar; the
        // controller's children come right after it.
        place(view, 0, views, &mut out, &mut placed);
    }
    out
}

/// The controller across the top: what it is doing and how far the leads are.
fn controller_card(
    workspace: &Entity<Workspace>,
    view: &StreamView,
    views: &[StreamView],
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let tiny = theme.text_size(TextSize::Xs);
    let small = theme.text_size(TextSize::Sm);
    let ws = workspace.read(cx);
    let id = view.stream.id.clone();
    let open = workspace.clone();
    let leads: Vec<&StreamView> = views
        .iter()
        .filter(|v| v.stream.role == Role::Lead)
        .collect();
    let (done, total) = leads
        .iter()
        .filter_map(|v| v.task.as_ref().and_then(Task::progress))
        .fold((0, 0), |(d, t), (dd, tt)| (d + dd, t + tt));
    let finished = leads
        .iter()
        .filter(|v| {
            v.task
                .as_ref()
                .is_some_and(|t| t.status == TaskStatus::Done)
        })
        .count();
    let queued = ws.queued_tasks(&id).len();
    let carry_on = workspace.clone();
    let continue_id = id.clone();
    let idle = matches!(view.state, StreamState::Idle | StreamState::Failed(_));
    div()
        .id("controller-card")
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded(theme.radius(Radius::Lg))
        .border_1()
        .border_color(match view.state {
            StreamState::NeedsYou(_) => colors.warning,
            _ => colors.border_strong,
        })
        .bg(colors.surface)
        .cursor_pointer()
        .hover(|card| card.bg(colors.hover))
        .on_click(move |_, _, cx| {
            let id = id.clone();
            open.update(cx, |ws, cx| ws.show_stream(&id, cx))
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(state_dot(&view.state, cx))
                .child(
                    Icon::new(IconName::Network)
                        .size(IconSize::Sm)
                        .color(colors.fg_muted),
                )
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(t("Controller")),
                )
                .child(Badge::new(agent_short(ws, &view.stream.backend)))
                .child(div().flex_1())
                .child(
                    div()
                        .text_size(tiny)
                        .text_color(state_color(&view.state, cx))
                        .child(state_label(&view.state)),
                ),
        )
        .child(
            div()
                .text_size(small)
                .text_color(colors.fg_muted)
                .child(clip(&view.stream.goal, 220)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .text_size(tiny)
                .text_color(colors.fg_subtle)
                .child(tf(
                    "{n} leads · {finished} done",
                    &[("n", &leads.len()), ("finished", &finished)],
                ))
                .when(total > 0, |row| {
                    row.child(tf(
                        "{done}/{total} milestones",
                        &[("done", &done), ("total", &total)],
                    ))
                })
                .when(queued > 0, |row| {
                    row.child(tf("{n} leads about to start", &[("n", &queued)]))
                })
                .children(view.said.clone().map(|said| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(format!("“{said}”"))
                })),
        )
        .when(total > 0, |card| {
            card.child(ProgressBar::new(
                "controller-progress",
                done as f32 / total.max(1) as f32,
            ))
        })
        .when(idle, |card| {
            card.child(
                div().flex().justify_end().child(
                    Button::new("controller-continue", t("Review progress"))
                        .size(ControlSize::Sm)
                        .variant(ButtonVariant::Ghost)
                        .icon(IconName::Play)
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            let id = continue_id.clone();
                            carry_on.update(cx, |ws, cx| ws.continue_stream(&id, cx))
                        }),
                ),
            )
        })
        .into_any_element()
}

/// Every lead as a lane of its roadmap's phases, left to right: done phases
/// fade, the current one stands out with its steps and sub-agents.
fn flow_view(workspace: &Entity<Workspace>, views: &[StreamView], cx: &App) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let tiny = theme.text_size(TextSize::Xs);
    let small = theme.text_size(TextSize::Sm);
    let ws = workspace.read(cx);
    let mut lanes = div().flex().flex_col().gap_3();
    for lead in views.iter().filter(|v| v.stream.role == Role::Lead) {
        let id = lead.stream.id.clone();
        let open = workspace.clone();
        let phases = lead
            .task
            .as_ref()
            .map(|t| t.phases.clone())
            .unwrap_or_default();
        let current = lead.task.as_ref().and_then(Task::current_phase);
        let finished = lead
            .task
            .as_ref()
            .is_some_and(|t| t.status == TaskStatus::Done)
            || (!phases.is_empty() && current.is_none());
        let workers: Vec<&StreamView> = views
            .iter()
            .filter(|v| v.stream.parent.as_deref() == Some(id.as_str()))
            .filter(|v| !v.task.as_ref().is_some_and(|t| t.status.finished()))
            .collect();
        let queued = ws.queued_tasks(&id).len();

        let summary = match (current, phases.len()) {
            (_, 0) => t("No roadmap yet").to_string(),
            (Some(ix), total) => tf(
                "Phase {n} of {total}: {title}",
                &[
                    ("n", &(ix + 1)),
                    ("total", &total),
                    ("title", &phases[ix].title),
                ],
            ),
            (None, _) => t("All phases done").to_string(),
        };
        let header = div()
            .id(SharedString::from(format!("lane-{id}")))
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .on_click(move |_, _, cx| {
                let id = id.clone();
                open.update(cx, |ws, cx| ws.show_stream(&id, cx))
            })
            .child(state_dot(&lead.state, cx))
            .child(
                div()
                    .text_size(small)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(lead.stream.name.clone()),
            )
            .child(Badge::new(agent_short(ws, &lead.stream.backend)))
            .child(
                div()
                    .text_size(tiny)
                    .text_color(state_color(&lead.state, cx))
                    .child(state_label(&lead.state)),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_size(tiny)
                    .text_color(if finished {
                        colors.success
                    } else {
                        colors.fg_muted
                    })
                    .truncate()
                    .child(summary),
            );

        let mut row = div().flex().items_stretch().gap_1();
        for (ix, phase) in phases.iter().enumerate() {
            let (done, total) = phase.progress();
            let state = match current {
                Some(c) if ix < c => PhaseState::Done,
                Some(c) if ix == c => PhaseState::Current,
                None => PhaseState::Done,
                _ => PhaseState::Ahead,
            };
            if ix > 0 {
                row = row.child(div().flex().items_center().child(
                    Icon::new(IconName::ChevronRight).size(IconSize::Xs).color(
                        if state == PhaseState::Ahead {
                            colors.fg_subtle
                        } else {
                            colors.fg_muted
                        },
                    ),
                ));
            }
            let next_step = phase.steps.iter().find(|m| !m.done).map(|m| m.text.clone());
            let mut block = div()
                .flex_1()
                .min_w(px(104.))
                .max_w(px(220.))
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded(theme.radius(Radius::Md))
                .border_1()
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap_1()
                        .child(match state {
                            PhaseState::Done => Icon::new(IconName::CircleCheck)
                                .size(IconSize::Xs)
                                .color(colors.success)
                                .into_any_element(),
                            PhaseState::Current => Icon::new(IconName::CircleDot)
                                .size(IconSize::Xs)
                                .color(colors.accent)
                                .into_any_element(),
                            PhaseState::Ahead => Icon::new(IconName::Circle)
                                .size(IconSize::Xs)
                                .color(colors.fg_subtle)
                                .into_any_element(),
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(small)
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(phase.title.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(tiny)
                                .text_color(colors.fg_subtle)
                                .child(format!("{done}/{total}")),
                        ),
                );
            match state {
                PhaseState::Done => {
                    block = block
                        .bg(colors.sunken)
                        .border_color(colors.border)
                        .opacity(0.45);
                }
                PhaseState::Ahead => {
                    block = block
                        .bg(colors.surface)
                        .border_color(colors.border)
                        .text_color(colors.fg_muted);
                }
                PhaseState::Current => {
                    block = block
                        .bg(colors.surface)
                        .border_color(colors.accent)
                        .child(ProgressBar::new(
                            SharedString::from(format!("phase-{}-{ix}", lead.stream.id)),
                            done as f32 / total.max(1) as f32,
                        ))
                        .children(next_step.map(|step| {
                            div()
                                .text_size(tiny)
                                .text_color(colors.fg_muted)
                                .child(tf("Next: {step}", &[("step", &step)]))
                        }));
                    for worker in &workers {
                        let wid = worker.stream.id.clone();
                        let go = workspace.clone();
                        block = block.child(
                            div()
                                .id(SharedString::from(format!("flow-worker-{wid}")))
                                .flex()
                                .items_center()
                                .gap_1()
                                .px_1()
                                .rounded_sm()
                                .text_size(tiny)
                                .text_color(colors.fg_muted)
                                .cursor_pointer()
                                .hover(|row| row.bg(colors.hover))
                                .on_click(move |_, _, cx| {
                                    let id = wid.clone();
                                    go.update(cx, |ws, cx| ws.show_stream(&id, cx))
                                })
                                .child(state_dot(&worker.state, cx))
                                .child(div().truncate().child(worker.stream.name.clone())),
                        );
                    }
                    if queued > 0 {
                        block = block.child(
                            div()
                                .text_size(tiny)
                                .text_color(colors.fg_subtle)
                                .child(tf("{n} queued", &[("n", &queued)])),
                        );
                    }
                }
            }
            row = row.child(block);
        }
        if phases.is_empty() {
            row = row.child(
                div()
                    .text_size(tiny)
                    .text_color(colors.fg_subtle)
                    .child(t("Its roadmap appears here once the controller writes it, or add a Roadmap section to its task file.")),
            );
        }
        lanes = lanes.child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .rounded(theme.radius(Radius::Lg))
                .border_1()
                .border_color(match lead.state {
                    StreamState::NeedsYou(_) => colors.warning,
                    _ => colors.border,
                })
                .bg(colors.surface)
                .child(header)
                .child(
                    div()
                        .id(SharedString::from(format!("phases-{}", lead.stream.id)))
                        .overflow_x_scroll()
                        .pb_1()
                        .child(row),
                ),
        );
    }
    lanes.into_any_element()
}

#[derive(Clone, Copy, PartialEq)]
enum PhaseState {
    Done,
    Current,
    Ahead,
}

/// Leads the controller proposed, waiting for the user's go-ahead.
fn proposals_strip(workspace: &Entity<Workspace>, cx: &App) -> Option<AnyElement> {
    let ws = workspace.read(cx);
    let proposed = ws.proposed_leads();
    if proposed.is_empty() {
        return None;
    }
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let tiny = theme.text_size(TextSize::Xs);
    let small = theme.text_size(TextSize::Sm);
    let approve_all = (
        workspace.clone(),
        proposed.iter().map(Task::key).collect::<Vec<_>>(),
    );
    let mut strip = div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded(theme.radius(Radius::Lg))
        .border_1()
        .border_color(colors.accent)
        .bg(colors.surface)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    Icon::new(IconName::ListChecks)
                        .size(IconSize::Sm)
                        .color(colors.accent),
                )
                .child(
                    div()
                        .flex_1()
                        .text_size(small)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(tf(
                            "The controller proposes {n} leads",
                            &[("n", &proposed.len())],
                        )),
                )
                .child(
                    Button::new("approve-all", t("Start all"))
                        .size(ControlSize::Sm)
                        .primary()
                        .on_click(move |_, _, cx| {
                            let (ws, keys) = approve_all.clone();
                            ws.update(cx, |ws, cx| {
                                for key in &keys {
                                    ws.approve_task(key, cx);
                                }
                            })
                        }),
                ),
        );
    for task in &proposed {
        let key = task.key();
        let (approve, reject) = (workspace.clone(), workspace.clone());
        let (approve_key, reject_key) = (key.clone(), key.clone());
        let agent = task
            .agent
            .clone()
            .map(|a| crate::agents::backend_label(&a))
            .unwrap_or_else(|| t("same agent").into());
        let steps: Vec<String> = task.roadmap.iter().map(|m| m.text.clone()).collect();
        strip = strip.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded(theme.radius(Radius::Md))
                .bg(colors.sunken)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(small)
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(task.title.clone()),
                        )
                        .child(Badge::new(agent))
                        .child(
                            Button::new(
                                SharedString::from(format!("reject-{key}")),
                                t("Turn down"),
                            )
                            .size(ControlSize::Sm)
                            .variant(ButtonVariant::Ghost)
                            .on_click(move |_, _, cx| {
                                let key = reject_key.clone();
                                reject.update(cx, |ws, cx| ws.reject_task(&key, cx))
                            }),
                        )
                        .child(
                            Button::new(SharedString::from(format!("approve-{key}")), t("Start"))
                                .size(ControlSize::Sm)
                                .on_click(move |_, _, cx| {
                                    let key = approve_key.clone();
                                    approve.update(cx, |ws, cx| ws.approve_task(&key, cx))
                                }),
                        ),
                )
                .child(
                    div()
                        .text_size(tiny)
                        .text_color(colors.fg_muted)
                        .child(clip(&task.body, 200)),
                )
                .child(
                    div()
                        .text_size(tiny)
                        .text_color(colors.fg_subtle)
                        .truncate()
                        .child(tf("Scope: {scope}", &[("scope", &scope_text(&task.scope))])),
                )
                .when(!steps.is_empty(), |item| {
                    item.child(
                        div()
                            .flex()
                            .flex_col()
                            .text_size(tiny)
                            .text_color(colors.fg_muted)
                            .children(
                                steps
                                    .iter()
                                    .enumerate()
                                    .map(|(ix, step)| format!("{}. {}", ix + 1, step)),
                            ),
                    )
                }),
        );
    }
    Some(strip.into_any_element())
}

/// A lead's sub-agents and queued tasks, one row each.
fn sub_agents(
    workspace: &Entity<Workspace>,
    lead: &StreamView,
    views: &[StreamView],
    cx: &App,
) -> Option<AnyElement> {
    let ws = workspace.read(cx);
    let workers: Vec<&StreamView> = views
        .iter()
        .filter(|v| v.stream.parent.as_deref() == Some(lead.stream.id.as_str()))
        .filter(|v| v.stream.role == Role::Worker)
        .collect();
    let queued = ws.queued_tasks(&lead.stream.id);
    if workers.is_empty() && queued.is_empty() {
        return None;
    }
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let tiny = theme.text_size(TextSize::Xs);
    let running = workers
        .iter()
        .filter(|w| !w.task.as_ref().is_some_and(|t| t.status.finished()))
        .count();
    let mut list = div()
        .flex()
        .flex_col()
        .gap_0p5()
        .pt_1()
        .border_t_1()
        .border_color(colors.border)
        .child(div().text_size(tiny).text_color(colors.fg_subtle).child(tf(
            "Sub-agents · {running} running · {queued} queued",
            &[("running", &running), ("queued", &queued.len())],
        )));
    for worker in workers {
        let id = worker.stream.id.clone();
        let open = workspace.clone();
        let status = worker.task.as_ref().map(|t| t.status);
        list = list.child(
            div()
                .id(SharedString::from(format!("worker-{id}")))
                .flex()
                .items_center()
                .gap_2()
                .px_1()
                .py_0p5()
                .rounded_md()
                .text_size(tiny)
                .hover(|row| row.bg(colors.hover))
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    let id = id.clone();
                    open.update(cx, |ws, cx| ws.show_stream(&id, cx))
                })
                .child(match status {
                    Some(TaskStatus::Done) => Icon::new(IconName::CircleCheck)
                        .size(IconSize::Xs)
                        .color(colors.success)
                        .into_any_element(),
                    Some(TaskStatus::Failed) => Icon::new(IconName::CircleX)
                        .size(IconSize::Xs)
                        .color(colors.danger)
                        .into_any_element(),
                    _ => state_dot(&worker.state, cx),
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(colors.fg)
                        .child(worker.stream.name.clone()),
                )
                .child(Badge::new(agent_short(ws, &worker.stream.backend)))
                .child(div().text_color(colors.fg_subtle).child(match status {
                    Some(TaskStatus::Done) => t("done").to_string(),
                    Some(TaskStatus::Failed) => t("failed").to_string(),
                    _ => state_label(&worker.state),
                })),
        );
    }
    for task in queued {
        list = list.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_1()
                .py_0p5()
                .text_size(tiny)
                .text_color(colors.fg_subtle)
                .child(
                    Icon::new(IconName::Clock)
                        .size(IconSize::Xs)
                        .color(colors.fg_subtle),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(task.title.clone()),
                )
                .child(t("queued")),
        );
    }
    Some(list.into_any_element())
}

/// The right panel: notices, the brief, contracts and statuses.
fn atrium_panel(
    workspace: &Entity<Workspace>,
    root: Entity<RootView>,
    view: AtriumPanel,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let ws = workspace.read(cx);
    let Some(project) = ws.active_project() else {
        return div().into_any_element();
    };
    let tab = view.tab;
    let small = theme.text_size(TextSize::Sm);
    let tiny = theme.text_size(TextSize::Xs);
    let name_of = |id: &str| -> String {
        match id {
            YOU => t("You").into(),
            ATRIUM => "Atrium".into(),
            "all" => t("everyone").into(),
            id => project
                .stream(id)
                .map(|s| s.name.clone())
                .unwrap_or_else(|| id.to_string()),
        }
    };

    let switch = root.clone();
    let tabs = SegmentedControl::new("atrium-tab", tab.value())
        .segment("notices", t("Notices"), None)
        .segment("plan", t("Plan"), None)
        .segment("brief", t("Brief"), None)
        .segment("contracts", t("Contracts"), None)
        .segment("status", t("Status"), None)
        .size(ControlSize::Sm)
        .on_change(move |value, _, cx| {
            let tab = AtriumTab::from_value(value.as_ref());
            switch.update(cx, |root, cx| {
                root.atrium_tab = tab;
                cx.notify();
            })
        });

    let open_file = |path: PathBuf| {
        move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| cx.open_with_system(&path)
    };
    let dir = project.dir();
    let content: AnyElement = match tab {
        AtriumTab::Notices => {
            let to = view.notice_to.clone();
            let mut targets =
                Menu::new().item(MenuItem::radio(t("Everyone"), to == "all").on_click({
                    let root = root.clone();
                    move |_, cx| {
                        root.update(cx, |r, cx| {
                            r.notice_to = "all".into();
                            cx.notify()
                        })
                    }
                }));
            for stream in &project.file.streams {
                let id = stream.id.clone();
                let root = root.clone();
                targets = targets.item(MenuItem::radio(stream.name.clone(), to == id).on_click(
                    move |_, cx| {
                        let id = id.clone();
                        root.update(cx, |r, cx| {
                            r.notice_to = id;
                            cx.notify()
                        })
                    },
                ));
            }
            let post = (workspace.clone(), view.notice_input.clone(), to.clone());
            let composer = div()
                .flex()
                .flex_col()
                .gap_2()
                .child(Input::new(&view.notice_input))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            DropdownMenu::new(
                                "notice-to",
                                tf("To {who}", &[("who", &name_of(&to))]),
                                targets,
                            )
                            .variant(ButtonVariant::Ghost),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("post-notice", t("Post"))
                                .size(ControlSize::Sm)
                                .on_click(move |_, window, cx| {
                                    let (ws, input, to) = post.clone();
                                    let text = input.read(cx).text().to_string();
                                    if text.trim().is_empty() {
                                        return;
                                    }
                                    ws.update(cx, |ws, cx| ws.post_notice(vec![to], text, cx));
                                    input.update(cx, |input, cx| input.set_text("", cx));
                                    let _ = window;
                                }),
                        ),
                );
            let mut list = div().flex().flex_col().gap_2();
            if project.notices.is_empty() {
                list = list.child(
                    div()
                        .text_size(small)
                        .text_color(colors.fg_subtle)
                        .child(t("No notices yet. Workstreams post here when they change something others depend on.")),
                );
            }
            for notice in project.notices.iter().rev().take(60) {
                let from_atrium = notice.from == ATRIUM;
                list = list.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .p_2()
                        .rounded(theme.radius(Radius::Md))
                        .bg(if from_atrium {
                            colors.warning_subtle
                        } else {
                            colors.sunken
                        })
                        .child(
                            div()
                                .flex()
                                .gap_1()
                                .text_size(tiny)
                                .text_color(colors.fg_muted)
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(name_of(&notice.from)),
                                )
                                .child("→")
                                .child(
                                    notice
                                        .to
                                        .iter()
                                        .map(|t| name_of(t))
                                        .collect::<Vec<_>>()
                                        .join(", "),
                                )
                                .child(div().flex_1())
                                .child(notice.at.clone()),
                        )
                        .child(div().text_size(small).child(notice.text.clone())),
                );
            }
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(composer)
                .child(list)
                .into_any_element()
        }
        AtriumTab::Plan => doc_view(
            t("Plan"),
            t("The controller's plan: the milestones in order and which lead owns each."),
            &project.plan,
            open_file(dir.join("plan.md")),
            cx,
        ),
        AtriumTab::Brief => doc_view(
            t("Brief"),
            t("Shared conventions and decisions. Every workstream reads it first."),
            &project.brief,
            open_file(dir.join("brief.md")),
            cx,
        ),
        AtriumTab::Contracts => {
            let mut column = div().flex().flex_col().gap_3().child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(tiny)
                            .text_color(colors.fg_muted)
                            .child(t("Interfaces between workstreams. Whoever changes one updates its file and posts a notice.")),
                    )
                    .child(
                        Button::new("open-contracts", t("Open folder"))
                            .size(ControlSize::Sm)
                            .variant(ButtonVariant::Ghost)
                            .on_click(open_file(dir.join("contracts"))),
                    ),
            );
            for (name, text) in project.contracts.iter().filter(|(n, _)| n != "README.md") {
                column = column.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .p_2()
                        .rounded(theme.radius(Radius::Md))
                        .bg(colors.sunken)
                        .child(
                            div()
                                .text_size(small)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(name.trim_end_matches(".md").to_string()),
                        )
                        .child(
                            div()
                                .text_size(tiny)
                                .text_color(colors.fg_muted)
                                .child(clip(text, 600)),
                        ),
                );
            }
            if project.contracts.iter().all(|(n, _)| n == "README.md") {
                column = column.child(
                    div()
                        .text_size(small)
                        .text_color(colors.fg_subtle)
                        .child(t("No contracts yet.")),
                );
            }
            column.into_any_element()
        }
        AtriumTab::Status => {
            let mut column = div().flex().flex_col().gap_3();
            for stream in &project.file.streams {
                let text = project.statuses.get(&stream.id);
                column = column.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .p_2()
                        .rounded(theme.radius(Radius::Md))
                        .bg(colors.sunken)
                        .child(
                            div()
                                .text_size(small)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(stream.name.clone()),
                        )
                        .child(div().text_size(tiny).text_color(colors.fg_muted).child(
                            text.map(|t| clip(t, 900)).unwrap_or_else(|| {
                                tf(
                                    "No status yet ({path}).",
                                    &[(
                                        "path",
                                        &format!("{}/status/{}.md", project::DIR, stream.id),
                                    )],
                                )
                            }),
                        )),
                );
            }
            if project.file.streams.is_empty() {
                column = column.child(
                    div()
                        .text_size(small)
                        .text_color(colors.fg_subtle)
                        .child(t("Workstreams write their status here.")),
                );
            }
            column.into_any_element()
        }
    };

    div()
        .w(px(360.))
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .border_l_1()
        .border_color(colors.border)
        .bg(colors.sunken.opacity(0.4))
        .child(
            div()
                .flex_none()
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Icon::new(IconName::Columns2)
                                .size(IconSize::Sm)
                                .color(colors.fg_muted),
                        )
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(t("Atrium")),
                        )
                        .child(
                            div()
                                .text_size(tiny)
                                .text_color(colors.fg_subtle)
                                .child(format!("{}/", project::DIR)),
                        ),
                )
                .child(tabs),
        )
        .child(
            div()
                .id("atrium-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px_3()
                .pb_3()
                .child(content),
        )
        .into_any_element()
}

fn doc_view(
    _title: &str,
    hint: &str,
    text: &str,
    open: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let colors = &theme.colors;
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(theme.text_size(TextSize::Xs))
                        .text_color(colors.fg_muted)
                        .child(hint.to_string()),
                )
                .child(
                    Button::new("open-doc", t("Edit"))
                        .size(ControlSize::Sm)
                        .variant(ButtonVariant::Ghost)
                        .icon(IconName::Pencil)
                        .on_click(open),
                ),
        )
        .child(
            div()
                .p_3()
                .rounded(theme.radius(Radius::Md))
                .bg(colors.surface)
                .border_1()
                .border_color(colors.border)
                .text_size(theme.text_size(TextSize::Sm))
                .child(ely_gpui_component::chat::StreamingMarkdown::new(
                    "atrium-doc",
                    text.to_string(),
                    false,
                )),
        )
        .into_any_element()
}

/// Above a workstream's chat: back to the project, its goal and scope, and
/// notices waiting for it.
pub fn stream_bar(workspace: &Entity<Workspace>, cx: &App) -> Option<AnyElement> {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let ws = workspace.read(cx);
    let (project, stream) = ws.open_stream()?;
    let unread = project.unread(&stream.id).len();
    let uncertain = project.notice_delivery_uncertain(&stream.id);
    let blocked = project.notice_delivery_blocked(&stream.id);
    let resolution = (workspace.clone(), project.root.clone(), stream.id.clone());
    let back = workspace.clone();
    let deliver = (workspace.clone(), stream.id.clone());
    let tiny = theme.text_size(TextSize::Xs);
    Some(
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_1p5()
            .border_b_1()
            .border_color(colors.border)
            .bg(colors.sunken.opacity(0.5))
            .text_size(tiny)
            .text_color(colors.fg_muted)
            .child(
                div()
                    .id("back-to-project")
                    .flex()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    .hover(|row| row.text_color(colors.fg))
                    .on_click(move |_, _, cx| back.update(cx, |ws, cx| ws.show_overview(cx)))
                    .child(Icon::new(IconName::ChevronLeft).size(IconSize::Xs))
                    .child(project.file.name.clone()),
            )
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(colors.fg)
                    .child(stream.name.clone()),
            )
            .child(div().flex_1().min_w_0().truncate().child(tf(
                "Goal: {goal}",
                &[("goal", &project::first_line(&stream.goal))],
            )))
            .child(div().flex_none().child(scope_text(&stream.scope)))
            .child({
                let remove = (workspace.clone(), stream.id.clone());
                ely_gpui_component::buttons::IconButton::new("leave-project", IconName::X)
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .tooltip(t("Take out of the project (the chat stays, under Chats)"))
                    .on_click(move |_, _, cx| {
                        let (ws, id) = remove.clone();
                        ws.update(cx, |ws, cx| ws.remove_stream(&id, cx))
                    })
            })
            .when(uncertain, |bar| {
                bar.child(
                    Button::new("resolve-notice-delivery", t("Review notice delivery"))
                        .size(ControlSize::Sm)
                        .icon(IconName::TriangleAlert)
                        .on_click(move |_, window, cx| {
                            let (workspace, root, stream) = resolution.clone();
                            let choice = window.prompt(
                                PromptLevel::Warning,
                                t("Notice delivery is uncertain"),
                                Some(t("Check the conversation first. Retry can send duplicate notices. It will never resend your earlier message.")),
                                &[
                                    PromptButton::cancel(t("Cancel")),
                                    PromptButton::new(t("Mark notices received")),
                                    PromptButton::new(t("Retry notices only")),
                                ],
                                cx,
                            );
                            cx.spawn(async move |cx| {
                                let outcome = match choice.await {
                                    Ok(1) => false,
                                    Ok(2) => true,
                                    _ => return,
                                };
                                workspace.update(cx, |ws, cx| {
                                    let result = ws.resolve_stream_notice_delivery(&root, &stream, outcome, cx);
                                    match result {
                                        Ok(true) => (),
                                        Ok(false) => cx.emit(crate::workspace::Notice::Info(
                                            t("Notice delivery changed. Check the current status and try again.").into(),
                                        )),
                                        Err(error) => cx.emit(crate::workspace::Notice::Error(tf(
                                            "Could not resolve notice delivery: {error}",
                                            &[("error", &format!("{error:#}"))],
                                        ))),
                                    }
                                });
                            }).detach();
                        }),
                )
            })
            .when(unread > 0 && !blocked, |bar| {
                bar.child(
                    Button::new(
                        "bar-deliver",
                        if unread == 1 {
                            t("Hand over 1 new notice").to_string()
                        } else {
                            tf("Hand over {n} new notices", &[("n", &unread)])
                        },
                    )
                    .size(ControlSize::Sm)
                    .icon(IconName::Mail)
                    .on_click(move |_, _, cx| {
                        let (ws, id) = deliver.clone();
                        ws.update(cx, |ws, cx| ws.deliver_notices(&id, cx))
                    }),
                )
            })
            .into_any_element(),
    )
}

impl RootView {
    pub fn open_new_stream(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // With no controller yet, the first thing to add is one.
        let role = match self.workspace.read(cx).active_project() {
            Some(p) if p.controller().is_none() && p.file.streams.is_empty() => Role::Controller,
            _ => Role::Lead,
        };
        self.open_new_stream_as(role, window, cx);
    }

    pub fn open_new_stream_as(&mut self, role: Role, window: &mut Window, cx: &mut Context<Self>) {
        let name = cx.new(|cx| TextInput::new(window, cx).placeholder(t("Name, such as Frontend")));
        let goal = cx.new(|cx| {
            TextInput::new(window, cx)
                .multi_line(3, 6)
                .placeholder(t("Its long-term goal: what done looks like"))
        });
        let scope = cx.new(|cx| {
            TextInput::new(window, cx)
                .placeholder(t("Scope: paths it owns, such as web/**, src/ui"))
        });
        let ws = self.workspace.read(cx);
        let backend = ws.draft_backend.clone();
        self.overlay = Some(Overlay::NewStream {
            name,
            goal,
            scope,
            backend,
            role,
        });
        cx.notify();
    }

    pub(super) fn new_stream_dialog(
        &self,
        name: &Entity<TextInput>,
        goal: &Entity<TextInput>,
        scope: &Entity<TextInput>,
        backend: &str,
        role: Role,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let ws = self.workspace.read(cx);
        let close = cx.entity();
        let pick = cx.entity();
        let mut agents = SegmentedControl::new("stream-agent", backend.to_string())
            .size(ControlSize::Sm)
            .segment(CODEX, t("Codex"), None);
        for agent in &ws.agents {
            // Projects are folders on this computer.
            if agent.status == AgentStatus::Ready && agent.remote.is_none() {
                agents =
                    agents.segment(agent.profile.id.clone(), agent.profile.short.clone(), None);
            }
        }
        let agents = agents.on_change(move |value, _, cx| {
            let value = value.to_string();
            pick.update(cx, |root, cx| {
                if let Some(Overlay::NewStream { backend, .. }) = &mut root.overlay {
                    *backend = value;
                }
                cx.notify();
            })
        });
        let label = |text: &str| {
            div()
                .text_size(theme.text_size(TextSize::Xs))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(colors.fg_muted)
                .child(text.to_string())
        };
        let has_controller = ws
            .active_project()
            .is_some_and(|p| p.controller().is_some());
        let controller = role == Role::Controller;
        let set_role = cx.entity();
        let roles = (!has_controller).then(|| {
            SegmentedControl::new(
                "stream-role",
                if controller { "controller" } else { "lead" },
            )
            .size(ControlSize::Sm)
            .segment("controller", t("Controller"), Some(IconName::Network))
            .segment("lead", t("Lead"), Some(IconName::User))
            .on_change(move |value, _, cx| {
                let value = value.to_string();
                set_role.update(cx, |root, cx| {
                    if let Some(Overlay::NewStream { role, .. }) = &mut root.overlay {
                        *role = if value == "controller" {
                            Role::Controller
                        } else {
                            Role::Lead
                        };
                    }
                    cx.notify();
                })
            })
        });
        let start = (
            cx.entity(),
            self.workspace.clone(),
            name.clone(),
            goal.clone(),
            scope.clone(),
            backend.to_string(),
        );
        Dialog::new(
            "new-stream",
            if controller { t("New controller") } else { t("New lead") },
            move |window, cx| close.update(cx, |this, cx| this.close_overlay(window, cx)),
        )
        .detail(if controller {
            t("The controller splits the project into leads, gives each a roadmap and keeps the plan. It writes no code, and wakes only when a lead reports.")
        } else {
            t("A long-running session that owns one part of the project. It gets its goal, its scope and the atrium's rules, then works on its own, handing pieces to sub-agents when it helps.")
        })
        .child(
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap_3()
                .children(roles.map(|roles| {
                    div().flex().flex_col().gap_1().child(label(t("Role"))).child(roles)
                }))
                .when(!controller, |form| {
                    form.child(div().flex().flex_col().gap_1().child(label(t("Name"))).child(Input::new(name)))
                })
                .child(div().flex().flex_col().gap_1().child(label(t("Agent"))).child(agents))
                .child(div().flex().flex_col().gap_1().child(label(if controller { t("The project's goal") } else { t("Goal") })).child(Input::new(goal)))
                .when(!controller, |form| form.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(label(t("Scope")))
                        .child(Input::new(scope))
                        .child(
                            div()
                                .text_size(theme.text_size(TextSize::Xs))
                                .text_color(colors.fg_subtle)
                                .child(t("Changes it makes in another workstream's scope are flagged and posted to that workstream.")),
                        ),
                )),
        )
        .action(|close| {
            Button::new("stream-cancel", t("Cancel"))
                .variant(ButtonVariant::Ghost)
                .on_click(move |_, window, cx| close(window, cx))
        })
        .action(move |close| {
            let (root, workspace, name, goal, scope, backend) = start;
            Button::new("stream-start", t("Start"))
                .primary()
                .on_click(move |_, window, cx| {
                    let title = if role == Role::Controller {
                        t("Controller").to_string()
                    } else {
                        name.read(cx).text().trim().to_string()
                    };
                    let objective = goal.read(cx).text().trim().to_string();
                    if title.is_empty() || objective.is_empty() {
                        root.update(cx, |root, cx| {
                            root.toast(
                                &crate::workspace::Notice::Info(
                                    t("Give the workstream a name and a goal.").into(),
                                ),
                                cx,
                            )
                        });
                        return;
                    }
                    let paths: Vec<String> = if role == Role::Controller { Vec::new() } else { scope
                        .read(cx)
                        .text()
                        .split([',', ' ', '\n'])
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect() };
                    let backend = backend.clone();
                    workspace.update(cx, |ws, cx| {
                        ws.create_stream(title, role, backend, objective, paths, cx)
                    });
                    close(window, cx);
                })
        })
        .into_any_element()
    }
}

/// The paths a workstream owns, for reading.
fn scope_text(scope: &[String]) -> String {
    if scope.is_empty() {
        t("the whole repository").to_string()
    } else {
        scope.join(", ")
    }
}

/// At most `max` characters, ending in an ellipsis when cut.
fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max).collect();
    cut.push('…');
    cut
}
