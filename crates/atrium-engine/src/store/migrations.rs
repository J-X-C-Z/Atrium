// Ported from apps/server/src/persistence/Migrations/001_OrchestrationEvents.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/persistence/Migrations/002_OrchestrationCommandReceipts.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/persistence/Migrations/005_Projections.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/persistence/Migrations/055_OrchestrationV2.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/persistence/Migrations/OrchestrationV2/*.ts (T3 Code v0.1.11, MIT).
//!
//! Portfolio of the migrations the store needs, applied in T3's order and
//! recorded in T3's `effect_sql_migrations` table. Table and index names,
//! columns, nullability and primary keys match the T3 DDL.
//!
//! `055` imports twelve sub-migrations; they are kept as separate functions
//! here in the same order. `ApplicationEventSource` reads
//! `projection_projects.default_model_selection_json`, which T3 adds in
//! migration 016 before 055 runs. This port's chain starts at 001, so it adds
//! that one column (and fills it from `default_model`, as 016 does) before the
//! 055 backfill; nothing else from 016 is needed by the ported tables.

use rusqlite::{Connection, TransactionBehavior, params};
use serde_json::{Map, Value as JsonValue};

use crate::store::events::Result;

const CREATE_MIGRATIONS_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS effect_sql_migrations (
      migration_id INTEGER PRIMARY KEY,
      name TEXT NOT NULL
    )
";

type MigrationFn = fn(&Connection) -> Result<()>;

const MIGRATIONS: &[(i64, &str, MigrationFn)] = &[
    (1, "OrchestrationEvents", m001_orchestration_events),
    (
        2,
        "OrchestrationCommandReceipts",
        m002_orchestration_command_receipts,
    ),
    (5, "Projections", m005_projections),
    (55, "OrchestrationV2", m055_orchestration_v2),
];

/// Apply every migration that has not run yet. Each migration and its
/// bookkeeping row commit together; a failure leaves the database at the
/// previous migration.
pub fn migrate(conn: &mut Connection) -> Result<()> {
    conn.execute_batch(CREATE_MIGRATIONS_TABLE)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for &(id, name, apply) in MIGRATIONS {
        let applied: bool = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM effect_sql_migrations WHERE migration_id = ?1
             )",
            params![id],
            |row| row.get(0),
        )?;
        if applied {
            continue;
        }
        apply(&tx)?;
        tx.execute(
            "INSERT INTO effect_sql_migrations (migration_id, name) VALUES (?1, ?2)",
            params![id, name],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn m001_orchestration_events(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS orchestration_events (
          sequence INTEGER PRIMARY KEY AUTOINCREMENT,
          event_id TEXT NOT NULL UNIQUE,
          aggregate_kind TEXT NOT NULL,
          stream_id TEXT NOT NULL,
          stream_version INTEGER NOT NULL,
          event_type TEXT NOT NULL,
          occurred_at TEXT NOT NULL,
          command_id TEXT,
          causation_event_id TEXT,
          correlation_id TEXT,
          actor_kind TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          metadata_json TEXT NOT NULL
        );

        CREATE UNIQUE INDEX IF NOT EXISTS idx_orch_events_stream_version
        ON orchestration_events(aggregate_kind, stream_id, stream_version);

        CREATE INDEX IF NOT EXISTS idx_orch_events_stream_sequence
        ON orchestration_events(aggregate_kind, stream_id, sequence);

        CREATE INDEX IF NOT EXISTS idx_orch_events_command_id
        ON orchestration_events(command_id);

        CREATE INDEX IF NOT EXISTS idx_orch_events_correlation_id
        ON orchestration_events(correlation_id);
        ",
    )?;
    Ok(())
}

fn m002_orchestration_command_receipts(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS orchestration_command_receipts (
          command_id TEXT PRIMARY KEY,
          aggregate_kind TEXT NOT NULL,
          aggregate_id TEXT NOT NULL,
          accepted_at TEXT NOT NULL,
          result_sequence INTEGER NOT NULL,
          status TEXT NOT NULL,
          error TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_orch_command_receipts_aggregate
        ON orchestration_command_receipts(aggregate_kind, aggregate_id);

        CREATE INDEX IF NOT EXISTS idx_orch_command_receipts_sequence
        ON orchestration_command_receipts(result_sequence);
        ",
    )?;
    Ok(())
}

