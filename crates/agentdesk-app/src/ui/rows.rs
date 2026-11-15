//! One transcript row to one element. Pure drawing: every action goes back to
//! the [`Workspace`] through the entity handle.

use std::time::Duration;

use agentdesk_core::{
    ApprovalDecision, ApprovalKind, ChangeKind, Entry, EntryKind, FileChange, Item, ItemStatus,
    PlanStep, Transcript, TurnStatus,
};
use ely_gpui_component::{
    agent::{
        AgentPlan, ChangeState, FileChange as DiffChange, FileChangeCard, Permission,
        PermissionPrompt, ToolCallCard,
    },
    chat::{
        ContextChips, ErrorMessage, MessageActions, MessageBubble, MessageFooter, Role, StepState,
        StreamingMarkdown, ThinkingBlock,
    },
    motion::{Spinner, SpinnerStyle, ease_out_cubic},
    primitives::{Icon, IconName},
    theme::{ActiveTheme, IconSize, TextSize},
};
use gpui::{
    Animation, AnimationExt, AnyElement, App, ElementId, Entity, IntoElement, ParentElement,
    Styled, Window, div, prelude::*, pulsating_between, px,
};

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
    let body = entry_body(workspace, thread.as_deref(), ix, &entry, cx);
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
                    Some(took) if took.as_secs() >= 2 => format!("Thinking · {}s", took.as_secs()),
                    _ => "Thinking".to_string(),
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
        TurnStatus::Interrupted => footer = footer.fact("Stopped"),
        TurnStatus::Failed => footer = footer.fact("Failed"),
        _ => {}
    }
    if let Some(ms) = summary.duration_ms {
        footer = footer.fact(format_duration(Duration::from_millis(ms)));
    }
    let retry = workspace.clone();
    let can_retry = transcript.turn_prompt(&turn).is_some() && !transcript.is_running();
    let mut actions = MessageActions::new(key("actions", &turn), answer);
    if can_retry {
        let turn = turn.clone();
        actions =
            actions.on_regenerate(move |_, cx| retry.update(cx, |ws, cx| ws.retry(&turn, cx)));
    }
    let _ = cx;
    Some(footer.child(actions).into_any_element())
}

pub fn format_duration(took: Duration) -> String {
    let secs = took.as_secs();
    match secs {
        0 => format!("{:.1} s", took.as_secs_f32()),
        1..60 => format!("{secs} s"),
        _ => format!("{}m {}s", secs / 60, secs % 60),
    }
}

fn entry_body(
    workspace: &Entity<Workspace>,
    thread_id: Option<&str>,
    ix: usize,
    entry: &Entry,
    cx: &App,
) -> AnyElement {
    match &entry.kind {
        EntryKind::Item(item) => self::item(workspace, item, entry.live, cx),
        EntryKind::Approval {
            request,
            answer,
            resolved,
        } => {
            let (title, detail) = match &request.kind {
                ApprovalKind::Command {
                    command,
                    cwd,
                    reason,
                } => (
                    reason
                        .clone()
                        .unwrap_or_else(|| "Codex wants to run a command".into()),
                    format!("$ {command}\n  in {cwd}"),
                ),
                ApprovalKind::FileChange { reason, grant_root } => (
                    reason
                        .clone()
                        .unwrap_or_else(|| "Codex wants to change files".into()),
                    match grant_root {
                        Some(root) => format!("Write access under {root}"),
                        None => "Apply the proposed edits".into(),
                    },
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
                        Some(ApprovalDecision::Accept) => ("Allowed once", IconName::Check, true),
                        Some(ApprovalDecision::AcceptForSession) => {
                            ("Allowed for this session", IconName::Check, true)
                        }
                        Some(ApprovalDecision::Decline) => ("Declined", IconName::X, false),
                        Some(ApprovalDecision::Cancel) => {
                            ("Declined and stopped", IconName::X, false)
                        }
                        None => ("No longer needed", IconName::Check, true),
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
                ErrorMessage::new(key("error", &ix.to_string()), "Something went wrong")
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
                        MessageBubble::new(Role::User)
                            .child(div().whitespace_normal().child(text.clone())),
                    )
                })
                .into_any_element()
        }
        Item::Agent { id, text } => MessageBubble::new(Role::Assistant)
            .child(StreamingMarkdown::new(key("agent", id), text.clone(), live))
            .into_any_element(),
        Item::Reasoning { id, text } => {
            let took = workspace.read(cx).thought_for(id);
            ThinkingBlock::new(key("thinking", id), text.clone(), live, took).into_any_element()
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
                ItemStatus::InProgress => "Running",
                ItemStatus::Declined => "Declined",
                ItemStatus::Failed => "Failed",
                ItemStatus::Completed => "Ran",
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
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .children(changes.iter().enumerate().map(|(ix, change)| {
                    FileChangeCard::new(
                        key("patch", &format!("{id}-{ix}")),
                        diff_change(change, state),
                    )
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
            card.into_any_element()
        }
        Item::WebSearch { id, query } => ToolCallCard::new(
            key("search", id),
            "Searched the web",
            if live {
                StepState::Working
            } else {
                StepState::Done
            },
        )
        .summary(query.clone())
        .into_any_element(),
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
        Some(text) => format!("Plan · {text}"),
        None => "Plan".to_string(),
    };
    AgentPlan::new(key("plan", turn_id), title, items).into_any_element()
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
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
