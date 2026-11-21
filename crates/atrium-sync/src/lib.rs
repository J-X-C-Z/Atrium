//! Replicated Atrium overlays across a user's devices.
//!
//! The store holds last-writer-wins registers keyed by strings, stamped with
//! hybrid logical clocks. It is deliberately free of GPUI, HTTP and async:
//! the node serves it, the app migrates into it, and both can test it without
//! a running agent.
//!
//! What is replicated is Atrium's own overlay: titles, pins, hidden flags,
//! project membership, environments, MCP server definitions without their
//! secrets, schedules and scheduler leases. Agent sessions are not copied.

mod cache;
mod clock;
mod dto;
mod lease;
mod migrate;
mod model;
mod peer;
mod store;

pub use cache::{CacheEntry, ThreadCache};
pub use clock::{Hlc, HybridClock};
pub use dto::{
    AnswerInput, ApprovalAnswer, DeviceDirectory, MovePackage, ThreadAnnouncement, ThreadEvent,
    ThreadSnapshot, TurnInput,
};
pub use lease::{
    LEASE_RENEW_MS, LEASE_TAKEOVER_GRACE_MS, Lease, LeaseDecision, lease_can_be_taken,
    lease_is_live,
};
pub use migrate::{SettingsView, migrate_settings};
pub use model::{
    Op, Register, SYNC_VERSION, TOMBSTONE_RETENTION_MS, Vector, is_secret_key, parse_thread_key,
    thread_key, validate_value,
};
pub use peer::{PeerClient, PeerError, SyncOutcome};
pub use store::{Store, StoreError};
