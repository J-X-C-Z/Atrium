//! One transcript row to one element. Pure drawing: every action goes back to
//! the [`Workspace`] through the entity handle.

use std::time::Duration;

use std::collections::HashMap;

use atrium_core::{
    ApprovalDecision, ApprovalKind, ChangeKind, Entry, EntryKind, FileChange, Item, ItemStatus,
    PlanStep, Transcript, TurnStatus, UserInputRequest,
};
use ely_gpui_component::{
    agent::{
        AgentPlan, ChangeState, FileChange as DiffChange, FileChangeCard, Permission,
        PermissionPrompt, ToolCallCard,
    },
    buttons::{Button, ButtonVariant, IconButton},
    chat::{
        ContextChips, ErrorMessage, MessageActions, MessageBubble, MessageFooter, Role, StepState,
        StreamingMarkdown, ThinkingBlock,
    },
    forms::{Input, TextInput},
    motion::{Skeleton, Spinner, SpinnerStyle, ease_out_cubic},
    primitives::{Icon, IconName},
    theme::{ActiveTheme, ControlSize, IconSize, Radius, TextSize},
};
use gpui::{
    Animation, AnimationExt, AnyElement, App, ElementId, Entity, IntoElement, ObjectFit,
    ParentElement, SharedString, Styled, StyledImage, Window, div, img, prelude::*,
    pulsating_between, px,
};

use crate::i18n::{t, tf};
use crate::settings::TranscriptMode;
use crate::workspace::{Workspace, is_image};

/// The reading column's width.
pub const COLUMN: f32 = 768.;

fn key(kind: &str, id: &str) -> ElementId {
    (ElementId::from("row"), format!("{kind}-{id}")).into()
}

fn step(status: ItemStatus) -> StepState {
    match status {
        ItemStatus::InProgress => StepState::Working,
        ItemStatus::Completed => StepState::Done,
        ItemStatus::Failed | ItemStatus::Declined => StepState::Failed,
    }
}

/// Centers `body` in the reading column, rising into place while `t` < 1.
pub fn column(body: impl IntoElement, t: f32, window: &mut Window) -> AnyElement {
    let inner = div().w_full().max_w(px(COLUMN)).child(body);
    let inner = if t < 1. {
        window.request_animation_frame();
        let eased = ease_out_cubic(t);
        inner.relative().top(px(10. * (1. - eased))).opacity(eased)
    } else {
        inner
    };
    div()
        .w_full()
        .flex()
        .justify_center()
        .px_6()
        .py_2()
        .child(inner)
        .into_any_element()
}

/// Like [`column`], with less room above and below, for the lines of a
/// folded run of steps.
fn column_tight(body: impl IntoElement, t: f32, window: &mut Window) -> AnyElement {
    let inner = div().w_full().max_w(px(COLUMN)).child(body);
    let inner = if t < 1. {
        window.request_animation_frame();
        inner.opacity(ease_out_cubic(t))
    } else {
        inner
    };
    div()
        .w_full()
        .flex()
        .justify_center()
        .px_6()
        .py_0p5()
        .child(inner)
        .into_any_element()
}

/// Draws row `ix` of the shown transcript. One row past the end is the
/// "thinking" placeholder while the agent has not answered yet.
pub fn row(
    workspace: &Entity<Workspace>,
    ix: usize,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let ws = workspace.read(cx);
    let thread = ws.active_thread_id().map(str::to_owned);
    let Some(transcript) = ws.transcript() else {
        return div().into_any_element();
    };
    let Some(entry) = transcript.entries().get(ix).cloned() else {
        let t = ws.entrance(ix.saturating_sub(1));
        return column(thinking(ws.running_for()), t.max(0.3), window);
    };
    let footer = footer(workspace, transcript, ix, &entry, cx);
    let t = ws.entrance(ix);
    // Steps in a row fold into one line; their rows show only when it is open.
    let folded = (ws.settings.transcript_mode == TranscriptMode::Normal)
        .then(|| super::steps::run_at(transcript.entries(), ix))
        .flatten();
    if let Some(run) = folded {
        let entries = transcript.entries().to_vec();
        let open = super::steps::is_open(ws, &entries, run);
        let header = (ix == run.start).then(|| super::steps::header(workspace, &entries, run, cx));
        let step = open.then(|| match &entry.kind {
            EntryKind::Item(item) => super::steps::line(
                workspace,
                item,
                || self::item(workspace, item, entry.live, cx),
                cx,
            ),
            _ => div()
                .pl_6()
                .child(entry_body(
                    workspace,
                    thread.as_deref(),
                    ix,
                    &entry,
                    window,
                    cx,
                ))
                .into_any_element(),
        });
        if header.is_none() && step.is_none() && footer.is_none() {
            return div().into_any_element();
        }
        let body = div()
            .flex()
            .flex_col()
            .gap_1()
            .children(header)
            .children(step)
            .children(footer);
        return column_tight(body, t, window);
    }
    let body = entry_body(workspace, thread.as_deref(), ix, &entry, window, cx);
    let body = div().flex().flex_col().gap_1().child(body).children(footer);
    column(body, t, window)
}

