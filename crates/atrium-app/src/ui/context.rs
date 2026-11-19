//! How full the chat's context is and how fast the agent writes: a quick view
//! in the title bar, a fact under each turn, and the details dialog.

use std::time::{Duration, Instant};

use atrium_core::{TokenCounts, TokenUsage, Transcript, TurnPace};
use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    motion::{ProgressBar, ProgressRing},
    overlays::Dialog,
    primitives::{Icon, IconName, Tooltip},
    theme::{ActiveTheme, IconSize, Radius, TextSize},
};
use gpui::{
    AnyElement, App, Context, Entity, Hsla, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, div, prelude::*, px, relative,
};

use super::{RootView, rows::format_duration};
use crate::i18n::{t, tf};
use crate::workspace::Workspace;

/// Context this full or more is worth a warning.
const NEARLY_FULL: f64 = 0.8;

pub fn format_tokens(tokens: u64) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..1_000_000 => format!("{:.1}k", tokens as f64 / 1_000.),
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.),
    }
}

/// "52 tok/s", once a turn has written enough to measure.
pub fn format_speed(pace: &TurnPace) -> Option<String> {
    pace.tokens_per_sec()
        .map(|rate| tf("{rate} tok/s", &[("rate", &format!("{rate:.0}"))]))
}

/// "640 tokens", or "≈640 tokens" when only estimated from the text.
pub fn format_out(pace: &TurnPace) -> Option<String> {
    pace.tokens_out().map(|(n, exact)| {
        let approx = if exact { "" } else { "≈" };
        tf(
            "{n} tokens",
            &[("n", &format!("{approx}{}", format_tokens(n)))],
        )
    })
}

/// "320 ms" or "2.4s".
fn format_wait(wait: Duration) -> String {
    if wait < Duration::from_secs(1) {
        format!("{} ms", wait.as_millis())
    } else {
        format!("{:.1}s", wait.as_secs_f64())
    }
}

fn format_share(share: f64) -> String {
    if share > 0. && share < 0.01 {
        "<1%".into()
    } else {
        format!("{:.0}%", share * 100.)
    }
}

/// Title bar: how full the context is and how fast the agent writes (live
/// while it runs). Opens the details.
pub fn quick_view(ws: &Workspace, root: Entity<RootView>, cx: &App) -> Option<AnyElement> {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let transcript = ws.transcript()?;
    let usage = transcript.usage();
    let pace = transcript.latest_pace(Instant::now());
    let speed = pace.as_ref().and_then(format_speed);
    let share = usage.and_then(TokenUsage::context_share);
    if usage.is_none() && speed.is_none() {
        return None;
    }

    let mut tip = Vec::new();
    if let Some(usage) = usage {
        match (usage.context_used, usage.context_window) {
            (Some(used), Some(window)) => tip.push(tf(
                "Context: {used} of {window} tokens",
                &[
                    ("used", &format_tokens(used)),
                    ("window", &format_tokens(window)),
                ],
            )),
            _ => tip.push(tf(
                "{n} tokens used",
                &[(
                    "n",
                    &format_tokens(usage.total.total.max(usage.context_used.unwrap_or(0))),
                )],
            )),
        }
    }
    if let Some(pace) = &pace
        && let Some(speed) = format_speed(pace)
    {
        tip.push(if pace.done {
            tf("Last answer: {speed}", &[("speed", &speed)])
        } else {
            tf("Writing at {speed}", &[("speed", &speed)])
        });
    }
    let tip = tip.join(" · ");

    let full = share.is_some_and(|s| s >= NEARLY_FULL);
    let live = pace.as_ref().is_some_and(|p| p.writing_now) && transcript.is_running();
    let small = theme.text_size(TextSize::Xs);
    Some(
        div()
            .id("context-quick-view")
            .flex_none()
            .flex()
            .items_center()
            .gap_1p5()
            .px_1p5()
            .py_0p5()
            .rounded_full()
            .cursor_pointer()
            .text_size(small)
            .text_color(colors.fg_muted)
            .hover(|pill| pill.bg(colors.hover))
            .tooltip(Tooltip::with_meta(tip, "⌘I"))
            .on_click(move |_, _, cx| root.update(cx, |root, cx| root.open_context(cx)))
            .when_some(share, |pill, share| {
                pill.child(ProgressRing::new("context-ring", share as f32).size(gpui::rems(0.9)))
                    .child(
                        div()
                            .when(full, |d| d.text_color(colors.warning))
                            .child(format_share(share)),
                    )
            })
            .when(share.is_none(), |pill| {
                pill.children(usage.map(|u| {
                    tf(
                        "{n} tokens",
                        &[(
                            "n",
                            &format_tokens(u.total.total.max(u.context_used.unwrap_or(0))),
                        )],
                    )
                }))
            })
            .when_some(speed, |pill, speed| {
                pill.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_0p5()
                        .child(Icon::new(IconName::Zap).size(IconSize::Xs).color(if live {
                            colors.accent
                        } else {
                            colors.fg_subtle
                        }))
                        .child(speed),
                )
            })
            .into_any_element(),
    )
}

