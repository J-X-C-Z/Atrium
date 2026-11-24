// Ported from apps/server/src/orchestration-v2/UsageLimitRecoveryWorker.ts (T3 Code v0.1.11, MIT).
//! Continue a thread after its usage limit resets.
//!
//! The decision is pure: a candidate thread, the two server preferences, and
//! `now`. An eligible failure first arms a recovery record (auto-resume or a
//! snooze); once the reset has passed the record becomes a "continue where you
//! left off" dispatch. A failure whose reset is more than a day overdue is not
//! armed, so a stale thread cannot start a retry loop.
//!
//! [`SqliteRecoveryJournal`] persists the armed record per thread so the
//! decision survives a server restart. It owns a table of its own and never
//! touches the orchestration store's migrations.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::contracts::item::ProviderFailureClass;
use crate::contracts::thread::SettledOverride;

use super::limits::parse_ms;

/// T3's `MAX_OVERDUE_ARM_MS`: one day.
pub const MAX_OVERDUE_ARM_MS: i64 = 24 * 60 * 60 * 1000;

/// The text T3 sends to continue a limited run.
pub const LIMIT_RECOVERY_TEXT: &str = "Continue where you left off.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryThreadStatus {
    Failed,
    Other,
}

/// T3's `OrchestrationV2LimitRecovery` as stored on the thread.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitRecovery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub run_id: String,
    pub reset_at: String,
    pub auto_resume: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snooze: Option<bool>,
}

/// The fields of `ProjectionLimitRecoveryCandidate` the decision reads.
#[derive(Clone, Debug, PartialEq)]
pub struct LimitRecoveryCandidate {
    pub id: String,
    pub status: RecoveryThreadStatus,
    pub last_error_class: Option<ProviderFailureClass>,
    pub usage_limit_reset_at: Option<String>,
    pub latest_run_id: Option<String>,
    pub latest_run_completed_at: Option<String>,
    pub updated_at: String,
    pub archived_at: Option<String>,
    pub settled_override: Option<SettledOverride>,
    pub pending_runtime_request: bool,
    pub limit_recovery: Option<LimitRecovery>,
    pub snoozed_until: Option<String>,
}

/// The two server preferences that change the decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LimitRecoverySettings {
    pub auto_resume: bool,
    pub snooze: bool,
}

/// What a scheduler should do next for one thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LimitRecoveryCommand {
    Arm {
        command_id: String,
        thread_id: String,
        limit_recovery: LimitRecovery,
    },
    Resume {
        command_id: String,
        message_id: String,
        thread_id: String,
        continuation_of_run_id: String,
        recovery_request_id: Option<String>,
        text: String,
    },
}

impl LimitRecoveryCommand {
    pub fn command_id(&self) -> &str {
        match self {
            Self::Arm { command_id, .. } | Self::Resume { command_id, .. } => command_id,
        }
    }

    pub fn thread_id(&self) -> &str {
        match self {
            Self::Arm { thread_id, .. } | Self::Resume { thread_id, .. } => thread_id,
        }
    }
}

fn epoch_ms(value: Option<&str>) -> Option<i64> {
    value.and_then(parse_ms)
}

