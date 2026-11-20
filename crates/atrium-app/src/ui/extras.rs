//! Settings shared by every agent: MCP servers, environments and skills.

use ely_gpui_component::{
    buttons::{Button, ButtonVariant, IconButton},
    forms::{Input, Switch, TextInput},
    primitives::{Icon, IconName},
    theme::{ActiveTheme, ControlSize, IconSize, Radius, TextSize},
};
use gpui::{
    AnyElement, Context, Entity, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*,
};

use super::RootView;
use crate::i18n::{t, tf};
use crate::settings::{SavedEnvironment, SharedMcp, parse_vars};
use crate::skills;

/// The inputs of the shared-settings sections.
pub struct ExtrasInputs {
    pub mcp_name: Entity<TextInput>,
    pub mcp_line: Entity<TextInput>,
    pub env_name: Entity<TextInput>,
    pub env_vars: Entity<TextInput>,
    pub push_topic: Entity<TextInput>,
    pub schedule_name: Entity<TextInput>,
    pub schedule_time: Entity<TextInput>,
    pub schedule_prompt: Entity<TextInput>,
    pub schedule_days: crate::schedule::Days,
}

impl ExtrasInputs {
    pub fn new(window: &mut Window, cx: &mut Context<RootView>) -> Self {
        Self {
            mcp_name: cx.new(|cx| TextInput::new(window, cx).placeholder(t("Name, such as docs"))),
            mcp_line: cx.new(|cx| {
                TextInput::new(window, cx).placeholder(t("Command, such as npx -y @acme/docs-mcp"))
            }),
            env_name: cx
                .new(|cx| TextInput::new(window, cx).placeholder(t("Name, such as staging"))),
            env_vars: cx.new(|cx| {
                TextInput::new(window, cx)
                    .multi_line(2, 6)
                    .placeholder(t("One KEY=value per line"))
            }),
            schedule_name: cx.new(|cx| TextInput::new(window, cx).placeholder(t("Name, such as Morning CI check"))),
            schedule_time: cx.new(|cx| TextInput::new(window, cx).placeholder("09:00")),
            schedule_prompt: cx.new(|cx| {
                TextInput::new(window, cx)
                    .multi_line(2, 5)
                    .placeholder(t("What to ask, such as: Check last night's CI runs and summarise the failures"))
            }),
            schedule_days: crate::schedule::Days::Weekdays,
            push_topic: cx.new(|cx| {
                TextInput::new(window, cx)
                    .placeholder(t("ntfy topic, such as atrium-7f3k, or a full URL"))
            }),
        }
    }
}

fn row_box(cx: &Context<RootView>) -> gpui::Div {
    let theme = cx.theme();
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded(theme.radius(Radius::Md))
        .border_1()
        .border_color(theme.colors.border)
        .text_size(theme.text_size(TextSize::Sm))
}

