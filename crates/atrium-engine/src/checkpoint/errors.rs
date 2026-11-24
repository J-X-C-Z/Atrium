// Ported from apps/server/src/checkpointing/Errors.ts (T3 Code v0.1.11, MIT).
//! Checkpoint errors. Messages keep T3's wording because clients match on
//! them; the tag names stay stable for callers switching on a reason.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::contracts::ThreadId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckpointDiffOperation {
    #[serde(rename = "CheckpointDiffQuery.getTurnDiff")]
    GetTurnDiff,
    #[serde(rename = "CheckpointDiffQuery.getFullThreadDiff")]
    GetFullThreadDiff,
}

impl CheckpointDiffOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GetTurnDiff => "CheckpointDiffQuery.getTurnDiff",
            Self::GetFullThreadDiff => "CheckpointDiffQuery.getFullThreadDiff",
        }
    }

    fn result_name(self) -> &'static str {
        match self {
            Self::GetTurnDiff => "turn diff",
            Self::GetFullThreadDiff => "full thread diff",
        }
    }
}

impl fmt::Display for CheckpointDiffOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which end of a turn range has no checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckpointSide {
    From,
    To,
}

/// The checkpoint store's own failure. T3 wraps its `VcsError` union; the
/// engine keeps the git facts callers need and the unsupported-operation
/// distinction that turns a capture into a `missing` checkpoint.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum CheckpointStoreError {
    #[error("{command} failed in {cwd} (exit {exit_code}): {detail}")]
    ProcessExit {
        operation: String,
        command: String,
        cwd: String,
        exit_code: i32,
        detail: String,
    },
    #[error("checkpoint storage error in {stage}: {detail}")]
    Storage {
        operation: String,
        stage: String,
        detail: String,
    },
    #[error("unsupported checkpoint operation: {detail}")]
    Unsupported { operation: String, detail: String },
    #[error("{0}")]
    Other(String),
}

impl CheckpointStoreError {
    pub fn process_exit(
        operation: impl Into<String>,
        command: impl Into<String>,
        cwd: impl Into<String>,
        exit_code: i32,
        detail: impl Into<String>,
    ) -> Self {
        Self::ProcessExit {
            operation: operation.into(),
            command: command.into(),
            cwd: cwd.into(),
            exit_code,
            detail: detail.into(),
        }
    }

    /// A Git operation the driver does not support (T3's
    /// `VcsUnsupportedOperationError`): a capture records `missing`.
    pub fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported { .. })
    }

    /// The held-index-lock detail, which rollback surfaces verbatim.
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::ProcessExit { detail, .. } => Some(detail),
            _ => None,
        }
    }
}

/// The errors `CheckpointDiffQuery` raises in addition to store failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckpointServiceError {
    DiffResultInvalid {
        operation: CheckpointDiffOperation,
        thread_id: ThreadId,
    },
    ThreadNotFound {
        operation: CheckpointDiffOperation,
        thread_id: ThreadId,
    },
    WorkspacePathMissing {
        operation: CheckpointDiffOperation,
        thread_id: ThreadId,
    },
    TurnRangeUnavailable {
        operation: CheckpointDiffOperation,
        thread_id: ThreadId,
        requested_turn_count: u32,
        available_turn_count: u32,
    },
    RefUnavailable {
        operation: CheckpointDiffOperation,
        thread_id: ThreadId,
        turn_count: u32,
        checkpoint: CheckpointSide,
    },
    Store(CheckpointStoreError),
}

impl From<CheckpointStoreError> for CheckpointServiceError {
    fn from(error: CheckpointStoreError) -> Self {
        Self::Store(error)
    }
}

impl fmt::Display for CheckpointServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DiffResultInvalid {
                operation,
                thread_id: _,
            } => write!(
                f,
                "Checkpoint invariant violation in {operation}: Computed {} result does not satisfy contract schema.",
                operation.result_name()
            ),
            Self::ThreadNotFound {
                operation,
                thread_id,
            } => write!(
                f,
                "Checkpoint invariant violation in {operation}: Thread '{thread_id}' not found."
            ),
            Self::WorkspacePathMissing {
                operation,
                thread_id,
            } => write!(
                f,
                "Checkpoint invariant violation in {operation}: Workspace path missing for thread '{thread_id}' when computing {}.",
                operation.result_name()
            ),
            Self::TurnRangeUnavailable {
                thread_id,
                requested_turn_count,
                available_turn_count,
                ..
            } => write!(
                f,
                "Checkpoint unavailable for thread {thread_id} turn {requested_turn_count}: Turn diff range exceeds current turn count: requested {requested_turn_count}, current {available_turn_count}."
            ),
            Self::RefUnavailable {
                thread_id,
                turn_count,
                ..
            } => write!(
                f,
                "Checkpoint unavailable for thread {thread_id} turn {turn_count}: Checkpoint ref is unavailable for turn {turn_count}."
            ),
            Self::Store(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for CheckpointServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            _ => None,
        }
    }
}
