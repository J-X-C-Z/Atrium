// Ported from apps/server/src/orchestration-v2/EventStore.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/persistence/OrchestrationEventStore.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/persistence/OrchestrationCommandReceipts.ts (T3 Code v0.1.11, MIT).
//!
//! The V2 event store: append domain events to the shared application log,
//! read them back, and reserve a command receipt so an orchestrator retry is
//! idempotent. The rows live in `orchestration_events` (migration 001 plus the
//! `application_event_version` column and indexes that migration 055 adds).
//!
//! Local payload types are deliberately minimal. The contracts port replaces
//! them with T3's schema types; until then they carry the same field names and
//! JSON passthrough.

use std::path::Path;
use std::time::Duration;

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params, params_from_iter};
use serde_json::{Map, Value as JsonValue};

use crate::contracts::StoredEvent;
use crate::store::migrations::migrate;
use crate::store::projections::{ProjectionStore, ProjectionWrite};

/// Errors from the store. T3 wraps these in tagged Effect errors; the port
/// keeps the SQLite and decode causes visible.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("command receipt {0} disappeared during its transaction")]
    ReceiptDisappeared(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// One domain event on its way into the store.
///
/// Mirrors T3's `OrchestrationV2DomainEvent` seed: `id`, `threadId`, `type`,
/// `occurredAt`, `payload`, and the optional index fields that the event store
/// copies into `metadata_json`. Marked for consolidation with
/// `crate::contracts` once that module ports the full schema.
#[derive(Clone, Debug, PartialEq)]
pub struct NewEvent {
    pub id: String,
    pub thread_id: String,
    pub event_type: String,
    pub occurred_at: String,
    pub payload: JsonValue,
    pub run_id: Option<String>,
    pub node_id: Option<String>,
    pub driver: Option<String>,
    pub provider_instance_id: Option<String>,
    pub raw_event_id: Option<String>,
}

impl NewEvent {
    pub fn new(
        id: impl Into<String>,
        thread_id: impl Into<String>,
        event_type: impl Into<String>,
        occurred_at: impl Into<String>,
        payload: JsonValue,
    ) -> Self {
        Self {
            id: id.into(),
            thread_id: thread_id.into(),
            event_type: event_type.into(),
            occurred_at: occurred_at.into(),
            payload,
            run_id: None,
            node_id: None,
            driver: None,
            provider_instance_id: None,
            raw_event_id: None,
        }
    }
}

/// One project event (`project.created`, `project.meta-updated`,
/// `project.deleted`). T3 writes these to the same log under
/// `aggregate_kind = 'project'`.
#[derive(Clone, Debug, PartialEq)]
pub struct NewProjectEvent {
    pub event_id: String,
    pub project_id: String,
    pub event_type: String,
    pub occurred_at: String,
    pub command_id: Option<String>,
    pub causation_event_id: Option<String>,
    pub correlation_id: Option<String>,
    pub payload: JsonValue,
    pub metadata: JsonValue,
}

/// `read` filters, matching `EventStoreV2.read`'s input.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadOptions {
    pub after_sequence: Option<i64>,
    pub through_sequence: Option<i64>,
    pub thread_id: Option<String>,
    pub event_type: Option<String>,
    pub limit: Option<usize>,
}

/// One row of `orchestration_command_receipts`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandReceipt {
    pub command_id: String,
    /// `project` or `thread`.
    pub aggregate_kind: String,
    pub aggregate_id: String,
    pub command_type: String,
    pub accepted_at: String,
    pub result_sequence: i64,
    /// `accepted` or `rejected`.
    pub status: String,
    pub error: Option<String>,
}

/// The metadata on one `apply` call.
#[derive(Clone, Copy, Debug)]
pub struct ApplyCommand<'a> {
    pub command_id: &'a str,
    pub thread_id: &'a str,
    pub command_type: &'a str,
    pub accepted_at: &'a str,
}

/// What one `apply` did. `applied` is false when the command id was already
/// used, in which case the receipt and events are the original ones.
#[derive(Clone, Debug, PartialEq)]
pub struct ApplyResult {
    pub applied: bool,
    pub receipt: CommandReceipt,
    pub events: Vec<StoredEvent>,
}

