//! The wire and storage contract ported from T3 Code.
//!
//! Seed types only: the port of `packages/contracts/src` replaces this module
//! (item 32), keeping the T3 field names, unions and defaults. Everything the
//! event store and the orchestration layer share lives here.

use serde::{Deserialize, Serialize};

pub type ThreadId = String;
pub type ProjectId = String;
pub type RunId = String;
pub type TurnId = String;
pub type ItemId = String;
pub type CommandId = String;
pub type EventId = String;

/// One row of `orchestration_v2_events`, as the store hands it back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredEvent {
    pub sequence: i64,
    pub event_id: EventId,
    pub command_id: Option<CommandId>,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub event_type: String,
    pub occurred_at: String,
    pub payload: serde_json::Value,
}
