// Ported from apps/server/src/orchestration-v2/CheckpointRollbackService.ts
// (T3 Code v0.1.11, MIT).
//!
//! The validation half of "edit from here": reject a rollback before touching
//! the filesystem when the target is invalid, the provider changed, or the
//! provider turn is gone. The provider session and the event sink stay in the
//! orchestration layer.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::contracts::{
    AppThread, Checkpoint, CheckpointId, CheckpointScope, CheckpointScopeId, CheckpointStatus,
    ExecutionNode, ProviderThread, ProviderThreadId, ProviderTurn, Run, RunAttempt, RunAttemptId,
    ThreadId, latest_provider_turn_for_attempt,
};

use super::errors::CheckpointStoreError;

pub const SHARED_WORKSPACE_RESTORE_MESSAGE: &str = "File restore requires an isolated worktree. This workspace may contain changes from another thread or a side chat. Rewind the conversation without restoring files instead, or delete side chats you no longer need.";
pub const ROLLBACK_FAILED_MESSAGE: &str = "The provider could not roll back this conversation. Try again; if it keeps failing, check the provider and server logs.";
pub const GIT_INDEX_LOCKED_DETAIL: &str =
    "Git's index is locked. Wait for other Git operations to finish, then try again.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RollbackFailureReason {
    RollbackTargetInvalid,
    ActiveProviderChanged,
    ProviderTurnUnavailable,
    UnexpectedFailure,
    SharedWorkspace,
}

