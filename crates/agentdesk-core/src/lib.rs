//! The backend-agnostic heart of AgentDesk.
//!
//! Every backend (Codex today; Claude Code, DeepSeek Harness and ACP agents
//! later) translates its own wire protocol into [`AgentEvent`]s. The UI never
//! sees a backend's protocol: it folds events into a [`Transcript`] and draws
//! that. Keeping this crate free of UI and IO makes it trivially testable and
//! lets a headless orchestrator reuse it unchanged.

mod event;
mod model;
mod transcript;

pub use event::{AgentEvent, ApprovalDecision, ApprovalKind, ApprovalRequest};
pub use model::{
    ChangeKind, FileChange, Item, ItemStatus, PlanStep, RequestId, ThreadMeta, TokenUsage,
    TurnStatus,
};
pub use transcript::{Entry, EntryKind, Transcript};
