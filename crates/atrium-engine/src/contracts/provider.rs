// Ported from packages/contracts/src/providerPolicy.ts (T3 Code v0.1.11, MIT).
//! Runtime modes, approval policy, provider sessions and provider turns.
//!
//! Also ports `ProviderDriverKind` / `ProviderInstanceId` /
//! `OrchestrationV2ProviderRef` from `providerInstance.ts` and
//! `orchestrationV2.ts`, and `TurnTokenUsage`'s shape stays raw JSON until
//! the usage module (item 23) takes it over.

use std::collections::BTreeMap;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::base::{
    ContextHandoffId, NodeId, NonNegativeInt, ProviderDriverKind, ProviderInstanceId,
    ProviderSessionId, ProviderThreadId, ProviderTurnId, RunAttemptId, RunId, ThreadId, TurnItemId,
    trim_non_empty,
};
use super::capabilities::{NativeRefStrength, ProviderCapabilities};
use super::chat::ChatAttachment;

/// T3's `RuntimeMode`. `Default` is `full-access`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeMode {
    ApprovalRequired,
    AutoAcceptEdits,
    Auto,
    #[default]
    FullAccess,
}

impl RuntimeMode {
    fn rank(self) -> u8 {
        match self {
            Self::ApprovalRequired => 0,
            Self::AutoAcceptEdits => 1,
            Self::Auto => 2,
            Self::FullAccess => 3,
        }
    }
}

pub const DEFAULT_RUNTIME_MODE: RuntimeMode = RuntimeMode::FullAccess;