/// "Thinking…", pulsing, with how long it has been.
fn thinking(running: Option<Duration>) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            Spinner::new("thinking-spinner")
                .style(SpinnerStyle::Wave)
                .size(IconSize::Md),
        )
        .child(
            div()
                .child(match running {
                    Some(took) if took.as_secs() >= 2 => {
                        tf("Thinking · {secs}s", &[("secs", &took.as_secs())])
                    }
                    _ => t("Thinking").to_string(),
                })
                .with_animation(
                    "thinking-pulse",
                    Animation::new(Duration::from_millis(1400))
                        .repeat()
                        .with_easing(pulsating_between(0.45, 1.)),
                    |label, t| label.opacity(t),
                ),
        )
        .into_any_element()
}

/// Under the last row of a finished turn: how it went, copy and retry.
fn footer(
    workspace: &Entity<Workspace>,
    transcript: &Transcript,
    ix: usize,
    entry: &Entry,
    cx: &App,
) -> Option<AnyElement> {
    if !transcript.ends_turn(ix) {
        return None;
    }
    let turn = entry.turn_id.clone();
    let summary = transcript.turn_summary(&turn)?;
    let answer = transcript.turn_answer(&turn);
    let mut footer = MessageFooter::new();
    match summary.status {
        TurnStatus::Interrupted => footer = footer.fact(t("Stopped")),
        TurnStatus::Failed => footer = footer.fact(t("Failed")),
        _ => {}
    }
    let pace = transcript.turn_pace(&turn, std::time::Instant::now());
    match (summary.duration_ms, pace) {
        (Some(ms), _) => footer = footer.fact(format_duration(Duration::from_millis(ms))),
        (None, Some(pace)) if pace.done => footer = footer.fact(format_duration(pace.elapsed)),
        _ => {}
    }
    if let Some(pace) = &pace {
        if let Some(out) = super::context::format_out(pace) {
            footer = footer.fact(out);
        }
        if let Some(speed) = super::context::format_speed(pace) {
            footer = footer.fact(speed);
        }
    }
    let retry = workspace.clone();
    let can_retry = transcript.turn_prompt(&turn).is_some() && !transcript.is_running();
    let mut actions = MessageActions::new(key("actions", &turn), answer);
    if can_retry {
        let turn = turn.clone();
        actions =
            actions.on_regenerate(move |_, cx| retry.update(cx, |ws, cx| ws.retry(&turn, cx)));
    }
    let codex = workspace.read(cx).is_codex_view();
    let branch = workspace.clone();
    let from = turn.clone();
    // Your messages count the turns; turn `n` has checkpoint `n`.
    let number = transcript.entries()[..=ix]
        .iter()
        .filter(|e| matches!(e.kind, EntryKind::Item(Item::User { .. })))
        .count()
        .checked_sub(1);
    let undo = number
        .filter(|n| workspace.read(cx).has_checkpoint(*n))
        .map(|n| {
            let restore = workspace.clone();
            IconButton::new(key("undo-files", &turn), IconName::History)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Sm)
                .disabled(transcript.is_running())
                .tooltip(t("Undo this turn's file changes"))
                .on_click(move |_, _, cx| restore.update(cx, |ws, cx| ws.restore_turn(n, cx)))
        });
    Some(
        footer
            .child(actions)
            .children(undo)
            .when(codex, |footer| {
                footer.child(
                    IconButton::new(key("branch", &turn), IconName::GitBranch)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip(t("Branch into a new chat from here"))
                        .on_click(move |_, _, cx| {
                            let from = from.clone();
                            branch.update(cx, |ws, cx| ws.branch(Some(from), cx))
                        }),
                )
            })
            .into_any_element(),
    )
}

