// Ported from apps/server/src/orchestration-v2/CheckpointCaptureService.ts
// (T3 Code v0.1.11, MIT).
//!
//! The decision half of the checkpoint capture effect: whether a run still
//! needs a capture, which baselines it materializes, and which ordinals were
//! already preserved. Writing events is the orchestration layer's job.

use thiserror::Error;

use crate::contracts::{
    CheckpointScope, CheckpointScopeId, ExecutionNode, ProviderThread, Run, RunId, RunStatus,
    ThreadId,
};

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("checkpoint capture for thread {thread_id} run {run_id} scope {scope_id} failed: {cause}")]
pub struct CheckpointCaptureExecutionError {
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub scope_id: CheckpointScopeId,
    pub cause: String,
}

/// T3's `getCheckpointCaptureContext`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CheckpointCaptureContext {
    pub run: Option<Run>,
    pub root_node: Option<ExecutionNode>,
    pub scope: Option<CheckpointScope>,
    pub provider_thread: Option<ProviderThread>,
    pub preserved_checkpoint_ordinals: Vec<u32>,
    pub side_chat: bool,
}

/// The ids the capture effect was asked to run for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureTarget {
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub scope_id: CheckpointScopeId,
}

/// What the capture effect should materialize and record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturePlan {
    /// The thread-start baseline, materialized only for a run after the first.
    pub thread_start_baseline: Option<u32>,
    /// The baseline immediately before the turn.
    pub baseline: Option<u32>,
    pub checkpoint_ordinal: u32,
    pub app_run_ordinal: u32,
    pub side_chat: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureDecision {
    Skip,
    Capture(CapturePlan),
}

/// T3's `execute` decision, without the event sink or clock.
///
/// A completed or stopped run that already carries a checkpoint proves the
/// at-least-once effect committed; a rolled-back run is skipped so a capture
/// retry cannot overwrite the restored workspace.
pub fn decide_capture(
    target: &CaptureTarget,
    context: &CheckpointCaptureContext,
) -> Result<CaptureDecision, CheckpointCaptureExecutionError> {
    let run = context.run.as_ref();
    let stopped =
        run.is_some_and(|run| matches!(run.status, RunStatus::Interrupted | RunStatus::Cancelled));

    if let Some(run) = run
        && run.checkpoint_id.is_some()
        && (run.status == RunStatus::Completed || stopped)
    {
        return Ok(CaptureDecision::Skip);
    }
    if run.is_some_and(|run| run.status == RunStatus::RolledBack) {
        return Ok(CaptureDecision::Skip);
    }

    let Some(run) = run else {
        return Err(incomplete(target));
    };
    if run.status != RunStatus::Waiting && !stopped {
        return Err(incomplete(target));
    }
    let Some(root_node) = context.root_node.as_ref() else {
        return Err(incomplete(target));
    };
    let Some(scope) = context.scope.as_ref() else {
        return Err(incomplete(target));
    };
    if root_node.checkpoint_scope_id.as_ref() != Some(&scope.id)
        || context.provider_thread.is_none()
    {
        return Err(incomplete(target));
    }

    let baseline_ordinal_within_scope = run.ordinal.saturating_sub(1);
    let is_preserved = |ordinal: u32| context.preserved_checkpoint_ordinals.contains(&ordinal);
    // The baselines this capture materializes are attributed to the run
    // materializing them. The thread's scope may already be a later run's.
    let thread_start_baseline = if baseline_ordinal_within_scope == 0 || is_preserved(0) {
        None
    } else {
        Some(0)
    };
    let baseline =
        (!is_preserved(baseline_ordinal_within_scope)).then_some(baseline_ordinal_within_scope);

    Ok(CaptureDecision::Capture(CapturePlan {
        thread_start_baseline,
        baseline,
        checkpoint_ordinal: run.ordinal,
        app_run_ordinal: run.ordinal,
        side_chat: context.side_chat,
    }))
}