/// One label and value on a line.
fn stat(label: impl Into<SharedString>, value: impl Into<SharedString>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .flex()
        .justify_between()
        .gap_4()
        .text_size(theme.text_size(TextSize::Sm))
        .child(div().text_color(theme.colors.fg_muted).child(label.into()))
        .child(div().child(value.into()))
        .into_any_element()
}

/// A big number with a caption, for the speed row.
fn tile(caption: &str, value: String, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_0p5()
        .p_3()
        .rounded(theme.radius(Radius::Md))
        .bg(theme.colors.sunken)
        .child(
            div()
                .text_size(theme.text_size(TextSize::Lg))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(value),
        )
        .child(
            div()
                .text_size(theme.text_size(TextSize::Xs))
                .text_color(theme.colors.fg_muted)
                .child(caption.to_string()),
        )
        .into_any_element()
}

fn section(title: &str, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .pt_1()
        .text_size(theme.text_size(TextSize::Xs))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.colors.fg_muted)
        .child(title.to_uppercase())
        .into_any_element()
}

/// What the last request put in the context: cached input, new input and
/// output, as one bar.
fn composition(last: &TokenCounts, window: u64, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let colors = &theme.colors;
    let parts: [(&str, u64, Hsla); 3] = [
        (t("Cached input"), last.cached_input, colors.chart[0]),
        (
            t("New input"),
            last.input.saturating_sub(last.cached_input),
            colors.chart[1],
        ),
        (t("Output"), last.output, colors.chart[2]),
    ];
    let mut bar = div()
        .w_full()
        .h(px(8.))
        .flex()
        .rounded_full()
        .overflow_hidden()
        .bg(colors.sunken);
    for (_, n, color) in parts {
        if n > 0 {
            bar = bar.child(
                div()
                    .h_full()
                    .w(relative((n as f64 / window as f64).min(1.) as f32))
                    .bg(color),
            );
        }
    }
    let legend = div().flex().flex_wrap().gap_x_4().gap_y_1().children(
        parts
            .into_iter()
            .filter(|(_, n, _)| *n > 0)
            .map(|(label, n, color)| {
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(theme.text_size(TextSize::Xs))
                    .text_color(colors.fg_muted)
                    .child(div().size(px(8.)).rounded_full().bg(color))
                    .child(format!("{label} {}", format_tokens(n)))
            }),
    );
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(bar)
        .child(legend)
        .into_any_element()
}

fn breakdown(counts: &TokenCounts, cx: &App) -> Vec<AnyElement> {
    let mut rows = Vec::new();
    if counts.input > 0 {
        let cached = if counts.cached_input > 0 {
            tf(
                " ({n} cached, {share}%)",
                &[
                    ("n", &format_tokens(counts.cached_input)),
                    (
                        "share",
                        &format!(
                            "{:.0}",
                            counts.cached_input as f64 * 100. / counts.input as f64
                        ),
                    ),
                ],
            )
        } else {
            String::new()
        };
        rows.push(stat(
            t("Input"),
            format!("{}{cached}", format_tokens(counts.input)),
            cx,
        ));
    }
    if counts.output > 0 {
        let reasoning = if counts.reasoning > 0 {
            tf(
                " ({n} reasoning)",
                &[("n", &format_tokens(counts.reasoning))],
            )
        } else {
            String::new()
        };
        rows.push(stat(
            t("Output"),
            format!("{}{reasoning}", format_tokens(counts.output)),
            cx,
        ));
    }
    if counts.total > 0 {
        rows.push(stat(t("Total"), format_tokens(counts.total), cx));
    }
    rows
}

/// A turn's first words, to tell turns apart in the table.
fn turn_label(transcript: &Transcript, turn: &str) -> String {
    let text = transcript
        .turn_prompt(turn)
        .map(|(text, _)| text)
        .unwrap_or_default();
    let line = text.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return t("(no message)").into();
    }
    let mut short: String = line.chars().take(28).collect();
    if line.chars().count() > 28 {
        short.push('…');
    }
    short
}

