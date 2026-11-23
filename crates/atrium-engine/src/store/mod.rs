// Ported from apps/server/src/orchestration-v2/EventStore.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/orchestration-v2/ProjectionStore.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/persistence/Migrations/055_OrchestrationV2.ts (T3 Code v0.1.11, MIT).
//!
//! The append-only application event log and its projections.
//!
//! T3 keeps two stores over one SQLite database: `OrchestrationEventStore`
//! writes the shared `orchestration_events` log (project events and V2 thread
//! events), and `ProjectionStore` folds the same domain events into indexed
//! projection rows that carry the original payload JSON. This port keeps the
//! same tables, columns and keys; it is synchronous (`rusqlite`) and has no
//! async or effect runtime.
//!
//! The orchestration layer writes a command through one path,
//! [`EventStore::apply`]: events, the command receipt and every projection row
//! commit in a single `BEGIN IMMEDIATE` transaction, so a failure leaves no
//! half-applied command behind.

mod events;
mod migrations;
mod projections;

#[cfg(test)]
mod tests;

pub use events::{
    ApplyCommand, ApplyResult, CommandReceipt, EventStore, NewEvent, NewProjectEvent, ReadOptions,
    StoreError, append_events, append_project_event, latest_application_sequence, latest_sequence,
    read, read_by_command_id, read_receipt, upsert_receipt,
};
pub use migrations::migrate;
pub use projections::{
    ActivityProjection, MessageProjection, NodeProjection, PendingApprovalProjection,
    ProjectionStore, ProjectionWrite, ProjectorState, ProviderSessionProjection,
    RunAttemptProjection, RunProjection, ThreadProjection, TurnProjection,
};
