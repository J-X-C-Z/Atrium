// Ported from apps/server/src/orchestration-v2/EventSink.ts and apps/server/src/orchestration-v2/CommandReceiptStore.ts (T3 Code v0.1.11, MIT).

//! The `Store` seam: one transaction commits events, projections, a command
//! receipt and outbox effects.
//!
//! T3's `EventSink` owns the transaction; `EventStore`, `ProjectionStore`,
//! `CommandReceiptStore` and `EffectOutbox` are the parts inside it. This
//! module exposes the same contract as a trait, so the SQLite store module can
//! implement it later (no SQLite here). [`MemoryStore`] is the reference
//! implementation the decision tests run on.
//!
//! Receipt semantics ported from `EventSink.commitCommand` and
//! `Orchestrator.dispatch`:
//! - the first command with an id reserves it with `insertIfAbsent`;
//! - a replay returns the recorded sequence and its events without appending;
//! - a replay for another thread is a `CommandIdConflict`, never a success;
//! - a rejection is recorded so the retry reports the same error.

use std::collections::HashMap;

use super::effect_worker::OutboxQueue;
use super::types::{CommandId, DomainEvent, PendingEffect, Projection, ThreadId, Timestamp};

/// One appended event's id.
pub use super::types::EventId;

#[derive(Clone, Debug, PartialEq)]
pub struct StoredEvent {
    pub sequence: i64,
    pub command_id: Option<CommandId>,
    pub event: DomainEvent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptStatus {
    Accepted,
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandReceipt {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub command_type: String,
    pub accepted_at: Timestamp,
    pub result_sequence: i64,
    pub status: ReceiptStatus,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelUnsettled {
    pub effect_types: Vec<String>,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WriteInput {
    pub command_id: Option<CommandId>,
    pub events: Vec<DomainEvent>,
    pub effects: Vec<PendingEffect>,
    pub now: Timestamp,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommitCommandInput {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub command_type: String,
    pub accepted_at: Timestamp,
    pub events: Vec<DomainEvent>,
    pub effects: Vec<PendingEffect>,
    pub cancel_unsettled: Option<CancelUnsettled>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommitCommandOutput {
    pub receipt: CommandReceipt,
    pub stored_events: Vec<StoredEvent>,
    pub committed: bool,
    pub cancelled_effect_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RejectedCommandInput {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub command_type: String,
    pub rejected_at: Timestamp,
    pub error: String,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("store failed: {0}")]
    Failed(String),
}

/// What the orchestration core needs from persistence.
///
/// Implementations must commit a command atomically: either the receipt, the
/// events, the projection writes and the effects all land, or none do.
pub trait Store {
    type Error: std::error::Error + Send + Sync + 'static;

    /// A consistent snapshot of one thread's projection.
    fn projection(&self, thread_id: &ThreadId) -> Option<Projection>;

    /// The latest committed sequence, globally or for one thread.
    fn latest_sequence(&self, thread_id: Option<&ThreadId>) -> i64;

    /// Events recorded under a command id, in sequence order.
    fn read_by_command_id(&self, command_id: &CommandId) -> Vec<StoredEvent>;

    fn command_receipt(&self, command_id: &CommandId) -> Option<CommandReceipt>;

    /// Append non-command events (provider output, control writes) atomically.
    fn write(&mut self, input: WriteInput) -> Result<Vec<StoredEvent>, Self::Error>;

    /// The one transaction a command needs.
    fn commit_command(
        &mut self,
        input: CommitCommandInput,
    ) -> Result<CommitCommandOutput, Self::Error>;

    fn commit_rejected_command(
        &mut self,
        input: RejectedCommandInput,
    ) -> Result<CommandReceipt, Self::Error>;
}

/// In-memory reference store for the pure decision tests.
#[derive(Debug, Default)]
pub struct MemoryStore {
    events: Vec<StoredEvent>,
    receipts: HashMap<CommandId, CommandReceipt>,
    projections: HashMap<ThreadId, Projection>,
    next_sequence: i64,
    outbox: OutboxQueue,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn projection_snapshot(&self, thread_id: &str) -> Option<&Projection> {
        self.projections.get(&ThreadId::new(thread_id))
    }

    pub fn events(&self) -> &[StoredEvent] {
        &self.events
    }

    pub fn outbox(&self) -> &OutboxQueue {
        &self.outbox
    }

    pub fn outbox_mut(&mut self) -> &mut OutboxQueue {
        &mut self.outbox
    }

    pub fn receipt(&self, command_id: &str) -> Option<&CommandReceipt> {
        self.receipts.get(&CommandId::new(command_id))
    }

    fn append_events(
        &mut self,
        command_id: Option<&CommandId>,
        events: Vec<DomainEvent>,
    ) -> Vec<StoredEvent> {
        let mut stored = Vec::with_capacity(events.len());
        for event in events {
            self.next_sequence += 1;
            let projected_thread_ids = event.projected_thread_ids();
            for thread_id in projected_thread_ids {
                let projection = self.projections.entry(thread_id).or_default();
                projection.apply(&event);
            }
            stored.push(StoredEvent {
                sequence: self.next_sequence,
                command_id: command_id.cloned(),
                event,
            });
        }
        self.events.extend(stored.iter().cloned());
        stored
    }
}

impl Store for MemoryStore {
    type Error = StoreError;

    fn projection(&self, thread_id: &ThreadId) -> Option<Projection> {
        self.projections.get(thread_id).cloned()
    }

    fn latest_sequence(&self, thread_id: Option<&ThreadId>) -> i64 {
        thread_id.map_or(self.next_sequence, |thread_id| {
            self.events
                .iter()
                .rev()
                .find(|stored| stored.event.thread_id() == Some(thread_id))
                .map_or(0, |stored| stored.sequence)
        })
    }

    fn read_by_command_id(&self, command_id: &CommandId) -> Vec<StoredEvent> {
        self.events
            .iter()
            .filter(|stored| stored.command_id.as_ref() == Some(command_id))
            .cloned()
            .collect()
    }

    fn command_receipt(&self, command_id: &CommandId) -> Option<CommandReceipt> {
        self.receipts.get(command_id).cloned()
    }

    fn write(&mut self, input: WriteInput) -> Result<Vec<StoredEvent>, Self::Error> {
        let stored = self.append_events(input.command_id.as_ref(), input.events);
        self.outbox.enqueue(input.effects, input.now);
        Ok(stored)
    }

    fn commit_command(
        &mut self,
        input: CommitCommandInput,
    ) -> Result<CommitCommandOutput, Self::Error> {
        if let Some(existing) = self.receipts.get(&input.command_id).cloned() {
            return Ok(CommitCommandOutput {
                stored_events: self.read_by_command_id(&input.command_id),
                receipt: existing,
                committed: false,
                cancelled_effect_count: 0,
            });
        }

        let stored_events = self.append_events(Some(&input.command_id), input.events);
        self.outbox.enqueue(input.effects, input.accepted_at);
        let cancelled_effect_count = match &input.cancel_unsettled {
            Some(cancel) => {
                let types: Vec<&str> = cancel.effect_types.iter().map(String::as_str).collect();
                self.outbox
                    .cancel_unsettled(&input.thread_id, &types, &cancel.reason, input.accepted_at)
                    .len()
            }
            None => 0,
        };
        let result_sequence = stored_events.last().map_or_else(
            || self.latest_sequence(Some(&input.thread_id)),
            |stored| stored.sequence,
        );
        let receipt = CommandReceipt {
            command_id: input.command_id.clone(),
            thread_id: input.thread_id,
            command_type: input.command_type,
            accepted_at: input.accepted_at,
            result_sequence,
            status: ReceiptStatus::Accepted,
            error: None,
        };
        self.receipts.insert(input.command_id, receipt.clone());
        Ok(CommitCommandOutput {
            receipt,
            stored_events,
            committed: true,
            cancelled_effect_count,
        })
    }

    fn commit_rejected_command(
        &mut self,
        input: RejectedCommandInput,
    ) -> Result<CommandReceipt, Self::Error> {
        if let Some(existing) = self.receipts.get(&input.command_id) {
            return Ok(existing.clone());
        }
        let receipt = CommandReceipt {
            command_id: input.command_id.clone(),
            thread_id: input.thread_id.clone(),
            command_type: input.command_type,
            accepted_at: input.rejected_at,
            result_sequence: self.latest_sequence(Some(&input.thread_id)),
            status: ReceiptStatus::Rejected,
            error: Some(input.error),
        };
        self.receipts.insert(input.command_id, receipt.clone());
        Ok(receipt)
    }
}

/// A command's stored effect (unused by the memory store; the SQLite store will
/// read it back per command). Kept so the public seam matches T3's
/// `EventSink.listByCommandId`.
pub fn effects_for_command(outbox: &OutboxQueue, command_id: &str) -> Vec<PendingEffect> {
    outbox
        .effects()
        .iter()
        .filter(|effect| effect.command_id.as_str() == command_id)
        .map(|effect| PendingEffect {
            id: effect.id.clone(),
            command_id: effect.command_id.clone(),
            thread_id: effect.thread_id.clone(),
            request: effect.request.clone(),
            available_at: Some(effect.available_at),
        })
        .collect()
}

/// Apply one stored event to an independent projection, for callers that fold
/// a snapshot without a store (T3's `applyToProjection`).
pub fn apply_to_projection(projection: &mut Projection, event: &DomainEvent) {
    projection.apply(event);
}

/// The event-type filter T3's `stream` uses; kept pure so a subscriber can
/// decide without an event loop.
pub fn event_matches(
    event: &DomainEvent,
    thread_id: Option<&str>,
    event_type: Option<&str>,
) -> bool {
    thread_id.is_none_or(|thread_id| event.thread_id().is_some_and(|id| id.as_str() == thread_id))
        && event_type.is_none_or(|event_type| event.type_name() == event_type)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::types::{
        Actor, CreationSource, DomainEventBase, EventPayload, InteractionMode, ModelSelection,
        RuntimeMode, Thread,
    };

    fn created(thread_id: &str) -> DomainEvent {
        DomainEvent::ThreadCreated(EventPayload {
            base: DomainEventBase {
                id: EventId::new(format!("event:created:{thread_id}")),
                thread_id: ThreadId::new(thread_id),
                run_id: None,
                node_id: None,
                driver: None,
                provider_instance_id: None,
                raw_event_id: None,
                occurred_at: "1970-01-01T00:00:00.001Z".into(),
            },
            payload: Thread {
                id: thread_id.into(),
                project_id: "project:store".into(),
                title: thread_id.into(),
                provider_instance_id: "codex".into(),
                model_selection: ModelSelection::new("codex", "gpt"),
                runtime_mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Default,
                created_by: Actor::User,
                creation_source: CreationSource::Web,
                created_at: "1970-01-01T00:00:00.001Z".into(),
                updated_at: "1970-01-01T00:00:00.001Z".into(),
                ..Thread::default()
            },
        })
    }

    #[test]
    fn one_commit_writes_events_projections_and_receipt() {
        let mut store = MemoryStore::new();
        let output = store
            .commit_command(CommitCommandInput {
                command_id: "command:create".into(),
                thread_id: "thread:a".into(),
                command_type: "thread.create".into(),
                accepted_at: 1,
                events: vec![created("thread:a")],
                effects: Vec::new(),
                cancel_unsettled: None,
            })
            .expect("commit");
        assert!(output.committed);
        assert_eq!(output.receipt.result_sequence, 1);
        assert_eq!(output.stored_events.len(), 1);
        assert_eq!(
            store
                .projection_snapshot("thread:a")
                .expect("projection")
                .thread
                .id,
            "thread:a"
        );

        // A replay appends nothing and returns the same sequence.
        let replay = store
            .commit_command(CommitCommandInput {
                command_id: "command:create".into(),
                thread_id: "thread:a".into(),
                command_type: "thread.create".into(),
                accepted_at: 2,
                events: vec![created("thread:a")],
                effects: Vec::new(),
                cancel_unsettled: None,
            })
            .expect("replay");
        assert!(!replay.committed);
        assert_eq!(
            replay.receipt.result_sequence,
            output.receipt.result_sequence
        );
        assert_eq!(replay.stored_events, output.stored_events);
        assert_eq!(store.events().len(), 1);
    }

    #[test]
    fn a_receipt_records_the_thread_that_committed_first() {
        let mut store = MemoryStore::new();
        store
            .commit_command(CommitCommandInput {
                command_id: "command:shared".into(),
                thread_id: "thread:a".into(),
                command_type: "thread.create".into(),
                accepted_at: 1,
                events: vec![created("thread:a")],
                effects: Vec::new(),
                cancel_unsettled: None,
            })
            .expect("commit");
        let receipt = store.receipt("command:shared").expect("receipt");
        assert_eq!(receipt.thread_id, "thread:a");
        // The store itself returns the first receipt; the orchestrator turns a
        // different dispatch thread into a CommandIdConflict.
        let second = store
            .commit_command(CommitCommandInput {
                command_id: "command:shared".into(),
                thread_id: "thread:b".into(),
                command_type: "thread.create".into(),
                accepted_at: 2,
                events: vec![created("thread:b")],
                effects: Vec::new(),
                cancel_unsettled: None,
            })
            .expect("second commit");
        assert!(!second.committed);
        assert_eq!(second.receipt.thread_id, "thread:a");
    }

    #[test]
    fn rejection_is_recorded_and_replayed() {
        let mut store = MemoryStore::new();
        let first = store
            .commit_rejected_command(RejectedCommandInput {
                command_id: "command:bad".into(),
                thread_id: "thread:a".into(),
                command_type: "message.dispatch".into(),
                rejected_at: 5,
                error: "nope".into(),
            })
            .expect("reject");
        assert_eq!(first.status, ReceiptStatus::Rejected);
        let second = store
            .commit_rejected_command(RejectedCommandInput {
                command_id: "command:bad".into(),
                thread_id: "thread:a".into(),
                command_type: "message.dispatch".into(),
                rejected_at: 6,
                error: "different".into(),
            })
            .expect("reject replay");
        assert_eq!(second.error.as_deref(), Some("nope"));
        assert_eq!(second.accepted_at, 5);
    }
}
