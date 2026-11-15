//! One transcript entry to one element. Pure drawing: every action goes back
//! to the [`Workspace`] through the entity handle.

use agentdesk_core::{
    ApprovalDecision, ApprovalKind, ChangeKind, Entry, EntryKind, FileChange, Item, ItemStatus,
};
use ely_gpui_component::{
    agent::{Permission, PermissionPrompt, ToolCallCard},
    chat::{ErrorMessage, MessageBubble, Role, StepState, StreamingMarkdown, ThinkingBlock},
    primitives::{Icon, IconName},
    theme::{ActiveTheme, IconSize, Radius, TextSize},
};
use gpui::{
    AnyElement, App, ElementId, Entity, IntoElement, ParentElement, Styled, div, prelude::*,
};

use crate::workspace::Workspace;

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

/// Draws row `ix` of the transcript in `thread_id`.
pub fn entry(
    workspace: &Entity<Workspace>,
    thread_id: Option<&str>,
    ix: usize,
    entry: &Entry,
    cx: &App,
) -> AnyElement {
    let body: AnyElement = match &entry.kind {
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
                    let said = match answer {
                        Some(ApprovalDecision::Accept) => "Allowed once",
                        Some(ApprovalDecision::AcceptForSession) => "Allowed for this session",
                        Some(ApprovalDecision::Decline) => "Declined",
                        Some(ApprovalDecision::Cancel) => "Declined and stopped",
                        None => "No longer needed",
                    };
                    MessageBubble::new(Role::System)
                        .child(format!("{said} · {}", first_line(&detail)))
                        .into_any_element()
                }
            }
        }
        EntryKind::Plan { explanation, steps } => plan(explanation.as_deref(), steps, cx),
        EntryKind::Error(message) => MessageBubble::new(Role::Error)
            .child(
                ErrorMessage::new(key("error", &ix.to_string()), "Something went wrong")
                    .detail(message.clone()),
            )
            .into_any_element(),
    };
    div().w_full().px_6().py_2().child(body).into_any_element()
}

fn item(workspace: &Entity<Workspace>, item: &Item, live: bool, cx: &App) -> AnyElement {
    match item {
        Item::User { text, .. } => MessageBubble::new(Role::User)
            .child(div().whitespace_normal().child(text.clone()))
            .into_any_element(),
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
                ItemStatus::InProgress => "Running command",
                ItemStatus::Declined => "Command declined",
                ItemStatus::Failed => "Command failed",
                ItemStatus::Completed => "Ran command",
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
                card = card.took(std::time::Duration::from_millis(*ms));
            }
            card.into_any_element()
        }
        Item::FileChange {
            id,
            changes,
            status,
        } => {
            let name = match status {
                ItemStatus::InProgress => "Editing files",
                ItemStatus::Declined => "Edit declined",
                ItemStatus::Failed => "Edit failed",
                ItemStatus::Completed => "Edited files",
            };
            let summary = changes
                .iter()
                .map(change_label)
                .collect::<Vec<_>>()
                .join(", ");
            let diff = changes
                .iter()
                .map(|change| format!("--- {}\n{}", change.path, change.diff))
                .collect::<Vec<_>>()
                .join("\n");
            ToolCallCard::new(key("patch", id), name, step(*status))
                .summary(summary)
                .result(diff)
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
                card = card.took(std::time::Duration::from_millis(*ms));
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
        Item::Other { label, .. } => MessageBubble::new(Role::System)
            .child(label.clone())
            .into_any_element(),
    }
}

fn change_label(change: &FileChange) -> String {
    let name = change.path.rsplit('/').next().unwrap_or(&change.path);
    match &change.kind {
        ChangeKind::Add => format!("+{name}"),
        ChangeKind::Delete => format!("−{name}"),
        ChangeKind::Update { move_to: Some(to) } => {
            format!("{name} → {}", to.rsplit('/').next().unwrap_or(to))
        }
        ChangeKind::Update { move_to: None } => name.to_string(),
    }
}

fn plan(explanation: Option<&str>, steps: &[agentdesk_core::PlanStep], cx: &App) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded(theme.radius(Radius::Lg))
        .border_1()
        .border_color(colors.border)
        .bg(colors.surface)
        .text_size(theme.text_size(TextSize::Sm))
        .child(div().text_color(colors.fg_muted).child("Plan"))
        .children(explanation.map(|text| div().text_color(colors.fg_muted).child(text.to_string())))
        .children(steps.iter().map(|step| {
            let (icon, color) = if step.done {
                (IconName::Check, colors.success)
            } else if step.active {
                (IconName::Sparkles, colors.accent)
            } else {
                (IconName::Square, colors.fg_subtle)
            };
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(Icon::new(icon).size(IconSize::Sm).color(color))
                .child(
                    div()
                        .text_color(if step.done {
                            colors.fg_muted
                        } else {
                            colors.fg
                        })
                        .when(step.done, |text| text.line_through())
                        .child(step.step.clone()),
                )
        }))
        .into_any_element()
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
}