fn m005_projections(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS projection_projects (
          project_id TEXT PRIMARY KEY,
          title TEXT NOT NULL,
          workspace_root TEXT NOT NULL,
          default_model TEXT,
          scripts_json TEXT NOT NULL,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          deleted_at TEXT
        );

        CREATE TABLE IF NOT EXISTS projection_threads (
          thread_id TEXT PRIMARY KEY,
          project_id TEXT NOT NULL,
          title TEXT NOT NULL,
          model TEXT NOT NULL,
          branch TEXT,
          worktree_path TEXT,
          latest_turn_id TEXT,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          deleted_at TEXT
        );

        CREATE TABLE IF NOT EXISTS projection_thread_messages (
          message_id TEXT PRIMARY KEY,
          thread_id TEXT NOT NULL,
          turn_id TEXT,
          role TEXT NOT NULL,
          text TEXT NOT NULL,
          is_streaming INTEGER NOT NULL,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS projection_thread_activities (
          activity_id TEXT PRIMARY KEY,
          thread_id TEXT NOT NULL,
          turn_id TEXT,
          tone TEXT NOT NULL,
          kind TEXT NOT NULL,
          summary TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS projection_thread_sessions (
          thread_id TEXT PRIMARY KEY,
          status TEXT NOT NULL,
          provider_name TEXT,
          provider_session_id TEXT,
          provider_thread_id TEXT,
          active_turn_id TEXT,
          last_error TEXT,
          updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS projection_turns (
          row_id INTEGER PRIMARY KEY AUTOINCREMENT,
          thread_id TEXT NOT NULL,
          turn_id TEXT,
          pending_message_id TEXT,
          assistant_message_id TEXT,
          state TEXT NOT NULL,
          requested_at TEXT NOT NULL,
          started_at TEXT,
          completed_at TEXT,
          checkpoint_turn_count INTEGER,
          checkpoint_ref TEXT,
          checkpoint_status TEXT,
          checkpoint_files_json TEXT NOT NULL,
          UNIQUE (thread_id, turn_id),
          UNIQUE (thread_id, checkpoint_turn_count)
        );

        CREATE TABLE IF NOT EXISTS projection_pending_approvals (
          request_id TEXT PRIMARY KEY,
          thread_id TEXT NOT NULL,
          turn_id TEXT,
          status TEXT NOT NULL,
          decision TEXT,
          created_at TEXT NOT NULL,
          resolved_at TEXT
        );

        CREATE TABLE IF NOT EXISTS projection_state (
          projector TEXT PRIMARY KEY,
          last_applied_sequence INTEGER NOT NULL,
          updated_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_projection_projects_updated_at
        ON projection_projects(updated_at);

        CREATE INDEX IF NOT EXISTS idx_projection_threads_project_id
        ON projection_threads(project_id);

        CREATE INDEX IF NOT EXISTS idx_projection_thread_messages_thread_created
        ON projection_thread_messages(thread_id, created_at);

        CREATE INDEX IF NOT EXISTS idx_projection_thread_activities_thread_created
        ON projection_thread_activities(thread_id, created_at);

        CREATE INDEX IF NOT EXISTS idx_projection_thread_sessions_provider_session
        ON projection_thread_sessions(provider_session_id);

        CREATE INDEX IF NOT EXISTS idx_projection_turns_thread_requested
        ON projection_turns(thread_id, requested_at);

        CREATE INDEX IF NOT EXISTS idx_projection_turns_thread_checkpoint_completed
        ON projection_turns(thread_id, checkpoint_turn_count, completed_at);

        CREATE INDEX IF NOT EXISTS idx_projection_pending_approvals_thread_status
        ON projection_pending_approvals(thread_id, status);
        ",
    )?;
    Ok(())
}

fn m055_orchestration_v2(conn: &Connection) -> Result<()> {
    m055_base(conn)?;
    m055_subagents(conn)?;
    m055_foundation(conn)?;
    m055_provider_session_bindings(conn)?;
    m055_thread_launch_workflows(conn)?;
    m055_application_event_source(conn)?;
    m055_effect_cancellation(conn)?;
    m055_scheduled_tasks(conn)?;
    m055_legacy_v1_import_state(conn)?;
    m055_application_event_sequence_indexes(conn)?;
    m055_recovery_indexes(conn)?;
    m055_shell_indexes(conn)
}

fn m055_base(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE orchestration_v2_events (
          sequence INTEGER PRIMARY KEY AUTOINCREMENT,
          event_id TEXT NOT NULL UNIQUE,
          command_id TEXT,
          thread_id TEXT NOT NULL,
          run_id TEXT,
          node_id TEXT,
          provider TEXT,
          raw_event_id TEXT,
          event_type TEXT NOT NULL,
          occurred_at TEXT NOT NULL,
          payload_json TEXT NOT NULL
        );
        CREATE INDEX orchestration_v2_events_command_idx ON orchestration_v2_events(command_id, sequence);
        CREATE INDEX orchestration_v2_events_thread_sequence_idx ON orchestration_v2_events(thread_id, sequence);
        CREATE INDEX orchestration_v2_events_thread_type_sequence_idx ON orchestration_v2_events(thread_id, event_type, sequence);
        CREATE INDEX orchestration_v2_events_run_sequence_idx ON orchestration_v2_events(run_id, sequence);
        CREATE INDEX orchestration_v2_events_node_sequence_idx ON orchestration_v2_events(node_id, sequence);
        CREATE INDEX orchestration_v2_events_raw_event_idx ON orchestration_v2_events(raw_event_id);

        CREATE TABLE orchestration_v2_command_receipts (
          command_id TEXT PRIMARY KEY,
          thread_id TEXT NOT NULL,
          command_type TEXT NOT NULL,
          accepted_at TEXT NOT NULL,
          result_sequence INTEGER NOT NULL,
          status TEXT NOT NULL,
          error TEXT
        );
        CREATE INDEX orchestration_v2_command_receipts_thread_sequence_idx
        ON orchestration_v2_command_receipts(thread_id, result_sequence);

        CREATE TABLE orchestration_v2_projection_threads (
          thread_id TEXT PRIMARY KEY,
          project_id TEXT NOT NULL,
          title TEXT NOT NULL,
          default_provider TEXT NOT NULL,
          runtime_mode TEXT NOT NULL,
          interaction_mode TEXT NOT NULL,
          active_provider_thread_id TEXT,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          archived_at TEXT,
          deleted_at TEXT,
          payload_json TEXT NOT NULL
        );
        CREATE INDEX orchestration_v2_projection_threads_project_updated_idx
        ON orchestration_v2_projection_threads(project_id, updated_at);

        CREATE TABLE orchestration_v2_projection_runs (
          run_id TEXT PRIMARY KEY,
          thread_id TEXT NOT NULL,
          ordinal INTEGER NOT NULL,
          provider TEXT NOT NULL,
          provider_thread_id TEXT,
          status TEXT NOT NULL,
          requested_at TEXT NOT NULL,
          completed_at TEXT,
          payload_json TEXT NOT NULL
        );
        CREATE UNIQUE INDEX orchestration_v2_projection_runs_thread_ordinal_idx
        ON orchestration_v2_projection_runs(thread_id, ordinal);
        CREATE INDEX orchestration_v2_projection_runs_provider_thread_idx
        ON orchestration_v2_projection_runs(provider_thread_id);
        CREATE INDEX orchestration_v2_projection_runs_thread_status_idx
        ON orchestration_v2_projection_runs(thread_id, status);

        CREATE TABLE orchestration_v2_projection_run_attempts (
          attempt_id TEXT PRIMARY KEY,
          thread_id TEXT NOT NULL,
          run_id TEXT NOT NULL,
          attempt_ordinal INTEGER NOT NULL,
          root_node_id TEXT NOT NULL,
          provider TEXT NOT NULL,
          provider_thread_id TEXT NOT NULL,
          provider_turn_id TEXT,
          status TEXT NOT NULL,
          payload_json TEXT NOT NULL
        );
        CREATE UNIQUE INDEX orchestration_v2_projection_run_attempts_run_ordinal_idx
        ON orchestration_v2_projection_run_attempts(run_id, attempt_ordinal);
        CREATE INDEX orchestration_v2_projection_run_attempts_thread_idx
        ON orchestration_v2_projection_run_attempts(thread_id, run_id);

        CREATE TABLE orchestration_v2_projection_nodes (
          node_id TEXT PRIMARY KEY,
          thread_id TEXT NOT NULL,
          run_id TEXT,
          parent_node_id TEXT,
          root_node_id TEXT NOT NULL,
          kind TEXT NOT NULL,
          status TEXT NOT NULL,
          provider_thread_id TEXT,
          provider_turn_id TEXT,
          runtime_request_id TEXT,
          checkpoint_scope_id TEXT,
          started_at TEXT,
          completed_at TEXT,
          payload_json TEXT NOT NULL
        );
        CREATE INDEX orchestration_v2_projection_nodes_thread_run_idx
        ON orchestration_v2_projection_nodes(thread_id, run_id);
        CREATE INDEX orchestration_v2_projection_nodes_parent_idx
        ON orchestration_v2_projection_nodes(parent_node_id);
        CREATE INDEX orchestration_v2_projection_nodes_provider_turn_idx
        ON orchestration_v2_projection_nodes(provider_turn_id);

        CREATE TABLE orchestration_v2_projection_provider_sessions (
          provider_session_id TEXT PRIMARY KEY,
          thread_id TEXT,
          provider TEXT NOT NULL,
          status TEXT NOT NULL,
          model TEXT,
          updated_at TEXT NOT NULL,
          payload_json TEXT NOT NULL
        );
        CREATE INDEX orchestration_v2_projection_provider_sessions_thread_idx
        ON orchestration_v2_projection_provider_sessions(thread_id);
        CREATE INDEX orchestration_v2_projection_provider_sessions_provider_status_idx
        ON orchestration_v2_projection_provider_sessions(provider, status);

        CREATE TABLE orchestration_v2_projection_provider_threads (
          provider_thread_id TEXT PRIMARY KEY,
          thread_id TEXT,
          owner_node_id TEXT,
          provider TEXT NOT NULL,
          provider_session_id TEXT,
          status TEXT NOT NULL,
          first_run_ordinal INTEGER,
          last_run_ordinal INTEGER,
          updated_at TEXT NOT NULL,
          payload_json TEXT NOT NULL
        );
        CREATE INDEX orchestration_v2_projection_provider_threads_thread_idx
        ON orchestration_v2_projection_provider_threads(thread_id);
        CREATE INDEX orchestration_v2_projection_provider_threads_session_idx
        ON orchestration_v2_projection_provider_threads(provider_session_id);
        CREATE INDEX orchestration_v2_projection_provider_threads_owner_idx
        ON orchestration_v2_projection_provider_threads(owner_node_id);

        CREATE TABLE orchestration_v2_projection_provider_turns (
          provider_turn_id TEXT PRIMARY KEY,
          thread_id TEXT NOT NULL,
          provider_thread_id TEXT NOT NULL,
          node_id TEXT NOT NULL,
          run_attempt_id TEXT,
          ordinal INTEGER NOT NULL,
          status TEXT NOT NULL,
          started_at TEXT,
          completed_at TEXT,
          payload_json TEXT NOT NULL
        );
        CREATE INDEX orchestration_v2_projection_provider_turns_thread_idx
        ON orchestration_v2_projection_provider_turns(thread_id);
        CREATE UNIQUE INDEX orchestration_v2_projection_provider_turns_thread_ordinal_idx
        ON orchestration_v2_projection_provider_turns(provider_thread_id, ordinal);

        CREATE TABLE orchestration_v2_projection_runtime_requests (
          runtime_request_id TEXT PRIMARY KEY,
          thread_id TEXT NOT NULL,
          node_id TEXT NOT NULL,
          provider_turn_id TEXT,
          kind TEXT NOT NULL,
          status TEXT NOT NULL,
          created_at TEXT NOT NULL,
          resolved_at TEXT,
          payload_json TEXT NOT NULL
        );
        CREATE INDEX orchestration_v2_projection_runtime_requests_thread_status_idx
        ON orchestration_v2_projection_runtime_requests(thread_id, status);
        CREATE INDEX orchestration_v2_projection_runtime_requests_provider_turn_idx
        ON orchestration_v2_projection_runtime_requests(provider_turn_id);

        CREATE TABLE orchestration_v2_projection_messages (
          message_id TEXT PRIMARY KEY,
          thread_id TEXT NOT NULL,
          run_id TEXT,
          node_id TEXT,
          role TEXT NOT NULL,
          streaming INTEGER NOT NULL,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          payload_json TEXT NOT NULL
        );
        CREATE INDEX orchestration_v2_projection_messages_thread_created_idx
        ON orchestration_v2_projection_messages(thread_id, created_at, message_id);
        CREATE INDEX orchestration_v2_projection_messages_run_idx
        ON orchestration_v2_projection_messages(run_id);
        CREATE INDEX orchestration_v2_projection_messages_node_idx
        ON orchestration_v2_projection_messages(node_id);

        CREATE TABLE orchestration_v2_projection_plans (
          plan_id TEXT PRIMARY KEY,
          thread_id TEXT NOT NULL,
          run_id TEXT,
          node_id TEXT NOT NULL,
          kind TEXT NOT NULL,
          status TEXT NOT NULL,
          payload_json TEXT NOT NULL
        );
        CREATE INDEX orchestration_v2_projection_plans_thread_idx
        ON orchestration_v2_projection_plans(thread_id);
        CREATE INDEX orchestration_v2_projection_plans_run_idx
        ON orchestration_v2_projection_plans(run_id);

        CREATE TABLE orchestration_v2_projection_turn_items (
          turn_item_id TEXT PRIMARY KEY,
          thread_id TEXT NOT NULL,
          run_id TEXT,
          node_id TEXT,
          provider_thread_id TEXT,
          provider_turn_id TEXT,
          parent_item_id TEXT,
          ordinal INTEGER NOT NULL,
          type TEXT NOT NULL,
          status TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          payload_json TEXT NOT NULL
        );
        CREATE INDEX orchestration_v2_projection_turn_items_thread_ordinal_idx
        ON orchestration_v2_projection_turn_items(thread_id, ordinal, turn_item_id);
        CREATE INDEX orchestration_v2_projection_turn_items_run_ordinal_idx
        ON orchestration_v2_projection_turn_items(run_id, ordinal);
        CREATE INDEX orchestration_v2_projection_turn_items_node_ordinal_idx
        ON orchestration_v2_projection_turn_items(node_id, ordinal);
        CREATE INDEX orchestration_v2_projection_turn_items_provider_turn_idx
        ON orchestration_v2_projection_turn_items(provider_turn_id);

        CREATE TABLE orchestration_v2_projection_checkpoint_scopes (
          scope_id TEXT PRIMARY KEY,
          thread_id TEXT NOT NULL,
          run_id TEXT,
          node_id TEXT NOT NULL,
          parent_scope_id TEXT,
          provider_thread_id TEXT,
          kind TEXT NOT NULL,
          ordinal_within_parent INTEGER NOT NULL,
          advances_app_run_count INTEGER NOT NULL,
          created_at TEXT NOT NULL,
          payload_json TEXT NOT NULL
        );
        CREATE INDEX orchestration_v2_projection_checkpoint_scopes_thread_idx
        ON orchestration_v2_projection_checkpoint_scopes(thread_id);
        CREATE INDEX orchestration_v2_projection_checkpoint_scopes_parent_idx
        ON orchestration_v2_projection_checkpoint_scopes(parent_scope_id);

        CREATE TABLE orchestration_v2_projection_checkpoints (
          checkpoint_id TEXT PRIMARY KEY,
          thread_id TEXT NOT NULL,
          scope_id TEXT NOT NULL,
          run_id TEXT,
          node_id TEXT NOT NULL,
          parent_checkpoint_id TEXT,
          ordinal_within_scope INTEGER NOT NULL,
          app_run_ordinal INTEGER,
          status TEXT NOT NULL,
          captured_at TEXT NOT NULL,
          payload_json TEXT NOT NULL
        );
        CREATE UNIQUE INDEX orchestration_v2_projection_checkpoints_scope_ordinal_idx
        ON orchestration_v2_projection_checkpoints(scope_id, ordinal_within_scope);
        CREATE INDEX orchestration_v2_projection_checkpoints_thread_idx
        ON orchestration_v2_projection_checkpoints(thread_id);
        CREATE INDEX orchestration_v2_projection_checkpoints_parent_idx
        ON orchestration_v2_projection_checkpoints(parent_checkpoint_id);

        CREATE TABLE orchestration_v2_projection_context_handoffs (
          context_handoff_id TEXT PRIMARY KEY,
          thread_id TEXT NOT NULL,
          target_run_id TEXT NOT NULL,
          to_provider_thread_id TEXT NOT NULL,
          strategy TEXT NOT NULL,
          status TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          payload_json TEXT NOT NULL
        );
        CREATE INDEX orchestration_v2_projection_context_handoffs_thread_idx
        ON orchestration_v2_projection_context_handoffs(thread_id);
        CREATE INDEX orchestration_v2_projection_context_handoffs_target_run_idx
        ON orchestration_v2_projection_context_handoffs(target_run_id);

        CREATE TABLE orchestration_v2_projection_context_transfers (
          context_transfer_id TEXT PRIMARY KEY,
          source_thread_id TEXT NOT NULL,
          target_thread_id TEXT NOT NULL,
          target_run_id TEXT,
          type TEXT NOT NULL,
          status TEXT NOT NULL,
          source_provider TEXT,
          target_provider TEXT,
          updated_at TEXT NOT NULL,
          payload_json TEXT NOT NULL
        );
        CREATE INDEX orchestration_v2_projection_context_transfers_source_thread_idx
        ON orchestration_v2_projection_context_transfers(source_thread_id);
        CREATE INDEX orchestration_v2_projection_context_transfers_target_thread_idx
        ON orchestration_v2_projection_context_transfers(target_thread_id, status);
        CREATE INDEX orchestration_v2_projection_context_transfers_target_run_idx
        ON orchestration_v2_projection_context_transfers(target_run_id);
        ",
    )?;
    Ok(())
}

fn m055_subagents(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE orchestration_v2_projection_subagents (
          subagent_id TEXT PRIMARY KEY,
          thread_id TEXT NOT NULL,
          run_id TEXT,
          parent_node_id TEXT NOT NULL,
          provider TEXT NOT NULL,
          provider_thread_id TEXT,
          child_thread_id TEXT,
          origin TEXT NOT NULL,
          status TEXT NOT NULL,
          started_at TEXT,
          completed_at TEXT,
          updated_at TEXT NOT NULL,
          payload_json TEXT NOT NULL
        );
        CREATE INDEX orchestration_v2_projection_subagents_thread_idx
        ON orchestration_v2_projection_subagents(thread_id, started_at, subagent_id);
        CREATE INDEX orchestration_v2_projection_subagents_parent_node_idx
        ON orchestration_v2_projection_subagents(parent_node_id);
        CREATE INDEX orchestration_v2_projection_subagents_provider_thread_idx
        ON orchestration_v2_projection_subagents(provider_thread_id);
        CREATE INDEX orchestration_v2_projection_subagents_child_thread_idx
        ON orchestration_v2_projection_subagents(child_thread_id);
        ",
    )?;
    Ok(())
}

/// Provider-instance columns, the effect outbox, item positions and projection
/// metadata. T3 keeps the legacy provider columns as write-through shadows.
fn m055_foundation(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        ALTER TABLE orchestration_v2_events ADD COLUMN driver TEXT;
        ALTER TABLE orchestration_v2_events ADD COLUMN provider_instance_id TEXT;
        UPDATE orchestration_v2_events
        SET
          driver = COALESCE(json_extract(payload_json, '$.driver'), provider),
          provider_instance_id = COALESCE(
            json_extract(payload_json, '$.providerInstanceId'),
            provider
          );
        CREATE INDEX orchestration_v2_events_instance_sequence_idx
        ON orchestration_v2_events(provider_instance_id, sequence);

        ALTER TABLE orchestration_v2_projection_threads ADD COLUMN provider_instance_id TEXT;
        UPDATE orchestration_v2_projection_threads
        SET provider_instance_id = COALESCE(
          json_extract(payload_json, '$.providerInstanceId'),
          default_provider
        );

        ALTER TABLE orchestration_v2_projection_runs ADD COLUMN provider_instance_id TEXT;
        UPDATE orchestration_v2_projection_runs
        SET provider_instance_id = COALESCE(
          json_extract(payload_json, '$.providerInstanceId'),
          provider
        );

        ALTER TABLE orchestration_v2_projection_run_attempts ADD COLUMN provider_instance_id TEXT;
        UPDATE orchestration_v2_projection_run_attempts
        SET provider_instance_id = COALESCE(
          json_extract(payload_json, '$.providerInstanceId'),
          provider
        );

        ALTER TABLE orchestration_v2_projection_provider_sessions ADD COLUMN driver TEXT;
        ALTER TABLE orchestration_v2_projection_provider_sessions ADD COLUMN provider_instance_id TEXT;
        UPDATE orchestration_v2_projection_provider_sessions
        SET
          driver = COALESCE(json_extract(payload_json, '$.driver'), provider),
          provider_instance_id = COALESCE(
            json_extract(payload_json, '$.providerInstanceId'),
            provider
          );
        CREATE INDEX orchestration_v2_projection_provider_sessions_instance_status_idx
        ON orchestration_v2_projection_provider_sessions(provider_instance_id, status);

        ALTER TABLE orchestration_v2_projection_provider_threads ADD COLUMN driver TEXT;
        ALTER TABLE orchestration_v2_projection_provider_threads ADD COLUMN provider_instance_id TEXT;
        UPDATE orchestration_v2_projection_provider_threads
        SET
          driver = COALESCE(json_extract(payload_json, '$.driver'), provider),
          provider_instance_id = COALESCE(
            json_extract(payload_json, '$.providerInstanceId'),
            provider
          );
        CREATE INDEX orchestration_v2_projection_provider_threads_instance_status_idx
        ON orchestration_v2_projection_provider_threads(provider_instance_id, status);

        ALTER TABLE orchestration_v2_projection_subagents ADD COLUMN driver TEXT;
        ALTER TABLE orchestration_v2_projection_subagents ADD COLUMN provider_instance_id TEXT;
        UPDATE orchestration_v2_projection_subagents
        SET
          driver = json_extract(payload_json, '$.driver'),
          provider_instance_id = COALESCE(
            json_extract(payload_json, '$.providerInstanceId'),
            provider
          );

        ALTER TABLE orchestration_v2_projection_context_transfers ADD COLUMN source_provider_instance_id TEXT;
        ALTER TABLE orchestration_v2_projection_context_transfers ADD COLUMN target_provider_instance_id TEXT;
        UPDATE orchestration_v2_projection_context_transfers
        SET
          source_provider_instance_id = COALESCE(
            json_extract(payload_json, '$.sourceProviderInstanceId'),
            source_provider
          ),
          target_provider_instance_id = COALESCE(
            json_extract(payload_json, '$.targetProviderInstanceId'),
            target_provider
          );

        CREATE TABLE orchestration_v2_effect_outbox (
          effect_id TEXT PRIMARY KEY,
          command_id TEXT NOT NULL,
          thread_id TEXT NOT NULL,
          effect_type TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'succeeded', 'failed')),
          attempt_count INTEGER NOT NULL DEFAULT 0,
          available_at TEXT NOT NULL,
          lease_owner TEXT,
          lease_expires_at TEXT,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          completed_at TEXT,
          last_error TEXT
        );
        CREATE INDEX orchestration_v2_effect_outbox_claim_idx
        ON orchestration_v2_effect_outbox(status, available_at, lease_expires_at, created_at);
        CREATE INDEX orchestration_v2_effect_outbox_command_idx
        ON orchestration_v2_effect_outbox(command_id, effect_id);

        CREATE TABLE orchestration_v2_turn_item_positions (
          thread_id TEXT NOT NULL,
          turn_item_id TEXT NOT NULL,
          ordinal INTEGER NOT NULL,
          PRIMARY KEY (thread_id, turn_item_id),
          UNIQUE (thread_id, ordinal)
        );
        INSERT INTO orchestration_v2_turn_item_positions (
          thread_id,
          turn_item_id,
          ordinal
        )
        SELECT
          item.thread_id,
          item.turn_item_id,
          COALESCE(run.ordinal, 0) * 1000000 + ROW_NUMBER() OVER (
            PARTITION BY item.thread_id, COALESCE(run.ordinal, 0)
            ORDER BY item.ordinal, item.turn_item_id
          )
        FROM orchestration_v2_projection_turn_items AS item
        LEFT JOIN orchestration_v2_projection_runs AS run
          ON run.thread_id = item.thread_id
          AND run.run_id = item.run_id;

        CREATE TABLE orchestration_v2_projection_metadata (
          projection_name TEXT PRIMARY KEY,
          schema_version INTEGER NOT NULL,
          last_sequence INTEGER NOT NULL,
          updated_at TEXT NOT NULL
        );
        INSERT INTO orchestration_v2_projection_metadata (
          projection_name,
          schema_version,
          last_sequence,
          updated_at
        )
        VALUES (
          'thread-projections',
          1,
          COALESCE((SELECT MAX(sequence) FROM orchestration_v2_events), 0),
          strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
        );
        ",
    )?;
    Ok(())
}

