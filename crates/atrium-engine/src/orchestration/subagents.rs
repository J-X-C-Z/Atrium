// Ported from packages/provider-core/src/server/subagentProjection.ts (T3 Code v0.1.11, MIT).

//! Subagent and delegated-task bookkeeping.
//!
//! A subagent is a child thread under a parent turn. The pure helpers here
//! create the child thread, report progress for a delegated task, pick the
//! text a parent receives, and settle a child whose record ended without its
//! node (or the reverse). Provider processes are represented by the folded
//! [`Projection`]; no I/O happens here.

use crate::contracts::{
    Actor, AssistantMessageTurnItem, CreationSource, DelegatedCompletionTaskDeliveryState,
    ErrorTurnItem, EventPayload, ExecutionNodeStatus, ForkSource, ProviderFailure,
    ProviderFailureClass, ProviderInstanceId, ProviderThreadId, RelationshipToParent,
    SubagentOrigin, SystemNoticeLevel, SystemNoticeTurnItem, TurnItem, TurnItemStatus,
    UserMessageTurnItem,
};

use super::types::{
    DomainEvent, DomainEventBase, EventId, InputIntent, Message, MessageId, MessageRole, NodeId,
    NodeKind, Projection, ProviderTurn, ProviderTurnId, Run, RunId, RunStatus, Thread as AppThread,
    ThreadId, Timestamp, TurnItemBase, TurnItemId, iso, run_ran_after,
};

pub use crate::contracts::{Subagent, SubagentStatus};

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// The title a subagent child thread starts with, clipped to 72 characters.
pub fn subagent_thread_title(
    parent_title: &str,
    title: Option<&str>,
    prompt: &str,
    ordinal: u32,
) -> String {
    let Some(detail) = trimmed(title).or_else(|| trimmed(Some(prompt))) else {
        return format!("{parent_title} subagent {ordinal}");
    };
    if detail.chars().count() <= 72 {
        return detail.to_owned();
    }
    let mut clipped: String = detail.chars().take(69).collect();
    while clipped.len() > 69 {
        clipped.pop();
    }
    format!("{clipped}...")
}

/// Build the child thread a subagent runs in. It inherits workspace, runtime
/// mode and branch, but not settlement protections, snoozes, the title job,
/// pinning or a remote delegation.
#[allow(clippy::too_many_arguments)]
pub fn make_subagent_child_thread(
    parent_thread: &AppThread,
    child_thread_id: ThreadId,
    parent_node_id: NodeId,
    active_provider_thread_id: Option<ProviderThreadId>,
    provider_instance_id: ProviderInstanceId,
    model_selection: super::types::ModelSelection,
    title: String,
    now: Timestamp,
    created_by: Actor,
    creation_source: CreationSource,
) -> AppThread {
    let mut child = parent_thread.clone();
    child.created_by = created_by;
    child.creation_source = creation_source;
    child.id = child_thread_id;
    child.title = title;
    child.title_regeneration = None;
    child.title_renamed = Some(false);
    child.linked_pull_request = None;
    child.pull_requests = Some(Vec::new());
    child.history_origin = None;
    child.provider_instance_id = provider_instance_id;
    child.model_selection = model_selection;
    child.active_provider_thread_id = active_provider_thread_id;
    child.lineage = crate::contracts::AppThreadLineage {
        parent_thread_id: Some(parent_thread.id.clone()),
        relationship_to_parent: Some(RelationshipToParent::Subagent),
        root_thread_id: parent_thread.lineage.root_thread_id.clone(),
        side_chat: None,
    };
    child.forked_from = Some(ForkSource::Node {
        node_id: parent_node_id,
    });
    child.created_at = iso(now);
    child.updated_at = iso(now);
    child.archived_at = None;
    child.settled_override = None;
    child.settled_at = None;
    child.snoozed_until = None;
    child.snoozed_at = None;
    child.pinned_at = None;
    child.pin_order_key = None;
    child.auto_settle_disabled_at = None;
    // A guest link belongs to the thread it was made for.
    child.guest_share = None;
    child.last_visited_at = None;
    child.deleted_at = None;
    // The remote parent delegated the parent thread, not its subagents.
    child.delegated_from = None;
    child
}

/// The message and turn item a subagent's user or assistant text lands in.
#[allow(clippy::too_many_arguments)]
pub fn make_subagent_conversation_artifacts(
    message_id: MessageId,
    turn_item_id: TurnItemId,
    thread_id: ThreadId,
    root_node_id: NodeId,
    provider_thread_id: Option<ProviderThreadId>,
    provider_turn_id: Option<ProviderTurnId>,
    role: MessageRole,
    text: &str,
    ordinal: u32,
    now: Timestamp,
) -> (Message, TurnItem) {
    let message = Message {
        created_by: Actor::Agent,
        creation_source: CreationSource::Provider,
        id: message_id.clone(),
        thread_id: thread_id.clone(),
        run_id: None,
        node_id: Some(root_node_id.clone()),
        role,
        text: text.to_owned(),
        created_at: iso(now),
        updated_at: iso(now),
        ..Message::default()
    };
    let base = TurnItemBase {
        id: turn_item_id,
        thread_id,
        run_id: None,
        node_id: Some(root_node_id),
        provider_thread_id,
        provider_turn_id,
        parent_item_id: None,
        ordinal,
        status: TurnItemStatus::Completed,
        title: None,
        started_at: Some(iso(now)),
        completed_at: Some(iso(now)),
        updated_at: iso(now),
        ..TurnItemBase::default()
    };
    let turn_item = match role {
        MessageRole::User => TurnItem::UserMessage(UserMessageTurnItem {
            base,
            created_by: Actor::Agent,
            creation_source: CreationSource::Provider,
            message_id,
            input_intent: InputIntent::TurnStart,
            text: text.to_owned(),
            attachments: Vec::new(),
            scheduled_task_id: None,
            sender_thread_id: None,
            linked_sender: None,
            idle_compaction: None,
            workflow_script: None,
            fresh_context: None,
            text_omitted: None,
            context: None,
            model_switch: None,
        }),
        MessageRole::Assistant | MessageRole::System => {
            TurnItem::AssistantMessage(AssistantMessageTurnItem {
                base,
                phase: None,
                message_id,
                text: text.to_owned(),
                text_omitted: None,
                attachments: None,
                streaming: false,
            })
        }
    };
    (message, turn_item)
}