pub fn format_duration(took: Duration) -> String {
    let secs = took.as_secs();
    match secs {
        0 => format!("{:.1} s", took.as_secs_f32()),
        1..60 => format!("{secs} s"),
        _ => tf("{m}m {s}s", &[("m", &(secs / 60)), ("s", &(secs % 60))]),
    }
}

fn entry_body(
    workspace: &Entity<Workspace>,
    thread_id: Option<&str>,
    ix: usize,
    entry: &Entry,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    match &entry.kind {
        EntryKind::Item(Item::User { .. }) => {
            // Finished turns can be edited: the message comes back to the composer.
            let ws = workspace.read(cx);
            // Only the message that started a finished turn: steered ones joined it later.
            let starts_turn = ws.transcript().is_some_and(|t| {
                t.entries().iter().position(|e| {
                    e.turn_id == entry.turn_id
                        && matches!(e.kind, EntryKind::Item(Item::User { .. }))
                }) == Some(ix)
            });
            let editable = thread_id.is_some()
                && ws.is_codex_view()
                && starts_turn
                && !ws.is_running()
                && ws
                    .transcript()
                    .and_then(|t| t.turn_summary(&entry.turn_id))
                    .is_some();
            let editing = ws
                .editing
                .as_ref()
                .is_some_and(|(_, turn)| *turn == entry.turn_id);
            let body = self::item(workspace, &entry_item(entry), entry.live, cx);
            let edit = editable.then(|| {
                let (workspace, turn) = (workspace.clone(), entry.turn_id.clone());
                div().flex().justify_end().child(
                    IconButton::new(key("edit", &entry.turn_id), IconName::Pencil)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip(t("Edit and resend"))
                        .on_click(move |_, _, cx| {
                            workspace.update(cx, |ws, cx| {
                                ws.start_edit(&turn, cx);
                            })
                        }),
                )
            });
            let colors = cx.theme().colors.clone();
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .when(editing, |column| column.opacity(0.55))
                .child(body)
                .children(edit)
                .when(editing, |column| {
                    column.child(
                        div()
                            .flex()
                            .justify_end()
                            .text_xs()
                            .text_color(colors.accent)
                            .child(t("Editing: sending replaces this message and what follows")),
                    )
                })
                .into_any_element()
        }
        EntryKind::Item(item) => self::item(workspace, item, entry.live, cx),
        EntryKind::Question {
            request,
            answers,
            resolved,
        } => question(
            workspace,
            thread_id,
            request,
            answers.as_ref(),
            *resolved,
            window,
            cx,
        ),
        EntryKind::Approval {
            request,
            answer,
            resolved,
        } => {
            let agent = crate::agents::backend_name(
                workspace.read(cx).backend_of(thread_id.unwrap_or_default()),
            );
            let (title, detail) = match &request.kind {
                ApprovalKind::Command {
                    command,
                    cwd,
                    reason,
                } => (
                    reason.clone().unwrap_or_else(|| {
                        tf("{agent} wants to run a command", &[("agent", &agent)])
                    }),
                    if cwd.is_empty() {
                        format!("$ {command}")
                    } else {
                        tf(
                            "$ {command}\n  in {cwd}",
                            &[("command", command), ("cwd", cwd)],
                        )
                    },
                ),
                ApprovalKind::FileChange { reason, grant_root } => (
                    reason.clone().unwrap_or_else(|| {
                        tf("{agent} wants to change files", &[("agent", &agent)])
                    }),
                    match grant_root {
                        Some(root) => tf("Write access under {root}", &[("root", root)]),
                        None => t("Apply the proposed edits").into(),
                    },
                ),
                ApprovalKind::Tool { title, detail } => (
                    format!("{agent}: {title}"),
                    detail
                        .clone()
                        .unwrap_or_else(|| t("Allow this tool to run").into()),
                ),
            };
            match (answer, resolved) {
                (None, false) => {
                    let (workspace, thread, request_id) = (
                        workspace.clone(),
                        thread_id.unwrap_or_default().to_string(),
                        request.request_id.clone(),
                    );
                    PermissionPrompt::new(
                        key("approval", &request.request_id.to_string()),
                        title,
                        move |answer, _, cx| {
                            let decision = match answer {
                                Permission::Once => ApprovalDecision::Accept,
                                Permission::Always => ApprovalDecision::AcceptForSession,
                                Permission::Deny => ApprovalDecision::Decline,
                            };
                            workspace.update(cx, |ws, cx| {
                                ws.answer(&thread, request_id.clone(), decision, cx)
                            });
                        },
                    )
                    .detail(detail)
                    .into_any_element()
                }
                (answer, _) => {
                    let (said, icon, ok) = match answer {
                        Some(ApprovalDecision::Accept) => {
                            (t("Allowed once"), IconName::Check, true)
                        }
                        Some(ApprovalDecision::AcceptForSession) => {
                            (t("Allowed for this session"), IconName::Check, true)
                        }
                        Some(ApprovalDecision::Decline) => (t("Declined"), IconName::X, false),
                        Some(ApprovalDecision::Cancel) => {
                            (t("Declined and stopped"), IconName::X, false)
                        }
                        None => (t("No longer needed"), IconName::Check, true),
                    };
                    note(icon, format!("{said} · {}", first_line(&detail)), ok, cx)
                }
            }
        }
        EntryKind::Plan { explanation, steps } => {
            plan(&entry.turn_id, explanation.as_deref(), steps)
        }
        EntryKind::Error(message) => {
            let mut error =
                ErrorMessage::new(key("error", &ix.to_string()), t("Something went wrong"))
                    .detail(message.clone());
            let turn = entry.turn_id.clone();
            if !turn.is_empty() {
                let retry = workspace.clone();
                error = error.on_retry(move |_, cx| retry.update(cx, |ws, cx| ws.retry(&turn, cx)));
            }
            MessageBubble::new(Role::Error)
                .child(error)
                .into_any_element()
        }
    }
}

