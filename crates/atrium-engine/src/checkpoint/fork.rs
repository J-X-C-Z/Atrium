// Ported from apps/server/src/orchestration-v2/ThreadForkService.ts
// (T3 Code v0.1.11, MIT).
//!
//! Fork planning: which source runs may fork, what the target thread inherits,
//! and which context transfer carries the conversation forward. Copying the
//! source's native refs and opening a provider session belong to later items.

use serde_json::Value;
use thiserror::Error;

use crate::contracts::{
    Actor, AppThread, ContextSourcePoint, ContextTransfer, ContextTransferId,
    ContextTransferStatus, ContextTransferType, CreationSource, NativeRefStrength, ProviderThread,
    ProviderTurnTokenUsage, RelationshipToParent, Run, RunStatus, ThreadId,
};

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("could not plan fork {source_thread_id} -> {target_thread_id}: {cause}")]
pub struct ThreadForkPlanError {
    pub source_thread_id: ThreadId,
    pub target_thread_id: ThreadId,
    pub cause: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThreadForkPlan {
    pub target_thread: AppThread,
    pub transfer: ContextTransfer,
}

pub struct ThreadForkInput<'a> {
    pub source_thread: &'a AppThread,
    /// `None` only for a side chat whose parent has no stable run yet.
    pub source_run: Option<&'a Run>,
    pub source_provider_thread: Option<&'a ProviderThread>,
    pub source_token_usage: Option<ProviderTurnTokenUsage>,
    pub canonical_source_point: ContextSourcePoint,
    pub transfer_id: ContextTransferId,
    pub target_thread_id: ThreadId,
    pub title: Option<String>,
    pub side_chat: bool,
    pub created_by: Actor,
    pub creation_source: CreationSource,
    pub created_at: String,
}

/// `waiting` means provider-finished with checkpoint capture pending.
pub fn is_forkable_source_run_status(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Completed
            | RunStatus::Waiting
            | RunStatus::Failed
            | RunStatus::Interrupted
            | RunStatus::Cancelled
    )
}

pub fn forkable_source_run_status_error(run: &Run) -> String {
    format!(
        "Fork source run {} is {}; in-progress and rolled-back runs cannot be forked.",
        run.id,
        run_status_str(run.status)
    )
}

fn run_status_str(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Preparing => "preparing",
        RunStatus::Queued => "queued",
        RunStatus::Starting => "starting",
        RunStatus::Running => "running",
        RunStatus::Waiting => "waiting",
        RunStatus::Completed => "completed",
        RunStatus::Interrupted => "interrupted",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
        RunStatus::RolledBack => "rolled_back",
    }
}

