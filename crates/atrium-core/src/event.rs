use crate::model::{
    AgentCommand, ConfigOption, Goal, Item, PlanStep, Question, RateLimits, RequestId, ThreadMeta,
    TokenUsage, TurnStatus,
};
use serde::{Deserialize, Serialize};

/// What the backend wants approved before it goes on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// The same event, attributed to `thread_id`.
    ///
    /// A runtime that owns sessions itself gives each session its own id but
    /// receives the provider's native thread id on every event. Retargeting
    /// keeps the app's bookkeeping (keyed by the runtime's session id) and the
    /// provider's identity separate; the runtime keeps the native id for
    /// resume and handoff.
    pub fn with_thread_id(self, thread_id: &str) -> Self {
        let id = || thread_id.to_string();
        match self {
            AgentEvent::ThreadStarted(mut meta) => {
                meta.id = id();
                AgentEvent::ThreadStarted(meta)
            }
            AgentEvent::ThreadNameUpdated { name, .. } => AgentEvent::ThreadNameUpdated {
                thread_id: id(),
                name,
            },
            AgentEvent::TurnStarted { turn_id, .. } => AgentEvent::TurnStarted {
                thread_id: id(),
                turn_id,
            },
            AgentEvent::TurnCompleted {
                turn_id,
                status,
                error,
                duration_ms,
                ..
            } => AgentEvent::TurnCompleted {
                thread_id: id(),
                turn_id,
                status,
                error,
                duration_ms,
            },
            AgentEvent::ItemStarted { turn_id, item, .. } => AgentEvent::ItemStarted {
                thread_id: id(),
                turn_id,
                item,
            },
            AgentEvent::ItemCompleted { turn_id, item, .. } => AgentEvent::ItemCompleted {
                thread_id: id(),
                turn_id,
                item,
            },
            AgentEvent::AgentMessageDelta {
                turn_id,
                item_id,
                delta,
                ..
            } => AgentEvent::AgentMessageDelta {
                thread_id: id(),
                turn_id,
                item_id,
                delta,
            },
            AgentEvent::ReasoningDelta {
                turn_id,
                item_id,
                delta,
                ..
            } => AgentEvent::ReasoningDelta {
                thread_id: id(),
                turn_id,
                item_id,
                delta,
            },
            AgentEvent::CommandOutputDelta {
                turn_id,
                item_id,
                delta,
                ..
            } => AgentEvent::CommandOutputDelta {
                thread_id: id(),
                turn_id,
                item_id,
                delta,
            },
            AgentEvent::PlanUpdated {
                turn_id,
                explanation,
                steps,
                ..
            } => AgentEvent::PlanUpdated {
                thread_id: id(),
                turn_id,
                explanation,
                steps,
            },
            AgentEvent::TokenUsage { usage, .. } => AgentEvent::TokenUsage {
                thread_id: id(),
                usage,
            },
            AgentEvent::ApprovalRequested(mut request) => {
                request.thread_id = id();
                AgentEvent::ApprovalRequested(request)
            }
            AgentEvent::UserInputRequested(mut request) => {
                request.thread_id = id();
                AgentEvent::UserInputRequested(request)
            }
            AgentEvent::PredictionUpdated { text, .. } => AgentEvent::PredictionUpdated {
                thread_id: id(),
                text,
            },
            AgentEvent::GoalUpdated { goal, .. } => AgentEvent::GoalUpdated {
                thread_id: id(),
                goal,
            },
            AgentEvent::ConfigUpdated { options, .. } => AgentEvent::ConfigUpdated {
                thread_id: id(),
                options,
            },
            AgentEvent::CommandsUpdated { commands, .. } => AgentEvent::CommandsUpdated {
                thread_id: id(),
                commands,
            },
            AgentEvent::ApprovalResolved { request_id, .. } => AgentEvent::ApprovalResolved {
                thread_id: id(),
                request_id,
            },
            AgentEvent::Error {
                message,
                will_retry,
                ..
            } => AgentEvent::Error {
                thread_id: Some(id()),
                message,
                will_retry,
            },
            // These carry no thread id; they are unchanged.
            other => other,
        }
    }

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
