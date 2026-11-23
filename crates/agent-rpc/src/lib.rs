//! The plumbing every agent backend shares: newline-delimited JSON-RPC over
//! a child process's stdio ([`rpc`]), and finding and starting that child
//! ([`process`]), also when a macOS app starts with a bare `PATH`.

pub mod claude_usage;
pub mod node;
pub mod process;
pub mod rpc;

pub use rpc::{Connection, Incoming, RpcError};
