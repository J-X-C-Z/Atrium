// Ported from apps/server/src/orchestration-v2/ProjectionStore.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/persistence/Migrations/005_Projections.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/persistence/Migrations/055_OrchestrationV2.ts (T3 Code v0.1.11, MIT).
//!
//! Projection rows the orchestration layer reads back.
//!
//! The 055 entities (threads, runs, run attempts, nodes, provider sessions)
//! live in `orchestration_v2_projection_*`; messages, activities, turns,
//! pending approvals and projector state come from 005's `projection_*`
//! tables. Every upsert keeps T3's key and indexed columns and stores the
//! payload JSON verbatim in `payload_json`.
//!
//! These are the store primitives, not T3's full read model: the shell,
//! timeline and recovery queries are ported with the orchestration layer.

use rusqlite::{Connection, OptionalExtension, params};

use crate::store::events::Result;

/// `orchestration_v2_projection_threads`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadProjection {
    pub thread_id: String,
    pub project_id: String,
    pub title: String,
    pub default_provider: String,
    pub provider_instance_id: String,
    pub runtime_mode: String,
    pub interaction_mode: String,
    pub active_provider_thread_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
    pub deleted_at: Option<String>,
    pub payload_json: String,
}

/// `orchestration_v2_projection_runs`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunProjection {
    pub run_id: String,
    pub thread_id: String,
    pub ordinal: i64,
    pub provider: String,
    pub provider_instance_id: String,
    pub provider_thread_id: Option<String>,
    pub status: String,
    pub requested_at: String,
    pub completed_at: Option<String>,
    pub payload_json: String,
}

/// `orchestration_v2_projection_run_attempts`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunAttemptProjection {
    pub attempt_id: String,
    pub thread_id: String,
    pub run_id: String,
    pub attempt_ordinal: i64,
    pub root_node_id: String,
    pub provider: String,
    pub provider_instance_id: String,
    pub provider_thread_id: String,
    pub provider_turn_id: Option<String>,
    pub status: String,
    pub payload_json: String,
}

/// `orchestration_v2_projection_nodes`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeProjection {
    pub node_id: String,
    pub thread_id: String,
    pub run_id: Option<String>,
    pub parent_node_id: Option<String>,
    pub root_node_id: String,
    pub kind: String,
    pub status: String,
    pub provider_thread_id: Option<String>,
    pub provider_turn_id: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub payload_json: String,
}

/// `orchestration_v2_projection_provider_sessions`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSessionProjection {
    pub provider_session_id: String,
    pub thread_id: Option<String>,
    pub provider: String,
    pub driver: Option<String>,
    pub provider_instance_id: Option<String>,
    pub status: String,
    pub updated_at: String,
    pub payload_json: String,
}

/// `projection_thread_messages`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageProjection {
    pub message_id: String,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub role: String,
    pub text: String,
    pub is_streaming: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// `projection_thread_activities`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityProjection {
    pub activity_id: String,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub tone: String,
    pub kind: String,
    pub summary: String,
    pub payload_json: String,
    pub created_at: String,
}

/// `projection_turns`. `row_id` is assigned by SQLite.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnProjection {
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub pending_message_id: Option<String>,
    pub assistant_message_id: Option<String>,
    pub state: String,
    pub requested_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub checkpoint_turn_count: Option<i64>,
    pub checkpoint_ref: Option<String>,
    pub checkpoint_status: Option<String>,
    pub checkpoint_files_json: String,
}

/// `projection_pending_approvals`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingApprovalProjection {
    pub request_id: String,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub status: String,
    pub decision: Option<String>,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

/// `projection_state`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectorState {
    pub projector: String,
    pub last_applied_sequence: i64,
    pub updated_at: String,
}

/// One projection write inside [`crate::store::EventStore::apply`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectionWrite {
    Thread(ThreadProjection),
    Run(RunProjection),
    RunAttempt(RunAttemptProjection),
    Node(NodeProjection),
    ProviderSession(ProviderSessionProjection),
    Message(MessageProjection),
    Activity(ActivityProjection),
    Turn(TurnProjection),
    PendingApproval(PendingApprovalProjection),
    State(ProjectorState),
}

