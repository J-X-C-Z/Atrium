// Ported from packages/contracts/src/orchestrationV2.ts (T3 Code v0.1.11, MIT).
//! Turn items: the user-visible records inside a run's tree, plus plan
//! artifacts.
//!
//! The union keeps T3's tag names. A type from a newer peer decodes as
//! [`TurnItem::Unknown`] with its raw JSON, matching the forward-compatible
//! wrapper T3 applies to projected item arrays; it never encodes again.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::base::Actor;
use super::base::{
    CheckpointId, CheckpointScopeId, ContextHandoffId, EnvironmentId, MessageId, NodeId,
    NonNegativeInt, PlanId, ProviderDriverKind, ProviderInstanceId, ProviderThreadId,
    ProviderTurnId, RunId, RuntimeRequestId, ScheduledTaskId, ThreadId, TurnItemId,
    UnknownUnionMember, open_union,
};
use super::chat::{ChatAttachment, OrchestrationMessageContext};
use super::model::ModelSelection;
use super::provider::{
    AuthMcpClientAccess, ProviderApprovalFullAccessOverride, ProviderApprovalOption, ProviderRef,
    ProviderRequestKind, UserInputAttachmentAnswerPayload,
};
use super::thread::{CreationSource, ForkSource, LinkedSender};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TurnItemStatus {
    Idle,
    Pending,
    Running,
    Waiting,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IdleCompaction {
    BeforeCacheExpires,
    OnReturn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserMessageInputIntent {
    TurnStart,
    QueuedTurn,
    Steer,
    PromotedQueuedToSteer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantMessagePhase {
    Commentary,
    PartialAnswer,
    FinalAnswer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SystemNoticeLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFailureClass {
    UsageLimit,
    ProviderError,
    TransportError,
    PermissionError,
    ValidationError,
    Unknown,
}

/// Producers must redact credentials before construction.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderFailure {
    pub class: ProviderFailureClass,
    pub message: String,
    pub code: Option<String>,
    pub retryable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRetry {
    pub attempt: u32,
    pub max_attempts: Option<u32>,
    pub retry_delay_ms: Option<u32>,
}

/// A same-harness model or effort change that took effect with this message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSwitch {
    pub from: ModelSelection,
    pub to: ModelSelection,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeDetail {
    pub operation: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSearchResult {
    pub file_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanStepStatus {
    Pending,
    Running,
    Completed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    pub id: String,
    pub text: String,
    pub status: PlanStepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_anchor_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<NonNegativeInt>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQuestionOption {
    pub label: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<UserInputQuestionOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_select: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_custom_answer: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanArtifactStatus {
    Draft,
    Active,
    Completed,
    Superseded,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanArtifactBase {
    pub id: PlanId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: NodeId,
    pub status: PlanArtifactStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_in_turn_item: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposedPlanArtifact {
    #[serde(flatten)]
    pub base: PlanArtifactBase,
    pub markdown: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoListArtifact {
    #[serde(flatten)]
    pub base: PlanArtifactBase,
    pub steps: Vec<PlanStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
}

/// T3's `OrchestrationV2PlanArtifact` union.
#[derive(Clone, Debug, PartialEq)]
pub enum PlanArtifact {
    ProposedPlan(ProposedPlanArtifact),
    TodoList(TodoListArtifact),
}

impl Serialize for PlanArtifact {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::ProposedPlan(value) => {
                super::base::serialize_tagged_with("kind", "proposed_plan", value, serializer)
            }
            Self::TodoList(value) => {
                super::base::serialize_tagged_with("kind", "todo_list", value, serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for PlanArtifact {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (tag, value) = super::base::tagged_union_tag_with(deserializer, "kind")?;
        match tag.as_str() {
            "proposed_plan" => serde_json::from_value(value)
                .map(Self::ProposedPlan)
                .map_err(D::Error::custom),
            "todo_list" => serde_json::from_value(value)
                .map(Self::TodoList)
                .map_err(D::Error::custom),
            other => Err(D::Error::custom(format!(
                "unknown plan artifact kind `{other}`"
            ))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointFileSummary {
    /// Exact as git reports it: names may start or end with whitespace.
    pub path: String,
    pub kind: String,
    pub additions: NonNegativeInt,
    pub deletions: NonNegativeInt,
}

/// Sent only when the wire carries a prefix of `files`; totals cover all.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointFileTotals {
    pub count: NonNegativeInt,
    pub additions: NonNegativeInt,
    pub deletions: NonNegativeInt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationOutcome {
    Completed,
    Failed,
    Cancelled,
    Updated,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestWakeSection {
    pub text: String,
    pub outcome: NotificationOutcome,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorPullRequestStamp {
    pub host: String,
    pub repository: String,
    pub number: u32,
    pub watch_started_at: String,
    /// Null when the wake is not about one head.
    pub head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch_ended: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<PullRequestWakeSection>,
}

/// What ended or woke a background turn. Unknown kinds from a newer server
/// become a generic `background_task`, dropping fields only that server knows.
#[derive(Clone, Debug, PartialEq)]
pub enum NotificationSource {
    DelegatedTask {
        task_ids: Vec<NodeId>,
        child_thread_id: Option<ThreadId>,
    },
    Subagent {
        child_thread_id: Option<ThreadId>,
    },
    Command,
    Monitor {
        pull_request: Option<MonitorPullRequestStamp>,
        other_pull_requests: Option<Vec<MonitorPullRequestStamp>>,
    },
    BackgroundTask,
    System,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotificationSourceRaw {
    kind: Option<String>,
    child_thread_id: Option<ThreadId>,
    task_ids: Option<Vec<NodeId>>,
    work: Option<String>,
    pull_request: Option<MonitorPullRequestStamp>,
    other_pull_requests: Option<Vec<MonitorPullRequestStamp>>,
}

impl<'de> Deserialize<'de> for NotificationSource {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw: NotificationSourceRaw =
            serde_json::from_value(serde_json::Value::deserialize(deserializer)?)
                .map_err(D::Error::custom)?;
        Ok(match raw.kind.as_deref() {
            Some("delegated_task") => Self::DelegatedTask {
                task_ids: raw
                    .task_ids
                    .ok_or_else(|| D::Error::custom("delegated_task needs taskIds"))?,
                child_thread_id: raw.child_thread_id,
            },
            Some("background_task") if raw.work.as_deref() == Some("subagent") => Self::Subagent {
                child_thread_id: raw.child_thread_id,
            },
            Some("background_task") => Self::BackgroundTask,
            Some("background_command") => Self::Command,
            Some("subagent") => Self::Subagent {
                child_thread_id: raw.child_thread_id,
            },
            Some("command") => Self::Command,
            Some("monitor") => Self::Monitor {
                pull_request: raw.pull_request,
                other_pull_requests: raw.other_pull_requests,
            },
            Some("system") => Self::System,
            // Unknown kind, or none at all: generic background work.
            _ => Self::BackgroundTask,
        })
    }
}

impl Serialize for NotificationSource {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Encode the legacy shapes first so clients from before specific
        // kinds still accept command and subagent sources.
        match self {
            Self::DelegatedTask {
                task_ids,
                child_thread_id,
            } => {
                #[derive(Serialize)]
                #[serde(rename_all = "camelCase")]
                struct Wire<'a> {
                    kind: &'static str,
                    task_ids: &'a [NodeId],
                    #[serde(skip_serializing_if = "Option::is_none")]
                    child_thread_id: Option<&'a ThreadId>,
                }
                Wire {
                    kind: "delegated_task",
                    task_ids,
                    child_thread_id: child_thread_id.as_ref(),
                }
                .serialize(serializer)
            }
            Self::Subagent { child_thread_id } => {
                #[derive(Serialize)]
                #[serde(rename_all = "camelCase")]
                struct Wire<'a> {
                    kind: &'static str,
                    work: &'static str,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    child_thread_id: Option<&'a ThreadId>,
                }
                Wire {
                    kind: "background_task",
                    work: "subagent",
                    child_thread_id: child_thread_id.as_ref(),
                }
                .serialize(serializer)
            }
            Self::Command => {
                #[derive(Serialize)]
                struct Wire {
                    kind: &'static str,
                }
                Wire {
                    kind: "background_command",
                }
                .serialize(serializer)
            }
            Self::Monitor {
                pull_request,
                other_pull_requests,
            } => {
                #[derive(Serialize)]
                #[serde(rename_all = "camelCase")]
                struct Wire<'a> {
                    kind: &'static str,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    pull_request: Option<&'a MonitorPullRequestStamp>,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    other_pull_requests: Option<&'a Vec<MonitorPullRequestStamp>>,
                }
                Wire {
                    kind: "monitor",
                    pull_request: pull_request.as_ref(),
                    other_pull_requests: other_pull_requests.as_ref(),
                }
                .serialize(serializer)
            }
            Self::BackgroundTask => {
                #[derive(Serialize)]
                struct Wire {
                    kind: &'static str,
                }
                Wire {
                    kind: "background_task",
                }
                .serialize(serializer)
            }
            Self::System => {
                #[derive(Serialize)]
                struct Wire {
                    kind: &'static str,
                }
                Wire { kind: "system" }.serialize(serializer)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    pub source: NotificationSource,
    pub outcome: NotificationOutcome,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeferredWork {
    pub cursor: String,
    pub item_count: u32,
    pub last_ordinal: NonNegativeInt,
}

/// The fields every turn item shares.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnItemBase {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_work: Option<DeferredWork>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_non_execution_kind: Option<String>,
    /// `ToolActivitySurface`; kept raw (UI-only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_surface: Option<serde_json::Value>,
    /// `ToolActivityIcon`; kept raw (UI-only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_icon: Option<serde_json::Value>,
    /// `ToolActivitySource`; kept raw (UI-only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_source: Option<serde_json::Value>,
    pub id: TurnItemId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: Option<NodeId>,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub native_item_ref: Option<ProviderRef>,
    pub parent_item_id: Option<TurnItemId>,
    pub ordinal: NonNegativeInt,
    pub status: TurnItemStatus,
    pub title: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    #[serde(flatten)]
    pub notification: Notification,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMessageTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    pub created_by: Actor,
    pub creation_source: CreationSource,
    pub message_id: MessageId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_task_id: Option<ScheduledTaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_sender: Option<LinkedSender>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_compaction: Option<IdleCompaction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_script: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_context: Option<bool>,
    pub input_intent: UserMessageInputIntent,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_omitted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<OrchestrationMessageContext>,
    pub attachments: Vec<ChatAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_switch: Option<ModelSwitch>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessageTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<AssistantMessagePhase>,
    pub message_id: MessageId,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_omitted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<ChatAttachment>>,
    pub streaming: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    pub text: String,
    pub streaming: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposedPlanTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    pub plan_id: PlanId,
    pub markdown: String,
    pub streaming: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoListTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    pub plan_id: PlanId,
    pub steps: Vec<PlanStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputRequestTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    pub request_id: RuntimeRequestId,
    pub questions: Vec<UserInputQuestion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_answer: Option<UserInputAttachmentAnswerPayload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_mode: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    pub file_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additions: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletions: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_str: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_str: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_str: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes: Option<Vec<FileChangeDetail>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecutionTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    pub input: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_omitted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_indicates_failure: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_wake: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSearchTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results: Option<Vec<FileSearchResult>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patterns: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results: Option<Vec<WebSearchResult>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequestTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    pub request_id: RuntimeRequestId,
    pub request_kind: ProviderRequestKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_access_override: Option<ProviderApprovalFullAccessOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<ProviderApprovalOption>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    pub checkpoint_id: CheckpointId,
    pub scope_id: CheckpointScopeId,
    pub files: Vec<CheckpointFileSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_total: Option<CheckpointFileTotals>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_omitted: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunInterruptRequestTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unresponsive_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunInterruptResultTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemNoticeTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<SystemNoticeLevel>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    pub failure: ProviderFailure,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<ProviderRetry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    pub driver: Option<ProviderDriverKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_token_count: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_token_count: Option<NonNegativeInt>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffStrategy {
    DeltaSinceTargetLastSeen,
    ForkDeltaSummary,
    FullThreadSummary,
    CheckpointSummary,
    ManualContext,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    pub context_handoff_id: ContextHandoffId,
    pub from_provider_thread_ids: Vec<ProviderThreadId>,
    pub to_provider_thread_id: ProviderThreadId,
    pub from_provider_instance_ids: Vec<ProviderInstanceId>,
    pub to_provider_instance_id: ProviderInstanceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_model_selections: Option<Vec<ModelSelection>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_model: Option<String>,
    pub strategy: HandoffStrategy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    pub source: ForkSource,
    pub target_thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_thread_id: Option<ProviderThreadId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadCreatedTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    pub target_thread_id: ThreadId,
    pub target_run_id: Option<RunId>,
    pub target_provider_instance_id: ProviderInstanceId,
    pub target_model: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecretRequestStatus {
    Pending,
    Saved,
    Declined,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretRequestTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    pub label: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<String>,
    pub secret_status: SecretRequestStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkRequestStatus {
    Pending,
    Linked,
    Declined,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkRequestTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<EnvironmentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_access: Option<AuthMcpClientAccess>,
    pub link_status: LinkRequestStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_environment_id: Option<EnvironmentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_access: Option<AuthMcpClientAccess>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTaskChild {
    pub environment_id: EnvironmentId,
    pub thread_id: ThreadId,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_there: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_count: Option<NonNegativeInt>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentOrigin {
    ProviderNative,
    AppOwned,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    pub subagent_id: NodeId,
    pub origin: SubagentOrigin,
    pub driver: ProviderDriverKind,
    pub provider_instance_id: ProviderInstanceId,
    pub child_thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_child: Option<RemoteTaskChild>,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_omitted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_omitted: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DynamicToolTurnItem {
    #[serde(flatten)]
    pub base: TurnItemBase,
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewed_image_path: Option<String>,
    pub input: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_omitted: Option<bool>,
}

/// T3's `OrchestrationV2TurnItem` union, plus a decode-only unknown member.
// Boxing the payloads would change the shape callers match on; the engine
// reads these one at a time, so the larger variants cost nothing here.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum TurnItem {
    Notification(NotificationTurnItem),
    UserMessage(UserMessageTurnItem),
    AssistantMessage(AssistantMessageTurnItem),
    Reasoning(ReasoningTurnItem),
    ProposedPlan(ProposedPlanTurnItem),
    TodoList(TodoListTurnItem),
    UserInputRequest(UserInputRequestTurnItem),
    FileChange(FileChangeTurnItem),
    CommandExecution(CommandExecutionTurnItem),
    FileSearch(FileSearchTurnItem),
    WebSearch(WebSearchTurnItem),
    ApprovalRequest(ApprovalRequestTurnItem),
    Checkpoint(CheckpointTurnItem),
    RunInterruptRequest(RunInterruptRequestTurnItem),
    RunInterruptResult(RunInterruptResultTurnItem),
    SystemNotice(SystemNoticeTurnItem),
    Error(ErrorTurnItem),
    Compaction(CompactionTurnItem),
    Handoff(HandoffTurnItem),
    Fork(ForkTurnItem),
    ThreadCreated(ThreadCreatedTurnItem),
    SecretRequest(SecretRequestTurnItem),
    LinkRequest(LinkRequestTurnItem),
    Subagent(SubagentTurnItem),
    DynamicTool(DynamicToolTurnItem),
    Unknown(UnknownUnionMember),
}

open_union!(
    TurnItem,
    Unknown,
    "notification" => Notification(NotificationTurnItem),
    "user_message" => UserMessage(UserMessageTurnItem),
    "assistant_message" => AssistantMessage(AssistantMessageTurnItem),
    "reasoning" => Reasoning(ReasoningTurnItem),
    "proposed_plan" => ProposedPlan(ProposedPlanTurnItem),
    "todo_list" => TodoList(TodoListTurnItem),
    "user_input_request" => UserInputRequest(UserInputRequestTurnItem),
    "file_change" => FileChange(FileChangeTurnItem),
    "command_execution" => CommandExecution(CommandExecutionTurnItem),
    "file_search" => FileSearch(FileSearchTurnItem),
    "web_search" => WebSearch(WebSearchTurnItem),
    "approval_request" => ApprovalRequest(ApprovalRequestTurnItem),
    "checkpoint" => Checkpoint(CheckpointTurnItem),
    "run_interrupt_request" => RunInterruptRequest(RunInterruptRequestTurnItem),
    "run_interrupt_result" => RunInterruptResult(RunInterruptResultTurnItem),
    "system_notice" => SystemNotice(SystemNoticeTurnItem),
    "error" => Error(ErrorTurnItem),
    "compaction" => Compaction(CompactionTurnItem),
    "handoff" => Handoff(HandoffTurnItem),
    "fork" => Fork(ForkTurnItem),
    "thread_created" => ThreadCreated(ThreadCreatedTurnItem),
    "secret_request" => SecretRequest(SecretRequestTurnItem),
    "link_request" => LinkRequest(LinkRequestTurnItem),
    "subagent" => Subagent(SubagentTurnItem),
    "dynamic_tool" => DynamicTool(DynamicToolTurnItem),
);

impl TurnItem {
    pub fn base(&self) -> Option<&TurnItemBase> {
        Some(match self {
            Self::Notification(item) => &item.base,
            Self::UserMessage(item) => &item.base,
            Self::AssistantMessage(item) => &item.base,
            Self::Reasoning(item) => &item.base,
            Self::ProposedPlan(item) => &item.base,
            Self::TodoList(item) => &item.base,
            Self::UserInputRequest(item) => &item.base,
            Self::FileChange(item) => &item.base,
            Self::CommandExecution(item) => &item.base,
            Self::FileSearch(item) => &item.base,
            Self::WebSearch(item) => &item.base,
            Self::ApprovalRequest(item) => &item.base,
            Self::Checkpoint(item) => &item.base,
            Self::RunInterruptRequest(item) => &item.base,
            Self::RunInterruptResult(item) => &item.base,
            Self::SystemNotice(item) => &item.base,
            Self::Error(item) => &item.base,
            Self::Compaction(item) => &item.base,
            Self::Handoff(item) => &item.base,
            Self::Fork(item) => &item.base,
            Self::ThreadCreated(item) => &item.base,
            Self::SecretRequest(item) => &item.base,
            Self::LinkRequest(item) => &item.base,
            Self::Subagent(item) => &item.base,
            Self::DynamicTool(item) => &item.base,
            Self::Unknown(_) => return None,
        })
    }

    pub fn id(&self) -> Option<&TurnItemId> {
        self.base().map(|base| &base.id)
    }

    pub fn type_name(&self) -> &str {
        match self {
            Self::Notification(_) => "notification",
            Self::UserMessage(_) => "user_message",
            Self::AssistantMessage(_) => "assistant_message",
            Self::Reasoning(_) => "reasoning",
            Self::ProposedPlan(_) => "proposed_plan",
            Self::TodoList(_) => "todo_list",
            Self::UserInputRequest(_) => "user_input_request",
            Self::FileChange(_) => "file_change",
            Self::CommandExecution(_) => "command_execution",
            Self::FileSearch(_) => "file_search",
            Self::WebSearch(_) => "web_search",
            Self::ApprovalRequest(_) => "approval_request",
            Self::Checkpoint(_) => "checkpoint",
            Self::RunInterruptRequest(_) => "run_interrupt_request",
            Self::RunInterruptResult(_) => "run_interrupt_result",
            Self::SystemNotice(_) => "system_notice",
            Self::Error(_) => "error",
            Self::Compaction(_) => "compaction",
            Self::Handoff(_) => "handoff",
            Self::Fork(_) => "fork",
            Self::ThreadCreated(_) => "thread_created",
            Self::SecretRequest(_) => "secret_request",
            Self::LinkRequest(_) => "link_request",
            Self::Subagent(_) => "subagent",
            Self::DynamicTool(_) => "dynamic_tool",
            Self::Unknown(member) => &member.tag,
        }
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectedItemVisibility {
    Local,
    Inherited,
    Synthetic,
}

/// T3's `OrchestrationV2ProjectedTurnItem`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectedTurnItem {
    pub position: NonNegativeInt,
    pub visibility: ProjectedItemVisibility,
    pub source_thread_id: ThreadId,
    pub source_item_id: TurnItemId,
    pub item: TurnItem,
}

/// Drop items a newer server added, keeping known broken items visible as
/// errors: T3's `ForwardCompatibleUnionArray` behavior.
pub fn known_turn_items(items: Vec<TurnItem>) -> Vec<TurnItem> {
    items
        .into_iter()
        .filter(|item| !item.is_unknown())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::base::trim_non_empty;
    use serde_json::json;

    fn item_value(id: &str, type_: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut value = json!({
            "id": id,
            "type": type_,
            "threadId": "thread-1",
            "runId": null,
            "nodeId": null,
            "providerThreadId": null,
            "providerTurnId": null,
            "nativeItemRef": null,
            "parentItemId": null,
            "ordinal": 1,
            "status": "completed",
            "title": null,
            "startedAt": null,
            "completedAt": null,
            "updatedAt": "2026-04-20T00:00:00.000Z"
        });
        if let (serde_json::Value::Object(target), serde_json::Value::Object(extra)) =
            (&mut value, extra)
        {
            target.extend(extra);
        }
        value
    }

    // Ported from packages/contracts/src/orchestrationV2.test.ts.
    #[test]
    fn skips_turn_item_types_from_a_newer_server() {
        let known = item_value("item-known", "system_notice", json!({ "message": "Hello" }));
        let future = item_value("item-future", "hologram", json!({ "beam": "ref-1" }));

        let known: TurnItem = serde_json::from_value(known).unwrap();
        let future: TurnItem = serde_json::from_value(future).unwrap();
        assert_eq!(known.type_name(), "system_notice");
        assert!(!known.is_unknown());
        assert!(future.is_unknown());
        assert_eq!(future.type_name(), "hologram");

        // The unknown member kept the raw payload so a store can carry it.
        let future_raw = item_value("item-future", "hologram", json!({ "beam": "ref-1" }));
        let future: TurnItem = serde_json::from_value(future_raw.clone()).unwrap();
        let TurnItem::Unknown(member) = &future else {
            panic!("expected an unknown member");
        };
        assert_eq!(member.tag, "hologram");
        assert_eq!(member.value, future_raw);
        // Unknown items are decode-only.
        assert!(serde_json::to_value(&future).is_err());

        let kept = known_turn_items(vec![future, known]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id().unwrap().as_str(), "item-known");
        assert!(serde_json::to_value(&kept[0]).is_ok());
    }

    #[test]
    fn a_known_turn_item_with_a_broken_payload_still_fails() {
        let broken = item_value("item-broken", "system_notice", json!({}));
        assert!(serde_json::from_value::<TurnItem>(broken).is_err());
    }

    #[test]
    fn round_trips_a_system_notice_verbatim() {
        let value = item_value("item-1", "system_notice", json!({ "message": "Hello" }));
        let item: TurnItem = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&item).unwrap(), value);
    }

    #[test]
    fn notification_sources_follow_the_legacy_wire_shapes() {
        let source_of = |source: serde_json::Value| {
            serde_json::from_value::<NotificationSource>(source).unwrap()
        };
        assert_eq!(
            source_of(
                json!({ "kind": "background_task", "nativeRef": { "driver": "claude", "nativeId": "task-1" } })
            ),
            NotificationSource::BackgroundTask
        );
        assert_eq!(
            source_of(json!({ "kind": "background_command" })),
            NotificationSource::Command
        );
        assert_eq!(
            source_of(json!({ "kind": "monitor" })),
            NotificationSource::Monitor {
                pull_request: None,
                other_pull_requests: None
            }
        );
        assert_eq!(
            source_of(json!({ "kind": "workflow", "workflowId": "wf-1" })),
            NotificationSource::BackgroundTask
        );

        // Sending writes the shapes older clients still read.
        assert_eq!(
            serde_json::to_value(NotificationSource::Subagent {
                child_thread_id: Some(ThreadId::new("child"))
            })
            .unwrap(),
            json!({ "kind": "background_task", "work": "subagent", "childThreadId": "child" })
        );
        assert_eq!(
            serde_json::to_value(NotificationSource::Command).unwrap(),
            json!({ "kind": "background_command" })
        );
        assert_eq!(
            serde_json::to_value(NotificationSource::DelegatedTask {
                task_ids: vec![NodeId::new("task-1")],
                child_thread_id: Some(ThreadId::new("child"))
            })
            .unwrap(),
            json!({ "kind": "delegated_task", "taskIds": ["task-1"], "childThreadId": "child" })
        );
    }

    #[test]
    fn notification_items_keep_decoding_and_re_encoding_their_source() {
        let stored = item_value(
            "notification",
            "notification",
            json!({
                "outcome": "completed",
                "summary": "Background activity updated",
                "source": { "kind": "subagent", "childThreadId": "child" }
            }),
        );
        let item: TurnItem = serde_json::from_value(stored.clone()).unwrap();
        let TurnItem::Notification(notification) = &item else {
            panic!("expected a notification");
        };
        assert_eq!(
            notification.notification.source,
            NotificationSource::Subagent {
                child_thread_id: Some(ThreadId::new("child"))
            }
        );
        let reencoded = serde_json::to_value(&item).unwrap();
        assert_eq!(
            reencoded["source"],
            json!({ "kind": "background_task", "work": "subagent", "childThreadId": "child" })
        );
        let again: TurnItem = serde_json::from_value(reencoded).unwrap();
        assert_eq!(again, item);
    }

    #[test]
    fn checkpoint_paths_keep_leading_and_trailing_whitespace() {
        for path in [" leading.ts", "src/trailing.ts "] {
            let summary: CheckpointFileSummary = serde_json::from_value(json!({
                "path": path,
                "kind": "added",
                "additions": 1,
                "deletions": 0
            }))
            .unwrap();
            assert_eq!(summary.path, path);
        }
    }

    #[test]
    fn plan_artifacts_use_their_kind_tags() {
        let todo: PlanArtifact = serde_json::from_value(json!({
            "id": "plan-1",
            "threadId": "thread-1",
            "runId": null,
            "nodeId": "node-1",
            "status": "active",
            "kind": "todo_list",
            "steps": [],
            "explanation": "why"
        }))
        .unwrap();
        assert!(matches!(todo, PlanArtifact::TodoList(_)));
        assert_eq!(
            serde_json::to_value(&todo).unwrap()["kind"],
            json!("todo_list")
        );
    }

    #[test]
    fn user_messages_require_their_intent() {
        let mut value = item_value(
            "item-1",
            "user_message",
            json!({
                "createdBy": "user",
                "creationSource": "web",
                "messageId": "message-1",
                "inputIntent": "steer",
                "text": "go",
                "attachments": []
            }),
        );
        let item: TurnItem = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(item.type_name(), "user_message");
        value["inputIntent"] = json!("teleport");
        assert!(serde_json::from_value::<TurnItem>(value).is_err());
    }

    #[test]
    fn trim_non_empty_is_used_for_ids() {
        assert!(trim_non_empty("  ").is_none());
        assert_eq!(trim_non_empty(" x ").as_deref(), Some("x"));
    }
}