/// T3's `limitRecoveryCommand`.
///
/// Returns `None` when the thread is not a live usage-limit failure, when its
/// reset has already passed without an armed record to resume, or when the
/// reset is too old to arm. `snooze` keeps an armed record waiting past its
/// reset until the user acts.
pub fn limit_recovery_command(
    thread: &LimitRecoveryCandidate,
    auto_resume: bool,
    now_ms: i64,
    snooze: bool,
) -> Option<LimitRecoveryCommand> {
    let latest_run_id = thread.latest_run_id.as_deref()?;
    let reset_at = thread.usage_limit_reset_at.as_deref()?;
    if thread.status != RecoveryThreadStatus::Failed
        || thread.last_error_class != Some(ProviderFailureClass::UsageLimit)
        || thread.archived_at.is_some()
        || thread.settled_override == Some(SettledOverride::Settled)
        || thread.pending_runtime_request
    {
        return None;
    }
    let reset_ms = parse_ms(reset_at)?;
    // An already-expired window reported with a fresh failure cannot start a
    // retry loop.
    let completed_or_updated = epoch_ms(thread.latest_run_completed_at.as_deref())
        .or_else(|| epoch_ms(Some(&thread.updated_at)))?;
    if reset_ms <= completed_or_updated {
        return None;
    }

    let identity = format!("{}:{latest_run_id}:{reset_ms}", thread.id);
    let recovery = thread.limit_recovery.as_ref();
    let already_armed = recovery
        .is_some_and(|recovery| recovery.run_id == latest_run_id && recovery.reset_at == reset_at);
    if !already_armed {
        if now_ms - reset_ms > MAX_OVERDUE_ARM_MS {
            return None;
        }
        if !auto_resume && (!snooze || reset_ms <= now_ms) {
            return None;
        }
        return Some(LimitRecoveryCommand::Arm {
            command_id: format!("limit-arm:{identity}"),
            thread_id: thread.id.clone(),
            limit_recovery: LimitRecovery {
                request_id: None,
                run_id: latest_run_id.to_owned(),
                reset_at: reset_at.to_owned(),
                auto_resume,
                snooze: Some(snooze && reset_ms > now_ms),
            },
        });
    }

    let recovery = recovery?;
    let snoozed = epoch_ms(thread.snoozed_until.as_deref()).is_some_and(|until| until > now_ms);
    if !recovery.auto_resume || reset_ms > now_ms || snoozed {
        return None;
    }
    let delivery_identity = format!(
        "{identity}:{}",
        recovery.request_id.as_deref().unwrap_or("legacy")
    );
    Some(LimitRecoveryCommand::Resume {
        command_id: format!("limit-resume:{delivery_identity}"),
        message_id: format!("limit-resume:{delivery_identity}"),
        thread_id: thread.id.clone(),
        continuation_of_run_id: latest_run_id.to_owned(),
        recovery_request_id: recovery.request_id.clone(),
        text: LIMIT_RECOVERY_TEXT.into(),
    })
}

/// The automatic path a scheduler calls: decide for every candidate, in input
/// order. Persisting and dispatching the returned commands is the caller's
/// job; this function only reads plain data and `now`.
pub fn sweep(
    candidates: &[LimitRecoveryCandidate],
    settings: LimitRecoverySettings,
    now_ms: i64,
) -> Vec<LimitRecoveryCommand> {
    candidates
        .iter()
        .filter_map(|candidate| {
            limit_recovery_command(candidate, settings.auto_resume, now_ms, settings.snooze)
        })
        .collect()
}

#[derive(Debug, Error)]
pub enum RecoveryError {
    #[error("usage-limit recovery store: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// The persisted record for one thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredRecovery {
    pub thread_id: String,
    pub run_id: String,
    pub reset_at: String,
    pub auto_resume: bool,
    pub snooze: bool,
    pub request_id: Option<String>,
    pub armed_at: String,
    pub resumed_at: Option<String>,
}

const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS usage_limit_recovery (
      thread_id TEXT PRIMARY KEY,
      run_id TEXT NOT NULL,
      reset_at TEXT NOT NULL,
      auto_resume INTEGER NOT NULL,
      snooze INTEGER NOT NULL,
      request_id TEXT,
      armed_at TEXT NOT NULL,
      resumed_at TEXT
    )
";

/// Persist each thread's recovery decision in SQLite.
///
/// A restart reads the same table, so an armed reset still resumes. The table
/// is created on first open and is independent of `store::migrate`.
pub struct SqliteRecoveryJournal {
    connection: Connection,
}

impl SqliteRecoveryJournal {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, RecoveryError> {
        let connection = Connection::open(path)?;
        Self::from_connection(connection)
    }