impl ProjectionWrite {
    pub fn apply(&self, conn: &Connection) -> Result<()> {
        match self {
            ProjectionWrite::Thread(row) => upsert_thread(conn, row),
            ProjectionWrite::Run(row) => upsert_run(conn, row),
            ProjectionWrite::RunAttempt(row) => upsert_run_attempt(conn, row),
            ProjectionWrite::Node(row) => upsert_node(conn, row),
            ProjectionWrite::ProviderSession(row) => upsert_provider_session(conn, row),
            ProjectionWrite::Message(row) => upsert_message(conn, row),
            ProjectionWrite::Activity(row) => upsert_activity(conn, row),
            ProjectionWrite::Turn(row) => upsert_turn(conn, row),
            ProjectionWrite::PendingApproval(row) => upsert_pending_approval(conn, row),
            ProjectionWrite::State(row) => upsert_projector_state(conn, row),
        }
    }
}

/// The projection store over a borrowed connection. Free functions carry the
/// work so [`crate::store::EventStore::apply`] can reuse them inside its
/// transaction.
#[derive(Clone, Copy, Debug)]
pub struct ProjectionStore<'a> {
    conn: &'a Connection,
}

impl<'a> ProjectionStore<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn upsert_thread(&self, row: &ThreadProjection) -> Result<()> {
        upsert_thread(self.conn, row)
    }

    pub fn get_thread(&self, thread_id: &str) -> Result<Option<ThreadProjection>> {
        get_thread(self.conn, thread_id)
    }

    pub fn list_threads(&self, project_id: &str) -> Result<Vec<ThreadProjection>> {
        list_threads(self.conn, project_id)
    }

    pub fn upsert_run(&self, row: &RunProjection) -> Result<()> {
        upsert_run(self.conn, row)
    }

    pub fn get_run(&self, run_id: &str) -> Result<Option<RunProjection>> {
        get_run(self.conn, run_id)
    }

    pub fn list_runs(&self, thread_id: &str) -> Result<Vec<RunProjection>> {
        list_runs(self.conn, thread_id)
    }

    pub fn upsert_run_attempt(&self, row: &RunAttemptProjection) -> Result<()> {
        upsert_run_attempt(self.conn, row)
    }

    pub fn get_run_attempt(&self, attempt_id: &str) -> Result<Option<RunAttemptProjection>> {
        get_run_attempt(self.conn, attempt_id)
    }

    pub fn list_run_attempts(&self, run_id: &str) -> Result<Vec<RunAttemptProjection>> {
        list_run_attempts(self.conn, run_id)
    }

    pub fn upsert_node(&self, row: &NodeProjection) -> Result<()> {
        upsert_node(self.conn, row)
    }

    pub fn get_node(&self, node_id: &str) -> Result<Option<NodeProjection>> {
        get_node(self.conn, node_id)
    }

    pub fn list_nodes(&self, thread_id: &str) -> Result<Vec<NodeProjection>> {
        list_nodes(self.conn, thread_id)
    }

    pub fn upsert_provider_session(&self, row: &ProviderSessionProjection) -> Result<()> {
        upsert_provider_session(self.conn, row)
    }

    pub fn get_provider_session(
        &self,
        provider_session_id: &str,
    ) -> Result<Option<ProviderSessionProjection>> {
        get_provider_session(self.conn, provider_session_id)
    }

    pub fn list_provider_sessions(
        &self,
        thread_id: &str,
    ) -> Result<Vec<ProviderSessionProjection>> {
        list_provider_sessions(self.conn, thread_id)
    }

    pub fn upsert_message(&self, row: &MessageProjection) -> Result<()> {
        upsert_message(self.conn, row)
    }

    pub fn list_messages(&self, thread_id: &str) -> Result<Vec<MessageProjection>> {
        list_messages(self.conn, thread_id)
    }

    pub fn upsert_activity(&self, row: &ActivityProjection) -> Result<()> {
        upsert_activity(self.conn, row)
    }

    pub fn list_activities(&self, thread_id: &str) -> Result<Vec<ActivityProjection>> {
        list_activities(self.conn, thread_id)
    }

    pub fn upsert_turn(&self, row: &TurnProjection) -> Result<()> {
        upsert_turn(self.conn, row)
    }

    pub fn get_turn(&self, thread_id: &str, turn_id: &str) -> Result<Option<TurnProjection>> {
        get_turn(self.conn, thread_id, turn_id)
    }

    pub fn list_turns(&self, thread_id: &str) -> Result<Vec<TurnProjection>> {
        list_turns(self.conn, thread_id)
    }

    pub fn upsert_pending_approval(&self, row: &PendingApprovalProjection) -> Result<()> {
        upsert_pending_approval(self.conn, row)
    }

    pub fn list_pending_approvals(
        &self,
        thread_id: &str,
        status: &str,
    ) -> Result<Vec<PendingApprovalProjection>> {
        list_pending_approvals(self.conn, thread_id, status)
    }

    pub fn upsert_projector_state(&self, row: &ProjectorState) -> Result<()> {
        upsert_projector_state(self.conn, row)
    }

    pub fn get_projector_state(&self, projector: &str) -> Result<Option<ProjectorState>> {
        get_projector_state(self.conn, projector)
    }
}