fn m055_provider_session_bindings(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE orchestration_v2_projection_provider_session_bindings (
          provider_session_id TEXT NOT NULL,
          thread_id TEXT NOT NULL,
          PRIMARY KEY (provider_session_id, thread_id)
        );
        INSERT OR IGNORE INTO orchestration_v2_projection_provider_session_bindings (
          provider_session_id,
          thread_id
        )
        SELECT provider_session_id, thread_id
        FROM orchestration_v2_projection_provider_sessions
        WHERE thread_id IS NOT NULL;
        CREATE INDEX orchestration_v2_projection_provider_session_bindings_thread_idx
        ON orchestration_v2_projection_provider_session_bindings(thread_id);
        ",
    )?;
    Ok(())
}

fn m055_thread_launch_workflows(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS orchestration_v2_thread_launch_workflows (
          command_id TEXT PRIMARY KEY,
          thread_id TEXT NOT NULL,
          project_id TEXT NOT NULL,
          status TEXT NOT NULL,
          title TEXT NOT NULL,
          worktree_path TEXT,
          branch TEXT,
          setup_committed INTEGER NOT NULL DEFAULT 0,
          thread_committed INTEGER NOT NULL DEFAULT 0,
          message_committed INTEGER NOT NULL DEFAULT 0,
          last_error TEXT,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL
        );
        ",
    )?;
    Ok(())
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn m055_application_event_source(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        ALTER TABLE orchestration_events
        ADD COLUMN application_event_version INTEGER NOT NULL DEFAULT 1;
        CREATE INDEX idx_orchestration_events_application_sequence
        ON orchestration_events(application_event_version, sequence);
        ALTER TABLE orchestration_command_receipts
        ADD COLUMN command_type TEXT NOT NULL DEFAULT 'legacy';
        ",
    )?;

    // 055 runs after 016 in T3. This chain starts at 005, so add the one column
    // the project backfill below reads, filling it the way 016 does.
    if !table_has_column(conn, "projection_projects", "default_model_selection_json")? {
        conn.execute_batch(
            "
            ALTER TABLE projection_projects
            ADD COLUMN default_model_selection_json TEXT;
            UPDATE projection_projects
            SET default_model_selection_json = CASE
              WHEN default_model IS NULL THEN NULL
              ELSE json_object('model', default_model)
            END
            WHERE default_model_selection_json IS NULL;
            ",
        )?;
    }

    // Migrate the legacy V2 log into the shared log.
    let legacy_events = {
        let mut statement = conn.prepare(
            "SELECT
                event_id,
                command_id,
                thread_id,
                run_id,
                node_id,
                driver,
                provider_instance_id,
                raw_event_id,
                event_type,
                occurred_at,
                payload_json
             FROM orchestration_v2_events
             ORDER BY sequence ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
            ))
        })?;
        let mut rows_out = Vec::new();
        for row in rows {
            rows_out.push(row?);
        }
        rows_out
    };

    for (
        event_id,
        command_id,
        thread_id,
        run_id,
        node_id,
        driver,
        provider_instance_id,
        raw_event_id,
        event_type,
        occurred_at,
        payload_json,
    ) in legacy_events
    {
        let actor_kind = if raw_event_id.is_none() {
            "server"
        } else {
            "provider"
        };
        let mut metadata = Map::new();
        metadata.insert(
            "applicationEventVersion".to_string(),
            JsonValue::from(2_i64),
        );
        for (key, value) in [
            ("runId", run_id),
            ("nodeId", node_id),
            ("driver", driver),
            ("providerInstanceId", provider_instance_id),
            ("rawEventId", raw_event_id),
        ] {
            if let Some(value) = value {
                metadata.insert(key.to_string(), JsonValue::String(value));
            }
        }
        let metadata_json = serde_json::to_string(&JsonValue::Object(metadata))?;
        conn.execute(
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
             SELECT
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
             WHERE NOT EXISTS (
                SELECT 1 FROM orchestration_events WHERE event_id = ?1
             )",
            params![
                event_id,
                thread_id,
                event_type,
                occurred_at,
                command_id,
                actor_kind,
                payload_json,
                metadata_json,
            ],
        )?;
    }

    // Re-baseline project projections so shared-log rebuilds keep pre-migration
    // project state.
    let projects = {
        let mut statement = conn.prepare(
            "SELECT
                project_id,
                title,
                workspace_root,
                default_model_selection_json,
                scripts_json,
                created_at,
                updated_at,
                deleted_at
             FROM projection_projects
             ORDER BY created_at ASC, project_id ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })?;
        let mut rows_out = Vec::new();
        for row in rows {
            rows_out.push(row?);
        }
        rows_out
    };

    let migration_metadata = serde_json::to_string(&serde_json::json!({
        "applicationEventVersion": 2,
        "migration": 39,
    }))?;

    for (
        project_id,
        title,
        workspace_root,
        default_model_selection,
        scripts_json,
        created_at,
        updated_at,
        deleted_at,
    ) in projects
    {
        let created_event_id = format!("migration:39:project:{project_id}:baseline");
        let created_payload = serde_json::json!({
            "projectId": project_id,
            "title": title,
            "workspaceRoot": workspace_root,
            "defaultModelSelection": parse_or_null(default_model_selection.as_deref()),
            "scripts": parse_or_null(Some(&scripts_json)),
            "createdAt": created_at,
            "updatedAt": updated_at,
        });
        let created_payload_json = serde_json::to_string(&created_payload)?;
        conn.execute(
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
                'project.created',
                ?3,
                NULL,
                NULL,
                NULL,
                'server',
                ?4,
                ?5,
                2
             )",
            params![
                created_event_id,
                project_id,
                updated_at,
                created_payload_json,
                migration_metadata,
            ],
        )?;

        if let Some(deleted_at) = deleted_at {
            let deleted_payload = serde_json::json!({
                "projectId": project_id,
                "deletedAt": deleted_at,
            });
            conn.execute(
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
                    (
                      SELECT MAX(stream_version) + 1
                      FROM orchestration_events
                      WHERE aggregate_kind = 'project' AND stream_id = ?2
                    ),
                    'project.deleted',
                    ?3,
                    NULL,
                    ?4,
                    NULL,
                    'server',
                    ?5,
                    ?6,
                    2
                 )",
                params![
                    format!("migration:39:project:{project_id}:deleted"),
                    project_id,
                    deleted_at,
                    created_event_id,
                    serde_json::to_string(&deleted_payload)?,
                    migration_metadata,
                ],
            )?;
        }
    }

    conn.execute_batch(
        "
        INSERT INTO orchestration_command_receipts (
          command_id,
          aggregate_kind,
          aggregate_id,
          accepted_at,
          result_sequence,
          status,
          error,
          command_type
        )
        SELECT
          command_id,
          'thread',
          thread_id,
          accepted_at,
          COALESCE(
            (
              SELECT MAX(events.sequence)
              FROM orchestration_events events
              WHERE events.application_event_version = 2
                AND events.command_id = receipts.command_id
            ),
            0
          ),
          status,
          error,
          command_type
        FROM orchestration_v2_command_receipts receipts
        WHERE TRUE
        ON CONFLICT(command_id) DO NOTHING;
        ",
    )?;
    Ok(())
}

