//! A client for agents that speak the Agent Client Protocol (ACP): Claude
//! Code (through `claude-agent-acp`), DeepSeek Harness (`dsh --profile acp`),
//! Google Antigravity (`agy acp`), Gemini CLI and others.
//!
//! * [`AcpClient`] starts the agent, runs the handshake and offers typed
//!   session calls.
//! * [`Translator`] turns ACP's stream of `session/update`s into
//!   [`atrium_core::AgentEvent`]s. ACP has no turn or item ids, so it
//!   makes them: a turn per prompt, a message per run of text chunks, an
//!   item per tool call; when history is replayed, user messages split turns.

mod client;
mod translate;

pub use client::{AcpClient, AcpConfig, AgentInfo, PromptDelivery, PromptOutcome, SessionStart};
pub use translate::{Translator, config_options};