pub fn upsert_thread(conn: &Connection, row: &ThreadProjection) -> Result<()> {
    conn.execute(
        "INSERT INTO orchestration_v2_projection_threads (
            thread_id,
            project_id,
            title,
            default_provider,
            provider_instance_id,
            runtime_mode,
            interaction_mode,
            active_provider_thread_id,
            created_at,
            updated_at,
            archived_at,
            deleted_at,
            payload_json
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(thread_id) DO UPDATE SET
            project_id = excluded.project_id,
            title = excluded.title,
            default_provider = excluded.default_provider,
            provider_instance_id = excluded.provider_instance_id,
            runtime_mode = excluded.runtime_mode,
            interaction_mode = excluded.interaction_mode,
            active_provider_thread_id = excluded.active_provider_thread_id,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            archived_at = excluded.archived_at,
            deleted_at = excluded.deleted_at,
            payload_json = excluded.payload_json",
        params![
            row.thread_id,
            row.project_id,
            row.title,
            row.default_provider,
            row.provider_instance_id,
            row.runtime_mode,
            row.interaction_mode,
            row.active_provider_thread_id,
            row.created_at,
            row.updated_at,
            row.archived_at,
            row.deleted_at,
            row.payload_json,
        ],
    )?;
    Ok(())
}

fn read_thread(row: &rusqlite::Row<'_>) -> rusqlite::Result<ThreadProjection> {
    Ok(ThreadProjection {
        thread_id: row.get(0)?,
        project_id: row.get(1)?,
        title: row.get(2)?,
        default_provider: row.get(3)?,
        provider_instance_id: row.get(4)?,
        runtime_mode: row.get(5)?,
        interaction_mode: row.get(6)?,
        active_provider_thread_id: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        archived_at: row.get(10)?,
        deleted_at: row.get(11)?,
        payload_json: row.get(12)?,
    })
}

const THREAD_COLUMNS: &str = "thread_id, project_id, title, default_provider,
    provider_instance_id, runtime_mode, interaction_mode, active_provider_thread_id,
    created_at, updated_at, archived_at, deleted_at, payload_json";

pub fn get_thread(conn: &Connection, thread_id: &str) -> Result<Option<ThreadProjection>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {THREAD_COLUMNS}
                 FROM orchestration_v2_projection_threads
                 WHERE thread_id = ?1"
            ),
            params![thread_id],
            read_thread,
        )
        .optional()?)
}

pub fn list_threads(conn: &Connection, project_id: &str) -> Result<Vec<ThreadProjection>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {THREAD_COLUMNS}
         FROM orchestration_v2_projection_threads
         WHERE project_id = ?1
         ORDER BY updated_at, thread_id"
    ))?;
    let rows = statement.query_map(params![project_id], read_thread)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn upsert_run(conn: &Connection, row: &RunProjection) -> Result<()> {
    conn.execute(
        "INSERT INTO orchestration_v2_projection_runs (
            run_id,
            thread_id,
            ordinal,
            provider,
            provider_instance_id,
            provider_thread_id,
            status,
            requested_at,
            completed_at,
            payload_json
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(run_id) DO UPDATE SET
            thread_id = excluded.thread_id,
            ordinal = excluded.ordinal,
            provider = excluded.provider,
            provider_instance_id = excluded.provider_instance_id,
            provider_thread_id = excluded.provider_thread_id,
            status = excluded.status,
            requested_at = excluded.requested_at,
            completed_at = excluded.completed_at,
            payload_json = excluded.payload_json",
        params![
            row.run_id,
            row.thread_id,
            row.ordinal,
            row.provider,
            row.provider_instance_id,
            row.provider_thread_id,
            row.status,
            row.requested_at,
            row.completed_at,
            row.payload_json,
        ],
    )?;
    Ok(())
}

