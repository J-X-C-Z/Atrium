//! The engine a node hosts for the agents it runs.
//!
//! A thread lives on the device that runs it: its events are appended to the
//! ported T3 store ([`atrium_engine::store`]) with one sequence number per
//! thread, and every Atrium reads that log from where it left off. This module
//! is Atrium's own node code around the ported store: it creates threads,
//! records a user message sent from any device, and answers a cursor with the
//! same replay-or-snapshot decision the engine already makes.
//!
//! The node is the only writer of a thread. Events appended here are ordered
//! by this process; remote callers ask for an append instead of writing a
//! second copy. Time enters as a `now` string so the decisions stay testable.

use std::{path::Path, sync::Mutex};

use atrium_engine::contracts::{
    Actor, AppThread, ConversationMessage, CreationSource, MessageId, MessageRole, ThreadId,
};
use atrium_engine::store::{
    ApplyCommand, BoundedThreadSnapshot, EventStore, MessageProjection, NewEvent, ProjectionWrite,
    StoreError, ThreadProjection, ThreadStreamBatch, ThreadStreamResume,
    build_bounded_thread_stream_snapshot, read_receipt,
};
use serde::Serialize;
use tokio::sync::broadcast;

/// How many thread notices a slow attachment may fall behind before it is told
/// to read the log again. A missed notice is safe: the log is the source.
const NOTICE_BACKLOG: usize = 1024;

/// Why the node's engine refused a request.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("thread {0} already exists on this node")]
    ThreadExists(String),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error("engine database: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, EngineError>;

/// What creating a thread produced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedThread {
    pub id: String,
    pub sequence: i64,
}

/// One thread in a node's list, with the last sequence a client can ask for.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSummary {
    #[serde(flatten)]
    pub thread: AppThread,
    pub high_water: i64,
}

/// A message a remote device asked the owning node to record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SentMessage {
    pub message_id: String,
    pub sequence: i64,
}

/// The append-only thread log a node hosts.
///
/// The store is synchronous (`rusqlite`); callers on an async runtime run the
/// work on a blocking task or accept the short lock. Append notices carry the
/// thread id so an attachment can skip other threads' traffic.
pub struct ThreadEngine {
    store: Mutex<EventStore>,
    notice: broadcast::Sender<String>,
}

