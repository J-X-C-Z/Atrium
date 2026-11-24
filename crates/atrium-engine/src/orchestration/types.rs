// Ported from packages/contracts/src/orchestrationV2.ts and packages/contracts/src/providerPolicy.ts (T3 Code v0.1.11, MIT).

//! The records and internal commands the orchestration core decides over.
//!
//! Every wire record is the ported `crate::contracts` type: a thread is
//! [`AppThread`](crate::contracts::AppThread), a run is
//! [`Run`](crate::contracts::Run), an event is
//! [`DomainEvent`](crate::contracts::DomainEvent). This module only adds the
//! pieces T3 keeps server-side (the internal command union, the folded
//! projection snapshot, outbox requests and decision errors) and the epoch
//! millisecond clock the pure decisions take as an argument.

use std::collections::BTreeSet;

pub use crate::contracts::{
    Actor, AppThread as Thread, ApprovalRequestTurnItem, ChatAttachment as Attachment,
    ConversationMessage as Message, CreationSource,
    DelegatedCompletionMessage as DelegatedCompletion, DomainEvent, DomainEventBase, EventPayload,
    ExecutionNode, ExecutionNodeKind as NodeKind, ExecutionNodeStatus as NodeStatus,
    InitialMessage, MessageDeliveryIntent as DeliveryIntent, MessageDispatchCommand,
    MessageDispatchMode as DispatchMode, MessageRole, ModelSelection, Notification,
    NotificationOutcome, NotificationSource, ProviderApprovalDecision as ApprovalDecision,
    ProviderCapabilities, ProviderInteractionMode as InteractionMode, ProviderRequestKind,
    ProviderSession, ProviderSessionStatus, ProviderThread, ProviderThreadStatus, ProviderTurn,
    ProviderTurnStatus, ResponseCapability, Run, RunAttempt, RunAttemptReason, RunAttemptStatus,
    RunInterruptCommand, RunInterruptRequestTurnItem, RunInterruptResultTurnItem, RunStatus,
    RuntimeMode, RuntimeRequest, RuntimeRequestKind as RequestKind, RuntimeRequestRespondCommand,
    RuntimeRequestStatus as RequestStatus, SettledOverride, ThreadCreateCommand,
    ThreadModelSelectionSetCommand, ThreadRuntimeModeSetCommand, ThreadStopCommand,
    TitleRegenerationMarker as TitleRegeneration, TurnCapabilities, TurnItem, TurnItemBase,
    TurnItemStatus, UserMessageInputIntent as InputIntent, UserMessageTurnItem,
};

pub use crate::contracts::{
    CommandId, EventId, MessageId, NodeId, ProjectId, ProviderDriverKind, ProviderInstanceId,
    ProviderSessionId, ProviderThreadId, ProviderTurnId, RunAttemptId, RunId, RuntimeRequestId,
    ScheduledTaskId, ThreadId, TurnItemId,
};

// The internal command union reuses the ported wire payloads; these aliases
// keep the orchestrator's T3 vocabulary.
pub use crate::contracts::{
    MessageDispatchCommand as MessageDispatch, RunInterruptCommand as RunInterrupt,
    RuntimeRequestRespondCommand as RuntimeRequestRespond, ThreadCreateCommand as ThreadCreate,
    ThreadModelSelectionSetCommand as ThreadModelSelectionSet,
    ThreadRuntimeModeSetCommand as ThreadRuntimeModeSet, ThreadStopCommand as ThreadStop,
};

/// Epoch milliseconds. Time enters every decision as an argument; wire records
/// carry the canonical ISO string [`iso`] produces.
pub type Timestamp = i64;

/// The ISO-8601 UTC string a T3 `DateTime.Utc` encodes.
pub fn iso(at: Timestamp) -> String {
    jiff::Timestamp::from_millisecond(at)
        .map(|timestamp| timestamp.to_string())
        .unwrap_or_else(|_| at.to_string())
}

/// Epoch milliseconds for a stored ISO timestamp; unparseable values sort first.
pub fn parse_iso(value: &str) -> Timestamp {
    value
        .parse::<jiff::Timestamp>()
        .map(|timestamp| timestamp.as_millisecond())
        .unwrap_or(0)
}

/// Whether the message came from a monitor wake.
pub fn notification_is_monitor(notification: &Notification) -> bool {
    matches!(notification.source, NotificationSource::Monitor { .. })
}

// ---------------------------------------------------------------------------
// Folded projection
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
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
    pub subagents: Vec<crate::orchestration::subagents::Subagent>,
    pub subagent_ids: BTreeSet<NodeId>,
}

