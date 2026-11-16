use crate::model::{
    AgentCommand, ConfigOption, Goal, Item, PlanStep, Question, RateLimits, RequestId, ThreadMeta,
    TokenUsage, TurnStatus,
};

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
    /// Any other tool the agent wants to use.
    Tool {
        title: String,
        detail: Option<String>,
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

/// The agent stopped to ask the user something.
#[derive(Clone, Debug, PartialEq)]
pub struct UserInputRequest {
    pub request_id: RequestId,
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub questions: Vec<Question>,
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
        duration_ms: Option<u64>,
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
    UserInputRequested(UserInputRequest),
    /// The message the user will likely send next, or `None` when there is no good guess.
    PredictionUpdated {
        thread_id: String,
        text: Option<String>,
    },
    /// The thread's goal changed, or was cleared (`None`).
    GoalUpdated {
        thread_id: String,
        goal: Option<Goal>,
    },
    RateLimitsUpdated(RateLimits),
    /// The settings the agent offers for this session, and their values.
    ConfigUpdated {
        thread_id: String,
        options: Vec<ConfigOption>,
    },
    /// The commands the agent offers for this session.
    CommandsUpdated {
        thread_id: String,
        commands: Vec<AgentCommand>,
    },
    /// The account or its plan changed; refetch it.
    AccountChanged,
    LoginCompleted {
        success: bool,
        error: Option<String>,
    },
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
            AgentEvent::UserInputRequested(request) => Some(&request.thread_id),
            AgentEvent::PredictionUpdated { thread_id, .. }
            | AgentEvent::GoalUpdated { thread_id, .. }
            | AgentEvent::ConfigUpdated { thread_id, .. }
            | AgentEvent::CommandsUpdated { thread_id, .. } => Some(thread_id),
            AgentEvent::Error { thread_id, .. } => thread_id.as_deref(),
            AgentEvent::Warning(_)
            | AgentEvent::Disconnected { .. }
            | AgentEvent::RateLimitsUpdated(_)
            | AgentEvent::AccountChanged
            | AgentEvent::LoginCompleted { .. } => None,
        }
    }
}
