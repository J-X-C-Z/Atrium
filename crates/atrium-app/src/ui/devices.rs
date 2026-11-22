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

/// This computer as the other devices see it: its name, whether it shares,
/// and pairing with a device of another Tailscale owner by a one-time code.
pub fn this_computer(
    workspace: &Entity<Workspace>,
    inputs: &super::extras::ExtrasInputs,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let ws = workspace.read(cx);
    let small = theme.text_size(TextSize::Sm);
    let tiny = theme.text_size(TextSize::Xs);
    let sharing = ws.sharing();
    let label = |text: &'static str| {
        div()
            .text_size(small)
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(colors.fg)
            .child(t(text))
    };
    let note = |text: String| {
        div()
            .text_size(tiny)
            .text_color(colors.fg_muted)
            .child(text)
    };

    let toggle = workspace.clone();
    let mut column = div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded(theme.radius(Radius::Lg))
        .border_1()
        .border_color(colors.border)
        .child(label("This computer"))
        .child(
            ely_gpui_component::forms::Switch::new("sync-sharing", sharing)
                .label(t("Share my chats and projects with my other devices"))
                .on_change(move |on, _, cx| toggle.update(cx, |ws, cx| ws.set_sharing(on, cx))),
        )
        .child(note(
            t("Only devices of the same Tailscale owner, or paired with a code, can see them. Keys and tokens never leave this computer.").into(),
        ));

    // Its name.
    let (name_input, rename) = (inputs.device_name.clone(), workspace.clone());
    let current = ws.settings.device_name.clone();
    column = column.child(
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_none()
                    .text_size(small)
                    .text_color(colors.fg_muted)
                    .child(if current.is_empty() {
                        t("Name").to_string()
                    } else {
                        tf("Name: {name}", &[("name", &current)])
                    }),
            )
            .child(div().flex_1().child(Input::new(&inputs.device_name)))
            .child(
                Button::new("device-rename", t("Rename"))
                    .size(ControlSize::Sm)
                    .on_click(move |_, _, cx| {
                        let text = name_input.read(cx).text().to_string();
                        rename.update(cx, |ws, cx| ws.set_device_name(&text, cx));
                        name_input.update(cx, |input, cx| input.set_text("", cx));
                    }),
            ),
    );

    if !sharing {
        return column.into_any_element();
    }

    // A code for another device to pair with this one.
    let make = workspace.clone();
    let code_row = match ws.shown_pairing_code() {
        Some((code, _left)) => div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .px_3()
                    .py_1()
                    .rounded(theme.radius(Radius::Md))
                    .bg(colors.sunken)
                    .font_family(theme.mono_family.clone())
                    .text_size(theme.text_size(TextSize::Lg))
                    .text_color(colors.fg)
                    .child(code),
            )
            .child(note(
                t("Type it on the other computer within 5 minutes. It works once.").into(),
            ))
            .into_any_element(),
        None => Button::new("pair-code", t("Show a pairing code"))
            .size(ControlSize::Sm)
            .icon(IconName::Link)
            .on_click(move |_, _, cx| make.update(cx, |ws, cx| ws.pairing_code(cx)))
            .into_any_element(),
    };
    column = column
        .child(div().h(px(1.)).bg(colors.border))
        .child(label("Pair with a computer of another owner"))
        .child(note(
            t("Your own devices on Tailscale find each other by themselves. For a computer signed in to another Tailscale account, show a code there and type it here, or the other way round.").into(),
        ))
        .child(code_row);

    let (address, code, pair) = (
        inputs.pair_address.clone(),
        inputs.pair_code.clone(),
        workspace.clone(),
    );
    column
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().child(Input::new(&inputs.pair_address)))
                .child(div().w(px(110.)).child(Input::new(&inputs.pair_code)))
                .child(
                    Button::new("pair-join", t("Pair"))
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| {
                            let a = address.read(cx).text().to_string();
                            let c = code.read(cx).text().to_string();
                            pair.update(cx, |ws, cx| ws.pair_with(a, c, cx));
                            code.update(cx, |input, cx| input.set_text("", cx));
                        }),
                ),
        )
        .into_any_element()
}
