//! The `orchestratorMcp` wire types the tools return.
//!
//! Ported from `packages/contracts/src/orchestratorMcp.ts` and
//! `packages/contracts/src/project.ts` (T3 Code v0.1.11, MIT). Field names,
//! optional-vs-nullable positions and union tag values are T3's, so a caller
//! that spoke to T3 Code reads the same `structuredContent` from Atrium.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::contracts::{
    Actor, CreationSource, DelegatedFrom, MessageId, ModelSelection, NodeId,
    ProviderInteractionMode, ProviderOptionSelections, ProviderOptionValue, ProviderRequestKind,
    RelationshipToParent, RunId, RunStatus, RuntimeMode, ThreadId, ThreadLaunchWorkspaceStrategy,
    ThreadLinkedPullRequest, TurnItemId,
};

/// A tool failure, as T3's `OrchestratorMcpFailure`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpError {
    pub code: &'static str,
    pub message: String,
}

impl McpError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_request", message)
    }

    pub fn thread_not_found(thread_id: &str) -> Self {
        Self::new(
            "thread_not_found",
            format!("Thread {thread_id} was not found."),
        )
    }

    pub fn task_not_found(task_id: &str) -> Self {
        Self::new(
            "task_not_found",
            format!("Delegated task {task_id} was not found."),
        )
    }

    pub fn orchestration(message: impl Into<String>) -> Self {
        Self::new("orchestration_error", message)
    }
}

impl std::fmt::Display for McpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for McpError {}