impl ThreadEngine {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let store = EventStore::open(path)?;
        Ok(Self::with_store(store))
    }

    /// A process-local engine. Tests use this; a running node keeps a file.
    pub fn in_memory() -> Result<Self> {
        Ok(Self::with_store(EventStore::in_memory()?))
    }

    fn with_store(store: EventStore) -> Self {
        let (notice, _) = broadcast::channel(NOTICE_BACKLOG);
        Self {
            store: Mutex::new(store),
            notice,
        }
    }

    /// Subscribe before reading the log, so an append between the read and the
    /// wait still wakes the attachment.
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.notice.subscribe()
    }

    /// Write a thread and its first event in one transaction. A repeated
    /// command id is the same request; a different command for an existing
    /// thread is refused so the node never forks one thread into two logs.
    pub fn create_thread(
        &self,
        command_id: &str,
        thread: AppThread,
        now: &str,
    ) -> Result<CreatedThread> {
        let thread_id = thread.id.as_str().to_owned();
        let mut store = self.store.lock().expect("engine store");
        if store.projections().get_thread(&thread_id)?.is_some() {
            let connection = store.connection();
            return match read_receipt(connection, command_id)? {
                Some(receipt) => Ok(CreatedThread {
                    id: thread_id,
                    sequence: receipt.result_sequence,
                }),
                None => Err(EngineError::ThreadExists(thread_id)),
            };
        }
        let payload = serde_json::to_value(&thread)?;
        let event = NewEvent::new(
            format!("event:{}:thread-created", uuid::Uuid::new_v4()),
            &thread_id,
            "thread.created",
            now,
            payload,
        );
        let row = thread_projection(&thread)?;
        let applied = store.apply(
            ApplyCommand {
                command_id,
                thread_id: &thread_id,
                command_type: "thread.create",
                accepted_at: now,
            },
            &[event],
            &[ProjectionWrite::Thread(row)],
        )?;
        let sequence = applied.receipt.result_sequence;
        let _ = self.notice.send(thread_id.clone());
        Ok(CreatedThread {
            id: thread_id,
            sequence,
        })
    }

    /// Every thread this node owns, newest first.
    pub fn list_threads(&self) -> Result<Vec<ThreadSummary>> {
        let store = self.store.lock().expect("engine store");
        let connection = store.connection();
        let mut statement = connection.prepare(
            "SELECT thread_id, payload_json
             FROM orchestration_v2_projection_threads
             WHERE deleted_at IS NULL
             ORDER BY updated_at DESC, thread_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut summaries = Vec::new();
        for row in rows {
            let (thread_id, payload) = row?;
            let thread: AppThread = serde_json::from_str(&payload)?;
            let high_water = atrium_engine::store::latest_sequence(connection, Some(&thread_id))?;
            summaries.push(ThreadSummary { thread, high_water });
        }
        Ok(summaries)
    }

    /// Resume from a client cursor. `None` asks for the current snapshot.
    pub fn resume(&self, thread_id: &str, after: Option<i64>) -> Result<ThreadStreamResume> {
        let store = self.store.lock().expect("engine store");
        let stream = store.thread_stream(thread_id);
        match after {
            Some(after) => Ok(stream.resume(after)?),
            None => {
                let high_water = stream.high_water()?;
                let projection = stream
                    .load_projection()?
                    .ok_or_else(|| StoreError::ThreadNotProjected(thread_id.to_owned()))?;
                let snapshot_sequence = u32::try_from(high_water).unwrap_or(u32::MAX);
                Ok(ThreadStreamResume {
                    high_water,
                    batch: ThreadStreamBatch::Snapshot {
                        snapshot: Box::new(build_bounded_thread_stream_snapshot(
                            snapshot_sequence,
                            projection,
                            false,
                        )),
                    },
                })
            }
        }
    }

    /// The snapshot a fresh attachment receives.
    pub fn snapshot(&self, thread_id: &str) -> Result<BoundedThreadSnapshot> {
        match self.resume(thread_id, None)?.batch {
            ThreadStreamBatch::Snapshot { snapshot } => Ok(*snapshot),
            ThreadStreamBatch::Replay { .. } => {
                unreachable!("a snapshot request cannot replay")
            }
        }
    }

    /// Record a user message from any device. The owning node is the writer:
    /// the message enters this thread's log once, with the next sequence, and
    /// every attachment sees it. A repeated command id returns the first
    /// sequence instead of writing twice.
    pub fn send_message(
        &self,
        command_id: &str,
        thread_id: &str,
        message_id: &str,
        text: &str,
        now: &str,
    ) -> Result<SentMessage> {
        let message = ConversationMessage {
            notification: None,
            created_by: Actor::User,
            creation_source: CreationSource::Web,
            scheduled_task_id: None,
            follows_thread_selection: None,
            sender_thread_id: None,
            linked_sender: None,
            idle_compaction: None,
            workflow_script: None,
            id: MessageId::new(message_id),
            thread_id: ThreadId::new(thread_id),
            run_id: None,
            node_id: None,
            role: MessageRole::User,
            text: text.to_owned(),
            context: None,
            attachments: Vec::new(),
            streaming: false,
            created_at: now.to_owned(),
            updated_at: now.to_owned(),
            delegated_completion: None,
        };
        let mut store = self.store.lock().expect("engine store");
        if store.projections().get_thread(thread_id)?.is_none() {
            return Err(EngineError::Store(StoreError::ThreadNotProjected(
                thread_id.to_owned(),
            )));
        }
        let event = NewEvent::new(
            format!("event:{}:message", uuid::Uuid::new_v4()),
            thread_id,
            "message.updated",
            now,
            serde_json::to_value(&message)?,
        );
        let row = MessageProjection {
            message_id: message_id.to_owned(),
            thread_id: thread_id.to_owned(),
            turn_id: None,
            role: "user".to_owned(),
            text: text.to_owned(),
            is_streaming: false,
            created_at: now.to_owned(),
            updated_at: now.to_owned(),
        };
        let applied = store.apply(
            ApplyCommand {
                command_id,
                thread_id,
                command_type: "message.dispatch",
                accepted_at: now,
            },
            &[event],
            &[ProjectionWrite::Message(row)],
        )?;
        let sequence = applied.receipt.result_sequence;
        let _ = self.notice.send(thread_id.to_owned());
        Ok(SentMessage {
            message_id: message_id.to_owned(),
            sequence,
        })
    }

    /// Append events from the local engine (the adapter that runs here). The
    /// same notice wakes remote attachments.
    pub fn append(
        &self,
        command_id: Option<&str>,
        events: &[NewEvent],
    ) -> Result<Vec<atrium_engine::contracts::StoredEvent>> {
        let store = self.store.lock().expect("engine store");
        let stored = store.append(command_id, events)?;
        for event in &stored {
            let _ = self.notice.send(event.thread_id.as_str().to_owned());
        }
        Ok(stored)
    }

    /// Whether this node owns a thread.
    pub fn exists(&self, thread_id: &str) -> Result<bool> {
        let store = self.store.lock().expect("engine store");
        Ok(store.projections().get_thread(thread_id)?.is_some())
    }

    /// The last sequence written to a thread.
    pub fn high_water(&self, thread_id: &str) -> Result<i64> {
        let store = self.store.lock().expect("engine store");
        Ok(atrium_engine::store::latest_sequence(
            store.connection(),
            Some(thread_id),
        )?)
    }
}