impl Projection {
    pub fn new(thread: Thread) -> Self {
        Self {
            thread,
            ..Self::default()
        }
    }

    pub fn run(&self, id: impl AsRef<str>) -> Option<&Run> {
        self.runs.iter().find(|run| run.id.as_str() == id.as_ref())
    }

    pub fn message(&self, id: impl AsRef<str>) -> Option<&Message> {
        self.messages
            .iter()
            .find(|message| message.id.as_str() == id.as_ref())
    }

    pub fn attempt(&self, id: impl AsRef<str>) -> Option<&RunAttempt> {
        self.attempts
            .iter()
            .find(|attempt| attempt.id.as_str() == id.as_ref())
    }

    pub fn node(&self, id: impl AsRef<str>) -> Option<&ExecutionNode> {
        self.nodes
            .iter()
            .find(|node| node.id.as_str() == id.as_ref())
    }

    pub fn provider_thread(&self, id: impl AsRef<str>) -> Option<&ProviderThread> {
        self.provider_threads
            .iter()
            .find(|row| row.id.as_str() == id.as_ref())
    }

    pub fn provider_session(&self, id: impl AsRef<str>) -> Option<&ProviderSession> {
        self.provider_sessions
            .iter()
            .find(|row| row.id.as_str() == id.as_ref())
    }

    pub fn provider_turn(&self, id: impl AsRef<str>) -> Option<&ProviderTurn> {
        self.provider_turns
            .iter()
            .find(|row| row.id.as_str() == id.as_ref())
    }

    pub fn turn_item(&self, id: impl AsRef<str>) -> Option<&TurnItem> {
        self.turn_items.iter().find(|item| {
            item.id()
                .is_some_and(|item_id| item_id.as_str() == id.as_ref())
        })
    }

    pub fn runtime_request(&self, id: impl AsRef<str>) -> Option<&RuntimeRequest> {
        self.runtime_requests
            .iter()
            .find(|row| row.id.as_str() == id.as_ref())
    }

    pub fn active_run(&self) -> Option<&Run> {
        self.runs.iter().rev().find(|run| run.status.is_active())
    }

    pub fn active_provider_thread(&self) -> Option<&ProviderThread> {
        self.thread
            .active_provider_thread_id
            .as_ref()
            .and_then(|id| self.provider_thread(id.as_str()))
    }

    pub fn next_run_ordinal(&self) -> u32 {
        self.runs.iter().map(|run| run.ordinal).max().unwrap_or(0) + 1
    }