/// A quiet one-line note, centered, with an icon.
fn note(icon: IconName, text: String, ok: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    div()
        .flex()
        .justify_center()
        .child(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .px_3()
                .py_1()
                .rounded_full()
                .bg(colors.sunken)
                .text_size(theme.text_size(TextSize::Xs))
                .text_color(colors.fg_muted)
                .child(Icon::new(icon).size(IconSize::Xs).color(if ok {
                    colors.success
                } else {
                    colors.danger
                }))
                .child(text),
        )
        .into_any_element()
}

fn item(workspace: &Entity<Workspace>, item: &Item, live: bool, cx: &App) -> AnyElement {
    let mode = workspace.read(cx).settings.transcript_mode;
    let verbose = mode == TranscriptMode::Verbose;
    match item {
        Item::User {
            id,
            text,
            attachments,
        } => {
            let chips = (!attachments.is_empty()).then(|| {
                div().flex().justify_end().child(ContextChips::new(
                    key("attached", id),
                    attachments.iter().map(|path| {
                        let name = path.rsplit('/').next().unwrap_or(path).to_string();
                        let icon = if is_image(std::path::Path::new(path)) {
                            IconName::Image
                        } else {
                            IconName::FileText
                        };
                        (name, icon)
                    }),
                ))
            });
            div()
                .flex()
                .flex_col()
                .gap_1()
                .children(chips)
                .when(!text.is_empty(), |column| {
                    column.child(
                        MessageBubble::new(Role::User).child(
                            div()
                                .whitespace_normal()
                                .child(atrium_memory::inject::without_memory(text).to_string()),
                        ),
                    )
                })
                .into_any_element()
        }
        Item::Agent { id, text } => MessageBubble::new(Role::Assistant)
            .child(StreamingMarkdown::new(key("agent", id), text.clone(), live))
            .into_any_element(),
        Item::Reasoning { id, text } => {
            let took = workspace.read(cx).thought_for(id);
            let block = ThinkingBlock::new(key("thinking", id), text.clone(), live, took);
            match mode {
                TranscriptMode::Normal => block,
                _ => block.expanded(true),
            }
            .into_any_element()
        }
        Item::Command {
            id,
            command,
            cwd,
            status,
            output,
            exit_code,
            duration_ms,
        } => {
            let name = match status {
                ItemStatus::InProgress => t("Running"),
                ItemStatus::Declined => t("Declined"),
                ItemStatus::Failed => t("Failed"),
                ItemStatus::Completed => t("Ran"),
            };
            let mut card = ToolCallCard::new(key("cmd", id), name, step(*status))
                .summary(command.clone())
                .arguments(format!("$ {command}\n# in {cwd}"));
            let mut result = output.clone();
            if let Some(code) = exit_code.filter(|code| *code != 0) {
                result.push_str(&format!("\n[exit code {code}]"));
            }
            if !result.trim().is_empty() {
                card = card.result(result);
            }
            if let Some(ms) = duration_ms {
                card = card.took(Duration::from_millis(*ms));
            }
            if verbose {
                card = card.expanded(true);
            }
            card.into_any_element()
        }
        Item::FileChange {
            id,
            changes,
            status,
        } => {
            let state = match status {
                ItemStatus::InProgress => ChangeState::Proposed,
                ItemStatus::Completed => ChangeState::Accepted,
                ItemStatus::Failed | ItemStatus::Declined => ChangeState::Rejected,
            };
            let cwd = workspace
                .read(cx)
                .active_thread()
                .map(|t| t.cwd.clone())
                .unwrap_or_default();
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .children(changes.iter().enumerate().map(|(ix, change)| {
                    let card = FileChangeCard::new(
                        key("patch", &format!("{id}-{ix}")),
                        diff_change(change, state),
                    );
                    let path = std::path::Path::new(&cwd).join(&change.path);
                    if !super::preview::previewable(&path)
                        || matches!(change.kind, ChangeKind::Delete)
                    {
                        return card.into_any_element();
                    }
                    let ask = workspace.clone();
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(card)
                        .child(
                            div().child(
                                Button::new(key("preview", &format!("{id}-{ix}")), t("Preview"))
                                    .variant(ButtonVariant::Ghost)
                                    .size(ControlSize::Sm)
                                    .icon(IconName::Eye)
                                    .on_click(move |_, _, cx| {
                                        let path = path.clone();
                                        ask.update(cx, |ws, cx| {
                                            ws.preview_request = Some(path);
                                            cx.notify();
                                        })
                                    }),
                            ),
                        )
                        .into_any_element()
                }))
                .into_any_element()
        }
        Item::Tool {
            id,
            name,
            status,
            arguments,
            result,
            duration_ms,
        } => {
            let mut card = ToolCallCard::new(key("tool", id), name.clone(), step(*status));
            if !arguments.is_empty() {
                card = card.arguments(arguments.clone());
            }
            if !result.is_empty() {
                card = card.result(result.clone());
            }
            if let Some(ms) = duration_ms {
                card = card.took(Duration::from_millis(*ms));
            }
            if verbose {
                card = card.expanded(true);
            }
            card.into_any_element()
        }
        Item::WebSearch { id, query, detail } => {
            let mut card = ToolCallCard::new(
                key("search", id),
                if live {
                    t("Searching the web")
                } else {
                    t("Searched the web")
                },
                if live {
                    StepState::Working
                } else {
                    StepState::Done
                },
            )
            .summary(query.clone());
            if let Some(detail) = detail {
                card = card.result(detail.clone());
            }
            card.into_any_element()
        }
        Item::Image {
            id,
            status,
            path,
            prompt,
        } => image(id, *status, path.as_deref(), prompt.as_deref(), cx),
        Item::SubAgent {
            id,
            label,
            status,
            detail,
        } => {
            let mut card = ToolCallCard::new(key("agent-sub", id), label.clone(), step(*status));
            if let Some(detail) = detail {
                card = card.summary(detail.clone());
            }
            card.into_any_element()
        }
        Item::Other { label, .. } => note(IconName::Sparkles, label.clone(), true, cx),
    }
}