fn read_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunProjection> {
    Ok(RunProjection {
        run_id: row.get(0)?,
        thread_id: row.get(1)?,
        ordinal: row.get(2)?,
        provider: row.get(3)?,
        provider_instance_id: row.get(4)?,
        provider_thread_id: row.get(5)?,
        status: row.get(6)?,
        requested_at: row.get(7)?,
        completed_at: row.get(8)?,
        payload_json: row.get(9)?,
    })
}

const RUN_COLUMNS: &str = "run_id, thread_id, ordinal, provider, provider_instance_id,
    provider_thread_id, status, requested_at, completed_at, payload_json";

pub fn get_run(conn: &Connection, run_id: &str) -> Result<Option<RunProjection>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {RUN_COLUMNS}
                 FROM orchestration_v2_projection_runs
                 WHERE run_id = ?1"
            ),
            params![run_id],
            read_run,
        )
        .optional()?)
}

pub fn list_runs(conn: &Connection, thread_id: &str) -> Result<Vec<RunProjection>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {RUN_COLUMNS}
         FROM orchestration_v2_projection_runs
         WHERE thread_id = ?1
         ORDER BY ordinal, run_id"
    ))?;
    let rows = statement.query_map(params![thread_id], read_run)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn upsert_run_attempt(conn: &Connection, row: &RunAttemptProjection) -> Result<()> {
    conn.execute(
        "INSERT INTO orchestration_v2_projection_run_attempts (
            attempt_id,
            thread_id,
            run_id,
            attempt_ordinal,
            root_node_id,
            provider,
            provider_instance_id,
            provider_thread_id,
            provider_turn_id,
            status,
            payload_json
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(attempt_id) DO UPDATE SET
            thread_id = excluded.thread_id,
            run_id = excluded.run_id,
            attempt_ordinal = excluded.attempt_ordinal,
            root_node_id = excluded.root_node_id,
            provider = excluded.provider,
            provider_instance_id = excluded.provider_instance_id,
            provider_thread_id = excluded.provider_thread_id,
            provider_turn_id = excluded.provider_turn_id,
            status = excluded.status,
            payload_json = excluded.payload_json",
        params![
            row.attempt_id,
            row.thread_id,
            row.run_id,
            row.attempt_ordinal,
            row.root_node_id,
            row.provider,
            row.provider_instance_id,
            row.provider_thread_id,
            row.provider_turn_id,
            row.status,
            row.payload_json,
        ],
    )?;
    Ok(())
}

fn read_run_attempt(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunAttemptProjection> {
    Ok(RunAttemptProjection {
        attempt_id: row.get(0)?,
        thread_id: row.get(1)?,
        run_id: row.get(2)?,
        attempt_ordinal: row.get(3)?,
        root_node_id: row.get(4)?,
        provider: row.get(5)?,
        provider_instance_id: row.get(6)?,
        provider_thread_id: row.get(7)?,
        provider_turn_id: row.get(8)?,
        status: row.get(9)?,
        payload_json: row.get(10)?,
    })
}

const RUN_ATTEMPT_COLUMNS: &str = "attempt_id, thread_id, run_id, attempt_ordinal,
    root_node_id, provider, provider_instance_id, provider_thread_id, provider_turn_id,
    status, payload_json";

pub fn get_run_attempt(
    conn: &Connection,
    attempt_id: &str,
) -> Result<Option<RunAttemptProjection>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {RUN_ATTEMPT_COLUMNS}
                 FROM orchestration_v2_projection_run_attempts
                 WHERE attempt_id = ?1"
            ),
            params![attempt_id],
            read_run_attempt,
        )
        .optional()?)
}

