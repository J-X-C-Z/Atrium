// Ported from packages/contracts/src/index.ts (T3 Code v0.1.11, MIT).
//! The wire and storage contract ported from T3 Code's `packages/contracts`.
//!
//! The engine's store and orchestration layer speak these types. Every struct
//! keeps T3's JSON field names (`camelCase`), every union keeps T3's tag
//! values, and every date is the ISO string T3's `DateTimeUtc` encodes. An
//! unknown union member is decode-only, as in T3: it keeps its raw JSON so the
//! caller can skip it and never serializes back to the wire.
//!
//! # Ported sources
//!
//! * [`base`] — `baseSchemas.ts`
//! * [`model`] — `modelSelection.ts`, `model.ts`
//! * [`chat`] — `chatAttachment.ts`, `composerContext.ts`
//! * [`provider`] — `providerPolicy.ts`, `providerInstance.ts`, `orchestrationV2.ts`
//! * [`capabilities`] — `orchestrationV2.ts`
//! * [`checkpoint`] — `orchestrationV2.ts` (checkpoint scope and record)
//! * [`transfer`] — `orchestrationV2.ts` (context source points and transfers)
//! * [`thread`] — `orchestrationV2.ts`, `threadTitle.ts`, `threadTags.ts`
//! * [`run`] — `orchestrationV2.ts`
//! * [`item`] — `orchestrationV2.ts`
//! * [`event`] — `orchestrationV2.ts`
//! * [`command`] — `orchestrationV2.ts`
//!
//! # Deliberately unported
//!
//! * Web/UI-only schemas: terminal, speech, preview automation, relay, theme,
//!   browser profiles, keybindings and settings.
//! * `OrchestrationV2Subagent` and `subagent.updated`; the subagent turn item
//!   is ported, the projection record belongs with item 20.
//! * Context handoff records and the checkpoint/context-transfer events; the
//!   scope, checkpoint and transfer records are ported for item 27.
//! * `ThreadGuestShare`, pull-request link/snapshot/stack and issue link
//!   records; on the thread they travel as raw JSON so the wire shape stays
//!   intact until their own modules land.
//! * `ThreadTokenUsageSnapshot` and `TurnTokenUsage` (item 23); they travel as
//!   raw JSON on provider threads and turns.
//! * Command variants outside the core subset (guest shares, PR/issue links,
//!   settle/snooze/pin/tag, fork, delegation, checkpoint rollback, queue
//!   editing, provider switch) and the server-only internal command union.
//! * `tools` arrays and provider option/descriptor catalogs.
//!
//! A T3 optional-and-nullable field (`Schema.optional(Schema.NullOr(...))`)
//! decodes both `null` and absence; re-encoding emits `null` for a required
//! nullable field and omits an optional one, which is semantically identical
//! to T3's encode of either input.

pub mod base;
pub mod capabilities;
pub mod chat;
pub mod checkpoint;
pub mod command;
pub mod event;
pub mod item;
pub mod model;
pub mod provider;
pub mod run;
pub mod thread;
pub mod transfer;

pub use base::*;
pub use capabilities::*;
pub use chat::*;
pub use checkpoint::*;
pub use command::*;
pub use event::*;
pub use item::*;
pub use model::*;
pub use provider::*;
pub use run::*;
pub use thread::*;
pub use transfer::*;

// T3 type names, for callers reading the upstream file beside this module.
pub type OrchestrationV2AppThread = AppThread;
pub type OrchestrationV2Run = Run;
pub type OrchestrationV2RunAttempt = RunAttempt;
pub type OrchestrationV2ExecutionNode = ExecutionNode;
pub type OrchestrationV2ProviderSession = ProviderSession;
pub type OrchestrationV2ProviderThread = ProviderThread;
pub type OrchestrationV2ProviderTurn = ProviderTurn;
pub type OrchestrationV2ProviderCapabilities = ProviderCapabilities;
pub type OrchestrationV2RuntimeRequest = RuntimeRequest;
pub type OrchestrationV2ConversationMessage = ConversationMessage;
pub type OrchestrationV2TurnItem = TurnItem;
pub type OrchestrationV2PlanArtifact = PlanArtifact;
pub type OrchestrationV2DomainEvent = DomainEvent;

use serde::{Deserialize, Serialize};

/// One row of `orchestration_v2_events`, as the store hands it back.
///
/// This is Atrium's seed row, not a T3 type: `payload` stays raw JSON so an
/// event this build cannot read is still stored and skipped.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_event_keeps_its_seed_shape() {
        let event = StoredEvent {
            sequence: 1,
            event_id: EventId::new("event-1"),
            command_id: Some(CommandId::new("command-1")),
            thread_id: ThreadId::new("thread-1"),
            run_id: None,
            event_type: "run.created".to_owned(),
            occurred_at: "2026-04-20T00:00:00.000Z".to_owned(),
            payload: serde_json::json!({ "id": "run-1" }),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["event_id"], serde_json::json!("event-1"));
        let again: StoredEvent = serde_json::from_value(value).unwrap();
        assert_eq!(again, event);
    }

    #[test]
    fn every_id_round_trips_its_wire_value() {
        assert_eq!(
            serde_json::to_string(&ApprovalRequestId::new("approval-1")).unwrap(),
            "\"approval-1\""
        );
        assert_eq!(
            serde_json::from_str::<CheckpointRef>("\"refs/checkpoints/1\"").unwrap(),
            CheckpointRef::new("refs/checkpoints/1")
        );
    }
}
