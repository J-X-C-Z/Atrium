//! Atrium's port of T3 Code's server core.
//!
//! Ported from pingdotgg/t3code v0.1.11 (MIT). Each ported file names its T3
//! source in a header; [`LICENSE-T3`](../LICENSE-T3) keeps the original
//! notice and [`UPSTREAM.md`](../UPSTREAM.md) pins the release and the source
//! path of every module.
//!
//! The engine is GPUI-free. The app reaches it through `atrium-runtime`, the
//! same trait today's clients implement, so the project, workflow and team
//! layers do not change when a module moves from track A to the port.

pub mod acp_runtime;
pub mod checkpoint;
pub mod contracts;
pub mod diff;
pub mod mcp;
pub mod orchestration;
pub mod providers;
pub mod runtime;
pub mod store;
pub mod usage;