    pub fn open_in_memory() -> Result<Self, RecoveryError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(connection: Connection) -> Result<Self, RecoveryError> {
        connection.execute_batch(CREATE_TABLE)?;
        Ok(Self { connection })
    }

    /// Record an armed recovery, replacing any earlier decision for the thread.
    pub fn record_arm(
        &self,
        thread_id: &str,
        recovery: &LimitRecovery,
        armed_at: &str,
    ) -> Result<(), RecoveryError> {
        self.connection.execute(
            "INSERT INTO usage_limit_recovery
               (thread_id, run_id, reset_at, auto_resume, snooze, request_id, armed_at, resumed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)
             ON CONFLICT(thread_id) DO UPDATE SET
               run_id = excluded.run_id,
               reset_at = excluded.reset_at,
               auto_resume = excluded.auto_resume,
               snooze = excluded.snooze,
               request_id = excluded.request_id,
               armed_at = excluded.armed_at,
               resumed_at = NULL",
            params![
                thread_id,
                &recovery.run_id,
                &recovery.reset_at,
                i64::from(recovery.auto_resume),
                i64::from(recovery.snooze.unwrap_or(false)),
                recovery.request_id.as_deref(),
                armed_at,
            ],
        )?;
        Ok(())
    }

    /// Mark the thread's armed recovery as delivered.
    pub fn record_resume(&self, thread_id: &str, resumed_at: &str) -> Result<(), RecoveryError> {
        self.connection.execute(
            "UPDATE usage_limit_recovery SET resumed_at = ?2 WHERE thread_id = ?1",
            params![thread_id, resumed_at],
        )?;
        Ok(())
    }

