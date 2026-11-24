//! Checkpoints over hidden Git refs.
//!
//! Ports `apps/server/src/checkpointing` and the checkpoint services in
//! `orchestration-v2`. The Git work reuses Atrium's `atrium_project::repo`
//! snapshot and restore helpers; every decision is synchronous and takes the
//! clock through a `captured_at`/`created_at` argument, so the module stays
//! free of GPUI and of any provider process.
//!
//! Ported from T3 Code v0.1.11 (MIT); each file names its upstream source.

pub mod capture;
pub mod context;
pub mod diffs;
pub mod errors;
pub mod fork;
pub mod query;
pub mod restore_safety;
pub mod rollback;
pub mod service;
pub mod store;

pub use capture::{
    CaptureDecision, CapturePlan, CaptureTarget, CheckpointCaptureContext,
    CheckpointCaptureExecutionError, decide_capture,
};
pub use context::{
    CheckpointContext, CheckpointContextError, CheckpointContextProvider, CheckpointRunRef,
};
pub use diffs::{TurnDiffFileSummary, is_git_import, parse_turn_diff_files_from_numstat};
pub use errors::{
    CheckpointDiffOperation, CheckpointServiceError, CheckpointSide, CheckpointStoreError,
};
pub use fork::{
    ThreadForkInput, ThreadForkPlan, ThreadForkPlanError, forkable_source_run_status_error,
    is_forkable_source_run_status, plan_fork,
};
pub use query::{
    CHECKPOINT_DIFF_TRUNCATED_MARKER, GetFullThreadDiffInput, GetTurnDiffInput, TurnDiffResult,
    get_full_thread_diff, get_turn_diff,
};
pub use restore_safety::{
    RestoreSafetyContext, RestoreSafetyError, RestoreThread, SHARED_WORKSPACE_RESTORE_MESSAGE,
    ShellSnapshot, ThreadProviderContext, ThreadRecords, ThreadRecordsFilter, ThreadShell,
    is_checkpoint_restore_isolated,
};
pub use rollback::{
    CheckpointRollbackContext, CheckpointRollbackExecutionError, GIT_INDEX_LOCKED_DETAIL,
    ROLLBACK_FAILED_MESSAGE, RollbackCause, RollbackFailureReason, RollbackPlan,
    RollbackProviderTarget, RollbackRequest, plan_rollback, rollback_failure_client_message,
};
pub use service::{
    CaptureRequest, CheckpointService, CheckpointServiceOpError, RootRunScopeInput,
    checkpoint_ref_for_scope_ordinal, make_checkpoint, make_root_run_scope,
};
pub use store::{
    CaptureCheckpointInput, CheckpointStore, DeleteCheckpointRefsInput, DiffCheckpointsInput,
    DiffFormat, GitCheckpointStore, ListAuthoredPathsInput, RestoreCheckpointInput,
};