/// Whether `mode` grants the agent more than `ceiling`; no ceiling allows
/// every mode.
pub fn is_runtime_mode_above(mode: RuntimeMode, ceiling: Option<RuntimeMode>) -> bool {
    ceiling.is_some_and(|ceiling| mode.rank() > ceiling.rank())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderInteractionMode {
    #[default]
    Default,
    Plan,
}

pub const DEFAULT_PROVIDER_INTERACTION_MODE: ProviderInteractionMode =
    ProviderInteractionMode::Default;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderRequestKind {
    Command,
    FileRead,
    FileChange,
    McpElicitation,
    Permission,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderApprovalDecision {
    Accept,
    AcceptForSession,
    AcceptAlways,
    Decline,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderApprovalFullAccessOverride {
    AskRule,
    ProviderCheck,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderApprovalPolicy {
    Untrusted,
    OnFailure,
    OnRequest,
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderSandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

/// T3's `AuthMcpClientAccess`, used by link origins and link-request items.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthMcpClientAccess {
    ReadOnly,
    ApprovalRequired,
    AutoAcceptEdits,
    Auto,
    FullAccess,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderApprovalOption {
    pub decision: ProviderApprovalDecision,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// Question id to answer. Values stay raw because the questions are
/// provider-defined.
pub type ProviderUserInputAnswers = BTreeMap<String, serde_json::Value>;

/// Question id to attachments. T3 restricts this to images and files; the
/// engine keeps the wider attachment union so a newer type still decodes.
pub type UserInputAttachments = BTreeMap<String, Vec<ChatAttachment>>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputAttachmentAnswerPayload {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_text_by_id: Option<BTreeMap<String, String>>,
    pub answers: ProviderUserInputAnswers,
    pub attachments_by_question_id: UserInputAttachments,
}

/// T3's `OrchestrationV2ProviderRef`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRef {
    pub driver: ProviderDriverKind,
    pub native_id: Option<String>,
    pub strength: NativeRefStrength,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<NonNegativeInt>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionStatus {
    pub key: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderSessionStatus {
    Starting,
    Ready,
    Running,
    Waiting,
    Stopped,
    Error,
}

/// T3's `OrchestrationV2ProviderSession`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSession {
    pub id: ProviderSessionId,
    pub driver: ProviderDriverKind,
    pub provider_instance_id: ProviderInstanceId,
    pub status: ProviderSessionStatus,
    pub cwd: String,
    pub model: Option<String>,
    pub capabilities: ProviderCapabilities,
    #[serde(default)]
    pub extension_statuses: Vec<ExtensionStatus>,
    pub created_at: String,
    pub updated_at: String,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSessionDetached {
    pub provider_session_id: ProviderSessionId,
    pub detached_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderTurnStatus {
    Pending,
    Running,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
}

/// T3's `OrchestrationV2ProviderTurn`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTurn {
    pub id: ProviderTurnId,
    pub provider_thread_id: ProviderThreadId,
    pub node_id: NodeId,
    pub run_attempt_id: Option<RunAttemptId>,
    pub native_turn_ref: Option<ProviderRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_thread_ref: Option<ProviderRef>,
    pub ordinal: u32,
    pub status: ProviderTurnStatus,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_token_usage: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Codex goals can span several native turns in one attempt; this returns the
/// attempt's highest-ordinal turn. A missing attempt id matches nothing.
pub fn latest_provider_turn_for_attempt<'a>(
    provider_turns: &'a [ProviderTurn],
    attempt_id: Option<&RunAttemptId>,
) -> Option<&'a ProviderTurn> {
    let attempt_id = attempt_id?;
    provider_turns
        .iter()
        .filter(|turn| turn.run_attempt_id.as_ref() == Some(attempt_id))
        .max_by_key(|turn| turn.ordinal)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderThreadStatus {
    NotLoaded,
    Idle,
    Active,
    Archived,
    Closed,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderGoalStatus {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderGoal {
    pub objective: String,
    pub status: ProviderGoalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_used_seconds: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderThreadNativeMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_store_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_selection: Option<super::model::ModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_identity_version: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_head_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_attachment_budget: Option<ClaudeAttachmentBudget>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeAttachmentBudget {
    pub bytes_used: NonNegativeInt,
    pub pages_used: NonNegativeInt,
}

/// A background task a provider thread still runs. Kinds this build does not
/// know decode as a generic `background_task`, dropping fields only a newer
/// server knows, as T3's `kindUnionWithFallback` does.
#[derive(Clone, Debug, PartialEq)]
pub enum PendingBackgroundTask {
    Subagent(PendingBackgroundTaskFields),
    Command(PendingBackgroundTaskFields),
    Monitor(PendingBackgroundTaskFields),
    BackgroundTask(PendingBackgroundTaskFields),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PendingBackgroundTaskFields {
    pub command: Option<String>,
    pub item_id: Option<TurnItemId>,
    pub source_thread_id: Option<ThreadId>,
    pub task_id: String,
    pub description: Option<String>,
    pub parent_task_id: Option<String>,
    pub native_item_id: Option<String>,
    pub run_id: Option<RunId>,
    pub awaited: Option<bool>,
    /// Only on a `subagent` task.
    pub provider_instance_id: Option<ProviderInstanceId>,
    /// Only on a `subagent` task.
    pub child_thread_id: Option<ThreadId>,
}

impl PendingBackgroundTask {
    pub fn fields(&self) -> &PendingBackgroundTaskFields {
        match self {
            Self::Subagent(fields)
            | Self::Command(fields)
            | Self::Monitor(fields)
            | Self::BackgroundTask(fields) => fields,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Subagent(_) => "subagent",
            Self::Command(_) => "command",
            Self::Monitor(_) => "monitor",
            Self::BackgroundTask(_) => "background_task",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingBackgroundTaskRaw {
    command: Option<String>,
    item_id: Option<TurnItemId>,
    source_thread_id: Option<ThreadId>,
    task_id: String,
    description: Option<String>,
    parent_task_id: Option<String>,
    native_item_id: Option<String>,
    run_id: Option<RunId>,
    awaited: Option<bool>,
    kind: Option<String>,
    provider_instance_id: Option<ProviderInstanceId>,
    child_thread_id: Option<ThreadId>,
}

impl<'de> Deserialize<'de> for PendingBackgroundTask {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw: PendingBackgroundTaskRaw =
            serde_json::from_value(serde_json::Value::deserialize(deserializer)?)
                .map_err(D::Error::custom)?;
        let task_id = trim_non_empty(&raw.task_id)
            .ok_or_else(|| D::Error::custom("a taskId must not be blank"))?;
        let fields = PendingBackgroundTaskFields {
            command: raw.command,
            item_id: raw.item_id,
            source_thread_id: raw.source_thread_id,
            task_id,
            description: raw.description,
            parent_task_id: raw.parent_task_id,
            native_item_id: raw.native_item_id,
            run_id: raw.run_id,
            awaited: raw.awaited,
            provider_instance_id: raw.provider_instance_id,
            child_thread_id: raw.child_thread_id,
        };
        Ok(match raw.kind.as_deref() {
            Some("subagent") => Self::Subagent(fields),
            Some("command") => Self::Command(fields),
            Some("monitor") => Self::Monitor(fields),
            Some("background_task") | None => Self::BackgroundTask(fallback_fields(fields)),
            Some(_) => Self::BackgroundTask(fallback_fields(fields)),
        })
    }
}

/// The fallback keeps only the fields every kind shares.
fn fallback_fields(fields: PendingBackgroundTaskFields) -> PendingBackgroundTaskFields {
    PendingBackgroundTaskFields {
        task_id: fields.task_id,
        description: fields.description,
        parent_task_id: fields.parent_task_id,
        ..PendingBackgroundTaskFields::default()
    }
}

impl Serialize for PendingBackgroundTask {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire<'a> {
            kind: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            command: Option<&'a String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            item_id: Option<&'a TurnItemId>,
            #[serde(skip_serializing_if = "Option::is_none")]
            source_thread_id: Option<&'a ThreadId>,
            task_id: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            description: Option<&'a String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            parent_task_id: Option<&'a String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            native_item_id: Option<&'a String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            run_id: Option<&'a RunId>,
            #[serde(skip_serializing_if = "Option::is_none")]
            awaited: Option<bool>,
            #[serde(skip_serializing_if = "Option::is_none")]
            provider_instance_id: Option<&'a ProviderInstanceId>,
            #[serde(skip_serializing_if = "Option::is_none")]
            child_thread_id: Option<&'a ThreadId>,
        }
        let fields = self.fields();
        let subagent = matches!(self, Self::Subagent(_));
        Wire {
            kind: self.kind(),
            command: fields.command.as_ref(),
            item_id: fields.item_id.as_ref(),
            source_thread_id: fields.source_thread_id.as_ref(),
            task_id: &fields.task_id,
            description: fields.description.as_ref(),
            parent_task_id: fields.parent_task_id.as_ref(),
            native_item_id: fields.native_item_id.as_ref(),
            run_id: fields.run_id.as_ref(),
            awaited: fields.awaited,
            provider_instance_id: subagent
                .then_some(fields.provider_instance_id.as_ref())
                .flatten(),
            child_thread_id: subagent
                .then_some(fields.child_thread_id.as_ref())
                .flatten(),
        }
        .serialize(serializer)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderThreadForkedFrom {
    pub provider_thread_id: ProviderThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_turn_id: Option<ProviderTurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<super::base::CheckpointId>,
}

/// T3's `OrchestrationV2ProviderThread`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderThread {
    pub id: ProviderThreadId,
    pub driver: ProviderDriverKind,
    pub provider_instance_id: ProviderInstanceId,
    pub provider_session_id: Option<ProviderSessionId>,
    pub app_thread_id: Option<ThreadId>,
    pub owner_node_id: Option<NodeId>,
    pub native_thread_ref: Option<ProviderRef>,
    pub native_conversation_head_ref: Option<ProviderRef>,
    pub status: ProviderThreadStatus,
    pub first_run_ordinal: Option<u32>,
    pub last_run_ordinal: Option<u32>,
    pub handoff_ids: Vec<ContextHandoffId>,
    pub forked_from: Option<ProviderThreadForkedFrom>,
    #[serde(default)]
    pub pending_background_tasks: Vec<PendingBackgroundTask>,
    /// `ThreadTokenUsageSnapshot`; ported with the usage module (item 23).
    #[serde(default)]
    pub context_usage: Option<serde_json::Value>,
    #[serde(default)]
    pub native_metadata: Option<ProviderThreadNativeMetadata>,
    #[serde(default)]
    pub goal: Option<ProviderGoal>,
    pub created_at: String,
    pub updated_at: String,
}

/// A queued provider thread reserves a seat for the next run; it does not own
/// the thread yet.
pub fn is_queued_provider_thread_placeholder(provider_thread: &ProviderThread) -> bool {
    provider_thread.status == ProviderThreadStatus::NotLoaded
        && provider_thread.first_run_ordinal.is_none()
        && provider_thread.native_thread_ref.is_none()
        && provider_thread.provider_session_id.is_none()
}

/// A provider request waiting for a person; ported for the approval commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeRequestStatus {
    Pending,
    Resolved,
    Expired,
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Ported from packages/contracts/src/orchestrationV2.test.ts.
    #[test]
    fn decodes_historical_provider_thread_json_with_empty_defaults() {
        let provider_thread: ProviderThread = serde_json::from_value(json!({
            "id": "provider-thread-1",
            "driver": "claude",
            "providerInstanceId": "claudeAgent",
            "providerSessionId": "provider-session-1",
            "appThreadId": "thread-1",
            "ownerNodeId": null,
            "nativeThreadRef": {
                "driver": "claude",
                "nativeId": "native-session-1",
                "strength": "strong"
            },
            "nativeConversationHeadRef": null,
            "status": "idle",
            "firstRunOrdinal": 1,
            "lastRunOrdinal": 1,
            "handoffIds": [],
            "forkedFrom": null,
            "createdAt": "2026-04-20T00:00:00.000Z",
            "updatedAt": "2026-04-20T00:00:00.000Z"
        }))
        .unwrap();
        assert!(provider_thread.pending_background_tasks.is_empty());
        assert!(provider_thread.context_usage.is_none());
        assert!(provider_thread.native_metadata.is_none());
    }

    #[test]
    fn decodes_rosters_stored_before_kinds_and_newer_kinds_as_generic_tasks() {
        let provider_thread: ProviderThread = serde_json::from_value(json!({
            "id": "provider-thread-1",
            "driver": "claude",
            "providerInstanceId": "claudeAgent",
            "providerSessionId": null,
            "appThreadId": "thread-1",
            "ownerNodeId": null,
            "nativeThreadRef": null,
            "nativeConversationHeadRef": null,
            "status": "idle",
            "firstRunOrdinal": 1,
            "lastRunOrdinal": 1,
            "handoffIds": [],
            "forkedFrom": null,
            "createdAt": "2026-04-20T00:00:00.000Z",
            "updatedAt": "2026-04-20T00:00:00.000Z",
            "pendingBackgroundTasks": [
                { "taskId": "bg-1", "description": "Background sleep", "taskType": "local_bash" },
                { "taskId": "bg-2" },
                { "taskId": "bg-3", "description": "Nightly", "kind": "workflow", "schedule": "0 3 * * *" },
                { "taskId": "bg-4", "description": "npm test", "kind": "command" },
                { "taskId": "bg-5", "kind": "subagent", "childThreadId": "thread-child" },
                { "taskId": "bg-6", "kind": "subagent", "providerInstanceId": "claudeAgent" }
            ]
        }))
        .unwrap();
        let encoded = serde_json::to_value(&provider_thread).unwrap();
        assert_eq!(
            encoded["pendingBackgroundTasks"],
            json!([
                { "kind": "background_task", "taskId": "bg-1", "description": "Background sleep" },
                { "kind": "background_task", "taskId": "bg-2" },
                { "kind": "background_task", "taskId": "bg-3", "description": "Nightly" },
                { "kind": "command", "taskId": "bg-4", "description": "npm test" },
                { "kind": "subagent", "taskId": "bg-5", "childThreadId": "thread-child" },
                { "kind": "subagent", "taskId": "bg-6", "providerInstanceId": "claudeAgent" }
            ])
        );
        let mut broken = encoded;
        broken["pendingBackgroundTasks"] = json!([{ "taskId": "", "kind": "command" }]);
        assert!(serde_json::from_value::<ProviderThread>(broken).is_err());
    }

    #[test]
    fn latest_provider_turn_for_attempt_picks_the_highest_ordinal() {
        fn turn(id: &str, attempt: Option<&str>, ordinal: u32) -> ProviderTurn {
            ProviderTurn {
                id: ProviderTurnId::new(id),
                provider_thread_id: ProviderThreadId::new("pt"),
                node_id: NodeId::new("node"),
                run_attempt_id: attempt.map(RunAttemptId::new),
                native_turn_ref: None,
                native_thread_ref: None,
                ordinal,
                status: ProviderTurnStatus::Completed,
                started_at: None,
                completed_at: None,
                token_usage: None,
                turn_token_usage: None,
                model: None,
            }
        }
        let turns = vec![
            turn("first", Some("goal-attempt"), 3),
            turn("other", Some("other-attempt"), 9),
            turn("last", Some("goal-attempt"), 5),
            turn("subagent", None, 7),
        ];
        assert_eq!(
            latest_provider_turn_for_attempt(&turns, Some(&RunAttemptId::new("goal-attempt")))
                .unwrap()
                .id
                .as_str(),
            "last"
        );
        assert!(latest_provider_turn_for_attempt(&turns, None).is_none());
    }

    #[test]
    fn goal_statuses_keep_their_underscored_wire_values() {
        assert_eq!(
            serde_json::to_value(ProviderGoalStatus::UsageLimited).unwrap(),
            json!("usage_limited")
        );
        assert_eq!(
            serde_json::to_value(ProviderGoalStatus::BudgetLimited).unwrap(),
            json!("budget_limited")
        );
    }

    #[test]
    fn runtime_mode_order_matches_t3() {
        assert!(is_runtime_mode_above(
            RuntimeMode::FullAccess,
            Some(RuntimeMode::Auto)
        ));
        assert!(!is_runtime_mode_above(
            RuntimeMode::Auto,
            Some(RuntimeMode::Auto)
        ));
        assert!(!is_runtime_mode_above(RuntimeMode::FullAccess, None));
    }
}