/// A unified diff as the before and after text a diff card compares.
/// Hunks are joined by a marker line so the card shows them apart.
fn split_diff(diff: &str) -> (String, String) {
    let mut old = Vec::new();
    let mut new = Vec::new();
    let mut hunks = 0;
    for line in diff.lines() {
        if line.starts_with("@@") {
            hunks += 1;
            if hunks > 1 {
                old.push("⋯".to_string());
                new.push("⋯".to_string());
            }
            continue;
        }
        if line.starts_with("---") || line.starts_with("+++") || line.starts_with("diff ") {
            continue;
        }
        match line.chars().next() {
            Some('+') => new.push(line[1..].to_string()),
            Some('-') => old.push(line[1..].to_string()),
            Some(' ') => {
                old.push(line[1..].to_string());
                new.push(line[1..].to_string());
            }
            Some('\\') => {}
            _ => {
                // A diff without prefixes (such as a whole new file's content).
                new.push(line.to_string());
            }
        }
    }
    (old.join("\n"), new.join("\n"))
}

fn diff_change(change: &FileChange, state: ChangeState) -> DiffChange {
    let (old, new) = match change.kind {
        ChangeKind::Add if !change.diff.lines().any(|l| l.starts_with("@@")) => {
            (String::new(), change.diff.clone())
        }
        ChangeKind::Delete if !change.diff.lines().any(|l| l.starts_with("@@")) => {
            (change.diff.clone(), String::new())
        }
        _ => split_diff(&change.diff),
    };
    let path = match &change.kind {
        ChangeKind::Update { move_to: Some(to) } => format!("{} → {to}", change.path),
        _ => change.path.clone(),
    };
    DiffChange {
        path: path.into(),
        old: old.into(),
        new: new.into(),
        state,
    }
}