/// Why a rollback failed. Only a held Git index lock or a runtime-policy
/// ceiling gets its own message; the rest fall back to the generic one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RollbackCause {
    RuntimePolicyAboveCeiling(String),
    CheckpointRestore(CheckpointStoreError),
    Other(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct CheckpointRollbackExecutionError {
    pub reason: RollbackFailureReason,
    pub thread_id: ThreadId,
    pub provider_thread_id: ProviderThreadId,
    pub checkpoint_id: CheckpointId,
    pub cause: Option<RollbackCause>,
    message: String,
}

impl CheckpointRollbackExecutionError {
    pub fn new(
        reason: RollbackFailureReason,
        thread_id: ThreadId,
        provider_thread_id: ProviderThreadId,
        checkpoint_id: CheckpointId,
        cause: Option<RollbackCause>,
    ) -> Self {
        let message = match reason {
            RollbackFailureReason::RollbackTargetInvalid => format!(
                "Rollback target {checkpoint_id} for provider thread {provider_thread_id} on thread {thread_id} is incomplete or invalid."
            ),
            RollbackFailureReason::ActiveProviderChanged => format!(
                "Active provider changed before rollback target {checkpoint_id} could execute on thread {thread_id}."
            ),
            RollbackFailureReason::ProviderTurnUnavailable => format!(
                "Provider turn for rollback target {checkpoint_id} is unavailable on provider thread {provider_thread_id}."
            ),
            RollbackFailureReason::SharedWorkspace => SHARED_WORKSPACE_RESTORE_MESSAGE.to_string(),
            RollbackFailureReason::UnexpectedFailure => ROLLBACK_FAILED_MESSAGE.to_string(),
        };
        Self {
            reason,
            thread_id,
            provider_thread_id,
            checkpoint_id,
            cause,
            message,
        }
    }
}

/// The message clients see when a rollback fails.
pub fn rollback_failure_client_message(error: &CheckpointRollbackExecutionError) -> String {
    match &error.cause {
        Some(RollbackCause::RuntimePolicyAboveCeiling(message)) => message.clone(),
        Some(RollbackCause::CheckpointRestore(cause))
            if cause.detail() == Some(GIT_INDEX_LOCKED_DETAIL) =>
        {
            GIT_INDEX_LOCKED_DETAIL.to_string()
        }
        _ => ROLLBACK_FAILED_MESSAGE.to_string(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RollbackRequest {
    pub thread_id: ThreadId,
    pub provider_thread_id: ProviderThreadId,
    pub checkpoint_id: CheckpointId,
    pub scope_id: CheckpointScopeId,
    pub restore_files: Option<bool>,
}

/// T3's `getCheckpointRollbackContext`, supplied by the projection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CheckpointRollbackContext {
    pub thread: Option<AppThread>,
    pub provider_thread: Option<ProviderThread>,
    pub checkpoint: Option<Checkpoint>,
    pub scope: Option<CheckpointScope>,
    pub target_attempt: Option<RunAttempt>,
    pub provider_turns: Vec<ProviderTurn>,
    pub rolled_back_attempt_ids: Vec<RunAttemptId>,
    pub runs_to_rollback: Vec<Run>,
    pub root_nodes: Vec<ExecutionNode>,
    pub stale_checkpoints: Vec<Checkpoint>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RollbackProviderTarget {
    ThreadStart {
        checkpoint_id: CheckpointId,
        app_run_ordinal: u32,
    },
    ProviderTurn {
        checkpoint_id: CheckpointId,
        app_run_ordinal: u32,
        provider_turn: Box<ProviderTurn>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct RollbackPlan {
    pub restore_files: bool,
    pub target: RollbackProviderTarget,
    pub runs_to_rollback: Vec<Run>,
    pub root_nodes: Vec<ExecutionNode>,
    pub stale_checkpoints: Vec<Checkpoint>,
    pub rolled_back_attempt_ids: Vec<RunAttemptId>,
}

/// T3's `execute` validation, before any filesystem or provider work.
pub fn plan_rollback(
    request: &RollbackRequest,
    context: &CheckpointRollbackContext,
) -> Result<RollbackPlan, Box<CheckpointRollbackExecutionError>> {
    let failure = |reason, cause: Option<RollbackCause>| {
        Box::new(CheckpointRollbackExecutionError::new(
            reason,
            request.thread_id.clone(),
            request.provider_thread_id.clone(),
            request.checkpoint_id.clone(),
            cause,
        ))
    };
    let Some(thread) = context.thread.as_ref() else {
        return Err(failure(RollbackFailureReason::RollbackTargetInvalid, None));
    };
    let Some(provider_thread) = context.provider_thread.as_ref() else {
        return Err(failure(RollbackFailureReason::RollbackTargetInvalid, None));
    };
    let Some(checkpoint) = context.checkpoint.as_ref() else {
        return Err(failure(RollbackFailureReason::RollbackTargetInvalid, None));
    };
    let Some(scope) = context.scope.as_ref() else {
        return Err(failure(RollbackFailureReason::RollbackTargetInvalid, None));
    };
    let restore_files = request.restore_files != Some(false);
    if provider_thread.provider_session_id.is_none()
        || checkpoint.scope_id != scope.id
        // Missing snapshots support conversation rewind only.
        || (checkpoint.status != CheckpointStatus::Ready
            && !(!restore_files && checkpoint.status == CheckpointStatus::Missing))
    {
        return Err(failure(RollbackFailureReason::RollbackTargetInvalid, None));
    }
    if thread.active_provider_thread_id.as_ref() != Some(&provider_thread.id)
        || provider_thread.provider_instance_id != thread.model_selection.instance_id
    {
        return Err(failure(RollbackFailureReason::ActiveProviderChanged, None));
    }

    let target_ordinal = checkpoint.app_run_ordinal.unwrap_or(0);
    let target = if target_ordinal == 0 {
        RollbackProviderTarget::ThreadStart {
            checkpoint_id: checkpoint.id.clone(),
            app_run_ordinal: 0,
        }
    } else {
        let target_attempt = context.target_attempt.as_ref();
        let target_turn = latest_provider_turn_for_attempt(
            &context.provider_turns,
            target_attempt.map(|attempt| &attempt.id),
        )
        .or_else(|| {
            let provider_turn_id = target_attempt?.provider_turn_id.as_ref()?;
            context
                .provider_turns
                .iter()
                .find(|turn| &turn.id == provider_turn_id)
        });
        let Some(target_turn) = target_turn else {
            return Err(failure(
                RollbackFailureReason::ProviderTurnUnavailable,
                None,
            ));
        };
        if target_attempt.is_none() || target_turn.provider_thread_id != provider_thread.id {
            return Err(failure(
                RollbackFailureReason::ProviderTurnUnavailable,
                None,
            ));
        }
        RollbackProviderTarget::ProviderTurn {
            checkpoint_id: checkpoint.id.clone(),
            app_run_ordinal: target_ordinal,
            provider_turn: Box::new(target_turn.clone()),
        }
    };

    Ok(RollbackPlan {
        restore_files,
        target,
        runs_to_rollback: context.runs_to_rollback.clone(),
        root_nodes: context.root_nodes.clone(),
        stale_checkpoints: context.stale_checkpoints.clone(),
        rolled_back_attempt_ids: context.rolled_back_attempt_ids.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{Actor, CreationSource, ModelSelection, ProviderInstanceId};

    fn thread() -> AppThread {
        AppThread {
            created_by: Actor::User,
            creation_source: CreationSource::Web,
            id: ThreadId::new("thread:rollback"),
            project_id: crate::contracts::ProjectId::new("project:rollback"),
            title: "Rollback".into(),
            provider_instance_id: ProviderInstanceId::new("codex"),
            model_selection: ModelSelection::new(ProviderInstanceId::new("codex"), "gpt-5.4"),
            runtime_mode: crate::contracts::RuntimeMode::FullAccess,
            interaction_mode: crate::contracts::ProviderInteractionMode::Default,
            branch: None,
            worktree_path: None,
            linked_pull_request: None,
            pull_requests: None,
            branch_pull_request: None,
            issues: None,
            active_provider_thread_id: Some(ProviderThreadId::new("provider-thread:rollback")),
            history_origin: None,
            link_origin: None,
            delegated_from: None,
            lineage: crate::contracts::AppThreadLineage {
                parent_thread_id: None,
                relationship_to_parent: None,
                root_thread_id: ThreadId::new("thread:rollback"),
                side_chat: None,
            },
            forked_from: None,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
            archived_at: None,
            settled_override: None,
            settled_at: None,
            unsettled_at: None,
            snoozed_until: None,
            snoozed_at: None,
            last_snooze_wake_at: None,
            limit_recovery: None,
            guest_share: None,
            pinned_at: None,
            auto_settle_disabled_at: None,
            pin_order_key: None,
            active_order_key: None,
            tags: None,
            last_visited_at: None,
            opens: None,
            title_regeneration: None,
            last_title_regeneration: None,
            title_renamed: None,
            rollback_request_id: None,
            rollback_failure: None,
            deleted_at: None,
        }
    }

    fn provider_thread() -> ProviderThread {
        serde_json::from_value(serde_json::json!({
            "id": "provider-thread:rollback",
            "driver": "codex",
            "providerInstanceId": "codex",
            "providerSessionId": "provider-session:rollback",
            "appThreadId": "thread:rollback",
            "ownerNodeId": null,
            "nativeThreadRef": null,
            "nativeConversationHeadRef": null,
            "status": "idle",
            "firstRunOrdinal": 1,
            "lastRunOrdinal": 1,
            "handoffIds": [],
            "forkedFrom": null,
            "pendingBackgroundTasks": [],
            "createdAt": "2026-01-01T00:00:00.000Z",
            "updatedAt": "2026-01-01T00:00:00.000Z"
        }))
        .unwrap()
    }

    #[test]
    fn rejects_an_invalid_target_before_any_provider_or_file_work() {
        let mut context = CheckpointRollbackContext {
            thread: Some(thread()),
            provider_thread: Some(provider_thread()),
            ..Default::default()
        };
        let request = RollbackRequest {
            thread_id: ThreadId::new("thread:rollback"),
            provider_thread_id: ProviderThreadId::new("provider-thread:rollback"),
            checkpoint_id: CheckpointId::new("checkpoint:rollback"),
            scope_id: CheckpointScopeId::new("scope:rollback"),
            restore_files: None,
        };
        let error = plan_rollback(&request, &context).unwrap_err();
        assert_eq!(error.reason, RollbackFailureReason::RollbackTargetInvalid);

        context.checkpoint = Some(crate::contracts::Checkpoint {
            id: CheckpointId::new("checkpoint:rollback"),
            thread_id: ThreadId::new("thread:rollback"),
            scope_id: CheckpointScopeId::new("scope:rollback"),
            run_id: None,
            node_id: crate::contracts::NodeId::new("node:rollback"),
            parent_checkpoint_id: None,
            ordinal_within_scope: 0,
            app_run_ordinal: Some(0),
            checkpoint_ref: crate::contracts::CheckpointRef::new("refs/checkpoints/rollback"),
            status: CheckpointStatus::Ready,
            files: Vec::new(),
            files_total: None,
            captured_at: "2026-01-01T00:00:00.000Z".into(),
        });
        context.scope = Some(CheckpointScope {
            id: CheckpointScopeId::new("scope:rollback"),
            thread_id: ThreadId::new("thread:rollback"),
            run_id: None,
            node_id: crate::contracts::NodeId::new("node:rollback"),
            parent_scope_id: None,
            provider_thread_id: Some(ProviderThreadId::new("provider-thread:rollback")),
            kind: crate::contracts::CheckpointScopeKind::RootRun,
            ordinal_within_parent: 0,
            advances_app_run_count: true,
            cwd: "/repo".into(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
        });
        let plan = plan_rollback(&request, &context).unwrap();
        assert!(plan.restore_files);
        assert_eq!(
            plan.target,
            RollbackProviderTarget::ThreadStart {
                checkpoint_id: CheckpointId::new("checkpoint:rollback"),
                app_run_ordinal: 0,
            }
        );

        let mut missing = context.clone();
        missing.checkpoint.as_mut().unwrap().status = CheckpointStatus::Missing;
        assert_eq!(
            plan_rollback(&request, &missing).unwrap_err().reason,
            RollbackFailureReason::RollbackTargetInvalid,
        );
        let mut rewind_only = request;
        rewind_only.restore_files = Some(false);
        assert!(!plan_rollback(&rewind_only, &missing).unwrap().restore_files);
    }
}