pub type McpResult<T> = Result<T, McpError>;

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetInput {
    #[serde(default)]
    pub provider_instance_id: Option<String>,
    #[serde(default)]
    pub driver_kind: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// The canonical array or the older `{ id: value }` record.
    #[serde(default)]
    pub options: Option<ProviderOptionSelections>,
    #[serde(default)]
    pub environment_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitiesInput {
    #[serde(default)]
    pub environment_id: Option<String>,
    #[serde(default)]
    pub search: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegateTaskInput {
    pub task: String,
    /// A target object, or the same object as JSON text from older providers.
    #[serde(default)]
    pub target: Option<Json>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub isolation: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<f64>,
    #[serde(default)]
    pub client_request_id: Option<String>,
    #[serde(default)]
    pub runtime_mode: Option<Json>,
    #[serde(default)]
    pub interaction_mode: Option<Json>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatusInput {
    pub task_id: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskCancelInput {
    pub task_id: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub client_request_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadLaunchInput {
    #[serde(default)]
    pub environment_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub scratch: Option<bool>,
    pub title: String,
    #[serde(default)]
    pub model_selection: Option<ModelSelection>,
    #[serde(default)]
    pub runtime_mode: Option<Json>,
    #[serde(default)]
    pub interaction_mode: Option<Json>,
    #[serde(default)]
    pub workspace_strategy: Option<ThreadLaunchWorkspaceStrategy>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub attachments: Vec<Json>,
    #[serde(default)]
    pub client_request_id: Option<String>,
    #[serde(default)]
    pub delegated_from: Option<DelegatedFrom>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListInput {
    #[serde(default)]
    pub environment_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub all_projects: Option<bool>,
    #[serde(default)]
    pub statuses: Option<Vec<String>>,
    #[serde(default)]
    pub title_contains: Option<String>,
    #[serde(default)]
    pub settled: Option<bool>,
    #[serde(default)]
    pub snoozed: Option<bool>,
    #[serde(default)]
    pub include_subagents: Option<bool>,
    #[serde(default)]
    pub cursor: Option<u32>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadReadInput {
    #[serde(default)]
    pub environment_id: Option<String>,
    pub thread_id: String,
    #[serde(default)]
    pub item_id: Option<String>,
    #[serde(default)]
    pub text_offset: Option<u32>,
    #[serde(default)]
    pub view: Option<String>,
    #[serde(default)]
    pub after_position: Option<i64>,
    #[serde(default)]
    pub before_position: Option<u32>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub run_limit: Option<u32>,
    #[serde(default)]
    pub max_chars_per_item: Option<u32>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSendInput {
    #[serde(default)]
    pub environment_id: Option<String>,
    pub thread_id: String,
    pub message: String,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub fresh_context: Option<bool>,
    #[serde(default)]
    pub client_request_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadWaitInput {
    #[serde(default)]
    pub environment_id: Option<String>,
    pub thread_id: String,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<f64>,
    #[serde(default)]
    pub until_delegated_work_settled: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadInterruptInput {
    #[serde(default)]
    pub environment_id: Option<String>,
    pub thread_id: String,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub stop: Option<bool>,
    #[serde(default)]
    pub client_request_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadConfigureInput {
    #[serde(default)]
    pub environment_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub model_selection: Option<ModelSelection>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRequestInput {
    pub action: String,
    #[serde(default)]
    pub environment_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub answers: Option<std::collections::BTreeMap<String, Json>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueInput {
    pub action: String,
    #[serde(default)]
    pub environment_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub queued_run_id: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    /// `Some(None)` reorders to the end; `None` leaves the order alone.
    #[serde(default, deserialize_with = "double_option")]
    pub before_run_id: Option<Option<String>>,
    #[serde(default)]
    pub target_run_id: Option<String>,
    #[serde(default)]
    pub cursor: Option<u32>,
    #[serde(default)]
    pub limit: Option<u32>,
}

fn double_option<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<Option<String>>::deserialize(deserializer)
}

/// `t3_thread_update`'s input, kept for its pure field/action check.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadUpdateInput {
    pub action: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub environment_id: Option<String>,
}

impl ThreadUpdateInput {
    /// T3's check: `rename` needs `title`; `regenerate_title` forbids it.
    pub fn validate(&self) -> bool {
        match self.action.as_str() {
            "rename" => self
                .title
                .as_deref()
                .is_some_and(|title| !title.trim().is_empty()),
            "regenerate_title" => self.title.is_none(),
            _ => false,
        }
    }
}

/// Codex and T3 clamp an oversized page instead of rejecting it.
pub fn clamped(value: Option<u32>, default: u32, maximum: u32) -> u32 {
    value.unwrap_or(default).max(1).min(maximum)
}

/// Parse T3's `"inherit" | Mode` runtime-mode input.
pub fn parse_runtime_mode(value: Option<&Json>, inherited: RuntimeMode) -> McpResult<RuntimeMode> {
    match value {
        None => Ok(inherited),
        Some(Json::String(text)) if text == "inherit" => Ok(inherited),
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|_| McpError::invalid(format!("Unknown runtime mode {value}."))),
    }
}

/// Parse T3's `"inherit" | Mode` interaction-mode input.
pub fn parse_interaction_mode(
    value: Option<&Json>,
    inherited: ProviderInteractionMode,
) -> McpResult<ProviderInteractionMode> {
    match value {
        None => Ok(inherited),
        Some(Json::String(text)) if text == "inherit" => Ok(inherited),
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|_| McpError::invalid(format!("Unknown interaction mode {value}."))),
    }
}

/// A `target` may arrive as the object or as JSON text (OpenCode 1.15).
pub fn parse_target(value: Option<&Json>) -> McpResult<Option<TargetInput>> {
    match value {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(text)) => serde_json::from_str(text)
            .map(Some)
            .map_err(|_| McpError::invalid("target must be an object or a JSON object string.")),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|_| McpError::invalid("target must be an object.")),
    }
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelOption {
    pub id: String,
    pub values: Vec<ProviderOptionValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<ProviderOptionValue>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<ProviderModelOption>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapability {
    pub provider_instance_id: String,
    pub driver_kind: String,
    pub display_name: Option<String>,
    pub models: Vec<ProviderModelInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted_models: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_modes: Option<Vec<RuntimeMode>>,
    pub can_run_child_task: bool,
    pub can_run_cross_provider_child_task: bool,
    pub constraints: Vec<String>,
}

impl ProviderCapability {
    /// A provider with no model catalog of its own: Atrium's drivers are
    /// configured per session, so the inherited selection is what children use.
    pub fn driver(instance_id: &str, driver_kind: &str) -> Self {
        Self {
            provider_instance_id: instance_id.to_owned(),
            driver_kind: driver_kind.to_owned(),
            display_name: None,
            models: Vec::new(),
            omitted_models: None,
            runtime_modes: Some(vec![
                RuntimeMode::FullAccess,
                RuntimeMode::Auto,
                RuntimeMode::AutoAcceptEdits,
                RuntimeMode::ApprovalRequired,
            ]),
            can_run_child_task: true,
            can_run_cross_provider_child_task: true,
            constraints: Vec::new(),
        }
    }

    /// The four drivers Atrium's runtime seam can start today.
    pub fn atrium_defaults() -> Vec<Self> {
        vec![
            Self::driver("codex", "codex"),
            Self::driver("claude", "claude"),
            Self::driver("deepseek", "deepseek"),
            Self::driver("antigravity", "antigravity"),
        ]
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityFeatures {
    pub app_owned_subagents: bool,
    pub async_polling: bool,
    pub cancellation: bool,
    pub batch_thread_creation: bool,
    pub thread_management: bool,
    pub incremental_thread_read: bool,
    pub scheduled_tasks: bool,
    pub max_batch_threads: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitiesResult {
    pub parent_thread_id: Option<ThreadId>,
    pub inherited_provider_instance_id: Option<String>,
    pub inherited_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherited_options: Option<Vec<crate::contracts::ProviderOptionSelection>>,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub providers: Vec<ProviderCapability>,
    pub features: CapabilityFeatures,
}

// ---------------------------------------------------------------------------
// Threads
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchResult {
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    pub project_id: String,
    pub model_selection: ModelSelection,
    pub run_id: Option<RunId>,
    pub status: Option<RunStatus>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListItem {
    pub thread_id: ThreadId,
    pub project_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    pub created_by: Actor,
    pub creation_source: CreationSource,
    pub status: String,
    pub latest_run_id: Option<RunId>,
    pub provider_instance_id: String,
    pub model: String,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub linked_pull_request: Option<ThreadLinkedPullRequest>,
    pub settled: bool,
    pub settled_at: Option<String>,
    pub snoozed: bool,
    pub snoozed_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_on_thread_wait: Option<bool>,
    pub parent_thread_id: Option<ThreadId>,
    pub relationship_to_parent: Option<RelationshipToParent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_from: Option<DelegatedFrom>,
    pub item_count: u32,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListResult {
    pub project_id: Option<String>,
    pub current_thread_id: Option<ThreadId>,
    pub threads: Vec<ThreadListItem>,
    pub next_cursor: Option<u32>,
    pub total: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadDetail {
    pub thread_id: ThreadId,
    pub project_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    pub created_by: Actor,
    pub creation_source: CreationSource,
    pub status: String,
    pub latest_run_id: Option<RunId>,
    pub active_run_id: Option<RunId>,
    pub provider_instance_id: String,
    pub model: String,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub linked_pull_request: Option<ThreadLinkedPullRequest>,
    pub title_regeneration: Option<Json>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_title_regeneration: Option<Json>,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub parent_thread_id: Option<ThreadId>,
    pub relationship_to_parent: Option<RelationshipToParent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_from: Option<DelegatedFrom>,
    pub run_count: u32,
    pub item_count: u32,
    pub pending_request_count: u32,
    pub archived: bool,
    pub settled: bool,
    pub settled_at: Option<String>,
    pub snoozed: bool,
    pub snoozed_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_on_thread_wait: Option<bool>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadRun {
    pub run_id: RunId,
    pub ordinal: u32,
    pub status: RunStatus,
    pub provider_instance_id: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executed_model: Option<Option<String>>,
    pub requested_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineItem {
    pub position: u32,
    pub visibility: String,
    pub source_thread_id: ThreadId,
    pub item_id: TurnItemId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_id: Option<NodeId>,
    pub run_id: Option<RunId>,
    pub message_id: Option<MessageId>,
    pub created_by: Option<Actor>,
    pub creation_source: Option<CreationSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_thread_id: Option<ThreadId>,
    #[serde(rename = "type")]
    pub item_type: String,
    pub status: String,
    pub title: Option<String>,
    pub text: Option<String>,
    pub text_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_text_offset: Option<Option<u32>>,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadReadResult {
    pub thread: ThreadDetail,
    pub recent_runs: Vec<ThreadRun>,
    pub items: Vec<TimelineItem>,
    pub next_position: Option<u32>,
    pub has_more: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub earlier_position: Option<Option<u32>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSendResult {
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    pub message_id: MessageId,
    pub run_id: RunId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<NodeId>,
    pub status: RunStatus,
    pub delivery: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultRun {
    pub run_id: RunId,
    pub status: RunStatus,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadWaitResult {
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub status: String,
    pub timed_out: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_run: Option<ResultRun>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadInterruptResult {
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadConfiguration {
    pub thread_id: ThreadId,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchCommandResult {
    pub sequence: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConfigureResult {
    Configuration(ThreadConfiguration),
    Dispatch(DispatchCommandResult),
}

// ---------------------------------------------------------------------------
// Queue and pending requests
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    pub queued_run_id: RunId,
    pub text: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueListResult {
    pub items: Vec<QueueEntry>,
    pub next_cursor: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum QueueResult {
    List(QueueListResult),
    Entry(QueueEntry),
    Dispatch(DispatchCommandResult),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_select: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_custom_answer: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRequestList {
    pub request_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRequestRead {
    pub request_id: String,
    pub questions: Vec<Question>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PendingRequestResult {
    List(PendingRequestList),
    Read(PendingRequestRead),
    Dispatch(DispatchCommandResult),
}

// ---------------------------------------------------------------------------
// Delegated tasks
// ---------------------------------------------------------------------------

pub const RUNNING_TASK_NEXT: &str = "Completion wakes this thread. Do not wait on it or poll it. Continue other work, and end your turn when none is left.";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteChild {
    pub environment_id: String,
    pub thread_id: ThreadId,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingApproval {
    pub request_id: String,
    pub kind: ProviderRequestKind,
    pub run_id: RunId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegateTaskResult {
    pub task_id: NodeId,
    pub child_thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_child: Option<RemoteChild>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    pub child_run_id: Option<RunId>,
    pub child_node_id: NodeId,
    pub status: String,
    pub work_state: String,
    pub has_pending_child_runs: bool,
    pub latest_terminal_run_id: Option<RunId>,
    pub latest_terminal_status: Option<String>,
    pub latest_terminal_summary: Option<String>,
    pub latest_terminal_result_context_transfer_id: Option<String>,
    pub provider_instance_id: String,
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executed_model: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<crate::contracts::ProviderOptionSelection>>,
    pub summary: Option<String>,
    pub result_context_transfer_id: Option<String>,
    pub has_pending_question: bool,
    pub pending_approval: Option<PendingApproval>,
    pub wait_timed_out: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskCancelResult {
    pub task_id: NodeId,
    pub status: String,
}

// ---------------------------------------------------------------------------
// Projects and environment
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWire {
    pub id: String,
    pub title: String,
    pub workspace_root: String,
    pub default_model_selection: Option<ModelSelection>,
    pub scripts: Vec<Json>,
    pub created_at: String,
    pub updated_at: String,
    pub deleted_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectListResult {
    pub projects: Vec<ProjectWire>,
    pub next_cursor: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformWire {
    pub os: String,
    pub arch: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundActivityWire {
    pub profile: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlWritingStyleWire {
    pub mode: String,
    pub follow_change_request_templates: bool,
    pub custom_instructions: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreferencesWire {
    pub default_thread_env_mode: Option<String>,
    pub new_worktrees_start_from_origin: bool,
    pub enable_provider_update_checks: bool,
    pub background_activity: BackgroundActivityWire,
    pub source_control_writing_style: SourceControlWritingStyleWire,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentReadResult {
    pub environment_id: String,
    pub label: String,
    pub server_version: String,
    pub platform: PlatformWire,
    pub preferences: PreferencesWire,
}

impl EnvironmentReadResult {
    pub fn current(environment_id: impl Into<String>, label: impl Into<String>) -> Self {
        let os = match std::env::consts::OS {
            "macos" => "darwin",
            "windows" => "windows",
            "linux" => "linux",
            other => other,
        };
        let arch = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            "x86_64" => "x64",
            other => other,
        };
        Self {
            environment_id: environment_id.into(),
            label: label.into(),
            server_version: env!("CARGO_PKG_VERSION").to_owned(),
            platform: PlatformWire {
                os: os.to_owned(),
                arch: arch.to_owned(),
            },
            preferences: PreferencesWire {
                default_thread_env_mode: None,
                new_worktrees_start_from_origin: false,
                enable_provider_update_checks: true,
                background_activity: BackgroundActivityWire {
                    profile: "balanced".to_owned(),
                },
                source_control_writing_style: SourceControlWritingStyleWire {
                    mode: "instructions".to_owned(),
                    follow_change_request_templates: true,
                    custom_instructions: String::new(),
                    truncated: false,
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_checks_fields_against_the_action() {
        let rename_without_title = ThreadUpdateInput {
            action: "rename".into(),
            ..ThreadUpdateInput::default()
        };
        assert!(!rename_without_title.validate());
        let regenerate_with_title = ThreadUpdateInput {
            action: "regenerate_title".into(),
            title: Some("New".into()),
            ..ThreadUpdateInput::default()
        };
        assert!(!regenerate_with_title.validate());
        let rename = ThreadUpdateInput {
            action: "rename".into(),
            title: Some("New".into()),
            environment_id: None,
        };
        assert!(rename.validate());
    }

    #[test]
    fn target_accepts_an_object_and_its_json_text() {
        let object = parse_target(Some(&Json::String(
            r#"{"model":"m","options":{"effort":"max"}}"#.into(),
        )))
        .unwrap()
        .unwrap();
        assert_eq!(object.model.as_deref(), Some("m"));
        assert!(object.options.is_some());
    }
}
