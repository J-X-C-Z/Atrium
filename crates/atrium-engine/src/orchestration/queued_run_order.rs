// Ported from apps/server/src/orchestration-v2/QueuedRunOrder.ts (T3 Code v0.1.11, MIT).

//! The order queued runs reach the provider.
//!
//! Delegated completions jump the manual queue but not each other, and the two
//! streams interleave by request time so neither can hold the other back.

use super::types::{Projection, Run, RunStatus};

/// Whether this run delivers an automatic delegated completion.
pub fn is_automatic_completion_run(projection: &Projection, run: &Run) -> bool {
    projection
        .messages
        .iter()
        .any(|message| message.id == run.user_message_id && message.delegated_completion.is_some())
}

/// Queued runs in the order they should start: manual queue order, with
/// automatic completions interleaved by their request time.
pub fn queued_runs_in_delivery_order(projection: &Projection) -> Vec<Run> {
    let automatic_completion_message_ids: std::collections::BTreeSet<&str> = projection
        .messages
        .iter()
        .filter(|message| message.delegated_completion.is_some())
        .map(|message| message.id.as_str())
        .collect();
    let mut queued: Vec<Run> = projection
        .runs
        .iter()
        .filter(|run| run.status == RunStatus::Queued)
        .cloned()
        .collect();
    queued.sort_by(|left, right| {
        let left_position = left.queue_position.unwrap_or(left.ordinal);
        let right_position = right.queue_position.unwrap_or(right.ordinal);
        left_position
            .cmp(&right_position)
            .then_with(|| left.ordinal.cmp(&right.ordinal))
    });

    let (completions, messages): (Vec<Run>, Vec<Run>) = queued
        .into_iter()
        .partition(|run| automatic_completion_message_ids.contains(run.user_message_id.as_str()));

    // Completions and messages each keep their queue order and interleave by
    // request time, so neither kind can hold the other back while more keeps
    // arriving.
    let mut ordered = Vec::with_capacity(completions.len() + messages.len());
    let mut completion_index = 0;
    let mut message_index = 0;
    loop {
        match (
            completions.get(completion_index),
            messages.get(message_index),
        ) {
            (Some(completion), Some(message)) => {
                if completion.requested_at <= message.requested_at {
                    ordered.push(completion.clone());
                    completion_index += 1;
                } else {
                    ordered.push(message.clone());
                    message_index += 1;
                }
            }
            _ => {
                ordered.extend(completions[completion_index..].iter().cloned());
                ordered.extend(messages[message_index..].iter().cloned());
                return ordered;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::types::{
        DelegatedCompletion, Message, MessageRole, ModelSelection, RunAttemptId,
    };
    use crate::orchestration::types::{NodeId, iso};

    fn at(minute: i64) -> String {
        // 2026-01-01T10:00:00Z plus `minute` minutes.
        iso(1_767_261_600_000 + minute * 60_000)
    }

    fn run(id: &str, ordinal: u32, queue_position: u32, requested_at: String) -> Run {
        let message_id = id.replacen("run:", "message:", 1);
        Run {
            id: id.into(),
            thread_id: "thread:queue".into(),
            ordinal,
            provider_instance_id: "codex".into(),
            model_selection: ModelSelection::new("codex", "test"),
            provider_thread_id: Some("provider-thread:queue".into()),
            user_message_id: message_id.into(),
            root_node_id: Some(NodeId::new(format!("node:{id}"))),
            active_attempt_id: Some(RunAttemptId::new(format!("attempt:{id}"))),
            status: RunStatus::Queued,
            queue_position: Some(queue_position),
            queue_held: Some(false),
            steer_when_ready_run_id: None,
            requested_at,
            started_at: None,
            completed_at: None,
            restart_continuation_of_run_id: None,
            manual_continuation_of_run_id: None,
            usage_limit_continuation_of_run_id: None,
            fresh_context: Some(false),
            ..Run::default()
        }
    }

    fn message(id: &str, delegated_completion: bool) -> Message {
        Message {
            id: id.into(),
            thread_id: "thread:queue".into(),
            role: MessageRole::User,
            delegated_completion: delegated_completion.then(|| DelegatedCompletion {
                parent_run_id: "run:parent".into(),
                generation: 1,
                task_ids: vec!["task:child".into()],
            }),
            ..Message::default()
        }
    }

    /// T3 `QueuedRunOrder.test.ts`: completions and queued messages are
    /// delivered in request order while manual queue order is kept.
    #[test]
    fn delivers_completions_and_messages_in_request_order() {
        let projection = Projection {
            messages: vec![
                message("message:old", false),
                message("message:moved", false),
                message("message:earlier-completion", true),
                message("message:later-completion", true),
            ],
            runs: vec![
                run("run:old", 3, 2, at(0)),
                run("run:moved", 4, 1, at(2)),
                run("run:earlier-completion", 2, 3, at(-1)),
                run("run:later-completion", 5, 4, at(3)),
            ],
            ..Projection::default()
        };

        let ordered = queued_runs_in_delivery_order(&projection);
        let ids: Vec<&str> = ordered.iter().map(|run| run.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "run:earlier-completion",
                "run:moved",
                "run:old",
                "run:later-completion",
            ]
        );
    }

    #[test]
    fn identifies_automatic_completion_runs() {
        let projection = Projection {
            messages: vec![
                message("message:auto", true),
                message("message:user", false),
            ],
            runs: vec![run("run:auto", 1, 1, at(0)), run("run:user", 2, 2, at(1))],
            ..Projection::default()
        };
        assert!(is_automatic_completion_run(
            &projection,
            &projection.runs[0]
        ));
        assert!(!is_automatic_completion_run(
            &projection,
            &projection.runs[1]
        ));
    }
}
