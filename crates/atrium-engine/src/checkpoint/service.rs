// Ported from apps/server/src/orchestration-v2/CheckpointService.ts
// (T3 Code v0.1.11, MIT).
//!
//! Scope refs, baseline and turn capture, restore and stale-ref cleanup. The
//! Git work is behind [`CheckpointStore`]; this module holds the decisions
//! T3's service makes around it, without any provider process.

use std::path::Path;

use base64::Engine as _;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::contracts::{
    Checkpoint, CheckpointFileSummary, CheckpointId, CheckpointRef, CheckpointScope,
    CheckpointScopeId, CheckpointScopeKind, CheckpointStatus, NodeId, ProviderThreadId, RunId,
    ThreadId,
};

use super::diffs::{is_git_import, parse_turn_diff_files_from_numstat};
use super::store::{CheckpointStore, DiffCheckpointsInput, DiffFormat, ListAuthoredPathsInput};

const CHECKPOINT_REFS_PREFIX: &str = "refs/t3/orchestration-v2/checkpoints";

/// T3's `checkpointRefForScopeOrdinal`: a ref derived only from the scope id
/// and ordinal, so two processes agree without shared state.
pub fn checkpoint_ref_for_scope_ordinal(
    scope_id: &CheckpointScopeId,
    ordinal_within_scope: u32,
) -> CheckpointRef {
    let digest = Sha256::digest(scope_id.as_str().as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    let scope_key = &hex[..32];
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(scope_key);
    CheckpointRef::new(format!(
        "{CHECKPOINT_REFS_PREFIX}/{encoded}/ordinal/{ordinal_within_scope}"
    ))
}

/// T3's `makeRootRunScope`, with the scope id allocated by the caller.
pub struct RootRunScopeInput {
    pub scope_id: CheckpointScopeId,
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub root_node_id: NodeId,
    pub provider_thread_id: ProviderThreadId,
    pub cwd: String,
    pub created_at: String,
}

pub fn make_root_run_scope(input: RootRunScopeInput) -> CheckpointScope {
    CheckpointScope {
        id: input.scope_id,
        thread_id: input.thread_id,
        run_id: Some(input.run_id),
        node_id: input.root_node_id,
        parent_scope_id: None,
        provider_thread_id: Some(input.provider_thread_id),
        kind: CheckpointScopeKind::RootRun,
        ordinal_within_parent: 0,
        advances_app_run_count: true,
        cwd: input.cwd,
        created_at: input.created_at,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum CheckpointServiceOpError {
    #[error("Failed to prepare root checkpoint scope for run {run_id}.")]
    RootScopePrepare {
        thread_id: ThreadId,
        run_id: RunId,
        cause: String,
    },
    #[error("Failed to capture checkpoint baseline {ordinal_within_scope} for scope {scope_id}.")]
    BaselineCapture {
        scope_id: CheckpointScopeId,
        ordinal_within_scope: u32,
        cause: String,
    },
    #[error("Failed to capture checkpoint for scope {scope_id}.")]
    Capture {
        scope_id: CheckpointScopeId,
        parent_checkpoint_id: Option<CheckpointId>,
        cause: String,
    },
    #[error("Failed to restore checkpoint {checkpoint_id} for scope {scope_id}.")]
    Restore {
        scope_id: CheckpointScopeId,
        checkpoint_id: CheckpointId,
        cause: String,
    },
    #[error("Failed to delete stale checkpoint refs for scope {scope_id}.")]
    DeleteStaleRefs {
        scope_id: CheckpointScopeId,
        checkpoint_ids: Vec<CheckpointId>,
        cause: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RepositoryProbe {
    Repository,
    Missing,
    Error,
}

/// The turn a capture records.
pub struct CaptureRequest<'a> {
    pub scope: &'a CheckpointScope,
    pub run_id: Option<RunId>,
    pub node_id: NodeId,
    pub ordinal_within_scope: u32,
    pub app_run_ordinal: Option<u32>,
    pub captured_at: &'a str,
    pub force_missing: bool,
}

pub struct CheckpointService<'a, S: CheckpointStore> {
    store: &'a S,
}

impl<'a, S: CheckpointStore> CheckpointService<'a, S> {
    pub fn new(store: &'a S) -> Self {
        Self { store }
    }

    fn probe_repository(&self, scope: &CheckpointScope) -> RepositoryProbe {
        match self.store.is_git_repository(Path::new(&scope.cwd)) {
            Ok(true) => RepositoryProbe::Repository,
            Ok(false) => RepositoryProbe::Missing,
            Err(error) => {
                log::warn!(
                    "orchestration V2 checkpoint repository probe failed: scope={} cause={error}",
                    scope.id
                );
                RepositoryProbe::Error
            }
        }
    }

    /// T3's `captureBaseline`: never overwrite an existing baseline and treat
    /// an unsupported workspace as nothing to do.
    pub fn capture_baseline(
        &self,
        scope: &CheckpointScope,
        ordinal_within_scope: u32,
    ) -> Result<(), CheckpointServiceOpError> {
        let result = (|| {
            let repository = self
                .store
                .is_git_repository(Path::new(&scope.cwd))
                .map_err(|error| error.to_string())?;
            if !repository {
                return Ok(());
            }
            let checkpoint_ref = checkpoint_ref_for_scope_ordinal(&scope.id, ordinal_within_scope);
            let exists = self
                .store
                .has_checkpoint_ref(Path::new(&scope.cwd), &checkpoint_ref)
                .map_err(|error| error.to_string())?;
            if exists {
                return Ok(());
            }
            match self
                .store
                .capture_checkpoint(&super::store::CaptureCheckpointInput {
                    cwd: scope.cwd.clone().into(),
                    checkpoint_ref,
                    previous_checkpoint_ref: None,
                }) {
                Ok(()) => Ok(()),
                Err(error) if error.is_unsupported() => Ok(()),
                Err(error) => Err(error.to_string()),
            }
        })();
        result.map_err(|cause| CheckpointServiceOpError::BaselineCapture {
            scope_id: scope.id.clone(),
            ordinal_within_scope,
            cause,
        })
    }

    /// T3's `materializeBaselineCheckpoint`: record a checkpoint row without
    /// capturing, for side chats that share a parent's worktree.
    pub fn materialize_baseline_checkpoint(
        &self,
        scope: &CheckpointScope,
        ordinal_within_scope: u32,
        node_id: NodeId,
        captured_at: &str,
        force_missing: bool,
    ) -> Result<Checkpoint, CheckpointServiceOpError> {
        let checkpoint_ref = checkpoint_ref_for_scope_ordinal(&scope.id, ordinal_within_scope);
        let checkpoint_id = checkpoint_id_for_scope_ordinal(&scope.id, ordinal_within_scope);
        let repository = if force_missing {
            RepositoryProbe::Missing
        } else {
            self.probe_repository(scope)
        };
        let status = if repository == RepositoryProbe::Repository {
            match self
                .store
                .has_checkpoint_ref(Path::new(&scope.cwd), &checkpoint_ref)
            {
                Ok(true) => CheckpointStatus::Ready,
                Ok(false) => CheckpointStatus::Missing,
                Err(error) => {
                    log::warn!(
                        "orchestration V2 baseline ref lookup failed: scope={} cause={error}",
                        scope.id
                    );
                    CheckpointStatus::Error
                }
            }
        } else {
            probe_status(repository)
        };
        Ok(make_checkpoint(MakeCheckpointInput {
            id: checkpoint_id,
            scope,
            run_id: None,
            node_id,
            parent_checkpoint_id: None,
            ordinal_within_scope,
            app_run_ordinal: None,
            checkpoint_ref,
            status,
            files: Vec::new(),
            captured_at,
        }))
    }

    /// T3's `capture`: capture the turn's ref and summarize the files it
    /// changed, recording `missing`/`error` instead of failing the turn.
    pub fn capture(
        &self,
        request: CaptureRequest<'_>,
    ) -> Result<Checkpoint, CheckpointServiceOpError> {
        self.capture_inner(&request)
            .map_err(|error| CheckpointServiceOpError::Capture {
                scope_id: request.scope.id.clone(),
                parent_checkpoint_id: None,
                cause: error,
            })
    }

    fn capture_inner(&self, request: &CaptureRequest<'_>) -> Result<Checkpoint, String> {
        let scope = request.scope;
        let run_id = request.run_id.clone();
        let node_id = request.node_id.clone();
        let ordinal_within_scope = request.ordinal_within_scope;
        let app_run_ordinal = request.app_run_ordinal;
        let captured_at = request.captured_at;
        let force_missing = request.force_missing;
        let checkpoint_id = checkpoint_id_for_scope_ordinal(&scope.id, ordinal_within_scope);
        let parent_checkpoint_id = (ordinal_within_scope > 0)
            .then(|| checkpoint_id_for_scope_ordinal(&scope.id, ordinal_within_scope - 1));
        let checkpoint_ref = checkpoint_ref_for_scope_ordinal(&scope.id, ordinal_within_scope);
        let previous_checkpoint_ref =
            checkpoint_ref_for_scope_ordinal(&scope.id, ordinal_within_scope.saturating_sub(1));

        let repository = if force_missing {
            RepositoryProbe::Missing
        } else {
            self.probe_repository(scope)
        };
        if repository != RepositoryProbe::Repository {
            return Ok(make_checkpoint(MakeCheckpointInput {
                id: checkpoint_id,
                scope,
                run_id,
                node_id,
                parent_checkpoint_id,
                ordinal_within_scope,
                app_run_ordinal,
                checkpoint_ref,
                status: probe_status(repository),
                files: Vec::new(),
                captured_at,
            }));
        }

        let capture_status =
            match self
                .store
                .capture_checkpoint(&super::store::CaptureCheckpointInput {
                    cwd: scope.cwd.clone().into(),
                    checkpoint_ref: checkpoint_ref.clone(),
                    previous_checkpoint_ref: (ordinal_within_scope > 0)
                        .then(|| previous_checkpoint_ref.clone()),
                }) {
                Ok(()) => CheckpointStatus::Ready,
                Err(error) if error.is_unsupported() => CheckpointStatus::Missing,
                Err(error) => {
                    log::warn!(
                        "orchestration V2 checkpoint capture failed: scope={} cause={error}",
                        scope.id
                    );
                    CheckpointStatus::Error
                }
            };
        if capture_status != CheckpointStatus::Ready {
            return Ok(make_checkpoint(MakeCheckpointInput {
                id: checkpoint_id,
                scope,
                run_id,
                node_id,
                parent_checkpoint_id,
                ordinal_within_scope,
                app_run_ordinal,
                checkpoint_ref,
                status: capture_status,
                files: Vec::new(),
                captured_at,
            }));
        }

        let previous_exists = self
            .store
            .has_checkpoint_ref(Path::new(&scope.cwd), &previous_checkpoint_ref)
            .unwrap_or_else(|error| {
                log::warn!(
                    "orchestration V2 previous checkpoint ref lookup failed: scope={} cause={error}",
                    scope.id
                );
                false
            });
        let files = if previous_exists {
            let comparison = (previous_checkpoint_ref, checkpoint_ref.clone());
            let numstat = self
                .store
                .diff_checkpoints(&DiffCheckpointsInput {
                    cwd: scope.cwd.clone().into(),
                    from_checkpoint_ref: comparison.0.clone(),
                    to_checkpoint_ref: comparison.1.clone(),
                    fallback_from_to_head: false,
                    ignore_whitespace: false,
                    format: DiffFormat::Numstat,
                    max_output_bytes: None,
                    file_paths: None,
                })
                .unwrap_or_else(|error| {
                    log::warn!(
                        "orchestration V2 checkpoint diff summary failed: scope={} cause={error}",
                        scope.id
                    );
                    String::new()
                });
            let authored_paths = self
                .store
                .list_authored_paths(&ListAuthoredPathsInput {
                    cwd: scope.cwd.clone().into(),
                    from_checkpoint_ref: comparison.0,
                    to_checkpoint_ref: comparison.1,
                })
                .unwrap_or_else(|error| {
                    log::warn!(
                        "orchestration V2 checkpoint authored paths failed: scope={} cause={error}",
                        scope.id
                    );
                    None
                });
            // A pull or rebase can change thousands of files the turn did not write.
            parse_turn_diff_files_from_numstat(&numstat)
                .into_iter()
                .filter(|file| !is_git_import(file, authored_paths.as_ref()))
                .map(|file| CheckpointFileSummary {
                    path: file.path,
                    kind: "modified".to_string(),
                    additions: file.additions,
                    deletions: file.deletions,
                })
                .collect()
        } else {
            Vec::new()
        };

        Ok(make_checkpoint(MakeCheckpointInput {
            id: checkpoint_id,
            scope,
            run_id,
            node_id,
            parent_checkpoint_id,
            ordinal_within_scope,
            app_run_ordinal,
            checkpoint_ref,
            status: CheckpointStatus::Ready,
            files,
            captured_at,
        }))
    }

    /// T3's `restore`: only a `ready` checkpoint can put files back.
    pub fn restore(
        &self,
        scope: &CheckpointScope,
        checkpoint: &Checkpoint,
    ) -> Result<(), CheckpointServiceOpError> {
        let restore_error = |cause: String| CheckpointServiceOpError::Restore {
            scope_id: scope.id.clone(),
            checkpoint_id: checkpoint.id.clone(),
            cause,
        };
        if checkpoint.status != CheckpointStatus::Ready {
            let status = match checkpoint.status {
                CheckpointStatus::Ready => "ready",
                CheckpointStatus::Missing => "missing",
                CheckpointStatus::Error => "error",
                CheckpointStatus::Stale => "stale",
            };
            return Err(restore_error(format!("Checkpoint status is {status}.")));
        }
        match self
            .store
            .restore_checkpoint(&super::store::RestoreCheckpointInput {
                cwd: scope.cwd.clone().into(),
                checkpoint_ref: checkpoint.checkpoint_ref.clone(),
                fallback_to_head: false,
            }) {
            Ok(true) => Ok(()),
            Ok(false) => Err(restore_error("Checkpoint ref is unavailable.".to_string())),
            Err(error) => Err(restore_error(error.to_string())),
        }
    }

    /// T3's `deleteStaleRefs`.
    pub fn delete_stale_refs(
        &self,
        scope: &CheckpointScope,
        checkpoints: &[Checkpoint],
    ) -> Result<(), CheckpointServiceOpError> {
        self.store
            .delete_checkpoint_refs(&super::store::DeleteCheckpointRefsInput {
                cwd: scope.cwd.clone().into(),
                checkpoint_refs: checkpoints
                    .iter()
                    .map(|checkpoint| checkpoint.checkpoint_ref.clone())
                    .collect(),
            })
            .map_err(|error| CheckpointServiceOpError::DeleteStaleRefs {
                scope_id: scope.id.clone(),
                checkpoint_ids: checkpoints
                    .iter()
                    .map(|checkpoint| checkpoint.id.clone())
                    .collect(),
                cause: error.to_string(),
            })
    }
}

fn probe_status(probe: RepositoryProbe) -> CheckpointStatus {
    match probe {
        RepositoryProbe::Repository => CheckpointStatus::Ready,
        RepositoryProbe::Missing => CheckpointStatus::Missing,
        RepositoryProbe::Error => CheckpointStatus::Error,
    }
}

fn checkpoint_id_for_scope_ordinal(
    scope_id: &CheckpointScopeId,
    ordinal_within_scope: u32,
) -> CheckpointId {
    CheckpointId::new(format!("{scope_id}:{ordinal_within_scope}"))
}

pub struct MakeCheckpointInput<'a> {
    pub id: CheckpointId,
    pub scope: &'a CheckpointScope,
    pub run_id: Option<RunId>,
    pub node_id: NodeId,
    pub parent_checkpoint_id: Option<CheckpointId>,
    pub ordinal_within_scope: u32,
    pub app_run_ordinal: Option<u32>,
    pub checkpoint_ref: CheckpointRef,
    pub status: CheckpointStatus,
    pub files: Vec<CheckpointFileSummary>,
    pub captured_at: &'a str,
}

pub fn make_checkpoint(input: MakeCheckpointInput<'_>) -> Checkpoint {
    Checkpoint {
        id: input.id,
        thread_id: input.scope.thread_id.clone(),
        scope_id: input.scope.id.clone(),
        run_id: input.run_id,
        node_id: input.node_id,
        parent_checkpoint_id: input.parent_checkpoint_id,
        ordinal_within_scope: input.ordinal_within_scope,
        app_run_ordinal: input.app_run_ordinal,
        checkpoint_ref: input.checkpoint_ref,
        status: input.status,
        files: input.files,
        files_total: None,
        captured_at: input.captured_at.to_string(),
    }
}

#[cfg(test)]
mod tests {
    // Ported from apps/server/src/orchestration-v2/CheckpointService.test.ts
    // (T3 Code v0.1.11, MIT).
    use super::*;
    use crate::checkpoint::errors::CheckpointStoreError;
    use crate::checkpoint::store::{
        CaptureCheckpointInput, DeleteCheckpointRefsInput, RestoreCheckpointInput,
    };
    use crate::contracts::CheckpointScopeKind;
    use std::path::Path;

    struct MockStore {
        repository: Result<bool, CheckpointStoreError>,
        capture: Result<(), CheckpointStoreError>,
        has_ref: bool,
    }

    impl CheckpointStore for MockStore {
        fn is_git_repository(&self, _cwd: &Path) -> Result<bool, CheckpointStoreError> {
            self.repository.clone()
        }
        fn capture_checkpoint(
            &self,
            _input: &CaptureCheckpointInput,
        ) -> Result<(), CheckpointStoreError> {
            self.capture.clone()
        }
        fn has_checkpoint_ref(
            &self,
            _cwd: &Path,
            _checkpoint_ref: &CheckpointRef,
        ) -> Result<bool, CheckpointStoreError> {
            Ok(self.has_ref)
        }
        fn restore_checkpoint(
            &self,
            _input: &RestoreCheckpointInput,
        ) -> Result<bool, CheckpointStoreError> {
            Ok(true)
        }
        fn diff_checkpoints(
            &self,
            _input: &DiffCheckpointsInput,
        ) -> Result<String, CheckpointStoreError> {
            Ok(String::new())
        }
        fn list_authored_paths(
            &self,
            _input: &ListAuthoredPathsInput,
        ) -> Result<Option<std::collections::HashSet<String>>, CheckpointStoreError> {
            Ok(None)
        }
        fn delete_checkpoint_refs(
            &self,
            _input: &DeleteCheckpointRefsInput,
        ) -> Result<(), CheckpointStoreError> {
            Ok(())
        }
    }

    fn scope(cwd: &str) -> CheckpointScope {
        CheckpointScope {
            id: CheckpointScopeId::new("checkpoint-scope:capture-failure"),
            thread_id: ThreadId::new("thread:capture-failure"),
            run_id: Some(RunId::new("run:capture-failure:1")),
            node_id: NodeId::new("node:capture-failure:1"),
            parent_scope_id: None,
            provider_thread_id: Some(ProviderThreadId::new("provider-thread:capture-failure")),
            kind: CheckpointScopeKind::RootRun,
            ordinal_within_parent: 0,
            advances_app_run_count: true,
            cwd: cwd.into(),
            created_at: "2026-07-28T00:00:00.000Z".into(),
        }
    }

    #[test]
    fn refs_are_derived_from_the_scope_and_ordinal_only() {
        let one = CheckpointScopeId::new("scope:audit");
        let two = CheckpointScopeId::new("scope:other");
        assert_eq!(
            checkpoint_ref_for_scope_ordinal(&one, 0),
            checkpoint_ref_for_scope_ordinal(&one, 0)
        );
        assert_ne!(
            checkpoint_ref_for_scope_ordinal(&one, 0),
            checkpoint_ref_for_scope_ordinal(&one, 1)
        );
        assert_ne!(
            checkpoint_ref_for_scope_ordinal(&one, 0),
            checkpoint_ref_for_scope_ordinal(&two, 0)
        );
    }

    #[test]
    fn skips_unsupported_baseline_and_turn_checkpoints_without_swallowing_capture_failures() {
        let scope = scope("/repo/ignored");
        for error in [
            CheckpointStoreError::Unsupported {
                operation: "test.capture".into(),
                detail: "ignored workspace".into(),
            },
            CheckpointStoreError::process_exit("test.capture", "git", scope.cwd.clone(), 1, "boom"),
        ] {
            let unsupported = error.is_unsupported();
            let store = MockStore {
                repository: Ok(true),
                capture: Err(error),
                has_ref: false,
            };
            let service = CheckpointService::new(&store);
            let baseline = service.capture_baseline(&scope, 0);
            let checkpoint = service
                .capture(CaptureRequest {
                    scope: &scope,
                    run_id: scope.run_id.clone(),
                    node_id: scope.node_id.clone(),
                    ordinal_within_scope: 1,
                    app_run_ordinal: Some(1),
                    captured_at: &scope.created_at,
                    force_missing: false,
                })
                .unwrap();
            if unsupported {
                assert!(baseline.is_ok());
                assert_eq!(checkpoint.status, CheckpointStatus::Missing);
                let materialized = service
                    .materialize_baseline_checkpoint(
                        &scope,
                        0,
                        scope.node_id.clone(),
                        &scope.created_at,
                        false,
                    )
                    .unwrap();
                assert_eq!(materialized.status, CheckpointStatus::Missing);
            } else {
                assert!(baseline.is_err());
                assert_eq!(checkpoint.status, CheckpointStatus::Error);
            }
            assert!(checkpoint.files.is_empty());
        }
    }

    #[test]
    fn records_a_repository_probe_failure_as_an_error_checkpoint_not_a_missing_one() {
        let scope = scope("/repo/bad-config");
        let store = MockStore {
            repository: Err(CheckpointStoreError::process_exit(
                "GitVcsDriver.detectRepository",
                "git",
                scope.cwd.clone(),
                128,
                "fatal: bad config line 1 in file .git/config",
            )),
            capture: Ok(()),
            has_ref: false,
        };
        let service = CheckpointService::new(&store);
        let baseline = service
            .materialize_baseline_checkpoint(
                &scope,
                0,
                scope.node_id.clone(),
                &scope.created_at,
                false,
            )
            .unwrap();
        let turn = service
            .capture(CaptureRequest {
                scope: &scope,
                run_id: scope.run_id.clone(),
                node_id: scope.node_id.clone(),
                ordinal_within_scope: 1,
                app_run_ordinal: Some(1),
                captured_at: &scope.created_at,
                force_missing: false,
            })
            .unwrap();
        assert_eq!(baseline.status, CheckpointStatus::Error);
        assert_eq!(turn.status, CheckpointStatus::Error);
    }
}