fn parse_or_null(value: Option<&str>) -> JsonValue {
    match value {
        Some(value) => serde_json::from_str(value).unwrap_or(JsonValue::Null),
        None => JsonValue::Null,
    }
}

fn m055_effect_cancellation(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE orchestration_v2_effect_outbox_next (
          effect_id TEXT PRIMARY KEY,
          command_id TEXT NOT NULL,
          thread_id TEXT NOT NULL,
          effect_type TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          status TEXT NOT NULL CHECK (
            status IN ('pending', 'running', 'succeeded', 'failed', 'cancelled')
          ),
          attempt_count INTEGER NOT NULL DEFAULT 0,
          available_at TEXT NOT NULL,
          lease_owner TEXT,
          lease_expires_at TEXT,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          completed_at TEXT,
          last_error TEXT
        );
        INSERT INTO orchestration_v2_effect_outbox_next (
          effect_id,
          command_id,
          thread_id,
          effect_type,
          payload_json,
          status,
          attempt_count,
          available_at,
          lease_owner,
          lease_expires_at,
          created_at,
          updated_at,
          completed_at,
          last_error
        )
        SELECT
          effect_id,
          command_id,
          thread_id,
          effect_type,
          payload_json,
          status,
          attempt_count,
          available_at,
          lease_owner,
          lease_expires_at,
          created_at,
          updated_at,
          completed_at,
          last_error
        FROM orchestration_v2_effect_outbox;
        DROP TABLE orchestration_v2_effect_outbox;
        ALTER TABLE orchestration_v2_effect_outbox_next
        RENAME TO orchestration_v2_effect_outbox;
        CREATE INDEX orchestration_v2_effect_outbox_claim_idx
        ON orchestration_v2_effect_outbox(status, available_at, lease_expires_at, created_at);
        CREATE INDEX orchestration_v2_effect_outbox_command_idx
        ON orchestration_v2_effect_outbox(command_id, effect_id);
        CREATE INDEX orchestration_v2_effect_outbox_thread_status_idx
        ON orchestration_v2_effect_outbox(thread_id, status, effect_type);
        ",
    )?;
    Ok(())
}