    pub fn next_turn_item_ordinal(&self) -> u32 {
        self.turn_items
            .iter()
            .filter_map(TurnItem::base)
            .map(|base| base.ordinal)
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
    pub fn stop_reached(&self, run_id: impl AsRef<str>) -> bool {
        self.turn_items.iter().any(|item| {
            item.base().is_some_and(|base| {
                base.run_id
                    .as_ref()
                    .is_some_and(|id| id.as_str() == run_id.as_ref())
            }) && matches!(item, TurnItem::RunInterruptRequest(_))
        })
    }

    pub fn provider_turn_capabilities(&self, run: &Run) -> Option<TurnCapabilities> {
        let provider_thread = self.provider_thread(run.provider_thread_id.as_ref()?)?;
        let session = self.provider_session(provider_thread.provider_session_id.as_ref()?)?;
        Some(session.capabilities.turns.clone())
    }

    pub fn running_provider_turn(&self, run: &Run) -> Option<&ProviderTurn> {
        let attempt_id = run.active_attempt_id.as_ref()?;
        self.provider_turns.iter().rev().find(|turn| {
            turn.run_attempt_id.as_ref() == Some(attempt_id)
                && turn.status == ProviderTurnStatus::Running
        })
    }

    pub fn apply(&mut self, event: &DomainEvent) {
        match event {
            DomainEvent::ThreadCreated(event) | DomainEvent::ThreadMetadata { event, .. } => {
                self.thread = event.payload.clone();
            }
            DomainEvent::RunCreated(event) | DomainEvent::RunUpdated(event) => {
                upsert(&mut self.runs, &event.payload, |run| run.id.as_str());
            }
            DomainEvent::RunAttemptCreated(event) | DomainEvent::RunAttemptUpdated(event) => {
                upsert(&mut self.attempts, &event.payload, |attempt| {
                    attempt.id.as_str()
                });
            }
            DomainEvent::NodeUpdated(event) => {
                upsert(&mut self.nodes, &event.payload, |node| node.id.as_str());
            }
            DomainEvent::MessageUpdated(event) => {
                upsert(&mut self.messages, &event.payload, |message| {
                    message.id.as_str()
                });
            }
            DomainEvent::TurnItemUpdated(event) => {
                let Some(id) = event.payload.id().map(|id| id.as_str().to_owned()) else {
                    return;
                };
                if let Some(existing) = self
                    .turn_items
                    .iter_mut()
                    .find(|item| item.id().is_some_and(|item_id| item_id.as_str() == id))
                {
                    *existing = event.payload.clone();
                } else {
                    self.turn_items.push(event.payload.clone());
                }
            }
            DomainEvent::SubagentUpdated(event) => {
                let Some(existing) = self
                    .subagents
                    .iter_mut()
                    .find(|subagent| subagent.id == event.payload.id)
                else {
                    self.subagent_ids.insert(event.payload.id.clone());
                    self.subagents.push(event.payload.clone());
                    return;
                };
                *existing = event.payload.clone();
            }
            DomainEvent::RuntimeRequestUpdated(event) => {
                upsert(&mut self.runtime_requests, &event.payload, |request| {
                    request.id.as_str()
                });
            }
            DomainEvent::ProviderTurnUpdated(event) => {
                upsert(&mut self.provider_turns, &event.payload, |turn| {
                    turn.id.as_str()
                });
            }
            DomainEvent::ProviderSessionAttached(event)
            | DomainEvent::ProviderSessionUpdated(event) => {
                upsert(&mut self.provider_sessions, &event.payload, |session| {
                    session.id.as_str()
                });
            }
            DomainEvent::ProviderThreadUpdated(event) => {
                upsert(&mut self.provider_threads, &event.payload, |thread| {
                    thread.id.as_str()
                });
                let provider_thread = &event.payload;
                if provider_thread.app_thread_id.as_ref() == Some(&self.thread.id)
                    && !matches!(provider_thread.status, ProviderThreadStatus::Closed)
                {
                    self.thread.active_provider_thread_id = Some(provider_thread.id.clone());
                }
            }
            DomainEvent::RunBackgroundWorkCancelled(_)
            | DomainEvent::ThreadVisitRecorded(_)
            | DomainEvent::ProviderSessionDetached(_)
            | DomainEvent::PlanUpdated(_)
            | DomainEvent::Unknown(_) => {}
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

fn run_end(candidate: &Run) -> &str {
    candidate
        .completed_at
        .as_deref()
        .unwrap_or("9999-12-31T23:59:59.999Z")
}

pub fn run_ran_after(run: &Run, other: &Run) -> bool {
    match run_end(run).cmp(run_end(other)) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Equal => run.ordinal > other.ordinal,
        std::cmp::Ordering::Less => false,
    }
}

// ---------------------------------------------------------------------------
// Internal commands
// ---------------------------------------------------------------------------

/// A dispatch limit is server policy, not part of the wire command: the
/// orchestrator receives it as a decision argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchModeLimit {
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    ThreadCreate(ThreadCreateCommand),
    MessageDispatch(MessageDispatchCommand),
    ThreadStop(ThreadStopCommand),
    RunInterrupt(RunInterruptCommand),
    RuntimeRequestRespond(RuntimeRequestRespondCommand),
    ThreadModelSelectionSet(ThreadModelSelectionSetCommand),
    ThreadRuntimeModeSet(ThreadRuntimeModeSetCommand),
    QueueResume {
        command_id: CommandId,
        thread_id: ThreadId,
    },
    QueuedRunStart {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
    },
    QueuedRunReorder {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
        before_run_id: Option<RunId>,
    },
    QueuedRunCancel {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
    },
    QueuedRunEdit {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
        text: String,
        attachments: Option<Vec<Attachment>>,
    },
    /// A close on a thread that never had a share; it changes nothing but
    /// still records a receipt.
    ThreadGuestShareClose {
        command_id: CommandId,
        thread_id: ThreadId,
    },
}

impl Command {
    pub fn command_id(&self) -> &CommandId {
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
            | Self::QueuedRunReorder { command_id, .. }
            | Self::QueuedRunCancel { command_id, .. }
            | Self::QueuedRunEdit { command_id, .. }
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
            Self::QueuedRunReorder { .. } => "queued-run.reorder",
            Self::QueuedRunCancel { .. } => "queued-run.cancel",
            Self::QueuedRunEdit { .. } => "queued-run.edit",
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
            | Self::QueuedRunReorder { thread_id, .. }
            | Self::QueuedRunCancel { thread_id, .. }
            | Self::QueuedRunEdit { thread_id, .. }
            | Self::ThreadGuestShareClose { thread_id, .. } => thread_id,
        }
    }

    /// A command that may legitimately produce no events.
    pub fn allows_empty_events(&self) -> bool {
        matches!(
            self,
            Self::ThreadStop(_)
                | Self::ThreadGuestShareClose { .. }
                | Self::QueueResume { .. }
                | Self::QueuedRunReorder { .. }
        )
    }
}

impl MessageDispatchCommand {
    /// The common client send, with every optional field unset.
    pub fn new(
        command_id: impl Into<CommandId>,
        thread_id: impl Into<ThreadId>,
        message_id: impl Into<MessageId>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            notification: None,
            guest_message_id: None,
            created_by: Actor::User,
            creation_source: CreationSource::Web,
            scheduled_task_id: None,
            sender_thread_id: None,
            linked_sender: None,
            idle_compaction: None,
            fresh_context: None,
            command_id: command_id.into(),
            thread_id: thread_id.into(),
            message_id: message_id.into(),
            text: text.into(),
            context: None,
            attachments: Vec::new(),
            title_seed: None,
            model_selection: None,
            source_plan_ref: None,
            restart_continuation_of_run_id: None,
            usage_limit_continuation_of_run_id: None,
            manual_continuation_of_run_id: None,
            steer_follow_up_of_run_id: None,
            steer_follow_up_after_run_ordinal: None,
            usage_limit_recovery_request_id: None,
            delivery_intent: None,
            delegated_completion: None,
            dispatch_mode: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Effects (the outbox)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum EffectRequest {
    #[serde(rename = "provider-turn.start")]
    ProviderTurnStart { run_id: RunId },
    #[serde(rename = "provider-turn.interrupt")]
    ProviderTurnInterrupt {
        provider_session_id: ProviderSessionId,
        provider_thread_id: ProviderThreadId,
        provider_turn_id: ProviderTurnId,
    },
    #[serde(rename = "provider-turn.steer")]
    ProviderTurnSteer {
        provider_session_id: ProviderSessionId,
        provider_thread_id: ProviderThreadId,
        provider_turn_id: ProviderTurnId,
        message_id: MessageId,
        model_selection: Option<ModelSelection>,
    },
    #[serde(rename = "provider-turn.restart")]
    ProviderTurnRestart {
        provider_session_id: ProviderSessionId,
        provider_thread_id: ProviderThreadId,
        provider_turn_id: ProviderTurnId,
        interrupted_attempt_id: RunAttemptId,
        run_id: RunId,
    },
    #[serde(rename = "runtime-request.respond")]
    RuntimeRequestRespond {
        provider_session_id: ProviderSessionId,
        request_id: RuntimeRequestId,
        decision: Option<ApprovalDecision>,
    },
    #[serde(rename = "provider-session.apply-model-selection")]
    ProviderSessionApplyModelSelection {
        provider_session_id: ProviderSessionId,
        provider_thread_id: ProviderThreadId,
        model_selection: ModelSelection,
    },
    #[serde(rename = "thread-title.generate")]
    ThreadTitleGenerate { message_id: MessageId },
    #[serde(rename = "delegated-tasks.stop")]
    DelegatedTasksStop { reason: Option<String> },
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

#[derive(Clone, Debug, PartialEq, Eq)]
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

    pub fn command_id(&self) -> Option<&CommandId> {
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
    if left.instance_id != right.instance_id || left.model != right.model {
        return false;
    }
    let canonical = |selection: &ModelSelection| {
        let mut options: Vec<(String, String)> = selection
            .options
            .as_ref()
            .map(|options| {
                options
                    .0
                    .iter()
                    .map(|option| {
                        let value = match &option.value {
                            crate::contracts::ProviderOptionValue::String(value) => {
                                format!("s:{value}")
                            }
                            crate::contracts::ProviderOptionValue::Boolean(value) => {
                                format!("b:{value}")
                            }
                        };
                        (option.id.clone(), value)
                    })
                    .collect()
            })
            .unwrap_or_default();
        options.sort();
        options
    };
    canonical(left) == canonical(right)
}

pub fn next_queued_run(projection: &Projection) -> Option<Run> {
    crate::orchestration::queued_run_order::queued_runs_in_delivery_order(projection)
        .into_iter()
        .find(|run| !run.queue_held.unwrap_or(false))
}

/// A run that still owns the thread's provider turn.
pub fn is_active_run(run: &Run) -> bool {
    run.status.is_active()
}

/// The selection the thread will be on once every queued run has started.
pub fn follows_current_conversation(message: Option<&Message>) -> bool {
    message.is_some_and(|message| {
        message.delegated_completion.is_some()
            || message.follows_thread_selection.unwrap_or(false)
            || message
                .notification
                .as_ref()
                .is_some_and(notification_is_monitor)
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