impl RootView {
    pub fn open_context(&mut self, cx: &mut Context<Self>) {
        self.overlay = Some(super::Overlay::Context);
        cx.notify();
    }

    pub(super) fn context_dialog(&self, cx: &mut Context<Self>) -> AnyElement {
        let close = cx.entity();
        let ws = self.workspace.read(cx);
        let codex = ws.is_codex_view();
        let agent = crate::agents::backend_name(ws.view_backend());
        let now = Instant::now();
        let mut body = div().w_full().flex().flex_col().gap_3();
        let mut nearly_full = false;

        match ws.transcript() {
            None => {
                body = body.child(
                    div()
                        .text_color(cx.theme().colors.fg_muted)
                        .child(t("Nothing measured yet: send a message first.")),
                );
            }
            Some(transcript) => {
                let usage = transcript.usage();
                // Context window.
                body = body.child(section(t("Context window"), cx));
                match usage.map(|u| (u, u.context_used, u.context_window)) {
                    Some((usage, Some(used), Some(window))) if window > 0 => {
                        let share = usage.context_share().unwrap_or(0.);
                        nearly_full = share >= NEARLY_FULL;
                        let theme = cx.theme();
                        body = body
                            .child(
                                div()
                                    .flex()
                                    .items_baseline()
                                    .justify_between()
                                    .child(
                                        div()
                                            .flex()
                                            .items_baseline()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_size(theme.text_size(TextSize::Xl))
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .child(format_tokens(used)),
                                            )
                                            .child(div().text_color(theme.colors.fg_muted).child(
                                                tf(
                                                    "of {n} tokens",
                                                    &[("n", &format_tokens(window))],
                                                ),
                                            )),
                                    )
                                    .child(
                                        div()
                                            .text_size(theme.text_size(TextSize::Sm))
                                            .text_color(if nearly_full {
                                                theme.colors.warning
                                            } else {
                                                theme.colors.fg_muted
                                            })
                                            .child(tf(
                                                "{share} used · {left} left",
                                                &[
                                                    ("share", &format_share(share)),
                                                    (
                                                        "left",
                                                        &format_tokens(window.saturating_sub(used)),
                                                    ),
                                                ],
                                            )),
                                    ),
                            )
                            .child(if usage.last.has_breakdown() {
                                composition(&usage.last, window, cx)
                            } else {
                                ProgressBar::new("context-bar", share as f32).into_any_element()
                            });
                        if nearly_full {
                            body = body.child(
                                div()
                                    .text_size(cx.theme().text_size(TextSize::Sm))
                                    .text_color(cx.theme().colors.warning)
                                    .child(if codex {
                                        t("The context is nearly full. Compact the conversation to make room; older details get summarized.")
                                    } else {
                                        t("The context is nearly full. Ask the agent to /compact, or start a new chat.")
                                    }),
                            );
                        }
                    }
                    Some((usage, used, _)) => {
                        body = body.child(stat(
                            t("In use"),
                            tf(
                                "{n} tokens (the window size isn't reported)",
                                &[("n", &format_tokens(used.unwrap_or(usage.last.total)))],
                            ),
                            cx,
                        ));
                    }
                    None => {
                        body = body.child(
                            div()
                                .text_size(cx.theme().text_size(TextSize::Sm))
                                .text_color(cx.theme().colors.fg_muted)
                                .child(tf(
                                    "{agent} hasn't reported its context yet.",
                                    &[("agent", &agent)],
                                )),
                        );
                    }
                }

                let mut table = None;
                let mut note = None;
                // Speed.
                let paces: Vec<(String, TurnPace)> = transcript.paces(now);
                if let Some((_, last)) = paces.last() {
                    let measured: Vec<f64> = paces
                        .iter()
                        .filter_map(|(_, p)| p.tokens_per_sec())
                        .collect();
                    let average = (!measured.is_empty())
                        .then(|| measured.iter().sum::<f64>() / measured.len() as f64);
                    body = body.child(section(t("Speed"), cx)).child(
                        div()
                            .flex()
                            .gap_2()
                            .child(tile(
                                if last.done {
                                    t("Last answer")
                                } else {
                                    t("Writing now")
                                },
                                format_speed(last).unwrap_or_else(|| "—".into()),
                                cx,
                            ))
                            .child(tile(
                                t("First token"),
                                last.first_token
                                    .map(format_wait)
                                    .unwrap_or_else(|| "—".into()),
                                cx,
                            ))
                            .child(tile(
                                t("Average"),
                                average
                                    .map(|r| tf("{rate} tok/s", &[("rate", &format!("{r:.0}"))]))
                                    .unwrap_or_else(|| "—".into()),
                                cx,
                            )),
                    );
                    table = Some(turn_table(transcript, &paces, cx));
                    note = Some(
                        div()
                            .text_size(cx.theme().text_size(TextSize::Xs))
                            .text_color(cx.theme().colors.fg_subtle)
                            .child(
                                t("Speed is measured on the text as it streams in; pauses for tools and new requests don't count. Counts marked ≈ are estimated from the text."),
                            )
                            .into_any_element(),
                    );
                }
                // Tokens.
                if let Some(usage) = usage {
                    // With one request so far, the last is the whole chat.
                    if usage.last.has_breakdown() && usage.last != usage.total {
                        body = body
                            .child(section(
                                if codex {
                                    t("Last request")
                                } else {
                                    t("Last turn")
                                },
                                cx,
                            ))
                            .children(breakdown(&usage.last, cx));
                    }
                    let total = breakdown(&usage.total, cx);
                    if !total.is_empty() || usage.cost.is_some() {
                        body = body.child(section(t("This chat"), cx)).children(total);
                        if let Some(cost) = &usage.cost {
                            let amount = if cost.currency == "USD" {
                                format!("${:.2}", cost.amount)
                            } else {
                                format!("{:.2} {}", cost.amount, cost.currency)
                            };
                            body = body.child(stat(t("Cost"), amount, cx));
                        }
                    }
                }

                body = body
                    .children(table.map(|turns| {
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(section(t("Recent turns"), cx))
                            .child(turns)
                    }))
                    .children(note);
            }
        }