fn m055_scheduled_tasks(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS scheduled_tasks (
          task_id TEXT PRIMARY KEY,
          title TEXT NOT NULL,
          prompt TEXT NOT NULL,
          enabled INTEGER NOT NULL,
          schedule_json TEXT NOT NULL,
          project_id TEXT NOT NULL,
          thread_id TEXT,
          workspace_strategy_json TEXT NOT NULL,
          model_selection_json TEXT NOT NULL,
          runtime_mode TEXT NOT NULL,
          interaction_mode TEXT NOT NULL,
          created_by TEXT NOT NULL,
          creation_source TEXT NOT NULL,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          next_run_at TEXT,
          last_run_at TEXT,
          last_run_status TEXT NOT NULL,
          last_run_error TEXT,
          run_count INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_scheduled_tasks_due
        ON scheduled_tasks(enabled, next_run_at)
        WHERE enabled = 1 AND next_run_at IS NOT NULL;
        CREATE INDEX IF NOT EXISTS idx_scheduled_tasks_project
        ON scheduled_tasks(project_id, updated_at);
        ",
    )?;
    Ok(())
}

fn m055_legacy_v1_import_state(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE orchestration_v2_legacy_imports (
          thread_id TEXT PRIMARY KEY,
          source_updated_at TEXT NOT NULL,
          shell_imported_at TEXT NOT NULL,
          transcript_imported_at TEXT,
          imported_message_count INTEGER NOT NULL DEFAULT 0,
          last_error TEXT
        );
        CREATE INDEX orchestration_v2_legacy_imports_pending_transcript_idx
        ON orchestration_v2_legacy_imports(transcript_imported_at, shell_imported_at, thread_id);
        ",
    )?;
    Ok(())
}

