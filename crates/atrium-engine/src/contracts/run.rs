// Ported from packages/contracts/src/orchestrationV2.ts (T3 Code v0.1.11, MIT).
//! Runs, run attempts and execution nodes.
//!
//! A run is one user turn's work; attempts retry it; nodes are the tree the
//! turn items hang on. These are the records the event store projects and the
//! scheduler waits on.

use serde::{Deserialize, Serialize};

use super::base::{
    CheckpointId, CheckpointScopeId, ContextHandoffId, MessageId, NodeId, PlanId,
    ProviderInstanceId, ProviderThreadId, ProviderTurnId, RunAttemptId, RunId, RuntimeRequestId,
    ThreadId,
};
use super::model::ModelSelection;
use super::provider::ProviderRef;

pub const ORCHESTRATION_V2_WORKSPACE_PREPARATION_FAILURE_CODE: &str =
    "workspace_preparation_failed";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Preparing,
    Queued,
    Starting,
    Running,
    Waiting,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    RolledBack,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RestartCancelledBackgroundWorkKind {
    Subagent,
    Shell,
    Monitor,
    Task,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestartCancelledBackgroundWork {
    pub kind: RestartCancelledBackgroundWorkKind,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The agent was waiting on this work, so its restart note wakes the agent
    /// even for a shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaited: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunBackgroundWorkCancelled {
    pub run_id: RunId,
    pub restart_cancelled_background_work: Vec<RestartCancelledBackgroundWork>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DelegatedCompletionTaskDeliveryState {
    Pending,
    Claimed,
    Acknowledged,
    Delivered,
    Disposed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedCompletionTaskDelivery {
    pub state: DelegatedCompletionTaskDeliveryState,
    pub observed_by_run_id: Option<RunId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedCompletionDelivery {
    pub generation: u32,
    pub message_id: MessageId,
    pub task_ids: Vec<NodeId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DelegatedCompletionDisposition {
    Open,
    Stopped,
    Disposed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedCompletionCohort {
    pub disposition: DelegatedCompletionDisposition,
    pub next_generation: u32,
    pub delivery: Option<DelegatedCompletionDelivery>,
}

/// How a new thread's workspace is prepared before its first run starts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ThreadLaunchWorkspaceStrategy {
    Root {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
    ExistingWorktree {
        worktree_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
    Worktree {
        base_ref: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_from_origin: Option<bool>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePlanRef {
    pub thread_id: ThreadId,
    pub plan_id: PlanId,
}

/// T3's `OrchestrationV2Run`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: RunId,
    pub thread_id: ThreadId,
    pub ordinal: u32,
    pub provider_instance_id: ProviderInstanceId,
    pub model_selection: ModelSelection,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub user_message_id: MessageId,
    pub root_node_id: Option<NodeId>,
    pub active_attempt_id: Option<RunAttemptId>,
    pub status: RunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_position: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_held: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steer_when_ready_run_id: Option<RunId>,
    pub requested_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub checkpoint_id: Option<CheckpointId>,
    pub context_handoff_id: Option<ContextHandoffId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_continuation_of_run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_continuation_of_run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_limit_continuation_of_run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_task_id: Option<NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_cancelled_background_work: Option<Vec<RestartCancelledBackgroundWork>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_plan_ref: Option<SourcePlanRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_completion: Option<DelegatedCompletionCohort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_preparation: Option<ThreadLaunchWorkspaceStrategy>,
    /// Starts a new provider conversation; earlier runs stay visible but never
    /// reach the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_context: Option<bool>,
}

/// The timestamp work actually started, falling back to the request time.
pub fn run_work_started_at(run: &Run) -> &str {
    run.work_started_at
        .as_deref()
        .or(run.started_at.as_deref())
        .unwrap_or(&run.requested_at)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunAttemptReason {
    Initial,
    SteeringRestart,
    Retry,
    ProviderRecovery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunAttemptStatus {
    Pending,
    Running,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    Superseded,
}

/// T3's `OrchestrationV2RunAttempt`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAttempt {
    pub id: RunAttemptId,
    /// Provider-thread rows can be reused after recovery; retain the native
    /// input destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_thread_id: Option<String>,
    pub run_id: RunId,
    pub attempt_ordinal: u32,
    pub root_node_id: NodeId,
    pub provider_instance_id: ProviderInstanceId,
    pub provider_thread_id: ProviderThreadId,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub reason: RunAttemptReason,
    pub status: RunAttemptStatus,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionNodeKind {
    RootTurn,
    AssistantMessage,
    Reasoning,
    Plan,
    TodoList,
    ToolCall,
    ApprovalRequest,
    UserInputRequest,
    Subagent,
    Hook,
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionNodeStatus {
    Idle,
    Pending,
    Running,
    Waiting,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    RolledBack,
}

/// T3's `OrchestrationV2ExecutionNode`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionNode {
    pub id: NodeId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub parent_node_id: Option<NodeId>,
    pub root_node_id: NodeId,
    pub kind: ExecutionNodeKind,
    pub status: ExecutionNodeStatus,
    pub counts_for_run: bool,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub native_item_ref: Option<ProviderRef>,
    pub runtime_request_id: Option<RuntimeRequestId>,
    pub checkpoint_scope_id: Option<CheckpointScopeId>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// `pending`, `running` and `waiting` are work in progress.
pub fn is_orchestration_v2_work_active(status: ExecutionNodeStatus) -> bool {
    matches!(
        status,
        ExecutionNodeStatus::Pending | ExecutionNodeStatus::Running | ExecutionNodeStatus::Waiting
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run_value() -> serde_json::Value {
        json!({
            "id": "run-1",
            "threadId": "thread-1",
            "ordinal": 1,
            "providerInstanceId": "codex",
            "modelSelection": { "instanceId": "codex", "model": "gpt-5-codex" },
            "providerThreadId": null,
            "userMessageId": "message-1",
            "rootNodeId": null,
            "activeAttemptId": null,
            "status": "queued",
            "requestedAt": "2026-04-20T00:00:00.000Z",
            "startedAt": null,
            "completedAt": null,
            "checkpointId": null,
            "contextHandoffId": null
        })
    }

    #[test]
    fn round_trips_a_run_and_fills_work_started_at() {
        let run: Run = serde_json::from_value(run_value()).unwrap();
        assert_eq!(run_work_started_at(&run), "2026-04-20T00:00:00.000Z");
        assert_eq!(serde_json::to_value(&run).unwrap(), run_value());

        let mut started = run_value();
        started["startedAt"] = json!("2026-04-20T00:00:01.000Z");
        started["workStartedAt"] = json!("2026-04-20T00:00:02.000Z");
        let run: Run = serde_json::from_value(started).unwrap();
        assert_eq!(run_work_started_at(&run), "2026-04-20T00:00:02.000Z");
    }

    #[test]
    fn workspace_strategies_keep_t3_tags() {
        let strategy: ThreadLaunchWorkspaceStrategy = serde_json::from_value(json!({
            "type": "existing_worktree",
            "worktreePath": "/tmp/wt"
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&strategy).unwrap(),
            json!({ "type": "existing_worktree", "worktreePath": "/tmp/wt" })
        );
    }

    #[test]
    fn active_work_statuses() {
        assert!(is_orchestration_v2_work_active(
            ExecutionNodeStatus::Waiting
        ));
        assert!(!is_orchestration_v2_work_active(
            ExecutionNodeStatus::Completed
        ));
    }
}