pub fn list_run_attempts(conn: &Connection, run_id: &str) -> Result<Vec<RunAttemptProjection>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {RUN_ATTEMPT_COLUMNS}
         FROM orchestration_v2_projection_run_attempts
         WHERE run_id = ?1
         ORDER BY attempt_ordinal, attempt_id"
    ))?;
    let rows = statement.query_map(params![run_id], read_run_attempt)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn upsert_node(conn: &Connection, row: &NodeProjection) -> Result<()> {
    conn.execute(
        "INSERT INTO orchestration_v2_projection_nodes (
            node_id,
            thread_id,
            run_id,
            parent_node_id,
            root_node_id,
            kind,
            status,
            provider_thread_id,
            provider_turn_id,
            started_at,
            completed_at,
            payload_json
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(node_id) DO UPDATE SET
            thread_id = excluded.thread_id,
            run_id = excluded.run_id,
            parent_node_id = excluded.parent_node_id,
            root_node_id = excluded.root_node_id,
            kind = excluded.kind,
            status = excluded.status,
            provider_thread_id = excluded.provider_thread_id,
            provider_turn_id = excluded.provider_turn_id,
            started_at = excluded.started_at,
            completed_at = excluded.completed_at,
            payload_json = excluded.payload_json",
        params![
            row.node_id,
            row.thread_id,
            row.run_id,
            row.parent_node_id,
            row.root_node_id,
            row.kind,
            row.status,
            row.provider_thread_id,
            row.provider_turn_id,
            row.started_at,
            row.completed_at,
            row.payload_json,
        ],
    )?;
    Ok(())
}

fn read_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<NodeProjection> {
    Ok(NodeProjection {
        node_id: row.get(0)?,
        thread_id: row.get(1)?,
        run_id: row.get(2)?,
        parent_node_id: row.get(3)?,
        root_node_id: row.get(4)?,
        kind: row.get(5)?,
        status: row.get(6)?,
        provider_thread_id: row.get(7)?,
        provider_turn_id: row.get(8)?,
        started_at: row.get(9)?,
        completed_at: row.get(10)?,
        payload_json: row.get(11)?,
    })
}

const NODE_COLUMNS: &str = "node_id, thread_id, run_id, parent_node_id, root_node_id,
    kind, status, provider_thread_id, provider_turn_id, started_at, completed_at,
    payload_json";

pub fn get_node(conn: &Connection, node_id: &str) -> Result<Option<NodeProjection>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {NODE_COLUMNS}
                 FROM orchestration_v2_projection_nodes
                 WHERE node_id = ?1"
            ),
            params![node_id],
            read_node,
        )
        .optional()?)
}

pub fn list_nodes(conn: &Connection, thread_id: &str) -> Result<Vec<NodeProjection>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {NODE_COLUMNS}
         FROM orchestration_v2_projection_nodes
         WHERE thread_id = ?1
         ORDER BY node_id"
    ))?;
    let rows = statement.query_map(params![thread_id], read_node)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn upsert_provider_session(conn: &Connection, row: &ProviderSessionProjection) -> Result<()> {
    conn.execute(
        "INSERT INTO orchestration_v2_projection_provider_sessions (
            provider_session_id,
            thread_id,
            provider,
            driver,
            provider_instance_id,
            status,
            updated_at,
            payload_json
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(provider_session_id) DO UPDATE SET
            thread_id = excluded.thread_id,
            provider = excluded.provider,
            driver = excluded.driver,
            provider_instance_id = excluded.provider_instance_id,
            status = excluded.status,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json",
        params![
            row.provider_session_id,
            row.thread_id,
            row.provider,
            row.driver,
            row.provider_instance_id,
            row.status,
            row.updated_at,
            row.payload_json,
        ],
    )?;
    Ok(())
}

fn read_provider_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProviderSessionProjection> {
    Ok(ProviderSessionProjection {
        provider_session_id: row.get(0)?,
        thread_id: row.get(1)?,
        provider: row.get(2)?,
        driver: row.get(3)?,
        provider_instance_id: row.get(4)?,
        status: row.get(5)?,
        updated_at: row.get(6)?,
        payload_json: row.get(7)?,
    })
}

const PROVIDER_SESSION_COLUMNS: &str = "provider_session_id, thread_id, provider, driver,
    provider_instance_id, status, updated_at, payload_json";

pub fn get_provider_session(
    conn: &Connection,
    provider_session_id: &str,
) -> Result<Option<ProviderSessionProjection>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {PROVIDER_SESSION_COLUMNS}
                 FROM orchestration_v2_projection_provider_sessions
                 WHERE provider_session_id = ?1"
            ),
            params![provider_session_id],
            read_provider_session,
        )
        .optional()?)
}