fn plan(turn_id: &str, explanation: Option<&str>, steps: &[PlanStep]) -> AnyElement {
    let items: Vec<(String, StepState)> = steps
        .iter()
        .map(|step| {
            let state = if step.done {
                StepState::Done
            } else if step.active {
                StepState::Working
            } else {
                StepState::Waiting
            };
            (step.step.clone(), state)
        })
        .collect();
    let title = match explanation {
        Some(text) => tf("Plan · {text}", &[("text", &text)]),
        None => t("Plan").to_string(),
    };
    AgentPlan::new(key("plan", turn_id), title, items).into_any_element()
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
}

fn entry_item(entry: &Entry) -> Item {
    match &entry.kind {
        EntryKind::Item(item) => item.clone(),
        _ => unreachable!("only item rows reach here"),
    }
}

/// A generated image, or its placeholder while it is being made.
fn image(
    id: &str,
    status: ItemStatus,
    path: Option<&str>,
    prompt: Option<&str>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let radius = theme.radius(Radius::Lg);
    let picture = match (status, path) {
        (ItemStatus::Completed, Some(path)) => img(std::path::PathBuf::from(path))
            .w(px(360.))
            .max_w_full()
            .object_fit(ObjectFit::Contain)
            .rounded(radius)
            .into_any_element(),
        (ItemStatus::InProgress, _) => div()
            .relative()
            .child(
                Skeleton::new(key("image-wait", id))
                    .w(px(360.))
                    .h(px(225.))
                    .rounded(radius),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .text_sm()
                    .text_color(colors.fg_muted)
                    .child(
                        Spinner::new(key("image-spin", id))
                            .style(SpinnerStyle::Ring)
                            .size(IconSize::Sm),
                    )
                    .child(t("Creating image…")),
            )
            .into_any_element(),
        _ => {
            return note(
                IconName::Image,
                t("The image could not be made").into(),
                false,
                cx,
            );
        }
    };
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(picture)
        .children(prompt.map(|prompt| {
            div()
                .text_xs()
                .text_color(colors.fg_muted)
                .child(prompt.to_string())
        }))
        .into_any_element()
}