/// A connection plus the committed-event buffer.
///
/// T3 publishes committed events through an Effect `PubSub`. The synchronous
/// port records them here; the orchestration layer drains them with
/// [`EventStore::take_published`]. Publishing happens only after the
/// surrounding transaction commits, and `apply` does the same internally.
pub struct EventStore {
    conn: Connection,
    published: Vec<StoredEvent>,
}

impl EventStore {
    /// Open (or create) a database and apply every ported migration.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::from_connection(conn)
    }

    /// Open an in-memory database and apply every ported migration.
    pub fn in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    /// Wrap an existing connection. Migrations are applied when missing.
    pub fn from_connection(mut conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        migrate(&mut conn)?;
        Ok(Self {
            conn,
            published: Vec::new(),
        })
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// The projection store over the same connection.
    pub fn projections(&self) -> ProjectionStore<'_> {
        ProjectionStore::new(&self.conn)
    }

    /// Append agent events on an existing connection, without a transaction
    /// of its own. Callers that need atomicity use [`EventStore::apply`], or
    /// wrap this in their own transaction seen through `Connection`.
    pub fn append(
        &self,
        command_id: Option<&str>,
        events: &[NewEvent],
    ) -> Result<Vec<StoredEvent>> {
        append_events(&self.conn, command_id, events)
    }

    pub fn append_project_event(&self, event: &NewProjectEvent) -> Result<StoredEvent> {
        append_project_event(&self.conn, event)
    }

    pub fn read(&self, options: &ReadOptions) -> Result<Vec<StoredEvent>> {
        read(&self.conn, options)
    }

    pub fn read_by_command_id(&self, command_id: &str) -> Result<Vec<StoredEvent>> {
        read_by_command_id(&self.conn, command_id)
    }

    /// The latest agent sequence, for one thread or for every thread.
    pub fn latest_sequence(&self, thread_id: Option<&str>) -> Result<i64> {
        latest_sequence(&self.conn, thread_id)
    }

    /// The latest sequence in the shared application log, project events
    /// included.
    pub fn latest_application_sequence(&self) -> Result<i64> {
        latest_application_sequence(&self.conn)
    }

    pub fn publish_committed(&mut self, events: &[StoredEvent]) {
        self.published.extend_from_slice(events);
    }

    /// Drain the committed-event buffer.
    pub fn take_published(&mut self) -> Vec<StoredEvent> {
        std::mem::take(&mut self.published)
    }

    /// Apply one command: events, receipt and projections in one
    /// `BEGIN IMMEDIATE` transaction.
    ///
    /// A repeated `command_id` (T3's `insertIfAbsent` reserve) returns the
    /// original receipt and events without writing anything. Any failure rolls
    /// back events, receipt and projections together, and nothing is
    /// published.
    pub fn apply(
        &mut self,
        command: ApplyCommand<'_>,
        events: &[NewEvent],
        projections: &[ProjectionWrite],
    ) -> Result<ApplyResult> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        let reserved = insert_receipt_if_absent(
            &tx,
            &CommandReceipt {
                command_id: command.command_id.to_string(),
                aggregate_kind: "thread".to_string(),
                aggregate_id: command.thread_id.to_string(),
                command_type: command.command_type.to_string(),
                accepted_at: command.accepted_at.to_string(),
                result_sequence: 0,
                status: "accepted".to_string(),
                error: None,
            },
        )?;

        if !reserved {
            let receipt = read_receipt(&tx, command.command_id)?
                .ok_or_else(|| StoreError::ReceiptDisappeared(command.command_id.to_string()))?;
            let events = read_by_command_id(&tx, command.command_id)?;
            tx.commit()?;
            return Ok(ApplyResult {
                applied: false,
                receipt,
                events,
            });
        }

        let stored = append_events(&tx, Some(command.command_id), events)?;
        for projection in projections {
            projection.apply(&tx)?;
        }

        // T3 takes the last appended sequence; a command that wrote no events
        // keeps the thread's high-water mark.
        let result_sequence = match stored.last() {
            Some(event) => event.sequence,
            None => latest_sequence(&tx, Some(command.thread_id))?,
        };
        let receipt = CommandReceipt {
            command_id: command.command_id.to_string(),
            aggregate_kind: "thread".to_string(),
            aggregate_id: command.thread_id.to_string(),
            command_type: command.command_type.to_string(),
            accepted_at: command.accepted_at.to_string(),
            result_sequence,
            status: "accepted".to_string(),
            error: None,
        };
        upsert_receipt(&tx, &receipt)?;
        tx.commit()?;

        // Publish only after the outer commit, as T3's `commitThenPublish` does.
        self.publish_committed(&stored);
        Ok(ApplyResult {
            applied: true,
            receipt,
            events: stored,
        })
    }
}