impl RootView {
    /// The editor files and folders open in.
    pub(super) fn editor_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let chosen = self.editor(cx).map(|e| e.command);
        if self.tools.editors.is_empty() {
            return div()
                .text_size(cx.theme().text_size(TextSize::Sm))
                .text_color(cx.theme().colors.fg_muted)
                .child(t("No editor found. Install the command-line launcher of VS Code, Cursor, Zed, Windsurf or Sublime Text."))
                .into_any_element();
        }
        let mut row = div().flex().flex_wrap().gap_1();
        for editor in &self.tools.editors {
            let on = chosen == Some(editor.command);
            let (ws, command) = (self.workspace.clone(), editor.command);
            row = row.child(
                Button::new(SharedString::from(format!("editor-{command}")), editor.name)
                    .variant(if on { ButtonVariant::Primary } else { ButtonVariant::Ghost })
                    .size(ControlSize::Sm)
                    .on_click(move |_, _, cx| {
                        ws.update(cx, |ws, cx| {
                            ws.settings.editor = Some(command.to_string());
                            ws.settings.save();
                            cx.notify();
                        })
                    }),
            );
        }
        row.into_any_element()
    }

    /// Scheduled chats: the list, and a form for a new one in the current
    /// folder with the current agent.
    pub(super) fn schedules_section(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::schedule::{Days, Schedule, parse_time};
        let ws = self.workspace.read(cx);
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let mut list = div().flex().flex_col().gap_1();
        for s in &ws.settings.schedules {
            let (run, toggle, remove) = (self.workspace.clone(), self.workspace.clone(), self.workspace.clone());
            let id = s.id.clone();
            let (id1, id2, id3) = (id.clone(), id.clone(), id);
            let folder = std::path::Path::new(&s.folder)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| s.folder.clone());
            list = list.child(
                row_box(cx)
                    .when(s.off, |el| el.opacity(0.55))
                    .child(Icon::new(IconName::AlarmClock).size(IconSize::Xs).color(colors.fg_muted))
                    .child(div().flex_none().font_weight(gpui::FontWeight::MEDIUM).child(s.name.clone()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(theme.text_size(TextSize::Xs))
                            .text_color(colors.fg_subtle)
                            .child(format!(
                                "{} {} · {} · {}",
                                t(s.days.label()),
                                s.time,
                                folder,
                                crate::agents::backend_label(&s.backend)
                            )),
                    )
                    .child(
                        Button::new(SharedString::from(format!("schedule-run-{id1}")), t("Run now"))
                            .variant(ButtonVariant::Ghost)
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| run.update(cx, |ws, cx| ws.run_schedule(&id1, cx))),
                    )
                    .child(
                        Button::new(
                            SharedString::from(format!("schedule-toggle-{id2}")),
                            if s.off { t("Off") } else { t("On") },
                        )
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| toggle.update(cx, |ws, cx| ws.toggle_schedule(&id2, cx))),
                    )
                    .child(
                        IconButton::new(SharedString::from(format!("schedule-remove-{id3}")), IconName::X)
                            .variant(ButtonVariant::Ghost)
                            .size(ControlSize::Sm)
                            .tooltip(t("Remove"))
                            .on_click(move |_, _, cx| remove.update(cx, |ws, cx| ws.remove_schedule(&id3, cx))),
                    ),
            );
        }
        let backend = ws.view_backend().to_string();
        let folder = ws.settings.working_dir();
        let mut days = div().flex().items_center().gap_1();
        for d in Days::ALL {
            let on = self.extras.schedule_days == d;
            let root = cx.entity();
            days = days.child(
                Button::new(SharedString::from(format!("schedule-days-{d:?}")), t(d.label()))
                    .variant(if on { ButtonVariant::Primary } else { ButtonVariant::Ghost })
                    .size(ControlSize::Sm)
                    .on_click(move |_, _, cx| {
                        root.update(cx, |r, cx| {
                            r.extras.schedule_days = d;
                            cx.notify();
                        })
                    }),
            );
        }
        let add = (
            cx.entity(),
            self.workspace.clone(),
            backend.clone(),
            folder.display().to_string(),
        );
        list.child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().flex_1().child(Input::new(&self.extras.schedule_name)))
                        .child(div().w(gpui::px(80.)).child(Input::new(&self.extras.schedule_time)))
                        .child(days),
                )
                .child(Input::new(&self.extras.schedule_prompt))
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
                                .text_size(theme.text_size(TextSize::Xs))
                                .text_color(colors.fg_subtle)
                                .child(tf(
                                    "Runs in {folder} with {agent}",
                                    &[
                                        ("folder", &crate::ui::composer::short_path(&folder)),
                                        ("agent", &crate::agents::backend_label(&backend)),
                                    ],
                                )),
                        )
                        .child(
                            Button::new("schedule-add", t("Add"))
                                .size(ControlSize::Sm)
                                .on_click(move |_, _, cx| {
                                    let (root, ws, backend, folder) = add.clone();
                                    let read = |e: &Entity<TextInput>, cx: &gpui::App| e.read(cx).text().trim().to_string();
                                    let (name, time, prompt, days) = {
                                        let r = root.read(cx);
                                        (
                                            read(&r.extras.schedule_name, cx),
                                            read(&r.extras.schedule_time, cx),
                                            read(&r.extras.schedule_prompt, cx),
                                            r.extras.schedule_days,
                                        )
                                    };
                                    let Some(at) = parse_time(&time) else {
                                        root.update(cx, |r, cx| {
                                            r.toast(&crate::workspace::Notice::Info(t("Write the time as HH:MM, such as 09:00.").into()), cx)
                                        });
                                        return;
                                    };
                                    if prompt.is_empty() {
                                        root.update(cx, |r, cx| {
                                            r.toast(&crate::workspace::Notice::Info(t("Write what to ask.").into()), cx)
                                        });
                                        return;
                                    }
                                    let name = if name.is_empty() { prompt.chars().take(24).collect() } else { name };
                                    ws.update(cx, |ws, cx| {
                                        ws.add_schedule(
                                            Schedule {
                                                id: uuid::Uuid::new_v4().simple().to_string(),
                                                name,
                                                prompt,
                                                folder,
                                                backend,
                                                time: at.strftime("%H:%M").to_string(),
                                                days,
                                                off: false,
                                                last_run: None,
                                            },
                                            cx,
                                        )
                                    });
                                    root.update(cx, |r, cx| {
                                        for e in [&r.extras.schedule_name, &r.extras.schedule_time, &r.extras.schedule_prompt] {
                                            e.update(cx, |i, cx| i.set_text("", cx));
                                        }
                                    });
                                }),
                        ),
                ),
        )
        .into_any_element()
    }

    /// Following projects from a phone: the server switch and the push topic.
    pub(super) fn phone_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let ws = self.workspace.read(cx);
        let on = ws.settings.remote_view;
        let saved = ws.settings.push_topic.clone().unwrap_or_default();
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let toggle = self.workspace.clone();
        let mut body = div().flex().flex_col().gap_2().child(
            Switch::new("remote-view", on)
                .label(t("Let my other devices view projects"))
                .on_change(move |on, _, cx| {
                    toggle.update(cx, |ws, cx| {
                        ws.settings.remote_view = on;
                        ws.settings.save();
                        cx.notify();
                    })
                })
                .into_any_element(),
        );
        if on {
            body = body.child(
                row_box(cx)
                    .child(
                        Icon::new(IconName::Globe)
                            .size(IconSize::Xs)
                            .color(colors.fg_muted),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(colors.fg_subtle)
                            .child(t("On the phone: <this computer>:7879")),
                    )
                    .child(
                        Button::new("remote-view-open", t("Open here"))
                            .variant(ButtonVariant::Ghost)
                            .size(ControlSize::Sm)
                            .on_click(|_, _, cx| cx.open_url("http://127.0.0.1:7879/")),
                    ),
            );
            let (ws, field) = (self.workspace.clone(), self.extras.push_topic.clone());
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.extras.push_topic)))
                    .child(
                        Button::new("push-topic-save", t("Save"))
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| {
                                let text = field.read(cx).text().trim().to_string();
                                ws.update(cx, |ws, cx| {
                                    ws.settings.push_topic = (!text.is_empty()).then_some(text);
                                    ws.settings.save();
                                    cx.notify();
                                });
                            }),
                    ),
            );
            if !saved.is_empty() {
                body = body.child(
                    div()
                        .text_size(theme.text_size(TextSize::Xs))
                        .text_color(colors.fg_subtle)
                        .child(tf("Pushes go to {topic}", &[("topic", &saved)])),
                );
            }
        }
        body.into_any_element()
    }

    /// MCP servers every agent gets.
    pub(super) fn mcp_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let ws = self.workspace.read(cx);
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let mut list = div().flex().flex_col().gap_1();
        for (ix, server) in ws.settings.mcp_servers.iter().enumerate() {
            let (toggle, remove) = (self.workspace.clone(), self.workspace.clone());
            let off = server.off;
            list = list.child(
                row_box(cx)
                    .when(off, |el| el.opacity(0.55))
                    .child(
                        Icon::new(IconName::Network)
                            .size(IconSize::Xs)
                            .color(colors.fg_muted),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(server.name.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(theme.mono_family.clone())
                            .text_size(theme.text_size(TextSize::Xs))
                            .text_color(colors.fg_subtle)
                            .child(format!("{} {}", server.command, server.args.join(" "))),
                    )
                    .child(
                        Button::new(
                            SharedString::from(format!("mcp-toggle-{ix}")),
                            if off { t("Off") } else { t("On") },
                        )
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| {
                            toggle.update(cx, |ws, cx| {
                                if let Some(s) = ws.settings.mcp_servers.get_mut(ix) {
                                    s.off = !s.off;
                                }
                                ws.settings.save();
                                cx.notify();
                            })
                        }),
                    )
                    .child(
                        IconButton::new(
                            SharedString::from(format!("mcp-remove-{ix}")),
                            IconName::X,
                        )
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip(t("Remove"))
                        .on_click(move |_, _, cx| {
                            remove.update(cx, |ws, cx| {
                                if ix < ws.settings.mcp_servers.len() {
                                    ws.settings.mcp_servers.remove(ix);
                                }
                                ws.settings.save();
                                cx.notify();
                            })
                        }),
                    ),
            );
        }
        let inputs = (
            self.workspace.clone(),
            self.extras.mcp_name.clone(),
            self.extras.mcp_line.clone(),
        );
        list.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w(gpui::px(140.))
                        .child(Input::new(&self.extras.mcp_name)),
                )
                .child(div().flex_1().child(Input::new(&self.extras.mcp_line)))
                .child(
                    Button::new("mcp-add", t("Add"))
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| {
                            let (ws, name, line) = inputs.clone();
                            let (n, l) = (
                                name.read(cx).text().to_string(),
                                line.read(cx).text().to_string(),
                            );
                            let Some(server) = SharedMcp::parse(&n, &l) else {
                                return;
                            };
                            ws.update(cx, |ws, cx| {
                                ws.settings.mcp_servers.retain(|s| s.name != server.name);
                                ws.settings.mcp_servers.push(server);
                                ws.settings.save();
                                cx.notify();
                            });
                            name.update(cx, |i, cx| i.set_text("", cx));
                            line.update(cx, |i, cx| i.set_text("", cx));
                        }),
                ),
        )
        .into_any_element()
    }

    /// Named sets of environment variables, and the one new sessions use.
    pub(super) fn environments_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let ws = self.workspace.read(cx);
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let active = ws.settings.environment.clone();
        let restart = self.workspace.clone();
        let mut list = div().flex().flex_col().gap_1().child(
            Button::new("restart-acp", t("Restart ACP agents"))
                .size(ControlSize::Sm)
                .on_click(move |_, _, cx| restart.update(cx, |ws, cx| ws.restart_acp_agents(cx))),
        );
        let none_on = active.is_none();
        let pick_none = self.workspace.clone();
        list = list.child(
            row_box(cx)
                .id("env-none")
                .cursor_pointer()
                .when(none_on, |el| el.border_color(colors.accent))
                .on_click(move |_, _, cx| {
                    pick_none.update(cx, |ws, cx| {
                        let previous = ws.session_env();
                        ws.settings.environment = None;
                        ws.save_environment(previous, cx);
                    })
                })
                .child(
                    Icon::new(if none_on {
                        IconName::CircleDot
                    } else {
                        IconName::Circle
                    })
                    .size(IconSize::Xs)
                    .color(colors.accent),
                )
                .child(
                    div()
                        .flex_1()
                        .child(t("None: agents get only their own environment")),
                ),
        );
        for (ix, env) in ws.settings.environments.iter().enumerate() {
            let on = active.as_deref() == Some(env.name.as_str());
            let (pick, remove, name) = (
                self.workspace.clone(),
                self.workspace.clone(),
                env.name.clone(),
            );
            let keys: Vec<&str> = env.vars.iter().map(|(k, _)| k.as_str()).collect();
            list = list.child(
                row_box(cx)
                    .id(SharedString::from(format!("env-{ix}")))
                    .cursor_pointer()
                    .when(on, |el| el.border_color(colors.accent))
                    .on_click(move |_, _, cx| {
                        let name = name.clone();
                        pick.update(cx, |ws, cx| {
                            let previous = ws.session_env();
                            ws.settings.environment = Some(name);
                            ws.save_environment(previous, cx);
                        })
                    })
                    .child(
                        Icon::new(if on {
                            IconName::CircleDot
                        } else {
                            IconName::Circle
                        })
                        .size(IconSize::Xs)
                        .color(colors.accent),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(env.name.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(theme.mono_family.clone())
                            .text_size(theme.text_size(TextSize::Xs))
                            .text_color(colors.fg_subtle)
                            .child(keys.join(" · ")),
                    )
                    .child(
                        IconButton::new(
                            SharedString::from(format!("env-remove-{ix}")),
                            IconName::X,
                        )
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip(t("Remove"))
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            remove.update(cx, |ws, cx| {
                                let previous = ws.session_env();
                                if ix < ws.settings.environments.len() {
                                    let gone = ws.settings.environments.remove(ix);
                                    if ws.settings.environment.as_deref()
                                        == Some(gone.name.as_str())
                                    {
                                        ws.settings.environment = None;
                                    }
                                }
                                ws.save_environment(previous, cx);
                            })
                        }),
                    ),
            );
        }
        let inputs = (
            self.workspace.clone(),
            self.extras.env_name.clone(),
            self.extras.env_vars.clone(),
        );
        list.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Input::new(&self.extras.env_name))
                .child(Input::new(&self.extras.env_vars))
                .child(
                    div().flex().justify_end().child(
                        Button::new("env-add", t("Save environment"))
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| {
                                let (ws, name, vars) = inputs.clone();
                                let n = name.read(cx).text().trim().to_string();
                                let v = parse_vars(vars.read(cx).text());
                                if n.is_empty() || v.is_empty() {
                                    return;
                                }
                                ws.update(cx, |ws, cx| {
                                    let previous = ws.session_env();
                                    ws.settings.environments.retain(|e| e.name != n);
                                    ws.settings.environments.push(SavedEnvironment {
                                        name: n.clone(),
                                        vars: v,
                                    });
                                    ws.settings.environment = Some(n);
                                    ws.save_environment(previous, cx);
                                });
                                name.update(cx, |i, cx| i.set_text("", cx));
                                vars.update(cx, |i, cx| i.set_text("", cx));
                            }),
                    ),
                ),
        )
        .into_any_element()
    }

    /// The skills each agent has, and sharing one with the others.
    pub(super) fn skills_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let Some(home) = dirs::home_dir() else {
            return div().into_any_element();
        };
        let found = skills::list(&home);
        let mut list = div().flex().flex_col().gap_1();
        let has_atrium = found
            .iter()
            .any(|s| s.name == "atrium" && s.installed.iter().all(|i| *i));
        if !has_atrium {
            let root = cx.entity();
            list = list.child(
                Button::new(
                    "skill-atrium",
                    t("Install the Atrium skill for every agent"),
                )
                .icon(IconName::Download)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Sm)
                .on_click(move |_, _, cx| {
                    let said = match dirs::home_dir().map(|h| atrium_project::skill::install(&h)) {
                        Some(Ok(_)) => {
                            crate::workspace::Notice::Success(t("Atrium skill installed").into())
                        }
                        Some(Err(e)) => crate::workspace::Notice::Error(format!("{e:#}")),
                        None => return,
                    };
                    root.update(cx, |r, cx| {
                        r.toast(&said, cx);
                        cx.notify();
                    })
                }),
            );
        }
        if found.is_empty() {
            list = list.child(
                div()
                    .text_size(theme.text_size(TextSize::Sm))
                    .text_color(colors.fg_muted)
                    .child(t("No skills installed yet.")),
            );
        }
        for (ix, skill) in found.into_iter().enumerate() {
            let missing = skill.installed.iter().any(|i| !*i);
            let root = cx.entity();
            let name = skill.name.clone();
            list = list.child(
                row_box(cx)
                    .child(
                        Icon::new(IconName::Sparkles)
                            .size(IconSize::Xs)
                            .color(colors.fg_muted),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(skill.name.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(theme.text_size(TextSize::Xs))
                            .text_color(colors.fg_subtle)
                            .child(skill.description.clone()),
                    )
                    .children(skills::AGENTS.iter().enumerate().map(|(a, (agent, _))| {
                        let on = skill.installed[a];
                        div()
                            .flex_none()
                            .px_1()
                            .rounded_sm()
                            .text_size(theme.text_size(TextSize::Xs))
                            .when(on, |el| {
                                el.bg(colors.success.opacity(0.12))
                                    .text_color(colors.success)
                            })
                            .when(!on, |el| el.text_color(colors.fg_subtle))
                            .child(agent.to_string())
                    }))
                    .when(missing, |el| {
                        el.child(
                            Button::new(
                                SharedString::from(format!("skill-share-{ix}")),
                                t("Share"),
                            )
                            .variant(ButtonVariant::Ghost)
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| {
                                let said = match dirs::home_dir().map(|h| skills::share(&h, &name))
                                {
                                    Some(Ok(n)) => crate::workspace::Notice::Success(tf(
                                        "Shared with {n} more agent(s)",
                                        &[("n", &n)],
                                    )),
                                    Some(Err(e)) => {
                                        crate::workspace::Notice::Error(format!("{e:#}"))
                                    }
                                    None => return,
                                };
                                root.update(cx, |r, cx| {
                                    r.toast(&said, cx);
                                    cx.notify();
                                })
                            }),
                        )
                    }),
            );
        }
        list.into_any_element()
    }
}
