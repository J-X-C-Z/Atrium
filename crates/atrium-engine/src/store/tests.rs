// Ported from apps/server/src/persistence/OrchestrationEventStore.test.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/persistence/OrchestrationEventStore.sequence.test.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/orchestration-v2/ProjectionStore.test.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/orchestration-v2/FoundationPersistence.test.ts (T3 Code v0.1.11, MIT).
//!
//! The store cases from T3 that this module owns: sequence ordering, event id
//! uniqueness, receipt idempotency, transaction rollback, projection upserts
//! and payload passthrough. A temp SQLite file is used throughout, as the T3
//! file-backed cases do.

use serde_json::{Value, json};

use crate::store::StoreError;
use crate::store::events::{
    ApplyCommand, CommandReceipt, EventStore, NewEvent, NewProjectEvent, ReadOptions,
};
use crate::store::projections::{
    ActivityProjection, MessageProjection, NodeProjection, PendingApprovalProjection,
    ProjectionWrite, ProjectorState, ProviderSessionProjection, RunAttemptProjection,
    RunProjection, ThreadProjection, TurnProjection,
};

const T0: &str = "2026-01-01T00:00:00.000Z";
const T1: &str = "2026-01-01T00:00:01.000Z";

fn event(id: &str, thread_id: &str, event_type: &str) -> NewEvent {
    NewEvent::new(id, thread_id, event_type, T0, json!({ "id": id }))
}

fn store() -> (tempfile::TempDir, EventStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = EventStore::open(dir.path().join("store.sqlite")).expect("open store");
    (dir, store)
}

fn command<'a>(command_id: &'a str, thread_id: &'a str) -> ApplyCommand<'a> {
    ApplyCommand {
        command_id,
        thread_id,
        command_type: "thread.create",
        accepted_at: T0,
    }
}

fn thread_projection(thread_id: &str, payload: &str) -> ThreadProjection {
    ThreadProjection {
        thread_id: thread_id.to_string(),
        project_id: "project:one".to_string(),
        title: "Thread".to_string(),
        default_provider: "codex".to_string(),
        provider_instance_id: "codex".to_string(),
        runtime_mode: "full-access".to_string(),
        interaction_mode: "default".to_string(),
        active_provider_thread_id: None,
        created_at: T0.to_string(),
        updated_at: T0.to_string(),
        archived_at: None,
        deleted_at: None,
        payload_json: payload.to_string(),
    }
}

fn run_projection(run_id: &str, thread_id: &str, ordinal: i64) -> RunProjection {
    RunProjection {
        run_id: run_id.to_string(),
        thread_id: thread_id.to_string(),
        ordinal,
        provider: "codex".to_string(),
        provider_instance_id: "codex".to_string(),
        provider_thread_id: None,
        status: "running".to_string(),
        requested_at: T0.to_string(),
        completed_at: None,
        payload_json: format!(r#"{{"id":"{run_id}","ordinal":{ordinal}}}"#),
    }
}

#[test]
fn migrations_create_the_t3_schema_and_rerun_cleanly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.sqlite");
    {
        let store = EventStore::open(&path).expect("first open");
        let conn = store.connection();
        let mut statement = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap();
        let tables: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        for expected in [
            "orchestration_events",
            "orchestration_command_receipts",
            "orchestration_v2_events",
            "orchestration_v2_command_receipts",
            "orchestration_v2_projection_threads",
            "orchestration_v2_projection_runs",
            "orchestration_v2_projection_run_attempts",
            "orchestration_v2_projection_nodes",
            "orchestration_v2_projection_provider_sessions",
            "projection_thread_messages",
            "projection_thread_activities",
            "projection_turns",
            "projection_pending_approvals",
            "projection_state",
            "effect_sql_migrations",
        ] {
            assert!(
                tables.iter().any(|name| name == expected),
                "missing {expected}"
            );
        }

        let unique_index: bool = conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master
                    WHERE type = 'index'
                      AND name = 'orchestration_v2_projection_runs_thread_ordinal_idx'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(unique_index);

        let mut statement = conn
            .prepare("PRAGMA table_info(orchestration_v2_projection_provider_sessions)")
            .unwrap();
        let columns: Vec<String> = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert!(columns.iter().any(|name| name == "driver"));
        assert!(columns.iter().any(|name| name == "provider_instance_id"));
    }

    // A second open sees every migration recorded and changes nothing.
    let store = EventStore::open(&path).expect("second open");
    let applied: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM effect_sql_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(applied, 4);
}