fn metadata_json(event: &NewEvent) -> Result<String> {
    let mut metadata = Map::new();
    if let Some(run_id) = &event.run_id {
        metadata.insert("runId".to_string(), JsonValue::String(run_id.clone()));
    }
    if let Some(node_id) = &event.node_id {
        metadata.insert("nodeId".to_string(), JsonValue::String(node_id.clone()));
    }
    if let Some(driver) = &event.driver {
        metadata.insert("driver".to_string(), JsonValue::String(driver.clone()));
    }
    if let Some(instance) = &event.provider_instance_id {
        metadata.insert(
            "providerInstanceId".to_string(),
            JsonValue::String(instance.clone()),
        );
    }
    if let Some(raw) = &event.raw_event_id {
        metadata.insert("rawEventId".to_string(), JsonValue::String(raw.clone()));
    }
    Ok(serde_json::to_string(&JsonValue::Object(metadata))?)
}

fn actor_kind(event: &NewEvent) -> &'static str {
    if event.raw_event_id.is_some() {
        "provider"
    } else {
        "server"
    }
}

/// Append agent events in order. `stream_version` is derived per thread from
/// the current maximum, and `sequence` comes from the table's AUTOINCREMENT
/// key. A repeated `event_id` fails on the unique constraint.
pub fn append_events(
    conn: &Connection,
    command_id: Option<&str>,
    events: &[NewEvent],
) -> Result<Vec<StoredEvent>> {
    let mut stored = Vec::with_capacity(events.len());
    for event in events {
        let payload_json = serde_json::to_string(&event.payload)?;
        let metadata = metadata_json(event)?;
        let sequence: i64 = conn.query_row(
            "INSERT INTO orchestration_events (
                event_id,
                aggregate_kind,
                stream_id,
                stream_version,
                event_type,
                occurred_at,
                command_id,
                causation_event_id,
                correlation_id,
                actor_kind,
                payload_json,
                metadata_json,
                application_event_version
             )
             VALUES (
                ?1,
                'thread',
                ?2,
                COALESCE(
                  (
                    SELECT MAX(stream_version) + 1
                    FROM orchestration_events
                    WHERE aggregate_kind = 'thread' AND stream_id = ?2
                  ),
                  0
                ),
                ?3,
                ?4,
                ?5,
                NULL,
                ?5,
                ?6,
                ?7,
                ?8,
                2
             )
             RETURNING sequence",
            params![
                event.id,
                event.thread_id,
                event.event_type,
                event.occurred_at,
                command_id,
                actor_kind(event),
                payload_json,
                metadata,
            ],
            |row| row.get(0),
        )?;
        stored.push(StoredEvent {
            sequence,
            event_id: event.id.clone(),
            command_id: command_id.map(str::to_string),
            thread_id: event.thread_id.clone(),
            run_id: event.run_id.clone(),
            event_type: event.event_type.clone(),
            occurred_at: event.occurred_at.clone(),
            payload: event.payload.clone(),
        });
    }
    Ok(stored)
}

fn infer_actor_kind(event: &NewProjectEvent) -> &'static str {
    match event.command_id.as_deref() {
        Some(command_id) if command_id.starts_with("provider:") => "provider",
        Some(command_id) if command_id.starts_with("server:") => "server",
        None => "server",
        Some(_) => "client",
    }
}