/// The agent's questions, with choices, free text where allowed, and send.
fn question(
    workspace: &Entity<Workspace>,
    thread_id: Option<&str>,
    request: &UserInputRequest,
    answers: Option<&Vec<(String, Vec<String>)>>,
    resolved: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let card_id = format!("question-{}", request.request_id);
    if answers.is_some() || resolved {
        let said = answers
            .map(|answers| {
                answers
                    .iter()
                    .map(|(_, given)| given.join(", "))
                    .collect::<Vec<_>>()
                    .join(" · ")
            })
            .filter(|said| !said.is_empty())
            .unwrap_or_else(|| t("No answer needed").into());
        return note(
            IconName::MessageSquare,
            tf("Answered: {said}", &[("said", &said)]),
            true,
            cx,
        );
    }
    let picked = window.use_keyed_state(
        SharedString::from(format!("{card_id}-picked")),
        cx,
        |_, _| HashMap::<String, String>::new(),
    );
    let fields: Vec<(String, Entity<TextInput>)> = request
        .questions
        .iter()
        .filter(|q| q.allow_other || q.options.is_empty())
        .map(|q| {
            let secret = q.secret;
            let field = window.use_keyed_state(
                SharedString::from(format!("{card_id}-{}", q.id)),
                cx,
                move |window, cx| {
                    let input = TextInput::new(window, cx).placeholder(t("Or type an answer"));
                    if secret { input.masked() } else { input }
                },
            );
            (q.id.clone(), field)
        })
        .collect();
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let chosen = picked.read(cx).clone();
    let typed: HashMap<String, String> = fields
        .iter()
        .map(|(id, field)| (id.clone(), field.read(cx).text().trim().to_string()))
        .collect();
    let answer_of = |id: &str| -> Option<String> {
        typed
            .get(id)
            .filter(|t| !t.is_empty())
            .cloned()
            .or_else(|| chosen.get(id).cloned())
    };
    let complete = request.questions.iter().all(|q| answer_of(&q.id).is_some());
    let answers: Vec<(String, Vec<String>)> = request
        .questions
        .iter()
        .filter_map(|q| answer_of(&q.id).map(|a| (q.id.clone(), vec![a])))
        .collect();

    let mut card = div()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded(theme.radius(Radius::Lg))
        .border_1()
        .border_color(colors.accent)
        .bg(colors.surface)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .text_color(colors.fg_muted)
                .child(
                    Icon::new(IconName::MessageSquare)
                        .size(IconSize::Sm)
                        .color(colors.accent),
                )
                .child(t("Codex has a question")),
        );
    for q in &request.questions {
        let field = fields
            .iter()
            .find(|(id, _)| *id == q.id)
            .map(|(_, f)| f.clone());
        let mut options = div().flex().flex_wrap().gap_2();
        for (label, description) in &q.options {
            let on = chosen.get(&q.id) == Some(label);
            let (picked, id, value) = (picked.clone(), q.id.clone(), label.clone());
            let tile = div()
                .id(key("opt", &format!("{card_id}-{}-{label}", q.id)))
                .flex()
                .flex_col()
                .gap_0p5()
                .px_3()
                .py_2()
                .min_w(px(140.))
                .rounded(theme.radius(Radius::Md))
                .border_1()
                .border_color(if on { colors.accent } else { colors.border })
                .bg(if on { colors.selection } else { colors.bg })
                .cursor_pointer()
                .hover(|tile| tile.bg(colors.hover))
                .on_click(move |_, _, cx| {
                    picked.update(cx, |picked, cx| {
                        picked.insert(id.clone(), value.clone());
                        cx.notify();
                    })
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .when(on, |row| {
                            row.child(
                                Icon::new(IconName::Check)
                                    .size(IconSize::Xs)
                                    .color(colors.accent),
                            )
                        })
                        .child(label.clone()),
                )
                .when(!description.is_empty(), |tile| {
                    tile.child(
                        div()
                            .text_xs()
                            .text_color(colors.fg_muted)
                            .child(description.clone()),
                    )
                });
            options = options.child(tile);
        }
        card = card.child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .when(!q.header.is_empty(), |column| {
                    column.child(
                        div()
                            .text_xs()
                            .text_color(colors.fg_subtle)
                            .child(q.header.clone()),
                    )
                })
                .child(
                    div()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(q.question.clone()),
                )
                .child(options)
                .children(field.map(|field| Input::new(&field))),
        );
    }
    let (workspace, thread, request_id) = (
        workspace.clone(),
        thread_id.unwrap_or_default().to_string(),
        request.request_id.clone(),
    );
    card.child(
        div().flex().justify_end().child(
            Button::new(key("answer", &card_id), t("Send answer"))
                .primary()
                .size(ControlSize::Sm)
                .disabled(!complete)
                .on_click(move |_, _, cx| {
                    let answers = answers.clone();
                    workspace.update(cx, |ws, cx| {
                        ws.answer_questions(&thread, request_id.clone(), answers, cx)
                    })
                }),
        ),
    )
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_unified_diff_splits_into_before_and_after() {
        let (old, new) = split_diff("@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n@@ -9 +9 @@\n-x\n+y\n");
        assert_eq!(old, "a\nb\nc\n⋯\nx");
        assert_eq!(new, "a\nB\nc\n⋯\ny");
    }

    #[test]
    fn durations_read_short() {
        assert_eq!(format_duration(Duration::from_millis(400)), "0.4 s");
        assert_eq!(format_duration(Duration::from_secs(42)), "42 s");
        assert_eq!(format_duration(Duration::from_secs(125)), "2m 5s");
    }
}
