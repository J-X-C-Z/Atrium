//! The orchestration MCP toolkit agents and outside clients call.
//!
//! Ported from `packages/contracts/src/orchestratorMcp.ts` and the tool
//! handlers in `apps/server/src/mcp/` (T3 Code v0.1.11, MIT). The toolkit
//! reads the engine's projections and changes them only through the ported
//! orchestration commands, so the handlers hold no orchestration logic of
//! their own.
//!
//! [`tools::tool_specs`] returns T3's tool names and input schemas.
//! [`McpService`] answers a call with T3's `structuredContent` shapes.
//! Atrium's `atrium` MCP server merges this toolkit with the project tools in
//! `crates/atrium-project/src/mcp.rs`: project task files divide a project's
//! work, and `delegate_task` is for ad-hoc help outside that structure.

pub mod service;
pub mod tools;
pub mod types;

#[cfg(test)]
mod tests;

pub use service::{Clock, McpConfig, McpService, SystemClock};
pub use types::*;