        let compact = self.workspace.clone();
        let mut dialog = Dialog::new("context", t("Context & speed"), move |window, cx| {
            close.update(cx, |this, cx| this.close_overlay(window, cx))
        })
        .child(
            div()
                .id("context-body")
                .max_h(px(520.))
                .overflow_y_scroll()
                .pr_1()
                .child(body),
        );
        if ws.can_compact() {
            dialog = dialog.action(move |close| {
                let compact = compact.clone();
                let button = Button::new("context-compact", t("Compact conversation"))
                    .variant(ButtonVariant::Ghost)
                    .on_click(move |_, window, cx| {
                        compact.update(cx, |ws, cx| ws.compact(cx));
                        close(window, cx);
                    });
                if nearly_full {
                    button.primary()
                } else {
                    button
                }
            });
        }
        dialog
            .action(|close| {
                Button::new("context-close", t("Done"))
                    .on_click(move |_, window, cx| close(window, cx))
            })
            .into_any_element()
    }
}

/// The latest turns, newest first: what each wrote and how fast.
fn turn_table(transcript: &Transcript, paces: &[(String, TurnPace)], cx: &App) -> AnyElement {
    let theme = cx.theme();
    let colors = &theme.colors;
    let small = theme.text_size(TextSize::Xs);
    let cell = |w: f32, text: String| div().w(px(w)).flex_none().text_right().child(text);
    let header = div()
        .flex()
        .gap_2()
        .pb_1()
        .text_size(small)
        .text_color(colors.fg_subtle)
        .child(div().flex_1().min_w_0().child(t("Turn")))
        .child(cell(76., t("Output").into()))
        .child(cell(64., t("Speed").into()))
        .child(cell(52., t("First").into()))
        .child(cell(52., t("Took").into()));
    let rows = paces.iter().rev().take(8).map(|(turn, pace)| {
        let dash = || "—".to_string();
        div()
            .flex()
            .gap_2()
            .py_1()
            .border_t_1()
            .border_color(colors.border)
            .text_size(small)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(colors.fg_muted)
                    .child(turn_label(transcript, turn)),
            )
            .child(cell(
                76.,
                pace.tokens_out()
                    .map(|(n, exact)| {
                        format!("{}{}", if exact { "" } else { "≈" }, format_tokens(n))
                    })
                    .unwrap_or_else(dash),
            ))
            .child(cell(
                64.,
                pace.tokens_per_sec()
                    .map(|r| format!("{r:.0}/s"))
                    .unwrap_or_else(dash),
            ))
            .child(cell(
                52.,
                pace.first_token.map(format_wait).unwrap_or_else(dash),
            ))
            .child(cell(
                52.,
                if pace.done {
                    format_duration(pace.elapsed)
                } else {
                    format!(
                        "{}…",
                        format_duration(pace.elapsed.max(Duration::from_secs(1)))
                    )
                },
            ))
    });
    div()
        .flex()
        .flex_col()
        .child(header)
        .children(rows)
        .into_any_element()
}
