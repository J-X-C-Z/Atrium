//! The backend-agnostic heart of Atrium.
//!
//! Every backend (Codex today; Claude Code, DeepSeek Harness and ACP agents
//! later) translates its own wire protocol into [`AgentEvent`]s. The UI never
//! sees a backend's protocol: it folds events into a [`Transcript`] and draws
//! that. Keeping this crate free of UI and IO makes it trivially testable and
//! lets a headless orchestrator reuse it unchanged.

mod capabilities;
mod claude_limits;
mod event;
mod model;
mod pace;
mod transcript;

pub use capabilities::Capabilities;
pub use claude_limits::{parse_meta_limits, parse_oauth_usage};
pub use event::{AgentEvent, ApprovalDecision, ApprovalKind, ApprovalRequest, UserInputRequest};
pub use model::{
    Account, AgentCommand, ChangeKind, ConfigChoice, ConfigOption, Cost, Credits, FileChange, Goal,
    GoalStatus, Item, ItemStatus, McpServer, PlanStep, Question, RateLimits, RateWindow, RequestId,
    ThreadMeta, TokenCounts, TokenUsage, TurnStatus, plan_label,
};
pub use pace::{TurnPace, estimate_tokens};
pub use transcript::{Entry, EntryKind, Transcript, TurnSummary};
