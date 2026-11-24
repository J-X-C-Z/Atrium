// Ported from apps/server/src/orchestration-v2/EventSink.ts, EventStore.ts,
// ProjectionStore.ts and EffectOutbox.ts (T3 Code v0.1.11, MIT).
//!
//! The orchestration [`Store`](crate::orchestration::event_sink::Store) over
//! SQLite. The ported decision core asks for one transaction per command:
//! events, projection rows, the command receipt and its outbox effects commit
//! together or not at all.
//!
//! Events use the shared `orchestration_events` log and receipts
//! `orchestration_command_receipts` (item 21). Projection rows are the
//! `orchestration_v2_projection_*` tables and effects are
//! `orchestration_v2_effect_outbox`, both created by migration 055.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use serde_json::json;

use crate::contracts::{self, DomainEvent};
use crate::orchestration::event_sink::{
    CancelUnsettled, CommandReceipt, CommitCommandInput, CommitCommandOutput, ReceiptStatus,
    RejectedCommandInput, Store, StoredEvent, WriteInput,
};
use crate::orchestration::types::{
    CommandId, EventId, PendingEffect, Projection, ThreadId, Timestamp, iso, parse_iso,
};
use crate::store::events::{
    self, EventStore, NewEvent, insert_receipt_if_absent, read_receipt, upsert_receipt,
};

impl Store for EventStore {
    type Error = events::StoreError;

    fn projection(&self, thread_id: &ThreadId) -> Option<Projection> {
        let stored = self
            .read(&events::ReadOptions {
                thread_id: Some(thread_id.to_string()),
                ..events::ReadOptions::default()
            })
            .ok()?;
        let mut projection: Option<Projection> = None;
        for row in &stored {
            let Some(event) = decode_stored(row) else {
                continue;
            };
            projection.get_or_insert_with(|| {
                let thread = match &event {
                    DomainEvent::ThreadCreated(payload) => payload.payload.clone(),
                    DomainEvent::ThreadMetadata { event, .. } => event.payload.clone(),
                    _ => Default::default(),
                };
                Projection::new(thread)
            });
            if let Some(projection) = projection.as_mut() {
                projection.apply(&event);
            }
        }
        projection
    }