/// A delegated result, led by a note when the provider reported running
/// another model than requested.
pub fn with_executed_model_note(
    result: &str,
    requested_model: &str,
    executed_model: Option<&str>,
) -> String {
    match executed_model {
        Some(executed) if executed != requested_model => format!(
            "Requested model {requested_model}; the provider reported running {executed}.\n\n{result}"
        ),
        _ => result.to_owned(),
    }
}

/// Run signal of the notice that carries a provider's stop reason.
pub const PROVIDER_STOP_REASON_SIGNAL: &str = "provider-stop-reason";

pub fn run_signal_turn_item_id(run_id: &RunId, signal: &str) -> TurnItemId {
    TurnItemId::new(format!("turn-item:run-signal:{}:{signal}", run_id.as_str()))
}

fn stop_notice(projection: &Projection, run: &Run) -> Option<String> {
    let id = run_signal_turn_item_id(&run.id, PROVIDER_STOP_REASON_SIGNAL);
    projection.turn_items.iter().find_map(|item| {
        if item.id() != Some(&id) {
            return None;
        }
        match item {
            TurnItem::SystemNotice(notice) => Some(notice.message.clone()),
            _ => None,
        }
    })
}

fn monitor_update_run_ids(messages: &[Message]) -> std::collections::BTreeSet<&str> {
    messages
        .iter()
        .filter(|message| {
            message.notification.as_ref().is_some_and(|notification| {
                matches!(
                    notification.source,
                    crate::contracts::NotificationSource::Monitor { .. }
                ) && notification.outcome == crate::contracts::NotificationOutcome::Updated
            })
        })
        .filter_map(|message| message.run_id.as_ref().map(RunId::as_str))
        .collect()
}

/// The same task's completed runs before `run`, latest first: where its report
/// is when `run` completed without saying anything.
pub fn delegated_task_report_runs<'a>(projection: &'a Projection, run: &Run) -> Vec<&'a Run> {
    if run.status != RunStatus::Completed {
        return Vec::new();
    }
    let monitor_runs = monitor_update_run_ids(&projection.messages);
    let mut earlier: Vec<&Run> = projection
        .runs
        .iter()
        .filter(|candidate| {
            candidate.id != run.id
                && candidate.started_at.is_some()
                && candidate.status != RunStatus::RolledBack
                && !monitor_runs.contains(candidate.id.as_str())
                && run_ran_after(run, candidate)
        })
        .collect();
    earlier.sort_by(|left, right| {
        if run_ran_after(left, right) {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        }
    });
    let end = earlier.iter().position(|candidate| {
        candidate.status != RunStatus::Completed
            || candidate.delegated_task_id != run.delegated_task_id
    });
    match end {
        Some(index) => earlier[..index].to_vec(),
        None => earlier,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentResult {
    pub text: String,
    pub message_id: Option<MessageId>,
    pub turn_item_id: Option<TurnItemId>,
}

fn result_of_run(projection: &Projection, run: &Run) -> SubagentResult {
    let turn_item = projection
        .turn_items
        .iter()
        .filter_map(|item| match item {
            TurnItem::AssistantMessage(message)
                if message.base.run_id.as_ref() == Some(&run.id)
                    && !message.text.trim().is_empty() =>
            {
                Some(message)
            }
            _ => None,
        })
        .max_by_key(|message| message.base.ordinal);
    let messages: Vec<&Message> = projection
        .messages
        .iter()
        .filter(|message| {
            message.run_id.as_ref() == Some(&run.id)
                && message.role == MessageRole::Assistant
                && !message.text.trim().is_empty()
        })
        .collect();
    let message = turn_item
        .and_then(|turn| {
            messages
                .iter()
                .find(|message| message.id == turn.message_id)
        })
        .copied()
        .or_else(|| {
            messages.iter().copied().max_by(|left, right| {
                left.updated_at
                    .cmp(&right.updated_at)
                    .then_with(|| left.created_at.cmp(&right.created_at))
            })
        });
    let failure = (run.status == RunStatus::Failed)
        .then(|| {
            projection
                .turn_items
                .iter()
                .filter_map(|item| match item {
                    TurnItem::Error(error) if error.base.run_id.as_ref() == Some(&run.id) => {
                        Some(error)
                    }
                    _ => None,
                })
                .max_by_key(|error| error.base.ordinal)
        })
        .flatten();
    let report_before_failure = failure.and_then(|failure| {
        turn_item
            .filter(|turn| turn.base.ordinal < failure.base.ordinal)
            .map(|turn| message.map_or_else(|| turn.text.clone(), |message| message.text.clone()))
    });
    let text = match failure {
        Some(failure) => match report_before_failure {
            Some(report) => format!(
                "{}\n\nLast assistant message before the run failed:\n\n{report}",
                failure.failure.message
            ),
            None => failure.failure.message.clone(),
        },
        None => message
            .map(|message| message.text.clone())
            .or_else(|| turn_item.map(|turn| turn.text.clone()))
            .unwrap_or_else(|| {
                if run.status == RunStatus::Completed {
                    ["Child task completed without an assistant result.".to_owned()]
                        .into_iter()
                        .chain(stop_notice(projection, run))
                        .collect::<Vec<_>>()
                        .join("\n\n")
                } else {
                    format!("Child task ended with status {:?}.", run.status).to_lowercase()
                }
            }),
    };
    let executed_model =
        latest_provider_turn_for_attempt(projection, run).and_then(|turn| turn.model.as_deref());
    SubagentResult {
        text: with_executed_model_note(&text, &run.model_selection.model, executed_model),
        message_id: if failure.is_some() {
            None
        } else {
            message
                .map(|message| message.id.clone())
                .or_else(|| turn_item.map(|turn| turn.message_id.clone()))
        },
        turn_item_id: failure
            .map(|failure| failure.base.id.clone())
            .or_else(|| turn_item.map(|turn| turn.base.id.clone())),
    }
}

fn latest_provider_turn_for_attempt<'a>(
    projection: &'a Projection,
    run: &Run,
) -> Option<&'a ProviderTurn> {
    let attempt_id = run.active_attempt_id.as_ref()?;
    projection
        .provider_turns
        .iter()
        .filter(|turn| turn.run_attempt_id.as_ref() == Some(attempt_id))
        .max_by_key(|turn| turn.ordinal)
}