    pub fn load(&self, thread_id: &str) -> Result<Option<StoredRecovery>, RecoveryError> {
        self.connection
            .query_row(
                "SELECT thread_id, run_id, reset_at, auto_resume, snooze, request_id, armed_at, resumed_at
                 FROM usage_limit_recovery WHERE thread_id = ?1",
                params![thread_id],
                |row| {
                    Ok(StoredRecovery {
                        thread_id: row.get(0)?,
                        run_id: row.get(1)?,
                        reset_at: row.get(2)?,
                        auto_resume: row.get::<_, i64>(3)? != 0,
                        snooze: row.get::<_, i64>(4)? != 0,
                        request_id: row.get(5)?,
                        armed_at: row.get(6)?,
                        resumed_at: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(RecoveryError::from)
    }

    /// Every thread still waiting to resume, oldest reset first.
    pub fn pending(&self) -> Result<Vec<StoredRecovery>, RecoveryError> {
        let mut statement = self.connection.prepare(
            "SELECT thread_id, run_id, reset_at, auto_resume, snooze, request_id, armed_at, resumed_at
             FROM usage_limit_recovery
             WHERE resumed_at IS NULL AND auto_resume = 1
             ORDER BY reset_at ASC, thread_id ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(StoredRecovery {
                thread_id: row.get(0)?,
                run_id: row.get(1)?,
                reset_at: row.get(2)?,
                auto_resume: row.get::<_, i64>(3)? != 0,
                snooze: row.get::<_, i64>(4)? != 0,
                request_id: row.get(5)?,
                armed_at: row.get(6)?,
                resumed_at: row.get(7)?,
            })
        })?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::item::ProviderFailureClass;
    use crate::contracts::thread::SettledOverride;

    const HOUR_MS: i64 = 60 * 60 * 1000;
    const NOW_MS: i64 = 1_792_411_200_000; // 2026-10-19T12:00:00.000Z

    fn iso(ms: i64) -> String {
        super::super::limits::format_iso_millis(ms).unwrap()
    }

    fn failed_thread(failed_ago_ms: i64, reset_ago_ms: i64) -> LimitRecoveryCandidate {
        let failed_at = NOW_MS - failed_ago_ms;
        LimitRecoveryCandidate {
            id: "thread:limited".into(),
            status: RecoveryThreadStatus::Failed,
            last_error_class: Some(ProviderFailureClass::UsageLimit),
            usage_limit_reset_at: Some(iso(NOW_MS - reset_ago_ms)),
            latest_run_id: Some("run:limited".into()),
            latest_run_completed_at: Some(iso(failed_at)),
            updated_at: iso(failed_at),
            archived_at: None,
            settled_override: None,
            pending_runtime_request: false,
            limit_recovery: None,
            snoozed_until: None,
        }
    }

    #[test]
    fn does_not_freshly_arm_a_failure_whose_reset_passed_days_ago() {
        let thread = failed_thread(5 * 24 * HOUR_MS, 4 * 24 * HOUR_MS);
        assert_eq!(limit_recovery_command(&thread, true, NOW_MS, false), None);
        assert_eq!(limit_recovery_command(&thread, true, NOW_MS, true), None);
    }

    #[test]
    fn still_arms_a_failure_whose_reset_only_just_passed() {
        let thread = failed_thread(6 * HOUR_MS, HOUR_MS);
        assert!(matches!(
            limit_recovery_command(&thread, true, NOW_MS, false),
            Some(LimitRecoveryCommand::Arm { .. })
        ));
    }

    #[test]
    fn arms_a_reset_that_is_exactly_24_hours_overdue() {
        let thread = failed_thread(30 * HOUR_MS, 24 * HOUR_MS);
        let command = limit_recovery_command(&thread, true, NOW_MS, false).unwrap();
        assert_eq!(
            command.command_id(),
            format!(
                "limit-arm:thread:limited:run:limited:{}",
                NOW_MS - 24 * HOUR_MS
            )
        );
    }

    #[test]
    fn does_not_arm_a_reset_that_is_24_hours_and_1ms_overdue() {
        let thread = failed_thread(30 * HOUR_MS, 24 * HOUR_MS + 1);
        assert_eq!(limit_recovery_command(&thread, true, NOW_MS, false), None);
    }

    #[test]
    fn arms_only_with_auto_resume_or_a_snooze() {
        let future = failed_thread(HOUR_MS, -HOUR_MS);
        assert!(limit_recovery_command(&future, false, NOW_MS, false).is_none());
        assert!(limit_recovery_command(&future, false, NOW_MS, true).is_some());

        let past = failed_thread(2 * HOUR_MS, HOUR_MS);
        // A snooze hides a past reset until the user acts; there is nothing to
        // snooze once the window is already over.
        assert!(limit_recovery_command(&past, false, NOW_MS, true).is_none());
    }

    #[test]
    fn refuses_archived_settled_and_runtime_blocked_threads() {
        let base = failed_thread(2 * HOUR_MS, -HOUR_MS);
        for mutate in [
            |thread: &mut LimitRecoveryCandidate| thread.archived_at = Some(iso(NOW_MS)),
            |thread: &mut LimitRecoveryCandidate| {
                thread.settled_override = Some(SettledOverride::Settled)
            },
            |thread: &mut LimitRecoveryCandidate| thread.pending_runtime_request = true,
            |thread: &mut LimitRecoveryCandidate| {
                thread.last_error_class = Some(ProviderFailureClass::ProviderError)
            },
        ] {
            let mut thread = base.clone();
            mutate(&mut thread);
            assert!(limit_recovery_command(&thread, true, NOW_MS, false).is_none());
        }
    }

    #[test]
    fn an_armed_reset_resumes_once_now_passes_it() {
        let reset_at = iso(NOW_MS - 1_000);
        let thread = LimitRecoveryCandidate {
            usage_limit_reset_at: Some(reset_at.clone()),
            limit_recovery: Some(LimitRecovery {
                request_id: None,
                run_id: "run:limited".into(),
                reset_at,
                auto_resume: true,
                snooze: Some(false),
            }),
            ..failed_thread(2 * HOUR_MS, 1_000)
        };
        let command = limit_recovery_command(&thread, true, NOW_MS, false).unwrap();
        match command {
            LimitRecoveryCommand::Resume {
                command_id,
                message_id,
                continuation_of_run_id,
                text,
                recovery_request_id,
                ..
            } => {
                assert_eq!(
                    command_id,
                    format!(
                        "limit-resume:thread:limited:run:limited:{}:legacy",
                        NOW_MS - 1_000
                    )
                );
                assert_eq!(message_id, command_id);
                assert_eq!(continuation_of_run_id, "run:limited");
                assert_eq!(text, LIMIT_RECOVERY_TEXT);
                assert_eq!(recovery_request_id, None);
            }
            other => panic!("expected resume, got {other:?}"),
        }
    }

    #[test]
    fn an_armed_reset_waits_for_its_time_or_a_snooze() {
        let reset_at = iso(NOW_MS + HOUR_MS);
        let thread = LimitRecoveryCandidate {
            usage_limit_reset_at: Some(reset_at.clone()),
            limit_recovery: Some(LimitRecovery {
                request_id: Some("request-1".into()),
                run_id: "run:limited".into(),
                reset_at,
                auto_resume: true,
                snooze: Some(false),
            }),
            ..failed_thread(HOUR_MS, -HOUR_MS)
        };
        assert!(limit_recovery_command(&thread, true, NOW_MS, false).is_none());

        let snoozed = LimitRecoveryCandidate {
            usage_limit_reset_at: Some(iso(NOW_MS - 1_000)),
            snoozed_until: Some(iso(NOW_MS + HOUR_MS)),
            limit_recovery: Some(LimitRecovery {
                request_id: None,
                run_id: "run:limited".into(),
                reset_at: iso(NOW_MS - 1_000),
                auto_resume: true,
                snooze: Some(true),
            }),
            ..failed_thread(2 * HOUR_MS, 1_000)
        };
        assert!(limit_recovery_command(&snoozed, true, NOW_MS, true).is_none());
    }

    #[test]
    fn sweep_returns_one_command_per_eligible_thread() {
        let eligible = failed_thread(2 * HOUR_MS, -HOUR_MS);
        let mut ineligible = eligible.clone();
        ineligible.id = "thread:ok".into();
        ineligible.last_error_class = Some(ProviderFailureClass::ProviderError);
        let commands = sweep(
            &[eligible, ineligible],
            LimitRecoverySettings {
                auto_resume: true,
                snooze: false,
            },
            NOW_MS,
        );
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].thread_id(), "thread:limited");
    }

    #[test]
    fn an_armed_decision_survives_reopening_the_database() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recovery.sqlite");
        let recovery = LimitRecovery {
            request_id: Some("request-1".into()),
            run_id: "run:limited".into(),
            reset_at: "2026-10-19T13:00:00.000Z".into(),
            auto_resume: true,
            snooze: Some(false),
        };
        {
            let journal = SqliteRecoveryJournal::open(&path).unwrap();
            journal
                .record_arm("thread:limited", &recovery, "2026-10-19T12:00:00.000Z")
                .unwrap();
            assert_eq!(
                journal.load("thread:limited").unwrap().unwrap().run_id,
                "run:limited"
            );
        }
        let reopened = SqliteRecoveryJournal::open(&path).unwrap();
        let stored = reopened.load("thread:limited").unwrap().unwrap();
        assert_eq!(stored.run_id, "run:limited");
        assert_eq!(stored.reset_at, recovery.reset_at);
        assert!(stored.auto_resume);
        assert!(!stored.snooze);
        assert_eq!(stored.request_id.as_deref(), Some("request-1"));
        assert_eq!(stored.resumed_at, None);
        assert_eq!(reopened.pending().unwrap().len(), 1);

        reopened
            .record_resume("thread:limited", "2026-10-19T13:00:01.000Z")
            .unwrap();
        assert_eq!(
            reopened
                .load("thread:limited")
                .unwrap()
                .unwrap()
                .resumed_at
                .as_deref(),
            Some("2026-10-19T13:00:01.000Z")
        );
        assert!(reopened.pending().unwrap().is_empty());
    }
}
