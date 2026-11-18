//! The settings' Devices section: the tailnet's devices that run
//! `atrium-node`, what each offers, and nodes added by address.

use ely_gpui_component::{
    buttons::{Button, ButtonVariant, IconButton},
    data_display::{Badge, Tone},
    forms::{Input, TextInput},
    primitives::{Icon, IconName},
    theme::{ActiveTheme, ControlSize, IconSize, Radius, TextSize},
};
use gpui::{
    AnyElement, App, Entity, InteractiveElement, IntoElement, ParentElement, SharedString, Styled,
    div, prelude::*, px,
};

use crate::i18n::{t, tf};
use crate::workspace::{AgentStatus, DeviceStatus, Workspace};

pub fn section(workspace: &Entity<Workspace>, address: &Entity<TextInput>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let ws = workspace.read(cx);
    let small = theme.text_size(TextSize::Sm);
    let tiny = theme.text_size(TextSize::Xs);

    let tailnet = match &ws.tailnet {
        None => t("Looking for devices…").to_string(),
        Some(Ok(online)) => tf(
            "Tailnet: {online} online · {nodes} running atrium-node",
            &[
                ("online", online),
                (
                    "nodes",
                    &ws.devices
                        .iter()
                        .filter(|d| d.status == DeviceStatus::Online)
                        .count(),
                ),
            ],
        ),
        Some(Err(why)) => tf("Tailnet not read: {why}", &[("why", why)]),
    };
    let refresh = workspace.clone();
    let mut column = div().flex().flex_col().gap_2().child(
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
                    .child(tailnet),
            )
            .child(
                Button::new(
                    "devices-refresh",
                    if ws.discovering {
                        t("Looking…")
                    } else {
                        t("Refresh")
                    },
                )
                .size(ControlSize::Sm)
                .variant(ButtonVariant::Ghost)
                .icon(IconName::RefreshCw)
                .disabled(ws.discovering)
                .on_click(move |_, _, cx| refresh.update(cx, |ws, cx| ws.discover(cx))),
            ),
    );

    for device in &ws.devices {
        let online = device.status == DeviceStatus::Online;
        let mut agents: Vec<String> = Vec::new();
        if device.codex_status != AgentStatus::NotInstalled {
            agents.push(match &device.codex_status {
                AgentStatus::Ready => "Codex".into(),
                AgentStatus::Starting => tf("{agent} (starting)", &[("agent", &"Codex")]),
                AgentStatus::Failed(_) => tf("{agent} (not running)", &[("agent", &"Codex")]),
                AgentStatus::NotInstalled => String::new(),
            });
        }
        for agent in ws
            .agents
            .iter()
            .filter(|a| a.remote.as_ref().is_some_and(|r| r.device == device.id))
        {
            agents.push(match &agent.status {
                AgentStatus::Ready => agent.profile.short.clone(),
                AgentStatus::Starting => {
                    tf("{agent} (starting)", &[("agent", &agent.profile.short)])
                }
                _ => tf("{agent} (not running)", &[("agent", &agent.profile.short)]),
            });
        }
        let missing: Vec<String> = device
            .info
            .agents
            .iter()
            .filter(|a| !a.available)
            .map(|a| a.name.clone())
            .collect();
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
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(small)
                        .child(div().size(px(7.)).flex_none().rounded_full().bg(if online {
                            colors.success
                        } else {
                            colors.fg_disabled
                        }))
                        .child(
                            Icon::new(IconName::Monitor)
                                .size(IconSize::Sm)
                                .color(colors.fg_muted),
                        )
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(device.id.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(tiny)
                                .text_color(colors.fg_subtle)
                                .child(format!(
                                    "{} · {}",
                                    device.info.device.os,
                                    device.node.base.trim_start_matches("http://")
                                )),
                        )
                        .when_some(device.info.magpie.as_ref(), |row, magpie| {
                            row.child(
                                Badge::new(format!("magpie {}", magpie.version)).tone(Tone::Accent),
                            )
                        }),
                )
                .child(div().text_size(tiny).text_color(colors.fg_muted).child(
                    match &device.status {
                        DeviceStatus::Offline(why) => tf("Offline: {why}", &[("why", why)]),
                        DeviceStatus::Online if agents.is_empty() => t("No agents").into(),
                        DeviceStatus::Online => agents.join(" · "),
                    },
                ))
                .when(!missing.is_empty(), |card| {
                    card.child(div().text_size(tiny).text_color(colors.fg_subtle).child(tf(
                        "Not installed there: {list}",
                        &[("list", &missing.join(", "))],
                    )))
                }),
        );
    }
    if ws.devices.is_empty() {
        column = column.child(
            div()
                .text_size(tiny)
                .text_color(colors.fg_subtle)
                .child(t("No devices yet. Run atrium-node on another computer of your tailnet; it shows up here.")),
        );
    }

    // Added by hand.
    for (ix, node) in ws.settings.nodes.iter().enumerate() {
        let (remove, node) = (workspace.clone(), node.clone());
        column = column.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(tiny)
                .text_color(colors.fg_muted)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(tf("Added: {node}", &[("node", &node)])),
                )
                .child(
                    IconButton::new(("remove-node", ix), IconName::X)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip(t("Forget this address"))
                        .on_click(move |_, _, cx| {
                            let node = node.clone();
                            remove.update(cx, |ws, cx| ws.remove_node(&node, cx))
                        }),
                ),
        );
    }
    let (add, input) = (workspace.clone(), address.clone());
    column
        .child(
            div()
                .id(SharedString::from("add-node"))
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().child(Input::new(address)))
                .child(
                    Button::new("add-node-button", t("Add"))
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| {
                            let text = input.read(cx).text().to_string();
                            add.update(cx, |ws, cx| ws.add_node(text, cx));
                            input.update(cx, |input, cx| input.set_text("", cx));
                        }),
                ),
        )
        .into_any_element()
}