/// The text a parent receives for a finished subagent run.
pub fn subagent_result_for_run(projection: &Projection, run: &Run) -> SubagentResult {
    let result = result_of_run(projection, run);
    if run.status != RunStatus::Completed
        || result.message_id.is_some()
        || result.turn_item_id.is_some()
        || stop_notice(projection, run).is_some()
    {
        return result;
    }
    for earlier in delegated_task_report_runs(projection, run) {
        let report = result_of_run(projection, earlier);
        if report.message_id.is_some() || report.turn_item_id.is_some() {
            return report;
        }
    }
    result
}

fn subagent_holds_completion(subagent: &Subagent) -> bool {
    subagent.status.is_active()
        // Publishing a child's result precedes scheduling its parent's wake.
        || (subagent.status != SubagentStatus::Idle
            && subagent.completion_delivery.as_ref().is_some_and(|delivery| {
                matches!(
                    delivery.state,
                    DelegatedCompletionTaskDeliveryState::Pending
                        | DelegatedCompletionTaskDeliveryState::Claimed
                )
            }))
}

pub fn background_work_holds_completion(tasks: &[crate::contracts::PendingBackgroundTask]) -> bool {
    tasks.iter().any(|task| {
        let fields = task.fields();
        fields.awaited == Some(true) || task.kind() == "subagent"
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DelegatedTaskState {
    Working,
    WaitingForChildren,
    ResultAvailable,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DelegatedTaskProgress {
    pub state: DelegatedTaskState,
    pub result_run: Option<Run>,
}

/// Port of `delegatedTaskProgress`.
pub fn delegated_task_progress(projection: &Projection) -> DelegatedTaskProgress {
    let monitor_runs = monitor_update_run_ids(&projection.messages);
    let work_runs: Vec<&Run> = projection
        .runs
        .iter()
        .filter(|run| {
            !monitor_runs.contains(run.id.as_str()) && run.status != RunStatus::RolledBack
        })
        .collect();
    let active = work_runs.iter().any(|run| {
        !run.status.is_terminal()
            && !(run.status == RunStatus::Queued && run.queue_held.unwrap_or(false))
    });
    let children = projection.subagents.iter().any(subagent_holds_completion)
        || projection
            .provider_threads
            .iter()
            .any(|thread| background_work_holds_completion(&thread.pending_background_tasks));
    let mut terminal: Vec<&Run> = work_runs
        .iter()
        .copied()
        .filter(|run| run.status.is_terminal() && (run.started_at.is_some() || run.ordinal == 1))
        .collect();
    terminal.sort_by(|left, right| {
        if run_ran_after(left, right) {
            std::cmp::Ordering::Less
        } else if run_ran_after(right, left) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    });
    let result_run = terminal.first().copied().cloned();
    DelegatedTaskProgress {
        state: if active || result_run.is_none() {
            DelegatedTaskState::Working
        } else if children {
            DelegatedTaskState::WaitingForChildren
        } else {
            DelegatedTaskState::ResultAvailable
        },
        result_run,
    }
}

fn event_base(
    event_id: impl Into<String>,
    thread_id: &ThreadId,
    run_id: Option<&RunId>,
    node_id: Option<&NodeId>,
    provider_instance_id: Option<&ProviderInstanceId>,
    now: Timestamp,
) -> DomainEventBase {
    DomainEventBase {
        id: EventId::new(event_id.into()),
        thread_id: thread_id.clone(),
        run_id: run_id.cloned(),
        node_id: node_id.cloned(),
        driver: None,
        provider_instance_id: provider_instance_id.cloned(),
        raw_event_id: None,
        occurred_at: iso(now),
    }
}

fn written_node_ids(emitted: &[DomainEvent]) -> std::collections::BTreeSet<&str> {
    emitted
        .iter()
        .filter_map(|event| match event {
            DomainEvent::NodeUpdated(payload) => Some(payload.payload.id.as_str()),
            _ => None,
        })
        .collect()
}

fn written_turn_item_ids(emitted: &[DomainEvent]) -> std::collections::BTreeSet<String> {
    emitted
        .iter()
        .filter_map(|event| match event {
            DomainEvent::TurnItemUpdated(payload) => {
                payload.payload.id().map(|id| id.as_str().to_owned())
            }
            _ => None,
        })
        .collect()
}

/// End a provider-native subagent's node when only its record already ended.
pub fn end_orphaned_native_subagent(
    projection: &Projection,
    subagent_id: &NodeId,
    status: ExecutionNodeStatus,
    now: Timestamp,
    emitted: &[DomainEvent],
) -> Vec<DomainEvent> {
    let Some(subagent) = projection
        .subagents
        .iter()
        .find(|entry| entry.id == *subagent_id)
    else {
        return Vec::new();
    };
    if subagent.origin != SubagentOrigin::ProviderNative {
        return Vec::new();
    }
    let Some(node) = projection.nodes.iter().find(|node| node.id == *subagent_id) else {
        return Vec::new();
    };
    let already_written_slot = written_node_ids(emitted);
    if !is_work_active(node.status) || already_written_slot.contains(node.id.as_str()) {
        return Vec::new();
    }
    let mut updated = node.clone();
    updated.status = status;
    updated.completed_at = Some(iso(now));
    vec![DomainEvent::NodeUpdated(EventPayload {
        base: event_base(
            format!("event:subagent-node:{}", subagent.id),
            &subagent.thread_id,
            subagent.run_id.as_ref(),
            Some(&subagent.id),
            Some(&subagent.provider_instance_id),
            now,
        ),
        payload: updated,
    })]
}

/// End a root node that has no run (a runless native turn) and its open items.
pub fn end_runless_root_turns(
    projection: &Projection,
    thread_id: &ThreadId,
    provider_instance_id: &ProviderInstanceId,
    status: TurnItemStatus,
    now: Timestamp,
    emitted: &[DomainEvent],
) -> Vec<DomainEvent> {
    let mut events = Vec::new();
    for node in projection
        .nodes
        .iter()
        .filter(|node| node.run_id.is_none() && node.kind == NodeKind::RootTurn)
    {
        if written_node_ids(emitted).contains(node.id.as_str()) {
            continue;
        }
        let written_items = written_turn_item_ids(emitted);
        if is_work_active(node.status) && !written_node_ids(emitted).contains(node.id.as_str()) {
            let mut updated = node.clone();
            updated.status = if status == TurnItemStatus::Completed {
                ExecutionNodeStatus::Completed
            } else {
                ExecutionNodeStatus::Cancelled
            };
            updated.completed_at = Some(iso(now));
            events.push(DomainEvent::NodeUpdated(EventPayload {
                base: event_base(
                    format!("event:runless-node:{}", node.id),
                    thread_id,
                    None,
                    Some(&node.id),
                    Some(provider_instance_id),
                    now,
                ),
                payload: updated,
            }));
        }
        for (index, item) in projection
            .turn_items
            .iter()
            .filter(|item| {
                item.base().is_some_and(|base| {
                    base.run_id.is_none()
                        && base.node_id.as_ref() == Some(&node.id)
                        && matches!(
                            base.status,
                            TurnItemStatus::Pending
                                | TurnItemStatus::Running
                                | TurnItemStatus::Waiting
                        )
                })
            })
            .enumerate()
        {
            let Some(id) = item.id() else {
                continue;
            };
            if written_items.contains(id.as_str()) {
                continue;
            }
            let mut updated = item.clone();
            if let Some(base) = updated.base_mut() {
                base.status = status;
                base.completed_at = Some(iso(now));
                base.updated_at = iso(now);
            }
            events.push(DomainEvent::TurnItemUpdated(EventPayload {
                base: event_base(
                    format!("event:runless-item:{}:{index}", id.as_str()),
                    thread_id,
                    None,
                    Some(&node.id),
                    Some(provider_instance_id),
                    now,
                ),
                payload: updated,
            }));
        }
    }
    events
}

/// A failed run's error item, for callers that read [`subagent_result_for_run`].
pub fn failure_turn_item(base: TurnItemBase, message: &str) -> TurnItem {
    TurnItem::Error(ErrorTurnItem {
        base,
        failure: ProviderFailure {
            class: ProviderFailureClass::Unknown,
            message: message.to_owned(),
            code: None,
            retryable: Some(false),
            reset_at: None,
        },
        retry: None,
    })
}

/// A provider stop-reason notice, for callers that read
/// [`subagent_result_for_run`].
pub fn stop_reason_notice(base: TurnItemBase, message: &str) -> TurnItem {
    TurnItem::SystemNotice(SystemNoticeTurnItem {
        base,
        message: message.to_owned(),
        level: Some(SystemNoticeLevel::Warning),
    })
}

/// The default status a terminal path gives an open subagent.
pub fn settled_status(status: RunStatus) -> SubagentStatus {
    match status {
        RunStatus::Completed => SubagentStatus::Completed,
        RunStatus::Interrupted => SubagentStatus::Interrupted,
        RunStatus::Failed => SubagentStatus::Failed,
        RunStatus::Cancelled | RunStatus::RolledBack => SubagentStatus::Cancelled,
        RunStatus::Preparing
        | RunStatus::Queued
        | RunStatus::Starting
        | RunStatus::Running
        | RunStatus::Waiting => SubagentStatus::Running,
    }
}

/// The node status a terminal path gives open work.
pub fn settled_node_status(status: RunStatus) -> ExecutionNodeStatus {
    match status {
        RunStatus::Completed => ExecutionNodeStatus::Completed,
        RunStatus::Interrupted => ExecutionNodeStatus::Interrupted,
        RunStatus::Failed => ExecutionNodeStatus::Failed,
        RunStatus::Cancelled | RunStatus::RolledBack => ExecutionNodeStatus::Cancelled,
        RunStatus::Preparing
        | RunStatus::Queued
        | RunStatus::Starting
        | RunStatus::Running
        | RunStatus::Waiting => ExecutionNodeStatus::Running,
    }
}

/// A node is open while it still counts as work in progress.
pub fn is_work_active(status: ExecutionNodeStatus) -> bool {
    matches!(
        status,
        ExecutionNodeStatus::Pending | ExecutionNodeStatus::Running | ExecutionNodeStatus::Waiting
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{
        DelegatedCompletionDisposition, DelegatedCompletionTaskDelivery,
        DelegatedCompletionTaskDeliveryState, Notification, NotificationOutcome,
        NotificationSource, PendingBackgroundTask, PendingBackgroundTaskFields, ProviderSession,
        ProviderThread,
    };
    use crate::orchestration::types::{
        ExecutionNode, ModelSelection, ProviderThreadStatus, RunAttemptStatus,
    };

    fn parent_thread() -> AppThread {
        AppThread {
            id: "thread:subagent-snoozed-parent".into(),
            project_id: "project:subagent-snooze".into(),
            title: "Snoozed parent".into(),
            provider_instance_id: "codex".into(),
            model_selection: ModelSelection::new("codex", "gpt-5.4"),
            runtime_mode: crate::orchestration::types::RuntimeMode::FullAccess,
            interaction_mode: crate::orchestration::types::InteractionMode::Plan,
            branch: Some("feature/source".into()),
            worktree_path: Some("/tmp/source-worktree".into()),
            active_provider_thread_id: Some("provider-thread:parent".into()),
            lineage: crate::contracts::AppThreadLineage {
                parent_thread_id: None,
                relationship_to_parent: None,
                root_thread_id: "thread:subagent-snoozed-parent".into(),
                side_chat: None,
            },
            created_at: "2026-07-24T09:00:00Z".into(),
            updated_at: "2026-07-24T09:05:00Z".into(),
            snoozed_until: Some("2026-07-25T09:00:00Z".into()),
            snoozed_at: Some("2026-07-24T09:05:00Z".into()),
            history_origin: Some(crate::contracts::ThreadHistoryOrigin::V1Import),
            pinned_at: Some("2026-07-24T09:00:00Z".into()),
            pin_order_key: Some("a0".into()),
            auto_settle_disabled_at: Some("2026-07-24T09:00:00Z".into()),
            ..AppThread::default()
        }
    }

    #[test]
    fn resets_a_subagent_childs_snooze_title_job_and_remote_delegation_but_keeps_its_workspace() {
        let mut parent = parent_thread();
        parent.title_regeneration = Some(crate::orchestration::types::TitleRegeneration {
            request_id: "command:parent-title".into(),
            started_at: "2026-07-24T09:05:00Z".into(),
        });
        parent.delegated_from = Some(crate::contracts::DelegatedFrom {
            environment_id: "linked-environment".into(),
            thread_id: "linked-parent".into(),
            title: "Linked parent".into(),
        });
        let child = make_subagent_child_thread(
            &parent,
            "thread:subagent-awake-child".into(),
            "node:subagent-parent".into(),
            Some("provider-thread:child".into()),
            "claude".into(),
            ModelSelection::new("claude", "claude-opus-4-1"),
            "Awake child".into(),
            1_753_354_200_000,
            Actor::Agent,
            CreationSource::Provider,
        );
        assert!(child.snoozed_until.is_none());
        assert!(child.snoozed_at.is_none());
        assert!(child.title_regeneration.is_none());
        assert!(child.delegated_from.is_none());
        assert_eq!(child.project_id, parent.project_id);
        assert_eq!(child.runtime_mode, parent.runtime_mode);
        assert_eq!(child.interaction_mode, parent.interaction_mode);
        assert_eq!(child.branch, parent.branch);
        assert_eq!(child.worktree_path, parent.worktree_path);
        assert_eq!(child.provider_instance_id.as_str(), "claude");
        assert_eq!(
            child.model_selection,
            ModelSelection::new("claude", "claude-opus-4-1")
        );
        assert_eq!(
            child.active_provider_thread_id,
            Some("provider-thread:child".into())
        );
        assert!(child.history_origin.is_none());
        assert_eq!(
            child.lineage,
            crate::contracts::AppThreadLineage {
                parent_thread_id: Some("thread:subagent-snoozed-parent".into()),
                relationship_to_parent: Some(RelationshipToParent::Subagent),
                root_thread_id: "thread:subagent-snoozed-parent".into(),
                side_chat: None,
            }
        );
        assert_eq!(
            child.forked_from,
            Some(ForkSource::Node {
                node_id: "node:subagent-parent".into()
            })
        );
    }

    #[test]
    fn a_subagent_child_does_not_inherit_parent_settlement_protections() {
        let parent = parent_thread();
        let child = make_subagent_child_thread(
            &parent,
            "thread:child".into(),
            "node:parent".into(),
            None,
            "claude".into(),
            ModelSelection::new("claude", "claude-opus-4-1"),
            "Child".into(),
            1_753_354_200_000,
            Actor::Agent,
            CreationSource::Provider,
        );
        assert_eq!(parent.pinned_at, Some("2026-07-24T09:00:00Z".into()));
        assert_eq!(parent.pin_order_key.as_deref(), Some("a0"));
        assert!(child.pinned_at.is_none());
        assert!(child.pin_order_key.is_none());
        assert!(child.auto_settle_disabled_at.is_none());
    }

    fn run(id: &str, ordinal: u32, status: RunStatus, started: Option<&str>) -> Run {
        Run {
            id: id.into(),
            thread_id: "thread:parent".into(),
            ordinal,
            provider_instance_id: "codex".into(),
            model_selection: ModelSelection::new("codex", "gpt-5.4"),
            user_message_id: format!("message:{id}").into(),
            root_node_id: Some(format!("node:{id}").into()),
            active_attempt_id: Some(format!("attempt:{id}").into()),
            status,
            requested_at: "2026-07-24T09:00:00Z".into(),
            started_at: started.map(str::to_owned),
            completed_at: None,
            ..Run::default()
        }
    }

    fn with_completed(mut run: Run, completed_at: &str) -> Run {
        run.completed_at = Some(completed_at.to_owned());
        run
    }

    #[test]
    fn reports_the_run_that_ended_last_not_the_highest_ordinal() {
        let projection = Projection {
            runs: vec![
                with_completed(
                    run("run:4", 4, RunStatus::Completed, Some("x")),
                    "2026-07-24T10:00:00Z",
                ),
                with_completed(
                    run("run:2", 2, RunStatus::Completed, Some("x")),
                    "2026-07-24T10:05:00Z",
                ),
                with_completed(
                    run("run:3", 3, RunStatus::Completed, Some("x")),
                    "2026-07-24T10:10:00Z",
                ),
            ],
            ..Projection::default()
        };
        let progress = delegated_task_progress(&projection);
        assert_eq!(progress.state, DelegatedTaskState::ResultAvailable);
        assert_eq!(progress.result_run.unwrap().ordinal, 3);
    }

    #[test]
    fn waits_for_nested_work_and_retains_the_report_across_monitor_acknowledgements() {
        let mut subagent = Subagent {
            status: SubagentStatus::Running,
            ..Subagent::default()
        };
        let mut projection = Projection {
            runs: vec![run("run:1", 1, RunStatus::Completed, Some("x"))],
            subagents: vec![subagent.clone()],
            ..Projection::default()
        };
        assert_eq!(
            delegated_task_progress(&projection).state,
            DelegatedTaskState::WaitingForChildren
        );
        subagent.status = SubagentStatus::Completed;
        projection.subagents = vec![subagent.clone()];
        assert_eq!(
            delegated_task_progress(&projection).state,
            DelegatedTaskState::ResultAvailable
        );
        for state in [
            DelegatedCompletionTaskDeliveryState::Pending,
            DelegatedCompletionTaskDeliveryState::Claimed,
        ] {
            let mut task = subagent.clone();
            task.status = SubagentStatus::Idle;
            task.completion_delivery = Some(DelegatedCompletionTaskDelivery {
                state,
                observed_by_run_id: None,
            });
            projection.subagents = vec![task];
            assert_eq!(
                delegated_task_progress(&projection).state,
                DelegatedTaskState::ResultAvailable
            );
            projection.subagents[0].status = SubagentStatus::Completed;
            assert_eq!(
                delegated_task_progress(&projection).state,
                DelegatedTaskState::WaitingForChildren
            );
        }
        for state in [
            DelegatedCompletionTaskDeliveryState::Acknowledged,
            DelegatedCompletionTaskDeliveryState::Delivered,
            DelegatedCompletionTaskDeliveryState::Disposed,
        ] {
            let mut task = subagent.clone();
            task.completion_delivery = Some(DelegatedCompletionTaskDelivery {
                state,
                observed_by_run_id: None,
            });
            projection.subagents = vec![task];
            assert_eq!(
                delegated_task_progress(&projection).state,
                DelegatedTaskState::ResultAvailable
            );
        }
    }

    #[test]
    fn awaited_background_work_holds_completion() {
        let task = |kind: &str, awaited: Option<bool>| {
            PendingBackgroundTask::Command(PendingBackgroundTaskFields {
                task_id: kind.into(),
                awaited,
                ..PendingBackgroundTaskFields::default()
            })
        };
        let mut projection = Projection {
            runs: vec![run("run:1", 1, RunStatus::Completed, Some("x"))],
            provider_threads: vec![ProviderThread {
                pending_background_tasks: vec![
                    task("background-audit", Some(true)),
                    task("log-watcher", None),
                ],
                ..ProviderThread::default()
            }],
            ..Projection::default()
        };
        assert_eq!(
            delegated_task_progress(&projection).state,
            DelegatedTaskState::WaitingForChildren
        );
        projection.provider_threads[0].pending_background_tasks =
            vec![task("background-audit", None), task("log-watcher", None)];
        assert_eq!(
            delegated_task_progress(&projection).state,
            DelegatedTaskState::ResultAvailable
        );
    }

    #[test]
    fn a_queued_held_follow_up_is_not_working_and_the_last_cancelled_run_reports() {
        let projection = Projection {
            runs: vec![
                run("run:1", 1, RunStatus::Cancelled, None),
                Run {
                    queue_held: Some(true),
                    ..run("run:2", 2, RunStatus::Queued, None)
                },
            ],
            ..Projection::default()
        };
        let progress = delegated_task_progress(&projection);
        assert_eq!(progress.state, DelegatedTaskState::ResultAvailable);
        assert_eq!(progress.result_run.unwrap().id, RunId::new("run:1"));
    }

    fn assistant_item(run_id: &str, text: &str, ordinal: u32) -> TurnItem {
        let base = TurnItemBase {
            id: TurnItemId::new(format!("turn-item:{run_id}:{ordinal}")),
            thread_id: "thread:parent".into(),
            run_id: Some(run_id.into()),
            node_id: Some("node:root".into()),
            ordinal,
            status: TurnItemStatus::Completed,
            updated_at: "x".into(),
            ..TurnItemBase::default()
        };
        TurnItem::AssistantMessage(AssistantMessageTurnItem {
            base,
            phase: None,
            message_id: format!("message:{run_id}:{ordinal}").into(),
            text: text.into(),
            text_omitted: None,
            attachments: None,
            streaming: false,
        })
    }

    fn assistant_message(run_id: &str, text: &str, ordinal: u32) -> Message {
        Message {
            id: format!("message:{run_id}:{ordinal}").into(),
            thread_id: "thread:parent".into(),
            run_id: Some(run_id.into()),
            node_id: Some("node:root".into()),
            role: MessageRole::Assistant,
            text: text.into(),
            created_at: "x".into(),
            updated_at: "x".into(),
            ..Message::default()
        }
    }

    #[test]
    fn keeps_the_report_when_a_run_queued_behind_it_ends_without_assistant_text() {
        let first = with_completed(
            run("run:1", 1, RunStatus::Completed, Some("x")),
            "2026-07-24T10:00:00Z",
        );
        let queued = with_completed(
            run(
                "run:2",
                2,
                RunStatus::Completed,
                Some("2026-07-24T10:00:00Z"),
            ),
            "2026-07-24T10:00:01Z",
        );
        let projection = Projection {
            runs: vec![first, queued.clone()],
            messages: vec![
                assistant_message("run:1", "REPORT-OK", 1),
                Message {
                    id: "message:queued".into(),
                    role: MessageRole::User,
                    text: "ACK-SILENT".into(),
                    run_id: Some(queued.id.clone()),
                    ..Message::default()
                },
            ],
            turn_items: vec![assistant_item("run:1", "REPORT-OK", 1)],
            ..Projection::default()
        };
        let progress = delegated_task_progress(&projection);
        assert_eq!(progress.state, DelegatedTaskState::ResultAvailable);
        assert_eq!(
            subagent_result_for_run(&projection, progress.result_run.as_ref().unwrap()).text,
            "REPORT-OK"
        );
    }

    #[test]
    fn exposes_the_provider_failure_rather_than_a_progress_message_from_the_failed_run() {
        let run = run("run:1", 1, RunStatus::Failed, Some("x"));
        let failure = "You've hit your usage limit. Try again Sep 14 at 7:20 PM.";
        let projection = Projection {
            runs: vec![run.clone()],
            messages: vec![assistant_message("run:1", "Starting the audit.", 1)],
            turn_items: vec![failure_turn_item(
                TurnItemBase {
                    id: "turn-item:error".into(),
                    thread_id: "thread:parent".into(),
                    run_id: Some("run:1".into()),
                    node_id: Some("node:root".into()),
                    ordinal: 2,
                    status: TurnItemStatus::Completed,
                    updated_at: "x".into(),
                    ..TurnItemBase::default()
                },
                failure,
            )],
            ..Projection::default()
        };
        let result = subagent_result_for_run(&projection, &run);
        assert_eq!(result.text, failure);
        assert!(result.message_id.is_none());
        assert_eq!(result.turn_item_id, Some("turn-item:error".into()));
    }

    #[test]
    fn orders_a_tasks_assistant_messages_by_turn_item_not_by_timestamps() {
        let run = with_completed(
            run("run:1", 1, RunStatus::Completed, Some("x")),
            "2026-07-24T10:00:00Z",
        );
        let projection = Projection {
            runs: vec![run.clone()],
            messages: vec![
                assistant_message("run:1", "Starting step 0.", 1),
                assistant_message("run:1", "All steps passed.", 2),
            ],
            turn_items: vec![
                assistant_item("run:1", "All steps passed.", 2),
                assistant_item("run:1", "Starting step 0.", 1),
            ],
            ..Projection::default()
        };
        let result = subagent_result_for_run(&projection, &run);
        assert_eq!(result.text, "All steps passed.");
        assert_eq!(result.message_id, Some("message:run:1:2".into()));

        let projection = Projection {
            provider_turns: vec![ProviderTurn {
                id: "turn:1".into(),
                provider_thread_id: "provider-thread:parent".into(),
                node_id: "node:root".into(),
                run_attempt_id: Some("attempt:run:1".into()),
                ordinal: 1,
                status: super::super::types::ProviderTurnStatus::Completed,
                model: Some("gpt-5.4-mini".into()),
                ..ProviderTurn::default()
            }],
            ..projection
        };
        assert_eq!(
            subagent_result_for_run(&projection, &run).text,
            "Requested model gpt-5.4; the provider reported running gpt-5.4-mini.\n\nAll steps passed."
        );
    }

    #[test]
    fn reports_a_providers_stop_reason_for_a_completed_run_without_an_assistant_result() {
        let run = with_completed(
            run("run:1", 1, RunStatus::Completed, Some("x")),
            "2026-07-24T10:00:00Z",
        );
        let id = run_signal_turn_item_id(&run.id, PROVIDER_STOP_REASON_SIGNAL);
        let notice = "sessionStart hook (user): Plugin guard blocked this session.";
        let projection = Projection {
            runs: vec![run.clone()],
            turn_items: vec![stop_reason_notice(
                TurnItemBase {
                    id,
                    thread_id: "thread:parent".into(),
                    run_id: Some("run:1".into()),
                    node_id: Some("node:root".into()),
                    ordinal: 98,
                    status: TurnItemStatus::Completed,
                    updated_at: "x".into(),
                    ..TurnItemBase::default()
                },
                notice,
            )],
            ..Projection::default()
        };
        assert_eq!(
            subagent_result_for_run(&projection, &run).text,
            format!("Child task completed without an assistant result.\n\n{notice}")
        );
    }

    #[test]
    fn keeps_the_final_report_of_a_run_that_failed_after_its_turn_ended() {
        let run = run("run:1", 1, RunStatus::Failed, Some("x"));
        let projection = Projection {
            runs: vec![run.clone()],
            messages: vec![assistant_message(
                "run:1",
                "Review complete: no blocking issues.",
                1,
            )],
            turn_items: vec![
                assistant_item("run:1", "Review complete: no blocking issues.", 1),
                failure_turn_item(
                    TurnItemBase {
                        id: "turn-item:error".into(),
                        thread_id: "thread:parent".into(),
                        run_id: Some("run:1".into()),
                        node_id: Some("node:root".into()),
                        ordinal: 2,
                        status: TurnItemStatus::Completed,
                        updated_at: "x".into(),
                        ..TurnItemBase::default()
                    },
                    "Connection failed repeatedly",
                ),
            ],
            ..Projection::default()
        };
        let result = subagent_result_for_run(&projection, &run);
        assert!(result.text.contains("Connection failed repeatedly"));
        assert!(result.text.contains("Review complete: no blocking issues."));
    }

    #[test]
    fn ends_an_orphaned_native_subagents_node_when_only_its_record_already_ended() {
        let id: NodeId = "node:orphaned-native-subagent".into();
        let projection = Projection {
            subagents: vec![Subagent {
                id: id.clone(),
                thread_id: "thread:child".into(),
                origin: SubagentOrigin::ProviderNative,
                status: SubagentStatus::Running,
                provider_instance_id: "codex".into(),
                ..Subagent::default()
            }],
            nodes: vec![ExecutionNode {
                id: id.clone(),
                thread_id: "thread:child".into(),
                run_id: None,
                root_node_id: id.clone(),
                kind: NodeKind::RootTurn,
                status: ExecutionNodeStatus::Running,
                ..ExecutionNode::default()
            }],
            ..Projection::default()
        };
        let events = end_orphaned_native_subagent(
            &projection,
            &id,
            ExecutionNodeStatus::Cancelled,
            1_000,
            &[],
        );
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            DomainEvent::NodeUpdated(payload) if payload.payload.status == ExecutionNodeStatus::Cancelled
        ));
    }

    #[test]
    fn ends_a_runless_root_turns_items_when_its_node_already_ended() {
        let root: NodeId = "node:runless-root".into();
        let item = |id: &str| {
            crate::orchestration::types::TurnItem::Reasoning(crate::contracts::ReasoningTurnItem {
                base: TurnItemBase {
                    id: id.into(),
                    thread_id: "thread:child".into(),
                    run_id: None,
                    node_id: Some(root.clone()),
                    ordinal: 1,
                    status: TurnItemStatus::Running,
                    updated_at: "x".into(),
                    ..TurnItemBase::default()
                },
                text: "thinking".into(),
                streaming: true,
            })
        };
        let projection = Projection {
            nodes: vec![ExecutionNode {
                id: root.clone(),
                thread_id: "thread:child".into(),
                run_id: None,
                root_node_id: root.clone(),
                kind: NodeKind::RootTurn,
                status: ExecutionNodeStatus::Cancelled,
                ..ExecutionNode::default()
            }],
            turn_items: vec![item("turn-item:command"), item("turn-item:reasoning")],
            ..Projection::default()
        };
        let emitted = vec![DomainEvent::TurnItemUpdated(EventPayload {
            base: event_base(
                "event:command",
                &"thread:child".into(),
                None,
                Some(&root),
                None,
                1_000,
            ),
            payload: item("turn-item:command"),
        })];
        let events = end_runless_root_turns(
            &projection,
            &"thread:child".into(),
            &"codex".into(),
            TurnItemStatus::Cancelled,
            1_000,
            &emitted,
        );
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            DomainEvent::TurnItemUpdated(payload)
                if payload.payload.id().is_some_and(|id| id == "turn-item:reasoning")
        ));
    }

    #[test]
    fn background_work_holds_completion_for_a_subagent_even_without_awaited() {
        let tasks = vec![PendingBackgroundTask::Subagent(
            PendingBackgroundTaskFields {
                task_id: "child".into(),
                ..PendingBackgroundTaskFields::default()
            },
        )];
        assert!(background_work_holds_completion(&tasks));
        let _ = (
            ProviderSession::default(),
            ProviderThreadStatus::NotLoaded,
            RunAttemptStatus::Pending,
            DelegatedCompletionDisposition::Open,
            Notification {
                source: NotificationSource::Command,
                outcome: NotificationOutcome::Failed,
                summary: String::new(),
                detail: None,
            },
        );
    }
}
