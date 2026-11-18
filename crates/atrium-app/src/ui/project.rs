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
    ParentElement, PathPromptOptions, SharedString, StatefulInteractiveElement, Styled, Window,
    div, prelude::*, px,
};

use super::{Overlay, RootView, TITLE_BAR_HEIGHT, composer::short_path};
use crate::{
    agents,
    project::{self, ATRIUM, YOU},
    workspace::{AgentStatus, CODEX, StreamState, StreamView, Workspace},
};

/// Which part of the atrium the side panel shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AtriumTab {
    #[default]
    Notices,
    Brief,
    Contracts,
    Status,
}

impl AtriumTab {
    fn value(self) -> &'static str {
        match self {
            AtriumTab::Notices => "notices",
            AtriumTab::Brief => "brief",
            AtriumTab::Contracts => "contracts",
            AtriumTab::Status => "status",
        }
    }

    fn from_value(value: &str) -> Self {
        match value {
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
        StreamState::Starting => "Starting…".into(),
        StreamState::Offline => "Agent not running".into(),
        StreamState::Working => "Working".into(),
        StreamState::NeedsYou(1) => "Needs you".into(),
        StreamState::NeedsYou(n) => format!("Needs you ({n})"),
        StreamState::Failed(_) => "Hit an error".into(),
        StreamState::Idle => "Idle".into(),
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
        prompt: Some("Open as a project".into()),
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
                .child("No projects yet.")
                .child(
                    div()
                        .text_size(theme.text_size(TextSize::Xs))
                        .text_color(colors.fg_subtle)
                        .child("A project is a folder that several long-running sessions work on, each toward its own goal."),
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
            for view in &views {
                let id = view.stream.id.clone();
                let open = workspace.clone();
                let selected = open_stream.as_deref() == Some(id.as_str());
                list = list.child(
                    div()
                        .id(SharedString::from(format!("stream-{id}")))
                        .ml_4()
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
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(view.stream.name.clone()),
                        )
                        .when(view.unread > 0, |row| {
                            row.child(
                                div()
                                    .text_size(theme.text_size(TextSize::Xs))
                                    .text_color(colors.accent)
                                    .child(format!("{} new", view.unread)),
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
                    "{} workstreams · {working} working{}",
                    views.len(),
                    if needing.is_empty() {
                        String::new()
                    } else {
                        format!(" · {} need you", needing.len())
                    }
                )),
        )
        .child(
            Button::new("new-stream", "New workstream")
                .icon(IconName::Plus)
                .size(ControlSize::Sm)
                .on_click(move |_, window, cx| {
                    add.update(cx, |root, cx| root.open_new_stream(window, cx))
                }),
        )
        .child({
            let remove = (workspace.clone(), project.root.clone());
            let open_dir = project.dir();
            let menu = Menu::new()
                .item(
                    MenuItem::new("Open the atrium folder")
                        .icon(IconName::FolderOpen)
                        .on_click(move |_, cx| cx.open_with_system(&open_dir)),
                )
                .separator()
                .item(
                    MenuItem::new("Remove from Atrium")
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
                    .child("Needs you"),
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
                        "is waiting for an approval or an answer".to_string()
                    } else {
                        format!("is waiting on {n} approvals or answers")
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

    let cards: Vec<AnyElement> = views
        .iter()
        .map(|view| stream_card(workspace, view, cx))
        .collect();
    let empty = cards.is_empty();
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
        .children(attention)
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
                            .child("Start a workstream"),
                    )
                    .child(
                        div()
                            .text_size(theme.text_size(TextSize::Sm))
                            .text_color(colors.fg_muted)
                            .child("Each workstream is a long-running session with its own goal and scope, such as the frontend, a backend feature, or performance work. They work on their own and align through the atrium on the right."),
                    )
                    .child(
                        Button::new("first-stream", "New workstream")
                            .primary()
                            .icon(IconName::Plus)
                            .on_click(move |_, window, cx| {
                                add.update(cx, |root, cx| root.open_new_stream(window, cx))
                            }),
                    ),
            )
        })
        .child(grid);

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

fn stream_card(workspace: &Entity<Workspace>, view: &StreamView, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let ws = workspace.read(cx);
    let id = view.stream.id.clone();
    let small = theme.text_size(TextSize::Sm);
    let tiny = theme.text_size(TextSize::Xs);
    let open = workspace.clone();

    let progress = match (view.plan, &view.step, &view.focus) {
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
                                .child(step.clone().unwrap_or_else(|| "Plan done".into())),
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
                .child(div().flex_none().child("Focus:"))
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
            format!(
                "{} new notice{}",
                view.unread,
                if view.unread == 1 { "" } else { "s" }
            ),
            colors.accent,
        ));
    }
    if view.crossings > 0 {
        facts.push(fact(
            IconName::TriangleAlert,
            format!(
                "{} change{} outside its scope",
                view.crossings,
                if view.crossings == 1 { "" } else { "s" }
            ),
            colors.warning,
        ));
    }
    if let Some(speed) = &view.speed {
        facts.push(fact(IconName::Zap, speed.clone(), colors.fg_subtle));
    }
    if let Some(share) = view.context {
        facts.push(fact(
            IconName::Gauge,
            format!("{:.0}% context", share * 100.),
            if share >= 0.8 {
                colors.warning
            } else {
                colors.fg_subtle
            },
        ));
    }

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
                    |row| row.child(Badge::new("Goal done").tone(Tone::Success)),
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
                    "No goal set".to_string()
                } else {
                    clip(&view.stream.goal, 160)
                }),
        )
        .child(
            div()
                .text_size(tiny)
                .text_color(colors.fg_subtle)
                .truncate()
                .child(format!(
                    "Scope: {}",
                    project::scope_label(&view.stream.scope).replace('`', "")
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
                            "Hand over notices",
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
                            "Carry on",
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
            YOU => "You".into(),
            ATRIUM => "Atrium".into(),
            "all" => "everyone".into(),
            id => project
                .stream(id)
                .map(|s| s.name.clone())
                .unwrap_or_else(|| id.to_string()),
        }
    };

    let switch = root.clone();
    let tabs = SegmentedControl::new("atrium-tab", tab.value())
        .segment("notices", "Notices", None)
        .segment("brief", "Brief", None)
        .segment("contracts", "Contracts", None)
        .segment("status", "Status", None)
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
                Menu::new().item(MenuItem::radio("Everyone", to == "all").on_click({
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
                            DropdownMenu::new("notice-to", format!("To {}", name_of(&to)), targets)
                                .variant(ButtonVariant::Ghost),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("post-notice", "Post")
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
                        .child("No notices yet. Workstreams post here when they change something others depend on."),
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
        AtriumTab::Brief => doc_view(
            "Brief",
            "Shared conventions and decisions. Every workstream reads it first.",
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
                            .child("Interfaces between workstreams. Whoever changes one updates its file and posts a notice."),
                    )
                    .child(
                        Button::new("open-contracts", "Open folder")
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
                        .child("No contracts yet."),
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
                                format!("No status yet ({}/status/{}.md).", project::DIR, stream.id)
                            }),
                        )),
                );
            }
            if project.file.streams.is_empty() {
                column = column.child(
                    div()
                        .text_size(small)
                        .text_color(colors.fg_subtle)
                        .child("Workstreams write their status here."),
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
                                .child("Atrium"),
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
                    Button::new("open-doc", "Edit")
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
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(format!("Goal: {}", project::first_line(&stream.goal))),
            )
            .child(
                div()
                    .flex_none()
                    .child(project::scope_label(&stream.scope).replace('`', "")),
            )
            .child({
                let remove = (workspace.clone(), stream.id.clone());
                ely_gpui_component::buttons::IconButton::new("leave-project", IconName::X)
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .tooltip("Take out of the project (the chat stays, under Chats)")
                    .on_click(move |_, _, cx| {
                        let (ws, id) = remove.clone();
                        ws.update(cx, |ws, cx| ws.remove_stream(&id, cx))
                    })
            })
            .when(unread > 0, |bar| {
                bar.child(
                    Button::new(
                        "bar-deliver",
                        format!(
                            "Hand over {unread} new notice{}",
                            if unread == 1 { "" } else { "s" }
                        ),
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
        let name = cx.new(|cx| TextInput::new(window, cx).placeholder("Name, such as Frontend"));
        let goal = cx.new(|cx| {
            TextInput::new(window, cx)
                .multi_line(3, 6)
                .placeholder("Its long-term goal: what done looks like")
        });
        let scope = cx.new(|cx| {
            TextInput::new(window, cx).placeholder("Scope: paths it owns, such as web/**, src/ui")
        });
        let ws = self.workspace.read(cx);
        let backend = ws.draft_backend.clone();
        self.overlay = Some(Overlay::NewStream {
            name,
            goal,
            scope,
            backend,
        });
        cx.notify();
    }

    pub(super) fn new_stream_dialog(
        &self,
        name: &Entity<TextInput>,
        goal: &Entity<TextInput>,
        scope: &Entity<TextInput>,
        backend: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let ws = self.workspace.read(cx);
        let close = cx.entity();
        let pick = cx.entity();
        let mut agents = SegmentedControl::new("stream-agent", backend.to_string())
            .size(ControlSize::Sm)
            .segment(CODEX, "Codex", None);
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
        let start = (
            cx.entity(),
            self.workspace.clone(),
            name.clone(),
            goal.clone(),
            scope.clone(),
            backend.to_string(),
        );
        Dialog::new("new-stream", "New workstream", move |window, cx| {
            close.update(cx, |this, cx| this.close_overlay(window, cx))
        })
        .detail("A long-running session in this project. It gets its goal, its scope and the atrium's rules, then works on its own.")
        .child(
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap_3()
                .child(div().flex().flex_col().gap_1().child(label("Name")).child(Input::new(name)))
                .child(div().flex().flex_col().gap_1().child(label("Agent")).child(agents))
                .child(div().flex().flex_col().gap_1().child(label("Goal")).child(Input::new(goal)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(label("Scope"))
                        .child(Input::new(scope))
                        .child(
                            div()
                                .text_size(theme.text_size(TextSize::Xs))
                                .text_color(colors.fg_subtle)
                                .child("Changes it makes in another workstream's scope are flagged and posted to that workstream."),
                        ),
                ),
        )
        .action(|close| {
            Button::new("stream-cancel", "Cancel")
                .variant(ButtonVariant::Ghost)
                .on_click(move |_, window, cx| close(window, cx))
        })
        .action(move |close| {
            let (root, workspace, name, goal, scope, backend) = start;
            Button::new("stream-start", "Start")
                .primary()
                .on_click(move |_, window, cx| {
                    let title = name.read(cx).text().trim().to_string();
                    let objective = goal.read(cx).text().trim().to_string();
                    if title.is_empty() || objective.is_empty() {
                        root.update(cx, |root, cx| {
                            root.toast(
                                &crate::workspace::Notice::Info(
                                    "Give the workstream a name and a goal.".into(),
                                ),
                                cx,
                            )
                        });
                        return;
                    }
                    let paths: Vec<String> = scope
                        .read(cx)
                        .text()
                        .split([',', ' ', '\n'])
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect();
                    let backend = backend.clone();
                    workspace.update(cx, |ws, cx| {
                        ws.create_stream(title, backend, objective, paths, cx)
                    });
                    close(window, cx);
                })
        })
        .into_any_element()
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