pub fn append_project_event(conn: &Connection, event: &NewProjectEvent) -> Result<StoredEvent> {
    let payload_json = serde_json::to_string(&event.payload)?;
    let metadata_json = serde_json::to_string(&event.metadata)?;
    let sequence: i64 = conn.query_row(
        "INSERT INTO orchestration_events (
            event_id,
            aggregate_kind,
            stream_id,
            stream_version,
            event_type,
            occurred_at,
            command_id,
            causation_event_id,
            correlation_id,
            actor_kind,
            payload_json,
            metadata_json,
            application_event_version
         )
         VALUES (
            ?1,
            'project',
            ?2,
            COALESCE(
              (
                SELECT MAX(stream_version) + 1
                FROM orchestration_events
                WHERE aggregate_kind = 'project' AND stream_id = ?2
              ),
              0
            ),
            ?3,
            ?4,
            ?5,
            ?6,
            ?7,
            ?8,
            ?9,
            ?10,
            2
         )
         RETURNING sequence",
        params![
            event.event_id,
            event.project_id,
            event.event_type,
            event.occurred_at,
            event.command_id,
            event.causation_event_id,
            event.correlation_id,
            infer_actor_kind(event),
            payload_json,
            metadata_json,
        ],
        |row| row.get(0),
    )?;
    Ok(StoredEvent {
        sequence,
        event_id: event.event_id.clone(),
        command_id: event.command_id.clone(),
        thread_id: event.project_id.clone(),
        run_id: None,
        event_type: event.event_type.clone(),
        occurred_at: event.occurred_at.clone(),
        payload: event.payload.clone(),
    })
}

struct RawEventRow {
    sequence: i64,
    event_id: String,
    command_id: Option<String>,
    stream_id: String,
    event_type: String,
    occurred_at: String,
    payload_json: String,
    metadata_json: String,
}

impl RawEventRow {
    fn into_stored(self) -> Result<StoredEvent> {
        let payload: JsonValue = serde_json::from_str(&self.payload_json)?;
        let metadata: JsonValue = serde_json::from_str(&self.metadata_json)?;
        let run_id = metadata
            .get("runId")
            .and_then(JsonValue::as_str)
            .map(str::to_string);
        Ok(StoredEvent {
            sequence: self.sequence,
            event_id: self.event_id,
            command_id: self.command_id,
            thread_id: self.stream_id,
            run_id,
            event_type: self.event_type,
            occurred_at: self.occurred_at,
            payload,
        })
    }
}

fn read_agent_rows(
    conn: &Connection,
    after_sequence: Option<i64>,
    through_sequence: Option<i64>,
    thread_id: Option<&str>,
    command_id: Option<&str>,
    event_type: Option<&str>,
    limit: Option<usize>,
) -> Result<Vec<StoredEvent>> {
    if limit == Some(0) {
        return Ok(Vec::new());
    }

    let mut sql = String::from(
        "SELECT
            sequence,
            event_id,
            command_id,
            stream_id,
            event_type,
            occurred_at,
            payload_json,
            metadata_json
         FROM orchestration_events
         WHERE application_event_version = 2
           AND aggregate_kind = 'thread'
           AND sequence > ?",
    );
    let mut values = vec![Value::Integer(after_sequence.unwrap_or(0))];

    if let Some(through) = through_sequence {
        sql.push_str(" AND sequence <= ?");
        values.push(Value::Integer(through));
    }
    if let Some(thread_id) = thread_id {
        sql.push_str(" AND stream_id = ?");
        values.push(Value::Text(thread_id.to_string()));
    }
    if let Some(command_id) = command_id {
        sql.push_str(" AND command_id = ?");
        values.push(Value::Text(command_id.to_string()));
    }
    if let Some(event_type) = event_type {
        sql.push_str(" AND event_type = ?");
        values.push(Value::Text(event_type.to_string()));
    }
    sql.push_str(" ORDER BY sequence ASC");
    if let Some(limit) = limit {
        sql.push_str(" LIMIT ?");
        values.push(Value::Integer(limit as i64));
    }

    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values), |row| {
        Ok(RawEventRow {
            sequence: row.get(0)?,
            event_id: row.get(1)?,
            command_id: row.get(2)?,
            stream_id: row.get(3)?,
            event_type: row.get(4)?,
            occurred_at: row.get(5)?,
            payload_json: row.get(6)?,
            metadata_json: row.get(7)?,
        })
    })?;

    let mut events = Vec::new();
    for row in rows {
        events.push(row?.into_stored()?);
    }
    Ok(events)
}