    fn list_thread_ids(&self, project_id: Option<&str>) -> Vec<ThreadId> {
        let conn = self.connection();
        let Ok(mut statement) = conn.prepare(
            "SELECT thread_id FROM orchestration_v2_projection_threads \
             WHERE deleted_at IS NULL AND (?1 IS NULL OR project_id = ?1) \
             ORDER BY updated_at DESC, thread_id",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = statement.query_map(params![project_id], |row| row.get::<_, String>(0))
        else {
            return Vec::new();
        };
        rows.filter_map(|row| row.ok()).map(ThreadId::new).collect()
    }

    fn latest_sequence(&self, thread_id: Option<&ThreadId>) -> i64 {
        events::latest_sequence(self.connection(), thread_id.map(ThreadId::as_str)).unwrap_or(0)
    }

    fn read_by_command_id(&self, command_id: &CommandId) -> Vec<StoredEvent> {
        self.read_by_command_id(command_id.as_str())
            .unwrap_or_default()
            .iter()
            .filter_map(|row| {
                Some(StoredEvent {
                    sequence: row.sequence,
                    command_id: row.command_id.clone(),
                    event: decode_stored(row)?,
                })
            })
            .collect()
    }

    fn command_receipt(&self, command_id: &CommandId) -> Option<CommandReceipt> {
        read_receipt(self.connection(), command_id.as_str())
            .ok()
            .flatten()
            .map(command_receipt)
    }

    fn write(&mut self, input: WriteInput) -> Result<Vec<StoredEvent>, Self::Error> {
        let conn = self.connection_mut();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = append_domain_events(&tx, input.command_id.as_ref(), &input.events)?;
        for event in &input.events {
            project_event(&tx, event)?;
        }
        enqueue_effects(&tx, &input.effects, input.now)?;
        tx.commit()?;
        Ok(stored)
    }

    fn commit_command(
        &mut self,
        input: CommitCommandInput,
    ) -> Result<CommitCommandOutput, Self::Error> {
        let conn = self.connection_mut();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let reserve = events::CommandReceipt {
            command_id: input.command_id.to_string(),
            aggregate_kind: "thread".into(),
            aggregate_id: input.thread_id.to_string(),
            command_type: input.command_type.clone(),
            accepted_at: iso(input.accepted_at),
            result_sequence: 0,
            status: "accepted".into(),
            error: None,
        };
        if !insert_receipt_if_absent(&tx, &reserve)? {
            let receipt = read_receipt(&tx, input.command_id.as_str())?.ok_or_else(|| {
                events::StoreError::ReceiptDisappeared(input.command_id.to_string())
            })?;
            let stored = events::read_by_command_id(&tx, input.command_id.as_str())?
                .iter()
                .filter_map(|row| {
                    Some(StoredEvent {
                        sequence: row.sequence,
                        command_id: row.command_id.clone(),
                        event: decode_stored(row)?,
                    })
                })
                .collect();
            tx.commit()?;
            return Ok(CommitCommandOutput {
                receipt: command_receipt(receipt),
                stored_events: stored,
                committed: false,
                cancelled_effect_count: 0,
            });
        }

        let stored = append_domain_events(&tx, Some(&input.command_id), &input.events)?;
        for event in &input.events {
            project_event(&tx, event)?;
        }
        enqueue_effects(&tx, &input.effects, input.accepted_at)?;
        let cancelled_effect_count = match &input.cancel_unsettled {
            Some(cancel) => cancel_unsettled(&tx, &input.thread_id, cancel, input.accepted_at)?,
            None => 0,
        };
        let result_sequence = stored.last().map_or_else(
            || events::latest_sequence(&tx, Some(input.thread_id.as_str())).unwrap_or(0),
            |event| event.sequence,
        );
        let receipt = events::CommandReceipt {
            command_id: input.command_id.to_string(),
            aggregate_kind: "thread".into(),
            aggregate_id: input.thread_id.to_string(),
            command_type: input.command_type,
            accepted_at: iso(input.accepted_at),
            result_sequence,
            status: "accepted".into(),
            error: None,
        };
        upsert_receipt(&tx, &receipt)?;
        tx.commit()?;
        Ok(CommitCommandOutput {
            receipt: command_receipt(receipt),
            stored_events: stored,
            committed: true,
            cancelled_effect_count,
        })
    }

    fn commit_rejected_command(
        &mut self,
        input: RejectedCommandInput,
    ) -> Result<CommandReceipt, Self::Error> {
        let conn = self.connection_mut();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let receipt = events::CommandReceipt {
            command_id: input.command_id.to_string(),
            aggregate_kind: "thread".into(),
            aggregate_id: input.thread_id.to_string(),
            command_type: input.command_type,
            accepted_at: iso(input.rejected_at),
            result_sequence: events::latest_sequence(&tx, Some(input.thread_id.as_str()))
                .unwrap_or(0),
            status: "rejected".into(),
            error: Some(input.error),
        };
        let inserted = insert_receipt_if_absent(&tx, &receipt)?;
        let receipt = if inserted {
            receipt
        } else {
            read_receipt(&tx, input.command_id.as_str())?.ok_or_else(|| {
                events::StoreError::ReceiptDisappeared(input.command_id.to_string())
            })?
        };
        tx.commit()?;
        Ok(command_receipt(receipt))
    }
}

fn command_receipt(row: events::CommandReceipt) -> CommandReceipt {
    CommandReceipt {
        command_id: CommandId::new(row.command_id),
        thread_id: ThreadId::new(row.aggregate_id),
        command_type: row.command_type,
        accepted_at: parse_iso(&row.accepted_at),
        result_sequence: row.result_sequence,
        status: if row.status == "rejected" {
            ReceiptStatus::Rejected
        } else {
            ReceiptStatus::Accepted
        },
        error: row.error,
    }
}

fn append_domain_events(
    conn: &Connection,
    command_id: Option<&CommandId>,
    events: &[DomainEvent],
) -> Result<Vec<StoredEvent>, events::StoreError> {
    let mut stored = Vec::with_capacity(events.len());
    for event in events {
        let stored_event = append_domain_event(conn, command_id, event)?;
        stored.push(stored_event);
    }
    Ok(stored)
}

fn append_domain_event(
    conn: &Connection,
    command_id: Option<&CommandId>,
    event: &DomainEvent,
) -> Result<StoredEvent, events::StoreError> {
    let new_event = new_event(event);
    let row = events::append_events(
        conn,
        command_id.map(CommandId::as_str),
        std::slice::from_ref(&new_event),
    )?
    .into_iter()
    .next()
    .expect("one appended event");
    Ok(StoredEvent {
        sequence: row.sequence,
        command_id: command_id.cloned(),
        event: event.clone(),
    })
}

fn new_event(event: &DomainEvent) -> NewEvent {
    let mut new = NewEvent::new(
        event.event_id().map(EventId::to_string).unwrap_or_default(),
        event
            .thread_id()
            .map(ThreadId::to_string)
            .unwrap_or_default(),
        event.type_name(),
        event.occurred_at().unwrap_or_default(),
        event.payload_json().unwrap_or(serde_json::Value::Null),
    );
    new.run_id = event.run_id().map(ToString::to_string);
    new.node_id = event.node_id().map(ToString::to_string);
    new.provider_instance_id = event.provider_instance_id().map(ToString::to_string);
    new
}

fn decode_stored(row: &contracts::StoredEvent) -> Option<DomainEvent> {
    let mut value = json!({
        "id": row.event_id.as_str(),
        "type": row.event_type,
        "threadId": row.thread_id.as_str(),
        "occurredAt": row.occurred_at,
        "payload": row.payload,
    });
    if let Some(run_id) = &row.run_id {
        value["runId"] = json!(run_id.as_str());
    }
    serde_json::from_value(value).ok()
}

fn wire<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn payload_json<T: Serialize>(value: &T) -> Result<String, events::StoreError> {
    Ok(serde_json::to_string(value)?)
}

fn project_event(conn: &Connection, event: &DomainEvent) -> Result<(), events::StoreError> {
    match event {
        DomainEvent::ThreadCreated(payload) => project_thread(conn, &payload.payload),
        DomainEvent::ThreadMetadata { event, .. } => project_thread(conn, &event.payload),
        DomainEvent::RunCreated(payload) | DomainEvent::RunUpdated(payload) => {
            project_run(conn, &payload.payload)
        }
        DomainEvent::RunAttemptCreated(payload) | DomainEvent::RunAttemptUpdated(payload) => {
            project_attempt(conn, &payload.payload)
        }
        DomainEvent::NodeUpdated(payload) => project_node(conn, &payload.payload),
        DomainEvent::ProviderThreadUpdated(payload) => {
            project_provider_thread(conn, &payload.payload)
        }
        DomainEvent::ProviderSessionAttached(payload)
        | DomainEvent::ProviderSessionUpdated(payload) => {
            project_provider_session(conn, &payload.payload)
        }
        DomainEvent::ProviderTurnUpdated(payload) => {
            let thread_id = event.thread_id().cloned().unwrap_or_default();
            project_provider_turn(conn, &thread_id, &payload.payload)
        }
        DomainEvent::RuntimeRequestUpdated(payload) => {
            let thread_id = event.thread_id().cloned().unwrap_or_default();
            project_runtime_request(conn, &thread_id, &payload.payload)
        }
        DomainEvent::MessageUpdated(payload) => project_message(conn, &payload.payload),
        DomainEvent::TurnItemUpdated(payload) => project_turn_item(conn, &payload.payload),
        DomainEvent::SubagentUpdated(payload) => project_subagent(conn, &payload.payload),
        DomainEvent::RunBackgroundWorkCancelled(_)
        | DomainEvent::ThreadVisitRecorded(_)
        | DomainEvent::ProviderSessionDetached(_)
        | DomainEvent::PlanUpdated(_)
        | DomainEvent::Unknown(_) => Ok(()),
    }
}

fn project_thread(
    conn: &Connection,
    thread: &contracts::AppThread,
) -> Result<(), events::StoreError> {
    conn.execute(
        "INSERT INTO orchestration_v2_projection_threads (
            thread_id, project_id, title, default_provider, runtime_mode, interaction_mode,
            active_provider_thread_id, created_at, updated_at, archived_at, deleted_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(thread_id) DO UPDATE SET
            project_id = excluded.project_id,
            title = excluded.title,
            default_provider = excluded.default_provider,
            runtime_mode = excluded.runtime_mode,
            interaction_mode = excluded.interaction_mode,
            active_provider_thread_id = excluded.active_provider_thread_id,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            archived_at = excluded.archived_at,
            deleted_at = excluded.deleted_at,
            payload_json = excluded.payload_json",
        params![
            thread.id.as_str(),
            thread.project_id.as_str(),
            thread.title,
            thread.provider_instance_id.as_str(),
            wire(&thread.runtime_mode),
            wire(&thread.interaction_mode),
            thread
                .active_provider_thread_id
                .as_ref()
                .map(ToString::to_string),
            thread.created_at,
            thread.updated_at,
            thread.archived_at,
            thread.deleted_at,
            payload_json(thread)?,
        ],
    )?;
    Ok(())
}

fn project_run(conn: &Connection, run: &contracts::Run) -> Result<(), events::StoreError> {
    conn.execute(
        "INSERT INTO orchestration_v2_projection_runs (
            run_id, thread_id, ordinal, provider, provider_thread_id, status, requested_at,
            completed_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(run_id) DO UPDATE SET
            thread_id = excluded.thread_id,
            ordinal = excluded.ordinal,
            provider = excluded.provider,
            provider_thread_id = excluded.provider_thread_id,
            status = excluded.status,
            requested_at = excluded.requested_at,
            completed_at = excluded.completed_at,
            payload_json = excluded.payload_json",
        params![
            run.id.as_str(),
            run.thread_id.as_str(),
            run.ordinal,
            run.provider_instance_id.as_str(),
            run.provider_thread_id.as_ref().map(ToString::to_string),
            wire(&run.status),
            run.requested_at,
            run.completed_at,
            payload_json(run)?,
        ],
    )?;
    Ok(())
}

fn project_attempt(
    conn: &Connection,
    attempt: &contracts::RunAttempt,
) -> Result<(), events::StoreError> {
    let thread_id: Option<String> = conn
        .query_row(
            "SELECT thread_id FROM orchestration_v2_projection_runs WHERE run_id = ?1",
            params![attempt.run_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    conn.execute(
        "INSERT INTO orchestration_v2_projection_run_attempts (
            attempt_id, thread_id, run_id, attempt_ordinal, root_node_id, provider,
            provider_thread_id, provider_turn_id, status, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(attempt_id) DO UPDATE SET
            thread_id = excluded.thread_id,
            run_id = excluded.run_id,
            attempt_ordinal = excluded.attempt_ordinal,
            root_node_id = excluded.root_node_id,
            provider = excluded.provider,
            provider_thread_id = excluded.provider_thread_id,
            provider_turn_id = excluded.provider_turn_id,
            status = excluded.status,
            payload_json = excluded.payload_json",
        params![
            attempt.id.as_str(),
            thread_id.unwrap_or_default(),
            attempt.run_id.as_str(),
            attempt.attempt_ordinal,
            attempt.root_node_id.as_str(),
            attempt.provider_instance_id.as_str(),
            attempt.provider_thread_id.as_str(),
            attempt.provider_turn_id.as_ref().map(ToString::to_string),
            wire(&attempt.status),
            payload_json(attempt)?,
        ],
    )?;
    Ok(())
}

fn project_node(
    conn: &Connection,
    node: &contracts::ExecutionNode,
) -> Result<(), events::StoreError> {
    conn.execute(
        "INSERT INTO orchestration_v2_projection_nodes (
            node_id, thread_id, run_id, parent_node_id, root_node_id, kind, status,
            provider_thread_id, provider_turn_id, runtime_request_id, checkpoint_scope_id,
            started_at, completed_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(node_id) DO UPDATE SET
            thread_id = excluded.thread_id,
            run_id = excluded.run_id,
            parent_node_id = excluded.parent_node_id,
            root_node_id = excluded.root_node_id,
            kind = excluded.kind,
            status = excluded.status,
            provider_thread_id = excluded.provider_thread_id,
            provider_turn_id = excluded.provider_turn_id,
            runtime_request_id = excluded.runtime_request_id,
            checkpoint_scope_id = excluded.checkpoint_scope_id,
            started_at = excluded.started_at,
            completed_at = excluded.completed_at,
            payload_json = excluded.payload_json",
        params![
            node.id.as_str(),
            node.thread_id.as_str(),
            node.run_id.as_ref().map(ToString::to_string),
            node.parent_node_id.as_ref().map(ToString::to_string),
            node.root_node_id.as_str(),
            wire(&node.kind),
            wire(&node.status),
            node.provider_thread_id.as_ref().map(ToString::to_string),
            node.provider_turn_id.as_ref().map(ToString::to_string),
            node.runtime_request_id.as_ref().map(ToString::to_string),
            node.checkpoint_scope_id.as_ref().map(ToString::to_string),
            node.started_at,
            node.completed_at,
            payload_json(node)?,
        ],
    )?;
    Ok(())
}

fn project_provider_thread(
    conn: &Connection,
    thread: &contracts::ProviderThread,
) -> Result<(), events::StoreError> {
    conn.execute(
        "INSERT INTO orchestration_v2_projection_provider_threads (
            provider_thread_id, thread_id, owner_node_id, provider, provider_session_id, status,
            first_run_ordinal, last_run_ordinal, updated_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(provider_thread_id) DO UPDATE SET
            thread_id = excluded.thread_id,
            owner_node_id = excluded.owner_node_id,
            provider = excluded.provider,
            provider_session_id = excluded.provider_session_id,
            status = excluded.status,
            first_run_ordinal = excluded.first_run_ordinal,
            last_run_ordinal = excluded.last_run_ordinal,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json",
        params![
            thread.id.as_str(),
            thread.app_thread_id.as_ref().map(ToString::to_string),
            thread.owner_node_id.as_ref().map(ToString::to_string),
            thread.driver.as_str(),
            thread.provider_session_id.as_ref().map(ToString::to_string),
            wire(&thread.status),
            thread.first_run_ordinal,
            thread.last_run_ordinal,
            thread.updated_at,
            payload_json(thread)?,
        ],
    )?;
    Ok(())
}

fn project_provider_session(
    conn: &Connection,
    session: &contracts::ProviderSession,
) -> Result<(), events::StoreError> {
    conn.execute(
        "INSERT INTO orchestration_v2_projection_provider_sessions (
            provider_session_id, thread_id, provider, status, model, updated_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(provider_session_id) DO UPDATE SET
            provider = excluded.provider,
            status = excluded.status,
            model = excluded.model,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json",
        params![
            session.id.as_str(),
            Option::<String>::None,
            session.provider_instance_id.as_str(),
            wire(&session.status),
            session.model,
            session.updated_at,
            payload_json(session)?,
        ],
    )?;
    Ok(())
}

fn project_provider_turn(
    conn: &Connection,
    thread_id: &ThreadId,
    turn: &contracts::ProviderTurn,
) -> Result<(), events::StoreError> {
    conn.execute(
        "INSERT INTO orchestration_v2_projection_provider_turns (
            provider_turn_id, thread_id, provider_thread_id, node_id, run_attempt_id, ordinal,
            status, started_at, completed_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(provider_turn_id) DO UPDATE SET
            thread_id = excluded.thread_id,
            provider_thread_id = excluded.provider_thread_id,
            node_id = excluded.node_id,
            run_attempt_id = excluded.run_attempt_id,
            ordinal = excluded.ordinal,
            status = excluded.status,
            started_at = excluded.started_at,
            completed_at = excluded.completed_at,
            payload_json = excluded.payload_json",
        params![
            turn.id.as_str(),
            thread_id.as_str(),
            turn.provider_thread_id.as_str(),
            turn.node_id.as_str(),
            turn.run_attempt_id.as_ref().map(ToString::to_string),
            turn.ordinal,
            wire(&turn.status),
            turn.started_at,
            turn.completed_at,
            payload_json(turn)?,
        ],
    )?;
    Ok(())
}

fn project_runtime_request(
    conn: &Connection,
    thread_id: &ThreadId,
    request: &contracts::RuntimeRequest,
) -> Result<(), events::StoreError> {
    conn.execute(
        "INSERT INTO orchestration_v2_projection_runtime_requests (
            runtime_request_id, thread_id, node_id, provider_turn_id, kind, status, created_at,
            resolved_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(runtime_request_id) DO UPDATE SET
            thread_id = excluded.thread_id,
            node_id = excluded.node_id,
            provider_turn_id = excluded.provider_turn_id,
            kind = excluded.kind,
            status = excluded.status,
            created_at = excluded.created_at,
            resolved_at = excluded.resolved_at,
            payload_json = excluded.payload_json",
        params![
            request.id.as_str(),
            thread_id.as_str(),
            request.node_id.as_str(),
            request.provider_turn_id.as_ref().map(ToString::to_string),
            request.kind.as_wire(),
            wire(&request.status),
            request.created_at,
            request.resolved_at,
            payload_json(request)?,
        ],
    )?;
    Ok(())
}

fn project_message(
    conn: &Connection,
    message: &contracts::ConversationMessage,
) -> Result<(), events::StoreError> {
    conn.execute(
        "INSERT INTO orchestration_v2_projection_messages (
            message_id, thread_id, run_id, node_id, role, streaming, created_at, updated_at,
            payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(message_id) DO UPDATE SET
            thread_id = excluded.thread_id,
            run_id = excluded.run_id,
            node_id = excluded.node_id,
            role = excluded.role,
            streaming = excluded.streaming,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json",
        params![
            message.id.as_str(),
            message.thread_id.as_str(),
            message.run_id.as_ref().map(ToString::to_string),
            message.node_id.as_ref().map(ToString::to_string),
            wire(&message.role),
            message.streaming,
            message.created_at,
            message.updated_at,
            payload_json(message)?,
        ],
    )?;
    Ok(())
}

fn project_turn_item(
    conn: &Connection,
    item: &contracts::TurnItem,
) -> Result<(), events::StoreError> {
    let Some(base) = item.base() else {
        return Ok(());
    };
    conn.execute(
        "INSERT INTO orchestration_v2_projection_turn_items (
            turn_item_id, thread_id, run_id, node_id, provider_thread_id, provider_turn_id,
            parent_item_id, ordinal, type, status, updated_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(turn_item_id) DO UPDATE SET
            thread_id = excluded.thread_id,
            run_id = excluded.run_id,
            node_id = excluded.node_id,
            provider_thread_id = excluded.provider_thread_id,
            provider_turn_id = excluded.provider_turn_id,
            parent_item_id = excluded.parent_item_id,
            ordinal = excluded.ordinal,
            type = excluded.type,
            status = excluded.status,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json",
        params![
            base.id.as_str(),
            base.thread_id.as_str(),
            base.run_id.as_ref().map(ToString::to_string),
            base.node_id.as_ref().map(ToString::to_string),
            base.provider_thread_id.as_ref().map(ToString::to_string),
            base.provider_turn_id.as_ref().map(ToString::to_string),
            base.parent_item_id.as_ref().map(ToString::to_string),
            base.ordinal,
            item.type_name(),
            wire(&base.status),
            base.updated_at,
            payload_json(item)?,
        ],
    )?;
    Ok(())
}

fn project_subagent(
    conn: &Connection,
    subagent: &contracts::Subagent,
) -> Result<(), events::StoreError> {
    conn.execute(
        "INSERT INTO orchestration_v2_projection_subagents (
            subagent_id, thread_id, run_id, parent_node_id, provider, provider_thread_id,
            child_thread_id, origin, status, started_at, completed_at, updated_at, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(subagent_id) DO UPDATE SET
            thread_id = excluded.thread_id,
            run_id = excluded.run_id,
            parent_node_id = excluded.parent_node_id,
            provider = excluded.provider,
            provider_thread_id = excluded.provider_thread_id,
            child_thread_id = excluded.child_thread_id,
            origin = excluded.origin,
            status = excluded.status,
            started_at = excluded.started_at,
            completed_at = excluded.completed_at,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json",
        params![
            subagent.id.as_str(),
            subagent.thread_id.as_str(),
            subagent.run_id.as_ref().map(ToString::to_string),
            subagent.parent_node_id.as_str(),
            subagent.provider_instance_id.as_str(),
            subagent
                .provider_thread_id
                .as_ref()
                .map(ToString::to_string),
            subagent.child_thread_id.as_ref().map(ToString::to_string),
            wire(&subagent.origin),
            wire(&subagent.status),
            subagent.started_at,
            subagent.completed_at,
            subagent.updated_at,
            payload_json(subagent)?,
        ],
    )?;
    Ok(())
}

fn enqueue_effects(
    conn: &Connection,
    effects: &[PendingEffect],
    now: Timestamp,
) -> Result<(), events::StoreError> {
    for effect in effects {
        conn.execute(
            "INSERT INTO orchestration_v2_effect_outbox (
                effect_id, command_id, thread_id, effect_type, payload_json, status,
                attempt_count, available_at, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', 0, ?6, ?7, ?7)
             ON CONFLICT(effect_id) DO NOTHING",
            params![
                effect.id,
                effect.command_id.as_str(),
                effect.thread_id.as_str(),
                effect.request.type_name(),
                payload_json(&effect.request)?,
                iso(effect.available_at.unwrap_or(now)),
                iso(now),
            ],
        )?;
    }
    Ok(())
}

fn cancel_unsettled(
    conn: &Connection,
    thread_id: &ThreadId,
    cancel: &CancelUnsettled,
    now: Timestamp,
) -> Result<usize, events::StoreError> {
    if cancel.effect_types.is_empty() {
        return Ok(0);
    }
    let placeholders = std::iter::repeat_n("?", cancel.effect_types.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "UPDATE orchestration_v2_effect_outbox
         SET status = 'failed', last_error = ?1, completed_at = ?2, updated_at = ?2
         WHERE thread_id = ?3
           AND status IN ('pending', 'running')
           AND effect_type IN ({placeholders})"
    );
    let mut values: Vec<rusqlite::types::Value> = vec![
        rusqlite::types::Value::Text(cancel.reason.clone()),
        rusqlite::types::Value::Text(iso(now)),
        rusqlite::types::Value::Text(thread_id.to_string()),
    ];
    values.extend(
        cancel
            .effect_types
            .iter()
            .map(|value| rusqlite::types::Value::Text(value.clone())),
    );
    Ok(conn.execute(&sql, rusqlite::params_from_iter(values))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::orchestrator::{dispatch, plan_run_finished};
    use crate::orchestration::types::{
        Actor, CreationSource, InitialMessage, InteractionMode, ModelSelection, RunStatus,
        RuntimeMode, ThreadCreateCommand,
    };

    fn create(thread_id: &str, command_id: &str) -> crate::orchestration::types::Command {
        crate::orchestration::types::Command::ThreadCreate(ThreadCreateCommand {
            created_by: Actor::User,
            creation_source: CreationSource::Web,
            command_id: command_id.into(),
            thread_id: thread_id.into(),
            project_id: "project:store".into(),
            title: "Thread".into(),
            model_selection: ModelSelection::new("codex", "gpt-5.1-codex"),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            branch: None,
            worktree_path: None,
            link_origin: None,
            delegated_from: None,
            imported_native_thread: None,
            initial_message: Some(InitialMessage {
                message_id: "message:one".into(),
                text: "hello".into(),
                attachments: Vec::new(),
                context: None,
                title_seed: Some("Hello".into()),
                scheduled_task_id: None,
                sender_thread_id: None,
                workspace_strategy: None,
            }),
        })
    }

    #[test]
    fn one_command_commits_events_projection_row_receipt_and_effects() {
        let mut store = EventStore::in_memory().expect("store");
        dispatch(&mut store, &create("thread:a", "command:create"), 1_000).expect("create");

        let projection = store
            .projection(&ThreadId::new("thread:a"))
            .expect("projection");
        assert_eq!(projection.thread.title, "Hello");
        assert_eq!(projection.runs.len(), 1);
        assert_eq!(projection.runs[0].status, RunStatus::Starting);
        assert_eq!(projection.messages.len(), 1);

        let (title, status): (String, String) = store
            .connection()
            .query_row(
                "SELECT title, status FROM orchestration_v2_projection_threads \
                 JOIN orchestration_v2_projection_runs USING (thread_id) \
                 WHERE thread_id = 'thread:a'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("projection row");
        assert_eq!(title, "Hello");
        assert_eq!(status, "starting");

        let receipt = store
            .command_receipt(&CommandId::new("command:create"))
            .expect("receipt");
        assert_eq!(receipt.status, ReceiptStatus::Accepted);
        assert_eq!(receipt.result_sequence, 8);

        let effects: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM orchestration_v2_effect_outbox WHERE status = 'pending'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(effects, 2);
    }

    #[test]
    fn a_failed_projection_write_rolls_back_every_part_of_the_command() {
        let mut store = EventStore::in_memory().expect("store");
        store
            .connection()
            .execute(
                "CREATE TRIGGER reject_run BEFORE INSERT ON orchestration_v2_projection_runs \
                 BEGIN SELECT RAISE(ABORT, 'no runs'); END",
                [],
            )
            .expect("trigger");

        assert!(dispatch(&mut store, &create("thread:a", "command:create"), 1_000).is_err());
        assert!(store.projection(&ThreadId::new("thread:a")).is_none());
        assert!(
            store
                .command_receipt(&CommandId::new("command:create"))
                .is_none()
        );
        assert_eq!(
            store
                .connection()
                .query_row("SELECT COUNT(*) FROM orchestration_events", [], |row| row
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM orchestration_v2_effect_outbox",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn a_replayed_command_returns_the_recorded_events() {
        let mut store = EventStore::in_memory().expect("store");
        let command = create("thread:a", "command:create");
        let first = dispatch(&mut store, &command, 1_000).expect("create");
        let replay = dispatch(&mut store, &command, 2_000).expect("replay");
        assert!(replay.replayed);
        assert_eq!(replay.sequence, first.sequence);
        assert_eq!(
            replay
                .stored_events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            first
                .stored_events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn finishing_a_run_starts_the_next_queued_run_through_the_same_store() {
        let mut store = EventStore::in_memory().expect("store");
        dispatch(&mut store, &create("thread:a", "command:create"), 1_000).expect("create");
        let mut projection = store
            .projection(&ThreadId::new("thread:a"))
            .expect("projection");
        let decision = plan_run_finished(
            &mut projection,
            "run:thread:a:1",
            RunStatus::Completed,
            2_000,
        )
        .expect("finish");
        assert!(decision.events.iter().any(|event| {
            matches!(event, DomainEvent::RunUpdated(payload)
                if payload.payload.status == RunStatus::Completed)
        }));
        assert!(decision.effects.is_empty());
    }
}