/// T3's `ThreadForkService.plan`.
pub fn plan_fork(input: &ThreadForkInput<'_>) -> Result<ThreadForkPlan, ThreadForkPlanError> {
    let source = input.source_thread;
    let source_run = input.source_run;
    let failure = |cause: String| ThreadForkPlanError {
        source_thread_id: source.id.clone(),
        target_thread_id: input.target_thread_id.clone(),
        cause,
    };
    // A task a linked environment delegated here is a subagent of a remote thread.
    if input.side_chat
        && (source.lineage.side_chat == Some(true)
            || source.lineage.relationship_to_parent == Some(RelationshipToParent::Subagent)
            || source.delegated_from.is_some())
    {
        return Err(failure(if source.lineage.side_chat == Some(true) {
            "A side chat cannot start another side chat.".to_string()
        } else {
            "A subagent cannot start a side chat.".to_string()
        }));
    }
    if let Some(source_run) = source_run
        && !is_forkable_source_run_status(source_run.status)
    {
        return Err(failure(forkable_source_run_status_error(source_run)));
    }

    // The remote parent delegated the source thread, not its forks.
    let mut target_thread = source.clone();
    target_thread.delegated_from = None;
    // PR context follows the conversation; watching remains the source
    // thread's responsibility.
    target_thread.pull_requests = source.pull_requests.as_ref().map(|links| {
        links
            .iter()
            .map(|link| {
                let mut link = link.clone();
                if let Value::Object(object) = &mut link {
                    object.remove("watch");
                }
                link
            })
            .collect()
    });
    target_thread.created_by = input.created_by;
    target_thread.creation_source = input.creation_source;
    target_thread.id = input.target_thread_id.clone();
    target_thread.title = input
        .title
        .clone()
        .unwrap_or_else(|| format!("{} fork", source.title));
    // The source's title job and rename stay with the source thread.
    target_thread.title_regeneration = None;
    target_thread.title_renamed = Some(false);
    target_thread.active_provider_thread_id = None;
    target_thread.lineage = crate::contracts::AppThreadLineage {
        parent_thread_id: Some(source.id.clone()),
        relationship_to_parent: Some(RelationshipToParent::Fork),
        root_thread_id: source.lineage.root_thread_id.clone(),
        side_chat: input.side_chat.then_some(true),
    };
    target_thread.forked_from = source_run.map(|run| crate::contracts::ForkSource::Run {
        thread_id: source.id.clone(),
        run_id: run.id.clone(),
    });
    target_thread.created_at = input.created_at.clone();
    target_thread.updated_at = input.created_at.clone();
    target_thread.archived_at = None;
    target_thread.settled_override = None;
    target_thread.settled_at = None;
    target_thread.unsettled_at = None;
    target_thread.snoozed_until = None;
    target_thread.snoozed_at = None;
    target_thread.last_snooze_wake_at = None;
    target_thread.pinned_at = None;
    target_thread.pin_order_key = None;
    target_thread.active_order_key = None;
    target_thread.auto_settle_disabled_at = None;
    // A guest link belongs to the thread it was made for.
    target_thread.guest_share = None;
    target_thread.last_visited_at = None;
    // A fork is a new thread; it does not inherit how often the source was opened.
    target_thread.opens = None;
    target_thread.deleted_at = None;

    let strong_native_ref = input
        .source_provider_thread
        .and_then(|provider| provider.native_thread_ref.as_ref())
        .is_some_and(|native| native.strength == NativeRefStrength::Strong);
    let source_run_stable = source_run
        .is_some_and(|run| matches!(run.status, RunStatus::Completed | RunStatus::Waiting));
    let transfer = ContextTransfer {
        id: input.transfer_id.clone(),
        transfer_type: ContextTransferType::Fork,
        source_thread_id: source.id.clone(),
        target_thread_id: input.target_thread_id.clone(),
        source_point: input.canonical_source_point.clone(),
        base_point: None,
        source_provider_instance_id: Some(
            source_run
                .map(|run| run.provider_instance_id.clone())
                .unwrap_or_else(|| source.provider_instance_id.clone()),
        ),
        // Only native forks retain this context; portable handoffs rebuild it.
        source_token_usage: (source_run_stable && strong_native_ref)
            .then(|| input.source_token_usage.clone())
            .flatten(),
        target_provider_instance_id: None,
        target_run_id: None,
        status: ContextTransferStatus::Pending,
        resolution: None,
        created_by: input.created_by,
        error: (!strong_native_ref).then(|| {
            "Source provider thread does not expose a strong native thread ref.".to_string()
        }),
        created_at: input.created_at.clone(),
        updated_at: input.created_at.clone(),
        consumed_at: None,
    };

    Ok(ThreadForkPlan {
        target_thread,
        transfer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{ModelSelection, ProviderInstanceId, RunId};
    use serde_json::json;

    // Ported from apps/server/src/orchestration-v2/ThreadForkService.test.ts
    // (T3 Code v0.1.11, MIT).

    fn source_thread_id() -> ThreadId {
        ThreadId::new("thread:fork-snoozed-source")
    }
    fn target_thread_id() -> ThreadId {
        ThreadId::new("thread:fork-awake-target")
    }
    fn source_run_id() -> RunId {
        RunId::new("run:fork-snoozed-source")
    }
    fn provider_instance() -> ProviderInstanceId {
        ProviderInstanceId::new("codex")
    }

    fn source_thread_value() -> Value {
        json!({
            "createdBy": "user",
            "creationSource": "web",
            "id": "thread:fork-snoozed-source",
            "projectId": "project:fork-snooze",
            "title": "Snoozed source",
            "providerInstanceId": "codex",
            "modelSelection": { "instanceId": "codex", "model": "gpt-5.4" },
            "runtimeMode": "full-access",
            "interactionMode": "plan",
            "branch": "feature/source",
            "worktreePath": "/tmp/source-worktree",
            "activeProviderThreadId": "provider-thread:fork-snoozed-source",
            "lineage": {
                "parentThreadId": null,
                "relationshipToParent": null,
                "rootThreadId": "thread:fork-snoozed-source"
            },
            "forkedFrom": null,
            "createdAt": "2026-07-24T09:00:00.000Z",
            "updatedAt": "2026-07-24T09:05:00.000Z",
            "archivedAt": null,
            "settledOverride": null,
            "settledAt": null,
            "lastVisitedAt": null,
            "snoozedUntil": "2026-07-25T09:00:00.000Z",
            "snoozedAt": "2026-07-24T09:05:00.000Z",
            "deletedAt": null
        })
    }

    fn source_thread() -> AppThread {
        serde_json::from_value(source_thread_value()).unwrap()
    }

    fn source_run_value(status: &str) -> Value {
        json!({
            "id": "run:fork-snoozed-source",
            "threadId": "thread:fork-snoozed-source",
            "ordinal": 1,
            "providerInstanceId": "codex",
            "modelSelection": { "instanceId": "codex", "model": "gpt-5.4" },
            "providerThreadId": "provider-thread:fork-snoozed-source",
            "userMessageId": "message:fork-snoozed-source",
            "rootNodeId": null,
            "activeAttemptId": null,
            "status": status,
            "queuePosition": null,
            "requestedAt": "2026-07-24T09:00:00.000Z",
            "startedAt": "2026-07-24T09:00:00.000Z",
            "completedAt": "2026-07-24T09:05:00.000Z",
            "checkpointId": null,
            "contextHandoffId": null
        })
    }

    fn source_run(status: RunStatus) -> Run {
        let status = run_status_str(status);
        serde_json::from_value(source_run_value(status)).unwrap()
    }

    fn plan(
        thread: AppThread,
        run: Option<Run>,
        side_chat: bool,
    ) -> Result<ThreadForkPlan, ThreadForkPlanError> {
        let source_point = ContextSourcePoint {
            thread_id: source_thread_id(),
            run_id: run.as_ref().map(|run| run.id.clone()),
            checkpoint_id: None,
            turn_item_id: None,
            provider_thread_ref: None,
            provider_turn_ref: None,
        };
        plan_fork(&ThreadForkInput {
            source_thread: &thread,
            source_run: run.as_ref(),
            source_provider_thread: None,
            source_token_usage: None,
            canonical_source_point: source_point,
            transfer_id: ContextTransferId::new("context-transfer:fork-snoozed-source"),
            target_thread_id: target_thread_id(),
            title: Some("Awake fork".into()),
            side_chat,
            created_by: Actor::User,
            creation_source: CreationSource::Mobile,
            created_at: "2026-07-24T09:10:00.000Z".into(),
        })
    }

    #[test]
    fn resets_a_forks_pin_snooze_settle_and_delegation_state_but_keeps_its_origin() {
        let mut value = source_thread_value();
        value["lastSnoozeWakeAt"] = json!("2026-07-24T09:05:00.000Z");
        value["pinnedAt"] = json!("2026-07-24T09:05:00.000Z");
        value["pinOrderKey"] = json!("a0");
        value["activeOrderKey"] = json!("a1");
        value["autoSettleDisabledAt"] = json!("2026-07-24T09:05:00.000Z");
        value["delegatedFrom"] = json!({
            "environmentId": "linked-environment",
            "threadId": "linked-parent",
            "title": "Linked parent"
        });
        value["historyOrigin"] = json!("v1_import");
        value["linkOrigin"] = json!({
            "sessionId": "mcp-session:fork-source",
            "label": "Linked laptop"
        });
        value["tags"] = json!(["private"]);
        let thread: AppThread = serde_json::from_value(value).unwrap();
        let run = source_run(RunStatus::Completed);
        let result = plan(thread.clone(), Some(run.clone()), false).unwrap();

        assert!(result.target_thread.snoozed_until.is_none());
        assert!(result.target_thread.snoozed_at.is_none());
        assert!(result.target_thread.last_snooze_wake_at.is_none());
        assert!(result.target_thread.pinned_at.is_none());
        assert!(result.target_thread.pin_order_key.is_none());
        assert!(result.target_thread.active_order_key.is_none());
        assert!(result.target_thread.auto_settle_disabled_at.is_none());
        assert!(result.target_thread.delegated_from.is_none());
        assert_eq!(result.target_thread.history_origin, thread.history_origin);
        assert_eq!(result.target_thread.link_origin, thread.link_origin);
        assert_eq!(result.target_thread.tags, thread.tags);
        assert_eq!(result.target_thread.project_id, thread.project_id);
        assert_eq!(
            result.target_thread.provider_instance_id,
            provider_instance()
        );
        assert_eq!(
            result.target_thread.model_selection,
            ModelSelection::new(provider_instance(), "gpt-5.4")
        );
        assert_eq!(result.target_thread.runtime_mode, thread.runtime_mode);
        assert_eq!(
            result.target_thread.interaction_mode,
            thread.interaction_mode
        );
        assert_eq!(result.target_thread.branch, thread.branch);
        assert_eq!(result.target_thread.worktree_path, thread.worktree_path);
        assert!(result.target_thread.active_provider_thread_id.is_none());
        assert_eq!(
            result.target_thread.lineage,
            crate::contracts::AppThreadLineage {
                parent_thread_id: Some(source_thread_id()),
                relationship_to_parent: Some(RelationshipToParent::Fork),
                root_thread_id: source_thread_id(),
                side_chat: None,
            }
        );
        assert_eq!(
            result.target_thread.forked_from,
            Some(crate::contracts::ForkSource::Run {
                thread_id: source_thread_id(),
                run_id: source_run_id(),
            })
        );
    }

    #[test]
    fn starts_a_fork_without_the_source_threads_pending_title_regeneration() {
        let mut value = source_thread_value();
        value["titleRegeneration"] = json!({
            "requestId": "command:fork-source-title",
            "startedAt": "2026-07-24T09:05:00.000Z"
        });
        let thread: AppThread = serde_json::from_value(value).unwrap();
        assert!(thread.title_regeneration.is_some());
        let result = plan(
            thread.clone(),
            Some(source_run(RunStatus::Completed)),
            false,
        )
        .unwrap();
        assert!(result.target_thread.title_regeneration.is_none());
        assert!(thread.title_regeneration.is_some());
    }

    #[test]
    fn preserves_pull_request_context_without_inheriting_any_watches() {
        let watch = json!({
            "startedAt": "2026-07-24T09:00:00.000Z",
            "headSha": "source-head",
            "failedChecks": ["lint"],
            "passed": false,
            "passedChecks": [],
            "remarksThrough": "2026-07-24T09:05:00.000Z",
            "remarkIds": ["review-1"],
            "conflicting": true,
            "wakes": 2
        });
        let sources = ["agent", "stack", "manual", "stack-dismissed"];
        let link = |index: u32| {
            let source = sources[(index - 1) as usize];
            json!({
                "host": "github.com",
                "repository": "example/repo",
                "number": index,
                "url": format!("https://github.com/example/repo/pull/{index}"),
                "source": source,
                "linkedAt": "2026-07-24T09:00:00.000Z",
                "snapshot": {
                    "state": "open",
                    "title": format!("Pull request {index}"),
                    "headBranch": format!("feature-{index}"),
                    "baseBranch": "main",
                    "isDraft": false,
                    "updatedAt": null,
                    "syncedAt": "2026-07-24T09:05:00.000Z"
                }
            })
        };
        let mut links: Vec<Value> = (1..=4).map(link).collect();
        links[0]["watch"] = watch.clone();
        links[1]["watch"] = watch.clone();
        let legacy = json!({
            "projectId": "project:fork-snooze",
            "repository": "example/repo",
            "number": 1,
            "url": "https://github.com/example/repo/pull/1"
        });
        let mut value = source_thread_value();
        value["pullRequests"] = json!(links);
        value["linkedPullRequest"] = legacy.clone();
        value["branchPullRequest"] = legacy.clone();
        let thread: AppThread = serde_json::from_value(value).unwrap();
        let result = plan(
            thread.clone(),
            Some(source_run(RunStatus::Completed)),
            false,
        )
        .unwrap();

        let mut expected = links.clone();
        for link in &mut expected {
            link.as_object_mut().unwrap().remove("watch");
        }
        assert_eq!(result.target_thread.pull_requests, Some(expected));
        assert_eq!(
            serde_json::to_value(&result.target_thread.linked_pull_request).unwrap(),
            legacy
        );
        assert_eq!(result.target_thread.branch_pull_request, Some(legacy));
        assert_eq!(thread.pull_requests.as_ref().unwrap()[0]["watch"], watch);
        assert_eq!(thread.pull_requests.as_ref().unwrap()[1]["watch"], watch);
    }

    #[test]
    fn rejects_in_progress_and_rolled_back_fork_sources() {
        for status in [RunStatus::Running, RunStatus::RolledBack] {
            let run = source_run(status);
            let error = plan(source_thread(), Some(run.clone()), false).unwrap_err();
            assert_eq!(error.source_thread_id, source_thread_id());
            assert_eq!(error.target_thread_id, target_thread_id());
            assert_eq!(error.cause, forkable_source_run_status_error(&run));
        }
    }

    #[test]
    fn marks_only_requested_forks_as_side_chats_and_inherits_parent_permissions() {
        let run = source_run(RunStatus::Completed);
        let side_chat = plan(source_thread(), Some(run.clone()), true).unwrap();
        assert_eq!(side_chat.target_thread.lineage.side_chat, Some(true));
        assert_eq!(
            side_chat.target_thread.runtime_mode,
            crate::contracts::RuntimeMode::FullAccess
        );

        for runtime_mode in [
            "approval-required",
            "auto-accept-edits",
            "auto",
            "full-access",
        ] {
            let mut value = source_thread_value();
            value["runtimeMode"] = json!(runtime_mode);
            let thread: AppThread = serde_json::from_value(value).unwrap();
            let inherited = plan(thread, Some(run.clone()), true).unwrap();
            assert_eq!(
                serde_json::to_value(inherited.target_thread.runtime_mode).unwrap(),
                json!(runtime_mode)
            );
        }

        let mut side_chat_source = source_thread_value();
        side_chat_source["lineage"]["sideChat"] = json!(true);
        let thread: AppThread = serde_json::from_value(side_chat_source).unwrap();
        let fork_of_side_chat = plan(thread.clone(), Some(run.clone()), false).unwrap();
        assert!(fork_of_side_chat.target_thread.lineage.side_chat.is_none());

        let nested = plan(thread, Some(run.clone()), true).unwrap_err();
        assert_eq!(nested.cause, "A side chat cannot start another side chat.");

        let mut subagent_source = source_thread_value();
        subagent_source["lineage"]["relationshipToParent"] = json!("subagent");
        let thread: AppThread = serde_json::from_value(subagent_source).unwrap();
        let from_subagent = plan(thread, Some(run.clone()), true).unwrap_err();
        assert_eq!(from_subagent.cause, "A subagent cannot start a side chat.");

        let mut delegated_source = source_thread_value();
        delegated_source["delegatedFrom"] = json!({
            "environmentId": "linked-environment",
            "threadId": "linked-parent",
            "title": "Linked parent"
        });
        let thread: AppThread = serde_json::from_value(delegated_source).unwrap();
        let from_delegated = plan(thread, Some(run), true).unwrap_err();
        assert_eq!(from_delegated.cause, "A subagent cannot start a side chat.");
    }

    #[test]
    fn transfer_records_strong_native_context_only() {
        let thread = source_thread();
        let run = source_run(RunStatus::Completed);
        let provider_value = json!({
            "id": "provider-thread:fork-snoozed-source",
            "driver": "codex",
            "providerInstanceId": "codex",
            "providerSessionId": "provider-session:fork",
            "appThreadId": "thread:fork-snoozed-source",
            "ownerNodeId": null,
            "nativeThreadRef": {
                "driver": "codex",
                "nativeId": "native-thread",
                "strength": "strong"
            },
            "nativeConversationHeadRef": null,
            "status": "idle",
            "firstRunOrdinal": 1,
            "lastRunOrdinal": 1,
            "handoffIds": [],
            "forkedFrom": null,
            "pendingBackgroundTasks": [],
            "createdAt": "2026-07-24T09:00:00.000Z",
            "updatedAt": "2026-07-24T09:05:00.000Z"
        });
        let provider: ProviderThread = serde_json::from_value(provider_value).unwrap();
        let usage = ProviderTurnTokenUsage {
            used_tokens: 10,
            max_tokens: None,
            input_tokens: Some(10),
            cached_input_tokens: None,
            output_tokens: None,
            reasoning_output_tokens: None,
            prompt_cache_ttl_ms: None,
            auto_compact_threshold: None,
            prompt_cache_expires_at: None,
            updated_at: "2026-07-24T09:05:00.000Z".into(),
        };
        let source_point = ContextSourcePoint {
            thread_id: source_thread_id(),
            run_id: Some(source_run_id()),
            checkpoint_id: None,
            turn_item_id: None,
            provider_thread_ref: None,
            provider_turn_ref: None,
        };
        let result = plan_fork(&ThreadForkInput {
            source_thread: &thread,
            source_run: Some(&run),
            source_provider_thread: Some(&provider),
            source_token_usage: Some(usage.clone()),
            canonical_source_point: source_point,
            transfer_id: ContextTransferId::new("transfer"),
            target_thread_id: target_thread_id(),
            title: None,
            side_chat: false,
            created_by: Actor::User,
            creation_source: CreationSource::Web,
            created_at: "2026-07-24T09:10:00.000Z".into(),
        })
        .unwrap();
        assert_eq!(result.transfer.source_token_usage, Some(usage));
        assert!(result.transfer.error.is_none());
        assert_eq!(
            result.transfer.source_point.run_id.as_ref(),
            Some(&source_run_id())
        );
        assert_eq!(result.transfer.status, ContextTransferStatus::Pending);
        assert_eq!(result.transfer.transfer_type, ContextTransferType::Fork);
    }
}