fn incomplete(target: &CaptureTarget) -> CheckpointCaptureExecutionError {
    CheckpointCaptureExecutionError {
        thread_id: target.thread_id.clone(),
        run_id: target.run_id.clone(),
        scope_id: target.scope_id.clone(),
        cause: "The persisted checkpoint capture target is incomplete or no longer waiting."
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    // Ported from apps/server/src/orchestration-v2/CheckpointCaptureService.test.ts
    // (T3 Code v0.1.11, MIT).
    use super::*;
    use serde_json::json;

    fn target() -> CaptureTarget {
        CaptureTarget {
            thread_id: ThreadId::new("thread:capture"),
            run_id: RunId::new("run:capture:1"),
            scope_id: CheckpointScopeId::new("scope:capture"),
        }
    }

    fn scope() -> CheckpointScope {
        serde_json::from_value(json!({
            "id": "scope:capture",
            "threadId": "thread:capture",
            "runId": "run:capture:1",
            "nodeId": "node:capture",
            "parentScopeId": null,
            "providerThreadId": "provider-thread:capture",
            "kind": "root_run",
            "ordinalWithinParent": 0,
            "advancesAppRunCount": true,
            "cwd": "/repo",
            "createdAt": "2026-01-01T00:00:00.000Z"
        }))
        .unwrap()
    }

    fn run(status: &str, ordinal: u32, checkpoint_id: Option<&str>) -> Run {
        let mut value = json!({
            "id": "run:capture:1",
            "threadId": "thread:capture",
            "ordinal": ordinal,
            "providerInstanceId": "codex",
            "modelSelection": { "instanceId": "codex", "model": "gpt-5.4" },
            "providerThreadId": "provider-thread:capture",
            "userMessageId": "message:capture",
            "rootNodeId": "node:capture",
            "activeAttemptId": null,
            "status": status,
            "requestedAt": "2026-01-01T00:00:00.000Z",
            "startedAt": null,
            "completedAt": null,
            "checkpointId": checkpoint_id,
            "contextHandoffId": null
        });
        if checkpoint_id.is_none() {
            value["checkpointId"] = serde_json::Value::Null;
        }
        serde_json::from_value(value).unwrap()
    }

    fn node() -> ExecutionNode {
        serde_json::from_value(json!({
            "id": "node:capture",
            "threadId": "thread:capture",
            "runId": "run:capture:1",
            "parentNodeId": null,
            "rootNodeId": "node:capture",
            "kind": "root_turn",
            "status": "waiting",
            "countsForRun": true,
            "providerThreadId": "provider-thread:capture",
            "providerTurnId": "provider-turn:capture",
            "nativeItemRef": null,
            "runtimeRequestId": null,
            "checkpointScopeId": "scope:capture",
            "startedAt": null,
            "completedAt": null
        }))
        .unwrap()
    }

    fn provider_thread() -> ProviderThread {
        serde_json::from_value(json!({
            "id": "provider-thread:capture",
            "driver": "codex",
            "providerInstanceId": "codex",
            "providerSessionId": "provider-session:capture",
            "appThreadId": "thread:capture",
            "ownerNodeId": null,
            "nativeThreadRef": null,
            "nativeConversationHeadRef": null,
            "status": "active",
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

    fn context(status: &str, ordinal: u32) -> CheckpointCaptureContext {
        CheckpointCaptureContext {
            run: Some(run(status, ordinal, None)),
            root_node: Some(node()),
            scope: Some(scope()),
            provider_thread: Some(provider_thread()),
            preserved_checkpoint_ordinals: Vec::new(),
            side_chat: false,
        }
    }

    #[test]
    fn skips_a_settled_run_that_already_captured_a_checkpoint() {
        for status in ["completed", "interrupted", "cancelled"] {
            let mut context = context(status, 1);
            context.run.as_mut().unwrap().checkpoint_id =
                Some(crate::contracts::CheckpointId::new("c"));
            assert_eq!(
                decide_capture(&target(), &context).unwrap(),
                CaptureDecision::Skip
            );
        }
        let mut rolled_back = context("waiting", 1);
        rolled_back.run.as_mut().unwrap().status = RunStatus::RolledBack;
        assert_eq!(
            decide_capture(&target(), &rolled_back).unwrap(),
            CaptureDecision::Skip
        );
    }

    #[test]
    fn captures_a_waiting_run_and_materializes_baselines() {
        let plan = decide_capture(&target(), &context("waiting", 1)).unwrap();
        assert_eq!(
            plan,
            CaptureDecision::Capture(CapturePlan {
                thread_start_baseline: None,
                baseline: Some(0),
                checkpoint_ordinal: 1,
                app_run_ordinal: 1,
                side_chat: false,
            })
        );
        let plan = decide_capture(&target(), &context("waiting", 2)).unwrap();
        assert_eq!(
            plan,
            CaptureDecision::Capture(CapturePlan {
                thread_start_baseline: Some(0),
                baseline: Some(1),
                checkpoint_ordinal: 2,
                app_run_ordinal: 2,
                side_chat: false,
            })
        );

        let mut preserved = context("waiting", 2);
        preserved.preserved_checkpoint_ordinals = vec![0, 1];
        assert_eq!(
            decide_capture(&target(), &preserved).unwrap(),
            CaptureDecision::Capture(CapturePlan {
                thread_start_baseline: None,
                baseline: None,
                checkpoint_ordinal: 2,
                app_run_ordinal: 2,
                side_chat: false,
            })
        );
    }

    #[test]
    fn rejects_an_incomplete_capture_target() {
        let mut missing_scope = context("waiting", 1);
        missing_scope.scope = None;
        let error = decide_capture(&target(), &missing_scope).unwrap_err();
        assert_eq!(error.thread_id, target().thread_id);
        assert_eq!(error.run_id, target().run_id);
        assert_eq!(error.scope_id, target().scope_id);
        assert!(error.cause.contains("incomplete"));

        let mut running = context("running", 1);
        running.run.as_mut().unwrap().status = RunStatus::Running;
        assert!(decide_capture(&target(), &running).is_err());
    }
}
