//! Settings: the team a project's controller hands work to. One row per
//! member: its agent, whether it can work now, the user's notes on it
//! (which the controller reads) and who stands in for it.

use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    forms::{Input, Switch, TextInput},
    theme::{ActiveTheme, ControlSize, Radius, TextSize},
};
use gpui::{
    AnyElement, AppContext, Context, Entity, IntoElement, ParentElement, Styled, Window, div,
};

use super::RootView;
use crate::agents;
use crate::i18n::{t, tf};

/// A notes field per member, filled with what the team file says.
pub fn note_inputs(
    root: &RootView,
    window: &mut Window,
    cx: &mut Context<RootView>,
) -> Vec<(String, Entity<TextInput>)> {
    let members: Vec<(String, String)> = root
        .workspace
        .read(cx)
        .team
        .team
        .members
        .iter()
        .map(|(name, m)| (name.clone(), m.notes.clone()))
        .collect();
    members
        .into_iter()
        .map(|(name, notes)| {
            let input = cx.new(|cx| {
                TextInput::new(window, cx)
                    .multi_line(1, 4)
                    .placeholder(t("What it is good and bad at, for the controller"))
            });
            input.update(cx, |input, cx| input.set_text(notes, cx));
            (name, input)
        })
        .collect()
}

impl RootView {
    pub(super) fn team_section(
        &self,
        notes: &[(String, Entity<TextInput>)],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ws = self.workspace.read(cx);
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let small = theme.text_size(TextSize::Sm);
        let tiny = theme.text_size(TextSize::Xs);
        let state = &ws.team;
        let now = jiff::Timestamp::now().as_second();
        let mut list = div().flex().flex_col().gap_2().child(
            Switch::new("team-on", state.team.on)
                .label(t("Let project controllers hand work to the team"))
                .on_change({
                    let workspace = self.workspace.clone();
                    move |on, _, cx| {
                        workspace.update(cx, |ws, cx| {
                            let mut team = ws.team.team.clone();
                            team.on = on;
                            ws.save_team(team, cx);
                        })
                    }
                }),
        );
        if let Some(error) = &state.error {
            list = list.child(div().text_size(tiny).text_color(colors.danger).child(tf(
                "Could not read the team file: {error}",
                &[("error", error)],
            )));
        }
        for (name, member) in &state.team.members {
            let (status, color) = if !ws.backend_ready(&member.agent) {
                (t("Not available").to_string(), colors.fg_muted)
            } else if let Some(at) = state.quotas.spent_until(name, now) {
                let mins = ((at - now + 59) / 60).to_string();
                (
                    tf(
                        "Limit reached, back in about {mins} min",
                        &[("mins", &mins)],
                    ),
                    colors.warning,
                )
            } else {
                (t("Ready").to_string(), colors.success)
            };
            let stand_ins = if member.stand_ins.is_empty() {
                t("No stand-in").to_string()
            } else {
                tf(
                    "Stand-in: {names}",
                    &[("names", &member.stand_ins.join(", "))],
                )
            };
            let input = notes
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, i)| i.clone());
            list = list.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .rounded(theme.radius(Radius::Md))
                    .border_1()
                    .border_color(colors.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(small)
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(name.clone()),
                            )
                            .child(
                                div()
                                    .text_color(colors.fg_muted)
                                    .child(agents::backend_label(&member.agent)),
                            )
                            .child(div().flex_1())
                            .child(div().text_size(tiny).text_color(color).child(status)),
                    )
                    .children(input.map(|i| Input::new(&i)))
                    .child(
                        div()
                            .text_size(tiny)
                            .text_color(colors.fg_muted)
                            .child(stand_ins),
                    ),
            );
        }
        let open = (cx.entity(), crate::workspace::team::team_path());
        let reload = self.workspace.clone();
        list.child(
            div()
                .flex()
                .gap_2()
                .child(
                    Button::new("team-file", t("Edit team.toml"))
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| {
                            let (root, path) = open.clone();
                            let Some(path) = path else { return };
                            root.update(cx, |this, cx| {
                                if !path.exists() {
                                    let team = this.workspace.read(cx).team.team.clone();
                                    this.workspace.update(cx, |ws, cx| ws.save_team(team, cx));
                                }
                                this.open_in_editor(&path, None, cx);
                            });
                        }),
                )
                .child(
                    Button::new("team-reload", t("Reload"))
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| reload.update(cx, |ws, cx| ws.reload_team(cx))),
                ),
        )
        .into_any_element()
    }
}

/// Puts the edited notes into the team and saves it, when any changed.
pub fn save_notes(
    workspace: &Entity<crate::workspace::Workspace>,
    notes: &[(String, Entity<TextInput>)],
    cx: &mut gpui::App,
) {
    let mut team = workspace.read(cx).team.team.clone();
    let mut changed = false;
    for (name, input) in notes {
        let text = input.read(cx).text().trim().to_string();
        if let Some(member) = team.members.get_mut(name)
            && member.notes != text
        {
            member.notes = text;
            changed = true;
        }
    }
    if changed {
        workspace.update(cx, |ws, cx| ws.save_team(team, cx));
    }
}
