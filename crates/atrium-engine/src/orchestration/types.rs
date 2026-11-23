// Ported from packages/contracts/src/orchestrationV2.ts and packages/contracts/src/providerPolicy.ts (T3 Code v0.1.11, MIT).

//! Local seed model for the pure orchestration core.
//!
//! `crate::contracts` is being ported in parallel. These structs keep the T3
//! field names and unions so the real contract module can replace them without
//! changing a decision. **Consolidation:** when `crate::contracts` grows the
//! `OrchestrationV2*` types, delete the duplicates here and re-export.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

pub use crate::contracts::{CommandId, EventId, ProjectId, RunId, ThreadId};

// Local ids that the seed contract does not carry yet. Consolidation: replace
// with the contract aliases once they land.
pub type NodeId = String;
pub type MessageId = String;
pub type RunAttemptId = String;
pub type ProviderThreadId = String;
pub type ProviderSessionId = String;
pub type ProviderTurnId = String;
pub type RuntimeRequestId = String;
pub type TurnItemId = String;
pub type SubagentId = String;

/// Epoch milliseconds. Time enters every decision as an argument.
pub type Timestamp = i64;

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelSelection {
    pub instance_id: String,
    pub model: String,
}

impl ModelSelection {
    pub fn new(instance_id: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            instance_id: instance_id.into(),
            model: model.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeMode {
    ApprovalRequired,
    AutoAcceptEdits,
    Auto,
    #[default]
    FullAccess,
}

impl RuntimeMode {
    pub const fn rank(self) -> u8 {
        match self {
            Self::ApprovalRequired => 0,
            Self::AutoAcceptEdits => 1,
            Self::Auto => 2,
            Self::FullAccess => 3,
        }
    }

    /// Whether `mode` grants the agent more than `ceiling`.
    pub fn is_above(self, ceiling: Option<RuntimeMode>) -> bool {
        ceiling.is_some_and(|ceiling| self.rank() > ceiling.rank())
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::ApprovalRequired => "Supervised",
            Self::AutoAcceptEdits => "Auto-accept edits",
            Self::Auto => "Auto",
            Self::FullAccess => "Full access",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionMode {
    #[default]
    Default,
    Plan,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    #[default]
    User,
    Agent,
    System,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreationSource {
    #[default]
    Web,
    Mobile,
    Server,
    Mcp,
    Provider,
    Import,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TitleRegeneration {
    pub request_id: CommandId,
    pub started_at: Timestamp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettledOverride {
    Settled,
    Active,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub id: ThreadId,
    pub project_id: ProjectId,
    pub title: String,
    pub provider_instance_id: String,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub active_provider_thread_id: Option<ProviderThreadId>,
    pub created_by: Actor,
    pub creation_source: CreationSource,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub archived_at: Option<Timestamp>,
    pub settled_override: Option<SettledOverride>,
    pub settled_at: Option<Timestamp>,
    pub last_visited_at: Option<Timestamp>,
    pub title_regeneration: Option<TitleRegeneration>,
    pub deleted_at: Option<Timestamp>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    #[default]
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

impl RunStatus {
    /// A run that still owns the thread's provider turn.
    pub const fn is_active(self) -> bool {
        matches!(
            self,
            Self::Preparing | Self::Starting | Self::Running | Self::Waiting
        )
    }

    /// A run whose agent is live; waiting runs only drain post-terminal output.
    pub const fn is_live(self) -> bool {
        matches!(self, Self::Preparing | Self::Starting | Self::Running)
    }

    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Interrupted | Self::Failed | Self::Cancelled | Self::RolledBack
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: RunId,
    pub thread_id: ThreadId,
    pub ordinal: u32,
    pub provider_instance_id: String,
    pub model_selection: ModelSelection,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub user_message_id: MessageId,
    pub root_node_id: Option<NodeId>,
    pub active_attempt_id: Option<RunAttemptId>,
    pub status: RunStatus,
    pub queue_position: Option<u32>,
    pub queue_held: bool,
    pub steer_when_ready_run_id: Option<RunId>,
    pub requested_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub restart_continuation_of_run_id: Option<RunId>,
    pub manual_continuation_of_run_id: Option<RunId>,
    pub usage_limit_continuation_of_run_id: Option<RunId>,
    pub fresh_context: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunAttemptReason {
    Initial,
    SteeringRestart,
    Retry,
    ProviderRecovery,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunAttempt {
    pub id: RunAttemptId,
    pub run_id: RunId,
    pub attempt_ordinal: u32,
    pub root_node_id: NodeId,
    pub provider_instance_id: String,
    pub provider_thread_id: ProviderThreadId,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub reason: RunAttemptReason,
    pub status: RunAttemptStatus,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    RootTurn,
    AssistantMessage,
    Reasoning,
    Plan,
    TodoList,
    ToolCall,
    ApprovalRequest,
    UserInputRequest,
    Subagent,
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionNode {
    pub id: NodeId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub parent_node_id: Option<NodeId>,
    pub root_node_id: NodeId,
    pub kind: NodeKind,
    pub status: NodeStatus,
    pub counts_for_run: bool,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub runtime_request_id: Option<RuntimeRequestId>,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegatedCompletion {
    pub parent_run_id: RunId,
    pub generation: u32,
    pub task_ids: Vec<NodeId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    pub outcome: NotificationOutcome,
    pub summary: String,
    pub source_kind: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationOutcome {
    Completed,
    Failed,
    Cancelled,
    Updated,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: Option<NodeId>,
    pub role: MessageRole,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub streaming: bool,
    pub created_by: Actor,
    pub creation_source: CreationSource,
    pub scheduled_task_id: Option<String>,
    pub sender_thread_id: Option<ThreadId>,
    /// Runs on the thread's selection at start when true.
    pub follows_thread_selection: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub delegated_completion: Option<DelegatedCompletion>,
    pub notification: Option<Notification>,
}

impl Default for Message {
    fn default() -> Self {
        Self {
            id: String::new(),
            thread_id: String::new(),
            run_id: None,
            node_id: None,
            role: MessageRole::User,
            text: String::new(),
            attachments: Vec::new(),
            streaming: false,
            created_by: Actor::default(),
            creation_source: CreationSource::default(),
            scheduled_task_id: None,
            sender_thread_id: None,
            follows_thread_selection: false,
            created_at: 0,
            updated_at: 0,
            delegated_completion: None,
            notification: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnItemStatus {
    Pending,
    Running,
    Waiting,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputIntent {
    TurnStart,
    Steer,
    QueuedTurn,
    PromotedQueuedToSteer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnItemKind {
    UserMessage {
        message_id: MessageId,
        text: String,
        input_intent: InputIntent,
    },
    RunInterruptRequest {
        message: String,
    },
    RunInterruptResult {
        message: String,
    },
    ApprovalRequest {
        request_id: RuntimeRequestId,
        prompt: Option<String>,
    },
    SystemNotice {
        message: String,
    },
}

impl TurnItemKind {
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::UserMessage { .. } => "user_message",
            Self::RunInterruptRequest { .. } => "run_interrupt_request",
            Self::RunInterruptResult { .. } => "run_interrupt_result",
            Self::ApprovalRequest { .. } => "approval_request",
            Self::SystemNotice { .. } => "system_notice",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnItem {
    pub id: TurnItemId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: Option<NodeId>,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub parent_item_id: Option<TurnItemId>,
    pub ordinal: i64,
    pub status: TurnItemStatus,
    pub title: Option<String>,
    pub created_by: Actor,
    pub creation_source: CreationSource,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub updated_at: Timestamp,
    pub kind: TurnItemKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Command,
    FileRead,
    FileChange,
    McpElicitation,
    Permission,
    DynamicToolCall,
    UserInput,
    AuthRefresh,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    Pending,
    Resolved,
    Expired,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalDecision {
    Accept,
    AcceptForSession,
    AcceptAlways,
    Decline,
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeRequest {
    pub id: RuntimeRequestId,
    pub thread_id: ThreadId,
    pub node_id: NodeId,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub kind: RequestKind,
    pub status: RequestStatus,
    pub provider_instance_id: String,
    pub created_at: Timestamp,
    pub resolved_at: Option<Timestamp>,
    pub decision: Option<ApprovalDecision>,
    pub feedback: Option<String>,
    /// `None` when the answer must arrive as a message on the next turn.
    pub live_provider_session_id: Option<ProviderSessionId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderThreadStatus {
    NotLoaded,
    Idle,
    Active,
    Archived,
    Closed,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderThread {
    pub id: ProviderThreadId,
    pub driver: String,
    pub provider_instance_id: String,
    pub provider_session_id: Option<ProviderSessionId>,
    pub app_thread_id: ThreadId,
    pub native_thread_ref: Option<String>,
    pub status: ProviderThreadStatus,
    pub first_run_ordinal: Option<u32>,
    pub last_run_ordinal: Option<u32>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderTurn {
    pub id: ProviderTurnId,
    pub provider_thread_id: ProviderThreadId,
    pub run_attempt_id: RunAttemptId,
    pub status: ProviderTurnStatus,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// The turn capabilities that decide how input reaches a live turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnCapabilities {
    pub supports_interrupt: bool,
    pub supports_active_steering: bool,
    pub active_steering_interrupts_tools: bool,
    pub active_steering_accepts_attachments: bool,
    pub supports_steering_by_interrupt_restart: bool,
    pub supports_queued_messages: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    pub turns: TurnCapabilities,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSession {
    pub id: ProviderSessionId,
    pub provider_instance_id: String,
    pub provider_thread_id: ProviderThreadId,
    pub capabilities: ProviderCapabilities,
    pub cwd: Option<String>,
    pub model: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Projection {
    pub thread: Thread,
    pub runs: Vec<Run>,
    pub messages: Vec<Message>,
    pub attempts: Vec<RunAttempt>,
    pub nodes: Vec<ExecutionNode>,
    pub provider_threads: Vec<ProviderThread>,
    pub provider_sessions: Vec<ProviderSession>,
    pub provider_turns: Vec<ProviderTurn>,
    pub turn_items: Vec<TurnItem>,
    pub runtime_requests: Vec<RuntimeRequest>,
    pub subagent_ids: BTreeSet<SubagentId>,
}

impl Projection {
    pub fn new(thread: Thread) -> Self {
        Self {
            thread,
            ..Self::default()
        }
    }

    pub fn run(&self, id: &str) -> Option<&Run> {
        self.runs.iter().find(|run| run.id == id)
    }

    pub fn message(&self, id: &str) -> Option<&Message> {
        self.messages.iter().find(|message| message.id == id)
    }

    pub fn attempt(&self, id: &str) -> Option<&RunAttempt> {
        self.attempts.iter().find(|attempt| attempt.id == id)
    }

    pub fn node(&self, id: &str) -> Option<&ExecutionNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    pub fn provider_thread(&self, id: &str) -> Option<&ProviderThread> {
        self.provider_threads.iter().find(|row| row.id == id)
    }

    pub fn provider_session(&self, id: &str) -> Option<&ProviderSession> {
        self.provider_sessions.iter().find(|row| row.id == id)
    }

    pub fn provider_turn(&self, id: &str) -> Option<&ProviderTurn> {
        self.provider_turns.iter().find(|row| row.id == id)
    }

    pub fn turn_item(&self, id: &str) -> Option<&TurnItem> {
        self.turn_items.iter().find(|item| item.id == id)
    }

    pub fn runtime_request(&self, id: &str) -> Option<&RuntimeRequest> {
        self.runtime_requests.iter().find(|row| row.id == id)
    }

    pub fn active_run(&self) -> Option<&Run> {
        self.runs.iter().rev().find(|run| run.status.is_active())
    }

    pub fn active_provider_thread(&self) -> Option<&ProviderThread> {
        self.thread
            .active_provider_thread_id
            .as_deref()
            .and_then(|id| self.provider_thread(id))
    }

    pub fn next_run_ordinal(&self) -> u32 {
        self.runs.iter().map(|run| run.ordinal).max().unwrap_or(0) + 1
    }

    pub fn next_turn_item_ordinal(&self) -> i64 {
        self.turn_items
            .iter()
            .map(|item| item.ordinal)
            .max()
            .map_or(0, |max| max + 1)
    }

    /// The latest run that actually executed, by end time then ordinal.
    pub fn latest_executed_run(&self) -> Option<&Run> {
        let mut latest: Option<&Run> = None;
        for run in &self.runs {
            if run.status == RunStatus::Queued {
                continue;
            }
            if run.status == RunStatus::Cancelled && run.started_at.is_none() {
                continue;
            }
            if latest.is_none_or(|current| run_ran_after(run, current)) {
                latest = Some(run);
            }
        }
        latest
    }

    /// Stop reached this run when its interrupt request item is stored.
    pub fn stop_reached(&self, run_id: &str) -> bool {
        self.turn_items.iter().any(|item| {
            item.run_id.as_deref() == Some(run_id)
                && matches!(item.kind, TurnItemKind::RunInterruptRequest { .. })
        })
    }

    pub fn provider_turn_capabilities(&self, run: &Run) -> Option<TurnCapabilities> {
        let provider_thread = self.provider_thread(run.provider_thread_id.as_deref()?)?;
        let session = self.provider_session(provider_thread.provider_session_id.as_deref()?)?;
        Some(session.capabilities.turns)
    }

    pub fn running_provider_turn(&self, run: &Run) -> Option<&ProviderTurn> {
        let attempt_id = run.active_attempt_id.as_deref()?;
        self.provider_turns.iter().rev().find(|turn| {
            turn.run_attempt_id == attempt_id && turn.status == ProviderTurnStatus::Running
        })
    }

    pub fn apply(&mut self, event: &DomainEvent) {
        match &event.kind {
            EventKind::ThreadCreated(thread)
            | EventKind::ThreadUpdated(thread)
            | EventKind::ThreadMetadataUpdated(thread)
            | EventKind::ThreadModelSelectionUpdated(thread)
            | EventKind::ThreadProviderSwitched(thread)
            | EventKind::ThreadRuntimeModeUpdated(thread) => {
                self.thread = thread.clone();
            }
            EventKind::RunCreated(run) | EventKind::RunUpdated(run) => {
                upsert(&mut self.runs, run, |r| r.id.as_str());
            }
            EventKind::RunAttemptCreated(attempt) | EventKind::RunAttemptUpdated(attempt) => {
                upsert(&mut self.attempts, attempt, |a| a.id.as_str());
            }
            EventKind::NodeUpdated(node) => upsert(&mut self.nodes, node, |n| n.id.as_str()),
            EventKind::MessageUpdated(message) => {
                upsert(&mut self.messages, message, |m| m.id.as_str());
            }
            EventKind::TurnItemUpdated(item) => {
                upsert(&mut self.turn_items, item, |i| i.id.as_str());
            }
            EventKind::RuntimeRequestUpdated(request) => {
                upsert(&mut self.runtime_requests, request, |r| r.id.as_str());
            }
            EventKind::ProviderTurnUpdated(provider_turn) => {
                upsert(&mut self.provider_turns, provider_turn, |p| p.id.as_str());
            }
            EventKind::ProviderThreadUpdated(provider_thread) => {
                upsert(&mut self.provider_threads, provider_thread, |p| {
                    p.id.as_str()
                });
                if provider_thread.app_thread_id == self.thread.id
                    && !matches!(provider_thread.status, ProviderThreadStatus::Closed)
                {
                    self.thread.active_provider_thread_id = Some(provider_thread.id.clone());
                }
            }
        }
    }
}

fn upsert<T>(rows: &mut Vec<T>, value: &T, key: impl Fn(&T) -> &str)
where
    T: Clone,
{
    let value_key = key(value).to_owned();
    if let Some(existing) = rows.iter_mut().find(|row| key(row) == value_key.as_str()) {
        *existing = value.clone();
    } else {
        rows.push(value.clone());
    }
}

pub fn run_ran_after(run: &Run, other: &Run) -> bool {
    let end = |candidate: &Run| candidate.completed_at.unwrap_or(Timestamp::MAX);
    match end(run).cmp(&end(other)) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Equal => run.ordinal > other.ordinal,
        std::cmp::Ordering::Less => false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadUpdate {
    Updated,
    Metadata,
    ModelSelection,
    ProviderSwitched,
    RuntimeMode,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    ThreadCreated(Thread),
    ThreadUpdated(Thread),
    ThreadMetadataUpdated(Thread),
    ThreadModelSelectionUpdated(Thread),
    ThreadProviderSwitched(Thread),
    ThreadRuntimeModeUpdated(Thread),
    RunCreated(Run),
    RunUpdated(Run),
    RunAttemptCreated(RunAttempt),
    RunAttemptUpdated(RunAttempt),
    NodeUpdated(ExecutionNode),
    MessageUpdated(Message),
    TurnItemUpdated(TurnItem),
    RuntimeRequestUpdated(RuntimeRequest),
    ProviderThreadUpdated(ProviderThread),
    ProviderTurnUpdated(ProviderTurn),
}

impl EventKind {
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::ThreadCreated(_) => "thread.created",
            Self::ThreadUpdated(_) => "thread.updated",
            Self::ThreadMetadataUpdated(_) => "thread.metadata-updated",
            Self::ThreadModelSelectionUpdated(_) => "thread.model-selection-updated",
            Self::ThreadProviderSwitched(_) => "thread.provider-switched",
            Self::ThreadRuntimeModeUpdated(_) => "thread.runtime-mode-updated",
            Self::RunCreated(_) => "run.created",
            Self::RunUpdated(_) => "run.updated",
            Self::RunAttemptCreated(_) => "run-attempt.created",
            Self::RunAttemptUpdated(_) => "run-attempt.updated",
            Self::NodeUpdated(_) => "node.updated",
            Self::MessageUpdated(_) => "message.updated",
            Self::TurnItemUpdated(_) => "turn-item.updated",
            Self::RuntimeRequestUpdated(_) => "runtime-request.updated",
            Self::ProviderThreadUpdated(_) => "provider-thread.updated",
            Self::ProviderTurnUpdated(_) => "provider-turn.updated",
        }
    }

    /// The thread a projection write touches (a provider-thread update also touches its app thread).
    pub fn projected_thread_ids(&self, event_thread_id: &ThreadId) -> Vec<ThreadId> {
        match self {
            Self::ProviderThreadUpdated(row) if &row.app_thread_id != event_thread_id => {
                vec![event_thread_id.clone(), row.app_thread_id.clone()]
            }
            _ => vec![event_thread_id.clone()],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainEvent {
    pub id: EventId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: Option<NodeId>,
    pub provider_instance_id: Option<String>,
    pub occurred_at: Timestamp,
    pub kind: EventKind,
}

impl DomainEvent {
    pub fn new(
        id: impl Into<EventId>,
        thread_id: impl Into<ThreadId>,
        occurred_at: Timestamp,
        kind: EventKind,
    ) -> Self {
        Self {
            id: id.into(),
            thread_id: thread_id.into(),
            run_id: None,
            node_id: None,
            provider_instance_id: None,
            occurred_at,
            kind,
        }
    }

    pub fn with_run(mut self, run_id: impl ToString) -> Self {
        self.run_id = Some(run_id.to_string());
        self
    }

    pub fn with_node(mut self, node_id: impl ToString) -> Self {
        self.node_id = Some(node_id.to_string());
        self
    }

    pub fn with_provider_instance(mut self, instance_id: impl ToString) -> Self {
        self.provider_instance_id = Some(instance_id.to_string());
        self
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DispatchMode {
    DeferStart { workspace_strategy: Option<String> },
    SteerActive { target_run_id: RunId },
    RestartActive { target_run_id: RunId },
    QueueAfterActive,
    StartImmediately,
}

impl DispatchMode {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::DeferStart { .. } => "defer_start",
            Self::SteerActive { .. } => "steer_active",
            Self::RestartActive { .. } => "restart_active",
            Self::QueueAfterActive => "queue_after_active",
            Self::StartImmediately => "start_immediately",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryIntent {
    Auto,
    Steer,
    Restart,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchModeLimit {
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitialMessage {
    pub message_id: MessageId,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub title_seed: Option<String>,
    pub scheduled_task_id: Option<String>,
    pub sender_thread_id: Option<ThreadId>,
    pub workspace_strategy: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadCreate {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub project_id: ProjectId,
    pub title: String,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub created_by: Actor,
    pub creation_source: CreationSource,
    pub initial_message: Option<InitialMessage>,
    /// Enforced when an agent launches; a user's own client is not limited here.
    pub mode_limit: Option<DispatchModeLimit>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageDispatch {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub message_id: MessageId,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub created_by: Actor,
    pub creation_source: CreationSource,
    pub model_selection: Option<ModelSelection>,
    pub dispatch_mode: DispatchMode,
    pub delivery_intent: Option<DeliveryIntent>,
    pub scheduled_task_id: Option<String>,
    pub sender_thread_id: Option<ThreadId>,
    pub title_seed: Option<String>,
    pub fresh_context: bool,
    pub notification: Option<Notification>,
    pub delegated_completion: Option<DelegatedCompletion>,
    pub steer_follow_up_of_run_id: Option<RunId>,
    pub steer_follow_up_after_run_ordinal: Option<u32>,
    pub mode_limit: Option<DispatchModeLimit>,
}

impl MessageDispatch {
    pub fn new(
        command_id: impl Into<CommandId>,
        thread_id: impl Into<ThreadId>,
        message_id: impl Into<MessageId>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            command_id: command_id.into(),
            thread_id: thread_id.into(),
            message_id: message_id.into(),
            text: text.into(),
            attachments: Vec::new(),
            created_by: Actor::User,
            creation_source: CreationSource::Web,
            model_selection: None,
            dispatch_mode: DispatchMode::StartImmediately,
            delivery_intent: None,
            scheduled_task_id: None,
            sender_thread_id: None,
            title_seed: None,
            fresh_context: false,
            notification: None,
            delegated_completion: None,
            steer_follow_up_of_run_id: None,
            steer_follow_up_after_run_ordinal: None,
            mode_limit: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadStop {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub reason: Option<String>,
    pub stop_delegated_tasks: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunInterrupt {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub reason: Option<String>,
    pub hold_queue: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeRequestRespond {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub request_id: RuntimeRequestId,
    pub decision: Option<ApprovalDecision>,
    pub feedback: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadModelSelectionSet {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub model_selection: ModelSelection,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadRuntimeModeSet {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub runtime_mode: RuntimeMode,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    ThreadCreate(ThreadCreate),
    MessageDispatch(MessageDispatch),
    ThreadStop(ThreadStop),
    RunInterrupt(RunInterrupt),
    RuntimeRequestRespond(RuntimeRequestRespond),
    ThreadModelSelectionSet(ThreadModelSelectionSet),
    ThreadRuntimeModeSet(ThreadRuntimeModeSet),
    QueueResume {
        command_id: CommandId,
        thread_id: ThreadId,
    },
    QueuedRunStart {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
    },
    /// A close on a thread that never had a share; it changes nothing but still records a receipt.
    ThreadGuestShareClose {
        command_id: CommandId,
        thread_id: ThreadId,
    },
}

impl Command {
    pub fn command_id(&self) -> &str {
        match self {
            Self::ThreadCreate(command) => &command.command_id,
            Self::MessageDispatch(command) => &command.command_id,
            Self::ThreadStop(command) => &command.command_id,
            Self::RunInterrupt(command) => &command.command_id,
            Self::RuntimeRequestRespond(command) => &command.command_id,
            Self::ThreadModelSelectionSet(command) => &command.command_id,
            Self::ThreadRuntimeModeSet(command) => &command.command_id,
            Self::QueueResume { command_id, .. }
            | Self::QueuedRunStart { command_id, .. }
            | Self::ThreadGuestShareClose { command_id, .. } => command_id,
        }
    }

    pub fn command_type(&self) -> &'static str {
        match self {
            Self::ThreadCreate(_) => "thread.create",
            Self::MessageDispatch(_) => "message.dispatch",
            Self::ThreadStop(_) => "thread.stop",
            Self::RunInterrupt(_) => "run.interrupt",
            Self::RuntimeRequestRespond(_) => "runtime-request.respond",
            Self::ThreadModelSelectionSet(_) => "thread.model-selection.set",
            Self::ThreadRuntimeModeSet(_) => "thread.runtime-mode.set",
            Self::QueueResume { .. } => "queue.resume",
            Self::QueuedRunStart { .. } => "queued-run.start",
            Self::ThreadGuestShareClose { .. } => "thread.guest-share.close",
        }
    }

    pub fn thread_id(&self) -> &ThreadId {
        match self {
            Self::ThreadCreate(command) => &command.thread_id,
            Self::MessageDispatch(command) => &command.thread_id,
            Self::ThreadStop(command) => &command.thread_id,
            Self::RunInterrupt(command) => &command.thread_id,
            Self::RuntimeRequestRespond(command) => &command.thread_id,
            Self::ThreadModelSelectionSet(command) => &command.thread_id,
            Self::ThreadRuntimeModeSet(command) => &command.thread_id,
            Self::QueueResume { thread_id, .. }
            | Self::QueuedRunStart { thread_id, .. }
            | Self::ThreadGuestShareClose { thread_id, .. } => thread_id,
        }
    }

    /// A command that may legitimately produce no events.
    pub fn allows_empty_events(&self) -> bool {
        matches!(
            self,
            Self::ThreadStop(_) | Self::ThreadGuestShareClose { .. } | Self::QueueResume { .. }
        )
    }
}

// ---------------------------------------------------------------------------
// Effects (the outbox)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectRequest {
    ProviderTurnStart {
        run_id: RunId,
    },
    ProviderTurnInterrupt {
        provider_session_id: ProviderSessionId,
        provider_thread_id: ProviderThreadId,
        provider_turn_id: ProviderTurnId,
    },
    ProviderTurnSteer {
        provider_session_id: ProviderSessionId,
        provider_thread_id: ProviderThreadId,
        provider_turn_id: ProviderTurnId,
        message_id: MessageId,
        model_selection: Option<ModelSelection>,
    },
    ProviderTurnRestart {
        provider_session_id: ProviderSessionId,
        provider_thread_id: ProviderThreadId,
        provider_turn_id: ProviderTurnId,
        interrupted_attempt_id: RunAttemptId,
        run_id: RunId,
    },
    RuntimeRequestRespond {
        provider_session_id: ProviderSessionId,
        request_id: RuntimeRequestId,
        decision: Option<ApprovalDecision>,
    },
    ProviderSessionApplyModelSelection {
        provider_session_id: ProviderSessionId,
        provider_thread_id: ProviderThreadId,
        model_selection: ModelSelection,
    },
    ThreadTitleGenerate {
        message_id: MessageId,
    },
    DelegatedTasksStop {
        reason: Option<String>,
    },
}

impl EffectRequest {
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::ProviderTurnStart { .. } => "provider-turn.start",
            Self::ProviderTurnInterrupt { .. } => "provider-turn.interrupt",
            Self::ProviderTurnSteer { .. } => "provider-turn.steer",
            Self::ProviderTurnRestart { .. } => "provider-turn.restart",
            Self::RuntimeRequestRespond { .. } => "runtime-request.respond",
            Self::ProviderSessionApplyModelSelection { .. } => {
                "provider-session.apply-model-selection"
            }
            Self::ThreadTitleGenerate { .. } => "thread-title.generate",
            Self::DelegatedTasksStop { .. } => "delegated-tasks.stop",
        }
    }

    /// Effects that bind a live provider process; a restart cancels, never replays, them.
    pub const fn is_process_bound(&self) -> bool {
        matches!(
            self,
            Self::ProviderSessionApplyModelSelection { .. }
                | Self::ProviderTurnStart { .. }
                | Self::ProviderTurnInterrupt { .. }
                | Self::ProviderTurnSteer { .. }
                | Self::ProviderTurnRestart { .. }
                | Self::RuntimeRequestRespond { .. }
        )
    }

    /// Effects a new server process may safely run again.
    pub const fn is_replay_safe_after_process_loss(&self) -> bool {
        matches!(
            self,
            Self::ThreadTitleGenerate { .. } | Self::DelegatedTasksStop { .. }
        )
    }

    /// Whether the effect owns starting a run; a last failed attempt must settle it.
    pub const fn owns_starting_run(&self) -> bool {
        matches!(
            self,
            Self::ProviderTurnStart { .. } | Self::ProviderTurnRestart { .. }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingEffect {
    pub id: String,
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub request: EffectRequest,
    pub available_at: Option<Timestamp>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OrchestratorError {
    #[error("thread {0} was not found")]
    ThreadNotFound(ThreadId),
    #[error("thread {thread_id} already exists")]
    ThreadAlreadyExists {
        command_id: CommandId,
        thread_id: ThreadId,
    },
    #[error(
        "command {command_id} was already handled for thread {receipt_thread_id} and cannot be replayed for {command_thread_id}"
    )]
    CommandIdConflict {
        command_id: CommandId,
        command_type: String,
        receipt_thread_id: ThreadId,
        command_thread_id: ThreadId,
    },
    #[error("command {command_id} ({command_type}) was previously rejected: {detail}")]
    CommandPreviouslyRejected {
        command_id: CommandId,
        command_type: String,
        detail: String,
    },
    #[error("thread {0} is deleted")]
    ThreadDeleted(ThreadId),
    #[error("{command_type} {command_id} was refused: {cause}")]
    Dispatch {
        command_id: CommandId,
        command_type: String,
        cause: String,
    },
    #[error(
        "thread {thread_id} runs above the launch limit ({runtime_mode:?}/{interaction_mode:?})"
    )]
    ModeLimitExceeded {
        command_id: CommandId,
        thread_id: ThreadId,
        runtime_mode: RuntimeMode,
        interaction_mode: InteractionMode,
    },
    #[error("run {0} is not interruptible")]
    RunNotInterruptible(RunId),
    #[error("runtime request {0} is not pending")]
    RequestNotPending(RuntimeRequestId),
    #[error("command {0} produced no domain events")]
    NoEvents(CommandId),
}

impl OrchestratorError {
    /// Whether a replay of this command should return the recorded rejection.
    pub fn is_recorded_rejection(&self) -> bool {
        !matches!(
            self,
            Self::ModeLimitExceeded { .. } | Self::ThreadNotFound(_) | Self::NoEvents(_)
        )
    }

    pub fn command_id(&self) -> Option<&str> {
        match self {
            Self::ThreadAlreadyExists { command_id, .. }
            | Self::CommandIdConflict { command_id, .. }
            | Self::CommandPreviouslyRejected { command_id, .. }
            | Self::Dispatch { command_id, .. }
            | Self::ModeLimitExceeded { command_id, .. }
            | Self::NoEvents(command_id) => Some(command_id),
            _ => None,
        }
    }
}

pub fn model_selections_equal(left: &ModelSelection, right: &ModelSelection) -> bool {
    left.instance_id == right.instance_id && left.model == right.model
}

pub fn next_queued_run(projection: &Projection) -> Option<Run> {
    crate::orchestration::queued_run_order::queued_runs_in_delivery_order(projection)
        .into_iter()
        .find(|run| !run.queue_held)
}

/// A run that still owns the thread's provider turn.
pub fn is_active_run(run: &Run) -> bool {
    run.status.is_active()
}

/// The selection the thread will be on once every queued run has started.
pub fn follows_current_conversation(message: Option<&Message>) -> bool {
    message.is_some_and(|message| {
        message.delegated_completion.is_some()
            || message.follows_thread_selection
            || message
                .notification
                .as_ref()
                .is_some_and(|notification| notification.source_kind == "monitor")
    })
}

pub fn queued_run_model_selection(projection: &Projection, run: &Run) -> ModelSelection {
    let message = projection.message(&run.user_message_id);
    if follows_current_conversation(message) {
        projection.thread.model_selection.clone()
    } else {
        run.model_selection.clone()
    }
}

pub fn queue_tail_model_selection(projection: &Projection) -> ModelSelection {
    crate::orchestration::queued_run_order::queued_runs_in_delivery_order(projection)
        .iter()
        .rev()
        .find(|run| !follows_current_conversation(projection.message(&run.user_message_id)))
        .map_or_else(
            || projection.thread.model_selection.clone(),
            |run| run.model_selection.clone(),
        )
}