#[test]
fn append_assigns_monotonic_sequences_and_derives_index_columns() {
    let (_dir, store) = store();
    let mut first = event("event:1", "thread:a", "thread.created");
    first.run_id = Some("run:1".to_string());
    first.node_id = Some("node:1".to_string());
    first.driver = Some("codex".to_string());
    first.provider_instance_id = Some("codex".to_string());
    first.raw_event_id = Some("raw:1".to_string());
    let second = event("event:2", "thread:a", "run.created");
    let other = event("event:3", "thread:b", "thread.created");

    let stored = store
        .append(Some("command:1"), &[first.clone(), second, other])
        .unwrap();
    assert_eq!(
        stored
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(stored[0].command_id.as_deref(), Some("command:1"));
    assert_eq!(stored[0].run_id.as_deref(), Some("run:1"));

    let conn = store.connection();
    let (kind, stream, version, event_type, command_id, correlation_id, actor): (
        String,
        String,
        i64,
        String,
        Option<String>,
        Option<String>,
        String,
    ) = conn
        .query_row(
            "SELECT aggregate_kind, stream_id, stream_version, event_type, command_id,
                    correlation_id, actor_kind
             FROM orchestration_events
             WHERE event_id = 'event:1'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(kind, "thread");
    assert_eq!(stream, "thread:a");
    assert_eq!(version, 0);
    assert_eq!(event_type, "thread.created");
    assert_eq!(command_id.as_deref(), Some("command:1"));
    assert_eq!(correlation_id.as_deref(), Some("command:1"));
    assert_eq!(actor, "provider");

    let metadata: Value = serde_json::from_str(
        &conn
            .query_row(
                "SELECT metadata_json FROM orchestration_events WHERE event_id = 'event:1'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(metadata["runId"], "run:1");
    assert_eq!(metadata["nodeId"], "node:1");
    assert_eq!(metadata["driver"], "codex");
    assert_eq!(metadata["providerInstanceId"], "codex");
    assert_eq!(metadata["rawEventId"], "raw:1");

    let versions: Vec<i64> = {
        let mut statement = conn
            .prepare(
                "SELECT stream_version FROM orchestration_events
                 WHERE stream_id = 'thread:a' ORDER BY sequence",
            )
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect()
    };
    assert_eq!(versions, vec![0, 1]);
    let other_version: i64 = conn
        .query_row(
            "SELECT stream_version FROM orchestration_events WHERE event_id = 'event:3'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(other_version, 0);
}

#[test]
fn duplicate_event_id_is_rejected_by_the_unique_index() {
    let (_dir, store) = store();
    let event = event("event:dup", "thread:a", "thread.created");
    store.append(None, std::slice::from_ref(&event)).unwrap();
    let error = store.append(None, &[event]).unwrap_err();
    assert!(matches!(error, StoreError::Sql(_)));
    let count: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM orchestration_events", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn read_orders_by_sequence_and_applies_every_filter() {
    let (_dir, store) = store();
    let events = vec![
        event("event:1", "thread:a", "thread.created"),
        event("event:2", "thread:a", "run.created"),
        event("event:3", "thread:b", "thread.created"),
        event("event:4", "thread:a", "thread.created"),
    ];
    store.append(None, &events).unwrap();

    let sequences = |options: ReadOptions| -> Vec<i64> {
        store
            .read(&options)
            .unwrap()
            .into_iter()
            .map(|event| event.sequence)
            .collect()
    };

    assert_eq!(sequences(ReadOptions::default()), vec![1, 2, 3, 4]);
    assert_eq!(
        sequences(ReadOptions {
            after_sequence: Some(1),
            ..ReadOptions::default()
        }),
        vec![2, 3, 4]
    );
    assert_eq!(
        sequences(ReadOptions {
            through_sequence: Some(2),
            ..ReadOptions::default()
        }),
        vec![1, 2]
    );
    assert_eq!(
        sequences(ReadOptions {
            thread_id: Some("thread:a".to_string()),
            ..ReadOptions::default()
        }),
        vec![1, 2, 4]
    );
    assert_eq!(
        sequences(ReadOptions {
            event_type: Some("thread.created".to_string()),
            ..ReadOptions::default()
        }),
        vec![1, 3, 4]
    );
    assert_eq!(
        sequences(ReadOptions {
            limit: Some(2),
            ..ReadOptions::default()
        }),
        vec![1, 2]
    );
    assert_eq!(
        sequences(ReadOptions {
            thread_id: Some("thread:a".to_string()),
            event_type: Some("thread.created".to_string()),
            ..ReadOptions::default()
        }),
        vec![1, 4]
    );
    assert!(
        sequences(ReadOptions {
            limit: Some(0),
            ..ReadOptions::default()
        })
        .is_empty()
    );
}

#[test]
fn read_by_command_id_returns_only_that_command_in_order() {
    let (_dir, store) = store();
    let first = event("event:c1:1", "thread:a", "thread.created");
    let second = event("event:c1:2", "thread:a", "run.created");
    let other = event("event:c2:1", "thread:a", "run.updated");
    store.append(Some("command:1"), &[first, second]).unwrap();
    store.append(Some("command:2"), &[other]).unwrap();

    let first_command = store.read_by_command_id("command:1").unwrap();
    assert_eq!(
        first_command
            .iter()
            .map(|event| event.event_id.as_str())
            .collect::<Vec<_>>(),
        vec!["event:c1:1", "event:c1:2"]
    );
    assert_eq!(store.read_by_command_id("command:2").unwrap().len(), 1);
    assert!(store.read_by_command_id("command:none").unwrap().is_empty());
}

#[test]
fn latest_marks_ignore_legacy_version_one_rows() {
    let (_dir, store) = store();
    store
        .connection()
        .execute(
            "INSERT INTO orchestration_events (
                event_id, aggregate_kind, stream_id, stream_version, event_type,
                occurred_at, actor_kind, payload_json, metadata_json,
                application_event_version
             ) VALUES (
                'event:legacy', 'thread', 'legacy-only', 1, 'provider-session.detached',
                ?1, 'server', '{}', '{}', 1
             )",
            rusqlite::params![T0],
        )
        .unwrap();

    // Legacy V1 history is not part of either high-water mark.
    assert_eq!(store.latest_sequence(None).unwrap(), 0);
    assert_eq!(store.latest_sequence(Some("legacy-only")).unwrap(), 0);
    assert_eq!(store.latest_application_sequence().unwrap(), 0);

    store
        .append(None, &[event("event:v2", "legacy-only", "thread.created")])
        .unwrap();
    assert_eq!(store.latest_sequence(None).unwrap(), 2);
    assert_eq!(store.latest_application_sequence().unwrap(), 2);
}

#[test]
fn latest_sequences_cover_agent_and_application_high_water_marks() {
    let (_dir, store) = store();
    store
        .append(
            None,
            &[
                event("event:1", "thread:a", "thread.created"),
                event("event:2", "thread:b", "thread.created"),
                event("event:3", "thread:a", "run.created"),
            ],
        )
        .unwrap();
    let project_event = store
        .append_project_event(&NewProjectEvent {
            event_id: "event:project".to_string(),
            project_id: "project:one".to_string(),
            event_type: "project.created".to_string(),
            occurred_at: T0.to_string(),
            command_id: Some("command:project".to_string()),
            causation_event_id: None,
            correlation_id: Some("command:project".to_string()),
            payload: json!({ "projectId": "project:one" }),
            metadata: json!({}),
        })
        .unwrap();

    assert_eq!(project_event.sequence, 4);
    assert_eq!(store.latest_sequence(Some("thread:a")).unwrap(), 3);
    assert_eq!(store.latest_sequence(Some("thread:b")).unwrap(), 2);
    assert_eq!(store.latest_sequence(None).unwrap(), 3);
    assert_eq!(store.latest_application_sequence().unwrap(), 4);
}

#[test]
fn apply_writes_events_receipt_and_projections_atomically() {
    let (_dir, mut store) = store();
    let events = vec![
        event("event:apply:1", "thread:apply", "thread.created"),
        event("event:apply:2", "thread:apply", "run.created"),
    ];
    let projections = vec![
        ProjectionWrite::Thread(thread_projection(
            "thread:apply",
            r#"{"id":"thread:apply"}"#,
        )),
        ProjectionWrite::Run(run_projection("run:apply", "thread:apply", 1)),
    ];

    let result = store
        .apply(
            command("command:apply", "thread:apply"),
            &events,
            &projections,
        )
        .unwrap();
    assert!(result.applied);
    assert_eq!(result.receipt.command_id, "command:apply");
    assert_eq!(result.receipt.result_sequence, 2);
    assert_eq!(result.receipt.status, "accepted");
    assert_eq!(
        result
            .events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );

    assert_eq!(store.take_published(), result.events);

    let projections = store.projections();
    assert_eq!(
        projections
            .get_thread("thread:apply")
            .unwrap()
            .unwrap()
            .title,
        "Thread"
    );
    assert_eq!(
        projections
            .list_runs("thread:apply")
            .unwrap()
            .first()
            .unwrap()
            .run_id,
        "run:apply"
    );
}

#[test]
fn apply_is_idempotent_for_a_repeated_command_id() {
    let (_dir, mut store) = store();
    let first = store
        .apply(
            command("command:once", "thread:once"),
            &[event("event:once:1", "thread:once", "thread.created")],
            &[ProjectionWrite::Thread(thread_projection(
                "thread:once",
                "{}",
            ))],
        )
        .unwrap();
    assert!(first.applied);
    assert_eq!(first.receipt.result_sequence, 1);
    store.take_published();

    let retry = store
        .apply(
            command("command:once", "thread:once"),
            &[event("event:once:retry", "thread:once", "thread.created")],
            &[],
        )
        .unwrap();
    assert!(!retry.applied);
    assert_eq!(retry.receipt, first.receipt);
    assert_eq!(retry.events, first.events);
    assert!(
        store.take_published().is_empty(),
        "a retry publishes nothing"
    );

    let count: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM orchestration_events", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
    let event = store.read_by_command_id("command:once").unwrap();
    assert_eq!(event.len(), 1);
    assert_eq!(event[0].event_id, "event:once:1");
}

#[test]
fn apply_rolls_back_events_receipt_and_projections_on_failure() {
    let (_dir, mut store) = store();
    // Two runs with the same (thread_id, ordinal) violate the unique index in
    // the middle of the transaction.
    let projections = vec![
        ProjectionWrite::Run(run_projection("run:first", "thread:rollback", 1)),
        ProjectionWrite::Run(run_projection("run:second", "thread:rollback", 1)),
    ];
    let error = store
        .apply(
            command("command:rollback", "thread:rollback"),
            &[event("event:rollback", "thread:rollback", "run.created")],
            &projections,
        )
        .unwrap_err();
    assert!(matches!(error, StoreError::Sql(_)));

    assert!(
        store
            .read_by_command_id("command:rollback")
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .projections()
            .list_runs("thread:rollback")
            .unwrap()
            .is_empty()
    );
    assert!(store.take_published().is_empty());
    let receipts: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM orchestration_command_receipts",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(receipts, 0);

    // The rollback leaves the command id free and later writes work.
    let applied = store
        .apply(
            command("command:rollback", "thread:rollback"),
            &[event("event:rollback", "thread:rollback", "run.created")],
            &[
                ProjectionWrite::Run(run_projection("run:first", "thread:rollback", 1)),
                ProjectionWrite::Run(run_projection("run:second", "thread:rollback", 2)),
            ],
        )
        .unwrap();
    assert!(applied.applied);
    assert_eq!(
        store
            .projections()
            .list_runs("thread:rollback")
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn receipts_read_back_with_the_original_result_sequence() {
    let (_dir, mut store) = store();
    let applied = store
        .apply(
            command("command:receipt", "thread:receipt"),
            &[event("event:receipt", "thread:receipt", "thread.created")],
            &[],
        )
        .unwrap();
    let receipt = CommandReceipt {
        command_id: "command:receipt".to_string(),
        aggregate_kind: "thread".to_string(),
        aggregate_id: "thread:receipt".to_string(),
        command_type: "thread.create".to_string(),
        accepted_at: T0.to_string(),
        result_sequence: applied.receipt.result_sequence,
        status: "accepted".to_string(),
        error: None,
    };
    assert_eq!(applied.receipt, receipt);
    assert_eq!(
        crate::store::events::read_receipt(store.connection(), "command:receipt").unwrap(),
        Some(receipt)
    );
}

#[test]
fn v055_projections_upsert_and_read_with_payload_passthrough() {
    let (_dir, store) = store();
    let projections = store.projections();

    let thread_payload = "{\"id\":\"thread:p\",\"modelSelection\":{\"model\":\"gpt-5.4\"}}";
    projections
        .upsert_thread(&thread_projection("thread:p", thread_payload))
        .unwrap();
    projections
        .upsert_run(&run_projection("run:p:1", "thread:p", 1))
        .unwrap();
    projections
        .upsert_run(&run_projection("run:p:2", "thread:p", 2))
        .unwrap();
    projections
        .upsert_run_attempt(&RunAttemptProjection {
            attempt_id: "attempt:1".to_string(),
            thread_id: "thread:p".to_string(),
            run_id: "run:p:1".to_string(),
            attempt_ordinal: 1,
            root_node_id: "node:root".to_string(),
            provider: "codex".to_string(),
            provider_instance_id: "codex".to_string(),
            provider_thread_id: "provider-thread:1".to_string(),
            provider_turn_id: None,
            status: "running".to_string(),
            payload_json: "{\"id\":\"attempt:1\"}".to_string(),
        })
        .unwrap();
    projections
        .upsert_node(&NodeProjection {
            node_id: "node:root".to_string(),
            thread_id: "thread:p".to_string(),
            run_id: Some("run:p:1".to_string()),
            parent_node_id: None,
            root_node_id: "node:root".to_string(),
            kind: "root".to_string(),
            status: "running".to_string(),
            provider_thread_id: None,
            provider_turn_id: None,
            started_at: Some(T0.to_string()),
            completed_at: None,
            payload_json: "{\"id\":\"node:root\"}".to_string(),
        })
        .unwrap();
    projections
        .upsert_provider_session(&ProviderSessionProjection {
            provider_session_id: "session:1".to_string(),
            thread_id: Some("thread:p".to_string()),
            provider: "codex".to_string(),
            driver: Some("codex".to_string()),
            provider_instance_id: Some("codex".to_string()),
            status: "running".to_string(),
            updated_at: T0.to_string(),
            payload_json: "{\"id\":\"session:1\",\"model\":\"gpt-5.4\"}".to_string(),
        })
        .unwrap();

    let thread = projections.get_thread("thread:p").unwrap().unwrap();
    assert_eq!(
        thread.payload_json, thread_payload,
        "payload is stored verbatim"
    );
    assert_eq!(thread.provider_instance_id, "codex");
    assert_eq!(
        projections
            .list_runs("thread:p")
            .unwrap()
            .iter()
            .map(|run| run.ordinal)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(
        projections
            .list_run_attempts("run:p:1")
            .unwrap()
            .first()
            .unwrap()
            .attempt_id,
        "attempt:1"
    );
    assert_eq!(
        projections
            .list_nodes("thread:p")
            .unwrap()
            .first()
            .unwrap()
            .node_id,
        "node:root"
    );
    let sessions = projections.list_provider_sessions("thread:p").unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].driver.as_deref(), Some("codex"));
    assert_eq!(
        sessions[0].payload_json,
        "{\"id\":\"session:1\",\"model\":\"gpt-5.4\"}"
    );

    // Upserting the same key rewrites the row instead of adding one.
    let mut updated = thread_projection("thread:p", thread_payload);
    updated.title = "Renamed".to_string();
    updated.updated_at = T1.to_string();
    projections.upsert_thread(&updated).unwrap();
    let threads = projections.list_threads("project:one").unwrap();
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].title, "Renamed");
}

#[test]
fn v055_unique_keys_reject_duplicate_run_and_attempt_ordinals() {
    let (_dir, store) = store();
    let projections = store.projections();
    projections
        .upsert_run(&run_projection("run:u:1", "thread:u", 1))
        .unwrap();
    assert!(
        projections
            .upsert_run(&run_projection("run:u:2", "thread:u", 1))
            .is_err()
    );

    projections
        .upsert_run_attempt(&RunAttemptProjection {
            attempt_id: "attempt:u:1".to_string(),
            thread_id: "thread:u".to_string(),
            run_id: "run:u:1".to_string(),
            attempt_ordinal: 1,
            root_node_id: "node:u".to_string(),
            provider: "codex".to_string(),
            provider_instance_id: "codex".to_string(),
            provider_thread_id: "provider-thread:u".to_string(),
            provider_turn_id: None,
            status: "running".to_string(),
            payload_json: "{}".to_string(),
        })
        .unwrap();
    let duplicate = RunAttemptProjection {
        attempt_id: "attempt:u:2".to_string(),
        attempt_ordinal: 1,
        ..RunAttemptProjection {
            attempt_id: "attempt:u:1".to_string(),
            thread_id: "thread:u".to_string(),
            run_id: "run:u:1".to_string(),
            attempt_ordinal: 1,
            root_node_id: "node:u".to_string(),
            provider: "codex".to_string(),
            provider_instance_id: "codex".to_string(),
            provider_thread_id: "provider-thread:u".to_string(),
            provider_turn_id: None,
            status: "running".to_string(),
            payload_json: "{}".to_string(),
        }
    };
    assert!(projections.upsert_run_attempt(&duplicate).is_err());
}

#[test]
fn v005_projections_upsert_and_read() {
    let (_dir, store) = store();
    let projections = store.projections();

    projections
        .upsert_message(&MessageProjection {
            message_id: "message:1".to_string(),
            thread_id: "thread:v1".to_string(),
            turn_id: Some("turn:1".to_string()),
            role: "user".to_string(),
            text: "Hello".to_string(),
            is_streaming: false,
            created_at: T0.to_string(),
            updated_at: T0.to_string(),
        })
        .unwrap();
    projections
        .upsert_message(&MessageProjection {
            message_id: "message:2".to_string(),
            thread_id: "thread:v1".to_string(),
            turn_id: Some("turn:1".to_string()),
            role: "assistant".to_string(),
            text: "Hi".to_string(),
            is_streaming: true,
            created_at: T1.to_string(),
            updated_at: T1.to_string(),
        })
        .unwrap();

    projections
        .upsert_activity(&ActivityProjection {
            activity_id: "activity:1".to_string(),
            thread_id: "thread:v1".to_string(),
            turn_id: Some("turn:1".to_string()),
            tone: "info".to_string(),
            kind: "tool".to_string(),
            summary: "Read a file".to_string(),
            payload_json: "{\"path\":\"a.rs\"}".to_string(),
            created_at: T0.to_string(),
        })
        .unwrap();

    let turn = TurnProjection {
        thread_id: "thread:v1".to_string(),
        turn_id: Some("turn:1".to_string()),
        pending_message_id: Some("message:1".to_string()),
        assistant_message_id: None,
        state: "running".to_string(),
        requested_at: T0.to_string(),
        started_at: Some(T0.to_string()),
        completed_at: None,
        checkpoint_turn_count: Some(1),
        checkpoint_ref: None,
        checkpoint_status: None,
        checkpoint_files_json: "[]".to_string(),
    };
    projections.upsert_turn(&turn).unwrap();
    let mut completed = turn.clone();
    completed.state = "completed".to_string();
    completed.completed_at = Some(T1.to_string());
    projections.upsert_turn(&completed).unwrap();

    projections
        .upsert_pending_approval(&PendingApprovalProjection {
            request_id: "request:1".to_string(),
            thread_id: "thread:v1".to_string(),
            turn_id: Some("turn:1".to_string()),
            status: "pending".to_string(),
            decision: None,
            created_at: T0.to_string(),
            resolved_at: None,
        })
        .unwrap();

    projections
        .upsert_projector_state(&ProjectorState {
            projector: "thread-projections".to_string(),
            last_applied_sequence: 7,
            updated_at: T0.to_string(),
        })
        .unwrap();

    let messages = projections.list_messages("thread:v1").unwrap();
    assert_eq!(
        messages
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        vec!["Hello", "Hi"]
    );
    assert!(!messages[0].is_streaming);
    assert!(messages[1].is_streaming);

    let activities = projections.list_activities("thread:v1").unwrap();
    assert_eq!(activities.len(), 1);
    assert_eq!(activities[0].payload_json, "{\"path\":\"a.rs\"}");

    let turns = projections.list_turns("thread:v1").unwrap();
    assert_eq!(turns.len(), 1, "the turn upsert keeps one row");
    assert_eq!(turns[0].state, "completed");
    assert_eq!(turns[0].completed_at.as_deref(), Some(T1));

    let approvals = projections
        .list_pending_approvals("thread:v1", "pending")
        .unwrap();
    assert_eq!(approvals.len(), 1);
    assert!(
        projections
            .list_pending_approvals("thread:v1", "resolved")
            .unwrap()
            .is_empty()
    );

    let state = projections
        .get_projector_state("thread-projections")
        .unwrap()
        .unwrap();
    assert_eq!(state.last_applied_sequence, 7);
}