fn m055_application_event_sequence_indexes(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE INDEX idx_orchestration_events_application_high_water
        ON orchestration_events(sequence)
        WHERE aggregate_kind = 'project'
          OR (application_event_version = 2 AND aggregate_kind = 'thread');
        CREATE INDEX idx_orchestration_events_agent_stream_sequence
        ON orchestration_events(stream_id, sequence)
        WHERE application_event_version = 2 AND aggregate_kind = 'thread';
        ",
    )?;
    Ok(())
}

fn m055_recovery_indexes(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE INDEX orchestration_events_v2_created_threads_idx
        ON orchestration_events(stream_id)
        WHERE application_event_version = 2
          AND aggregate_kind = 'thread'
          AND event_type = 'thread.created';
        CREATE INDEX orchestration_v2_projection_runs_recovery_idx
        ON orchestration_v2_projection_runs(status, thread_id)
        WHERE status IN ('queued', 'preparing', 'starting', 'running', 'waiting');
        CREATE INDEX orchestration_v2_projection_requests_recovery_idx
        ON orchestration_v2_projection_runtime_requests(thread_id)
        WHERE status = 'pending';
        CREATE INDEX orchestration_v2_projection_turn_items_recovery_idx
        ON orchestration_v2_projection_turn_items(thread_id)
        WHERE type IN ('command_execution', 'dynamic_tool', 'subagent')
          AND status IN ('pending', 'running', 'waiting');
        ",
    )?;
    Ok(())
}

fn m055_shell_indexes(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE INDEX orchestration_v2_projection_turn_items_thread_run_idx
        ON orchestration_v2_projection_turn_items(thread_id, run_id);
        CREATE INDEX orchestration_v2_projection_turn_items_shell_pending_idx
        ON orchestration_v2_projection_turn_items(thread_id, run_id)
        WHERE type IN ('command_execution', 'dynamic_tool', 'subagent')
          AND status NOT IN ('completed', 'interrupted', 'failed', 'cancelled');
        CREATE INDEX orchestration_v2_projection_messages_latest_user_idx
        ON orchestration_v2_projection_messages(thread_id, updated_at DESC, message_id DESC)
        WHERE role = 'user';
        ",
    )?;
    Ok(())
}