pub fn list_provider_sessions(
    conn: &Connection,
    thread_id: &str,
) -> Result<Vec<ProviderSessionProjection>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {PROVIDER_SESSION_COLUMNS}
         FROM orchestration_v2_projection_provider_sessions
         WHERE thread_id = ?1
         ORDER BY updated_at, provider_session_id"
    ))?;
    let rows = statement.query_map(params![thread_id], read_provider_session)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn upsert_message(conn: &Connection, row: &MessageProjection) -> Result<()> {
    conn.execute(
        "INSERT INTO projection_thread_messages (
            message_id,
            thread_id,
            turn_id,
            role,
            text,
            is_streaming,
            created_at,
            updated_at
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(message_id) DO UPDATE SET
            thread_id = excluded.thread_id,
            turn_id = excluded.turn_id,
            role = excluded.role,
            text = excluded.text,
            is_streaming = excluded.is_streaming,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at",
        params![
            row.message_id,
            row.thread_id,
            row.turn_id,
            row.role,
            row.text,
            if row.is_streaming { 1_i64 } else { 0_i64 },
            row.created_at,
            row.updated_at,
        ],
    )?;
    Ok(())
}

fn read_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageProjection> {
    let streaming: i64 = row.get(5)?;
    Ok(MessageProjection {
        message_id: row.get(0)?,
        thread_id: row.get(1)?,
        turn_id: row.get(2)?,
        role: row.get(3)?,
        text: row.get(4)?,
        is_streaming: streaming != 0,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

pub fn list_messages(conn: &Connection, thread_id: &str) -> Result<Vec<MessageProjection>> {
    let mut statement = conn.prepare(
        "SELECT
            message_id,
            thread_id,
            turn_id,
            role,
            text,
            is_streaming,
            created_at,
            updated_at
         FROM projection_thread_messages
         WHERE thread_id = ?1
         ORDER BY created_at, message_id",
    )?;
    let rows = statement.query_map(params![thread_id], read_message)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn upsert_activity(conn: &Connection, row: &ActivityProjection) -> Result<()> {
    conn.execute(
        "INSERT INTO projection_thread_activities (
            activity_id,
            thread_id,
            turn_id,
            tone,
            kind,
            summary,
            payload_json,
            created_at
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(activity_id) DO UPDATE SET
            thread_id = excluded.thread_id,
            turn_id = excluded.turn_id,
            tone = excluded.tone,
            kind = excluded.kind,
            summary = excluded.summary,
            payload_json = excluded.payload_json,
            created_at = excluded.created_at",
        params![
            row.activity_id,
            row.thread_id,
            row.turn_id,
            row.tone,
            row.kind,
            row.summary,
            row.payload_json,
            row.created_at,
        ],
    )?;
    Ok(())
}

fn read_activity(row: &rusqlite::Row<'_>) -> rusqlite::Result<ActivityProjection> {
    Ok(ActivityProjection {
        activity_id: row.get(0)?,
        thread_id: row.get(1)?,
        turn_id: row.get(2)?,
        tone: row.get(3)?,
        kind: row.get(4)?,
        summary: row.get(5)?,
        payload_json: row.get(6)?,
        created_at: row.get(7)?,
    })
}

pub fn list_activities(conn: &Connection, thread_id: &str) -> Result<Vec<ActivityProjection>> {
    let mut statement = conn.prepare(
        "SELECT
            activity_id,
            thread_id,
            turn_id,
            tone,
            kind,
            summary,
            payload_json,
            created_at
         FROM projection_thread_activities
         WHERE thread_id = ?1
         ORDER BY created_at, activity_id",
    )?;
    let rows = statement.query_map(params![thread_id], read_activity)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn upsert_turn(conn: &Connection, row: &TurnProjection) -> Result<()> {
    conn.execute(
        "INSERT INTO projection_turns (
            thread_id,
            turn_id,
            pending_message_id,
            assistant_message_id,
            state,
            requested_at,
            started_at,
            completed_at,
            checkpoint_turn_count,
            checkpoint_ref,
            checkpoint_status,
            checkpoint_files_json
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(thread_id, turn_id) DO UPDATE SET
            pending_message_id = excluded.pending_message_id,
            assistant_message_id = excluded.assistant_message_id,
            state = excluded.state,
            requested_at = excluded.requested_at,
            started_at = excluded.started_at,
            completed_at = excluded.completed_at,
            checkpoint_turn_count = excluded.checkpoint_turn_count,
            checkpoint_ref = excluded.checkpoint_ref,
            checkpoint_status = excluded.checkpoint_status,
            checkpoint_files_json = excluded.checkpoint_files_json",
        params![
            row.thread_id,
            row.turn_id,
            row.pending_message_id,
            row.assistant_message_id,
            row.state,
            row.requested_at,
            row.started_at,
            row.completed_at,
            row.checkpoint_turn_count,
            row.checkpoint_ref,
            row.checkpoint_status,
            row.checkpoint_files_json,
        ],
    )?;
    Ok(())
}

fn read_turn(row: &rusqlite::Row<'_>) -> rusqlite::Result<TurnProjection> {
    Ok(TurnProjection {
        thread_id: row.get(0)?,
        turn_id: row.get(1)?,
        pending_message_id: row.get(2)?,
        assistant_message_id: row.get(3)?,
        state: row.get(4)?,
        requested_at: row.get(5)?,
        started_at: row.get(6)?,
        completed_at: row.get(7)?,
        checkpoint_turn_count: row.get(8)?,
        checkpoint_ref: row.get(9)?,
        checkpoint_status: row.get(10)?,
        checkpoint_files_json: row.get(11)?,
    })
}

const TURN_COLUMNS: &str = "thread_id, turn_id, pending_message_id, assistant_message_id,
    state, requested_at, started_at, completed_at, checkpoint_turn_count, checkpoint_ref,
    checkpoint_status, checkpoint_files_json";

pub fn get_turn(
    conn: &Connection,
    thread_id: &str,
    turn_id: &str,
) -> Result<Option<TurnProjection>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {TURN_COLUMNS}
                 FROM projection_turns
                 WHERE thread_id = ?1 AND turn_id = ?2"
            ),
            params![thread_id, turn_id],
            read_turn,
        )
        .optional()?)
}

pub fn list_turns(conn: &Connection, thread_id: &str) -> Result<Vec<TurnProjection>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {TURN_COLUMNS}
         FROM projection_turns
         WHERE thread_id = ?1
         ORDER BY requested_at, row_id"
    ))?;
    let rows = statement.query_map(params![thread_id], read_turn)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn upsert_pending_approval(conn: &Connection, row: &PendingApprovalProjection) -> Result<()> {
    conn.execute(
        "INSERT INTO projection_pending_approvals (
            request_id,
            thread_id,
            turn_id,
            status,
            decision,
            created_at,
            resolved_at
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(request_id) DO UPDATE SET
            thread_id = excluded.thread_id,
            turn_id = excluded.turn_id,
            status = excluded.status,
            decision = excluded.decision,
            created_at = excluded.created_at,
            resolved_at = excluded.resolved_at",
        params![
            row.request_id,
            row.thread_id,
            row.turn_id,
            row.status,
            row.decision,
            row.created_at,
            row.resolved_at,
        ],
    )?;
    Ok(())
}

