//! A client for `codex app-server`, the JSON-RPC interface Codex exposes for
//! rich clients (the same one its IDE extensions use).
//!
//! * [`rpc`] is the transport: newline-delimited JSON-RPC over any reader and
//!   writer, with request/response matching. No async runtime is assumed:
//!   IO runs on plain threads and results come back through `futures`
//!   channels, so it works under GPUI's executor or any other.
//! * [`process`] finds the `codex` binary (also when a macOS app is started
//!   from Finder with a bare `PATH`) and spawns it.
//! * [`CodexClient`] is the typed API the app calls.
//! * [`translate`] turns Codex notifications and requests into
//!   [`agentdesk_core::AgentEvent`]s.

pub mod process;
pub mod rpc;
pub mod translate;

mod client;

pub use client::{CodexClient, CodexConfig, ModelInfo, StartThread};
pub use rpc::{Incoming, RpcError};
