use crate::model::{Item, PlanStep, RequestId, ThreadMeta, TokenUsage, TurnStatus};

/// What the backend wants approved before it goes on.
#[derive(Clone, Debug, PartialEq)]
pub enum ApprovalKind {
    Command {
        command: String,
        cwd: String,
        reason: Option<String>,
    },
    FileChange {
        reason: Option<String>,
        grant_root: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ApprovalRequest {
    /// Echoed back with the decision.
    pub request_id: RequestId,
    pub thread_id: String,
    pub turn_id: String,
    /// The item the approval gates, when the backend names one.
    pub item_id: String,
    pub kind: ApprovalKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Allow this once.
    Accept,
    /// Allow this and anything like it for the rest of the session.
    AcceptForSession,
    /// Refuse; the agent carries on without it.
    Decline,
    /// Refuse and stop the turn.
    Cancel,
}

/// Everything a backend can tell the UI, after translation from its protocol.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentEvent {
    ThreadStarted(ThreadMeta),
    ThreadNameUpdated {
        thread_id: String,
        name: Option<String>,
    },
    TurnStarted {
        thread_id: String,
        turn_id: String,
    },
    TurnCompleted {
        thread_id: String,
        turn_id: String,
        status: TurnStatus,
        error: Option<String>,
    },
    /// An item began, or was replaced wholesale with newer state.
    ItemStarted {
        thread_id: String,
        turn_id: String,
        item: Item,
    },
    /// An item's final state.
    ItemCompleted {
        thread_id: String,
        turn_id: String,
        item: Item,
    },
    AgentMessageDelta {
        thread_id: String,
        turn_id: String,
        item_id: String,
        delta: String,
    },
    ReasoningDelta {
        thread_id: String,
        turn_id: String,
        item_id: String,
        delta: String,
    },
    CommandOutputDelta {
        thread_id: String,
        turn_id: String,
        item_id: String,
        delta: String,
    },
    PlanUpdated {
        thread_id: String,
        turn_id: String,
        explanation: Option<String>,
        steps: Vec<PlanStep>,
    },
    TokenUsage {
        thread_id: String,
        usage: TokenUsage,
    },
    ApprovalRequested(ApprovalRequest),
    /// The backend no longer waits on this request (answered elsewhere, or the turn ended).
    ApprovalResolved {
        thread_id: String,
        request_id: RequestId,
    },
    Error {
        thread_id: Option<String>,
        message: String,
        will_retry: bool,
    },
    Warning(String),
    /// The backend process ended; nothing more will arrive.
    Disconnected {
        reason: String,
    },
}

impl AgentEvent {
    /// The thread this event belongs to, if any.
    pub fn thread_id(&self) -> Option<&str> {
        match self {
            AgentEvent::ThreadStarted(meta) => Some(&meta.id),
            AgentEvent::ThreadNameUpdated { thread_id, .. }
            | AgentEvent::TurnStarted { thread_id, .. }
            | AgentEvent::TurnCompleted { thread_id, .. }
            | AgentEvent::ItemStarted { thread_id, .. }
            | AgentEvent::ItemCompleted { thread_id, .. }
            | AgentEvent::AgentMessageDelta { thread_id, .. }
            | AgentEvent::ReasoningDelta { thread_id, .. }
            | AgentEvent::CommandOutputDelta { thread_id, .. }
            | AgentEvent::PlanUpdated { thread_id, .. }
            | AgentEvent::TokenUsage { thread_id, .. }
            | AgentEvent::ApprovalResolved { thread_id, .. } => Some(thread_id),
            AgentEvent::ApprovalRequested(request) => Some(&request.thread_id),
            AgentEvent::Error { thread_id, .. } => thread_id.as_deref(),
            AgentEvent::Warning(_) | AgentEvent::Disconnected { .. } => None,
        }
    }
}