fn read_pending_approval(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingApprovalProjection> {
    Ok(PendingApprovalProjection {
        request_id: row.get(0)?,
        thread_id: row.get(1)?,
        turn_id: row.get(2)?,
        status: row.get(3)?,
        decision: row.get(4)?,
        created_at: row.get(5)?,
        resolved_at: row.get(6)?,
    })
}

pub fn list_pending_approvals(
    conn: &Connection,
    thread_id: &str,
    status: &str,
) -> Result<Vec<PendingApprovalProjection>> {
    let mut statement = conn.prepare(
        "SELECT
            request_id,
            thread_id,
            turn_id,
            status,
            decision,
            created_at,
            resolved_at
         FROM projection_pending_approvals
         WHERE thread_id = ?1 AND status = ?2
         ORDER BY created_at, request_id",
    )?;
    let rows = statement.query_map(params![thread_id, status], read_pending_approval)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn upsert_projector_state(conn: &Connection, row: &ProjectorState) -> Result<()> {
    conn.execute(
        "INSERT INTO projection_state (
            projector,
            last_applied_sequence,
            updated_at
         )
         VALUES (?1, ?2, ?3)
         ON CONFLICT(projector) DO UPDATE SET
            last_applied_sequence = excluded.last_applied_sequence,
            updated_at = excluded.updated_at",
        params![row.projector, row.last_applied_sequence, row.updated_at],
    )?;
    Ok(())
}

pub fn get_projector_state(conn: &Connection, projector: &str) -> Result<Option<ProjectorState>> {
    Ok(conn
        .query_row(
            "SELECT projector, last_applied_sequence, updated_at
             FROM projection_state
             WHERE projector = ?1",
            params![projector],
            |row| {
                Ok(ProjectorState {
                    projector: row.get(0)?,
                    last_applied_sequence: row.get(1)?,
                    updated_at: row.get(2)?,
                })
            },
        )
        .optional()?)
}