fn thread_projection(thread: &AppThread) -> Result<ThreadProjection> {
    let runtime_mode = wire_string(&thread.runtime_mode)?;
    let interaction_mode = wire_string(&thread.interaction_mode)?;
    Ok(ThreadProjection {
        thread_id: thread.id.as_str().to_owned(),
        project_id: thread.project_id.as_str().to_owned(),
        title: thread.title.clone(),
        default_provider: thread.provider_instance_id.as_str().to_owned(),
        provider_instance_id: thread.provider_instance_id.as_str().to_owned(),
        runtime_mode,
        interaction_mode,
        active_provider_thread_id: thread
            .active_provider_thread_id
            .as_ref()
            .map(|id| id.as_str().to_owned()),
        created_at: thread.created_at.clone(),
        updated_at: thread.updated_at.clone(),
        archived_at: thread.archived_at.clone(),
        deleted_at: thread.deleted_at.clone(),
        payload_json: serde_json::to_string(thread)?,
    })
}

fn wire_string(value: &impl serde::Serialize) -> Result<String> {
    let value = serde_json::to_value(value)?;
    Ok(value.as_str().unwrap_or_default().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use atrium_engine::contracts::StoredEvent;
    use serde_json::json;

    const NOW: &str = "2026-11-24T00:00:00.000Z";

    fn thread_value(id: &str) -> serde_json::Value {
        json!({
            "createdBy": "user",
            "creationSource": "web",
            "id": id,
            "projectId": "project-1",
            "title": "Thread",
            "providerInstanceId": "codex",
            "modelSelection": { "instanceId": "codex", "model": "gpt-5" },
            "runtimeMode": "full-access",
            "interactionMode": "default",
            "branch": null,
            "worktreePath": null,
            "activeProviderThreadId": null,
            "lineage": {
                "parentThreadId": null,
                "relationshipToParent": null,
                "rootThreadId": id,
            },
            "forkedFrom": null,
            "createdAt": NOW,
            "updatedAt": NOW,
            "archivedAt": null,
            "settledOverride": null,
            "settledAt": null,
            "deletedAt": null,
        })
    }

    fn thread(id: &str) -> AppThread {
        serde_json::from_value(thread_value(id)).expect("app thread")
    }

    fn engine_with_thread() -> (ThreadEngine, String) {
        let engine = ThreadEngine::in_memory().unwrap();
        let id = "thread-node-1".to_owned();
        engine
            .create_thread("command:create", thread(&id), NOW)
            .unwrap();
        (engine, id)
    }

    fn append(engine: &ThreadEngine, thread_id: &str, count: usize) -> Vec<StoredEvent> {
        let events = (0..count)
            .map(|index| {
                NewEvent::new(
                    format!("event:append-{index}"),
                    thread_id,
                    "run.updated",
                    NOW,
                    json!({ "index": index }),
                )
            })
            .collect::<Vec<_>>();
        engine.append(None, &events).unwrap()
    }

    fn set_pruned_through(engine: &ThreadEngine, sequence: i64) {
        let store = engine.store.lock().unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO orchestration_v2_projection_metadata (
                    projection_name, schema_version, last_sequence, updated_at
                 ) VALUES ('event-log-pruned-through', 2, ?1, ?2)
                 ON CONFLICT(projection_name) DO UPDATE SET last_sequence = excluded.last_sequence",
                rusqlite::params![sequence, NOW],
            )
            .unwrap();
    }

    #[test]
    fn log_replays_from_the_client_sequence_and_repeats_nothing() {
        let (engine, id) = engine_with_thread();
        let stored = append(&engine, &id, 2);
        assert_eq!(stored[0].sequence, 2);
        let resumed = engine.resume(&id, Some(1)).unwrap();
        match resumed.batch {
            ThreadStreamBatch::Replay {
                after_sequence,
                through_sequence,
                events,
            } => {
                assert_eq!(after_sequence, 1);
                assert_eq!(through_sequence, 3);
                assert_eq!(
                    events
                        .iter()
                        .map(|event| event.sequence)
                        .collect::<Vec<_>>(),
                    vec![2, 3]
                );
            }
            other => panic!("expected replay, got {other:?}"),
        }
        let caught_up = engine.resume(&id, Some(3)).unwrap();
        assert!(matches!(
            caught_up.batch,
            ThreadStreamBatch::Replay { ref events, .. } if events.is_empty()
        ));
    }

    #[test]
    fn a_cursor_below_the_pruned_boundary_receives_one_snapshot() {
        let (engine, id) = engine_with_thread();
        append(&engine, &id, 4);
        set_pruned_through(&engine, 3);
        let resumed = engine.resume(&id, Some(1)).unwrap();
        assert_eq!(resumed.high_water, 5);
        match resumed.batch {
            ThreadStreamBatch::Snapshot { snapshot } => {
                assert_eq!(snapshot.snapshot_sequence, 5);
                assert_eq!(snapshot.projection.thread.id.as_str(), id);
            }
            other => panic!("expected snapshot, got {other:?}"),
        }
    }

    #[test]
    fn a_replay_over_the_engine_cap_receives_one_snapshot() {
        let (engine, id) = engine_with_thread();
        append(
            &engine,
            &id,
            atrium_engine::store::THREAD_RESUME_MAX_REPLAY_EVENTS + 1,
        );
        let resumed = engine.resume(&id, Some(1)).unwrap();
        assert!(matches!(resumed.batch, ThreadStreamBatch::Snapshot { .. }));
    }

    #[test]
    fn a_cursor_ahead_of_the_log_receives_the_current_snapshot() {
        let (engine, id) = engine_with_thread();
        append(&engine, &id, 1);
        let resumed = engine.resume(&id, Some(99)).unwrap();
        assert!(matches!(resumed.batch, ThreadStreamBatch::Snapshot { .. }));
    }

    #[test]
    fn sending_from_another_device_appends_once_and_returns_its_sequence() {
        let (engine, id) = engine_with_thread();
        let first = engine
            .send_message("command:send", &id, "message-1", "hello", NOW)
            .unwrap();
        assert_eq!(first.sequence, 2);
        let again = engine
            .send_message("command:send", &id, "message-1", "hello", NOW)
            .unwrap();
        assert_eq!(again.sequence, 2, "a retry does not write twice");
        assert_eq!(engine.high_water(&id).unwrap(), 2);
        let replay = engine.resume(&id, Some(1)).unwrap();
        match replay.batch {
            ThreadStreamBatch::Replay { events, .. } => {
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].event_type, "message.updated");
                assert_eq!(events[0].payload["text"], "hello");
            }
            other => panic!("expected replay, got {other:?}"),
        }
    }

    #[test]
    fn creating_the_same_thread_again_needs_the_same_command() {
        let engine = ThreadEngine::in_memory().unwrap();
        engine
            .create_thread("command:create", thread("thread-node-2"), NOW)
            .unwrap();
        let same = engine
            .create_thread("command:create", thread("thread-node-2"), NOW)
            .unwrap();
        assert_eq!(same.sequence, 1);
        assert!(matches!(
            engine.create_thread("command:other", thread("thread-node-2"), NOW),
            Err(EngineError::ThreadExists(_))
        ));
    }

    #[test]
    fn the_node_lists_its_threads_with_their_high_water() {
        let (engine, id) = engine_with_thread();
        append(&engine, &id, 2);
        let threads = engine.list_threads().unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].thread.id.as_str(), id);
        assert_eq!(threads[0].high_water, 3);
    }
}