pub fn read(conn: &Connection, options: &ReadOptions) -> Result<Vec<StoredEvent>> {
    read_agent_rows(
        conn,
        options.after_sequence,
        options.through_sequence,
        options.thread_id.as_deref(),
        None,
        options.event_type.as_deref(),
        options.limit,
    )
}

pub fn read_by_command_id(conn: &Connection, command_id: &str) -> Result<Vec<StoredEvent>> {
    read_agent_rows(conn, None, None, None, Some(command_id), None, None)
}

pub fn latest_sequence(conn: &Connection, thread_id: Option<&str>) -> Result<i64> {
    let sequence: i64 = match thread_id {
        Some(thread_id) => conn.query_row(
            "SELECT COALESCE(MAX(sequence), 0)
             FROM orchestration_events
             WHERE application_event_version = 2
               AND aggregate_kind = 'thread'
               AND stream_id = ?1",
            params![thread_id],
            |row| row.get(0),
        )?,
        None => conn.query_row(
            "SELECT COALESCE(MAX(sequence), 0)
             FROM orchestration_events
             WHERE application_event_version = 2
               AND aggregate_kind = 'thread'",
            [],
            |row| row.get(0),
        )?,
    };
    Ok(sequence)
}

pub fn latest_application_sequence(conn: &Connection) -> Result<i64> {
    let sequence: i64 = conn.query_row(
        "SELECT COALESCE(MAX(sequence), 0)
         FROM orchestration_events
         WHERE aggregate_kind = 'project'
            OR (application_event_version = 2 AND aggregate_kind = 'thread')",
        [],
        |row| row.get(0),
    )?;
    Ok(sequence)
}

pub fn read_receipt(conn: &Connection, command_id: &str) -> Result<Option<CommandReceipt>> {
    let receipt = conn
        .query_row(
            "SELECT
                command_id,
                aggregate_kind,
                aggregate_id,
                command_type,
                accepted_at,
                result_sequence,
                status,
                error
             FROM orchestration_command_receipts
             WHERE command_id = ?1",
            params![command_id],
            |row| {
                Ok(CommandReceipt {
                    command_id: row.get(0)?,
                    aggregate_kind: row.get(1)?,
                    aggregate_id: row.get(2)?,
                    command_type: row.get(3)?,
                    accepted_at: row.get(4)?,
                    result_sequence: row.get(5)?,
                    status: row.get(6)?,
                    error: row.get(7)?,
                })
            },
        )
        .optional()?;
    Ok(receipt)
}

/// Reserve a receipt. False means the command id was already used, matching
/// T3's `insertIfAbsent`.
pub fn insert_receipt_if_absent(conn: &Connection, receipt: &CommandReceipt) -> Result<bool> {
    let inserted: Option<String> = conn
        .query_row(
            "INSERT INTO orchestration_command_receipts (
                command_id,
                aggregate_kind,
                aggregate_id,
                command_type,
                accepted_at,
                result_sequence,
                status,
                error
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(command_id) DO NOTHING
             RETURNING command_id",
            params![
                receipt.command_id,
                receipt.aggregate_kind,
                receipt.aggregate_id,
                receipt.command_type,
                receipt.accepted_at,
                receipt.result_sequence,
                receipt.status,
                receipt.error,
            ],
            |row| row.get(0),
        )
        .optional()?;
    Ok(inserted.is_some())
}

pub fn upsert_receipt(conn: &Connection, receipt: &CommandReceipt) -> Result<()> {
    conn.execute(
        "INSERT INTO orchestration_command_receipts (
            command_id,
            aggregate_kind,
            aggregate_id,
            command_type,
            accepted_at,
            result_sequence,
            status,
            error
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(command_id) DO UPDATE SET
            aggregate_kind = excluded.aggregate_kind,
            aggregate_id = excluded.aggregate_id,
            command_type = excluded.command_type,
            accepted_at = excluded.accepted_at,
            result_sequence = excluded.result_sequence,
            status = excluded.status,
            error = excluded.error",
        params![
            receipt.command_id,
            receipt.aggregate_kind,
            receipt.aggregate_id,
            receipt.command_type,
            receipt.accepted_at,
            receipt.result_sequence,
            receipt.status,
            receipt.error,
        ],
    )?;
    Ok(())
}
