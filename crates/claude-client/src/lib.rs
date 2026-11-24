//! The Claude Agent SDK's side of the `claude` CLI's streaming-JSON protocol.
//!
//! T3 Code drives Claude Code through `@anthropic-ai/claude-agent-sdk`
//! (`apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts`). This
//! crate implements the same wire side in Rust so an Atrium adapter can talk
//! to the `claude` binary directly:
//!
//! * [`ClaudeClient`] spawns the CLI, sends the `initialize` control request,
//!   writes user turns (text and images), answers `can_use_tool` permission
//!   requests, interrupts, resumes by session id, switches model/permission
//!   mode, and asks for usage/context data.
//! * [`events`] translates the CLI's stream-JSON frames into a typed
//!   [`ClaudeEvent`] enum. It deliberately does not depend on
//!   `atrium_core`; item 23's adapter maps it later.
//! * [`usage`] ports T3's Claude usage-limit classification and rate-limit
//!   window mapping.
//! * `mock-claude` is a scripted CLI for tests and development.
//!
//! The protocol shapes are taken from the Agent SDK 0.3.321 implementation
//! (the version T3 0.1.11 pins) and from T3's recorded fixtures under
//! `apps/server/src/orchestration-v2/testkit/fixtures/*/claude_transcript.ndjson`.

pub mod args;
pub mod events;
pub mod exe;
pub mod protocol;
pub mod usage;

mod client;

pub use args::{LaunchArgs, Tools, build_args};
pub use client::{ClaudeClient, ClaudeConfig, Incoming, TurnOptions};
pub use events::{
    AssistantMessage, ClaudeEvent, ContentBlock, Effort, PermissionMode, StreamEvent, TurnResult,
    TurnStatus, UserEvent,
};
pub use exe::{Platform, resolve_claude_sdk_executable_path};
pub use protocol::{
    ControlRequestBody, ControlRequestEnvelope, ControlResponseBody, ControlResponseEnvelope,
    ImageSource, PermissionDecision, PermissionRequest, SessionInfo, SlashCommand,
    UserContentBlock, UserMessage, UserTurnInput,
};
pub use usage::{
    ScopedLimitNames, UsageLimits, UsageLimitsUpdate, UsageResponse, UsageWindow,
    claude_rate_limit_event_to_update, claude_usage_response_to_limits,
};
