// Ported from apps/server/src/checkpointing/CheckpointDiffQuery.ts
// (T3 Code v0.1.11, MIT).
//!
//! Per-turn and full-thread diffs. The caller supplies the checkpoint
//! projection and the Git store; this module only decides which refs a turn
//! spans and how the shared output budget is split.

use std::collections::HashSet;

use crate::contracts::{Checkpoint, CheckpointRef, RunId, ThreadId};

use super::context::CheckpointContextProvider;
use super::diffs::{TurnDiffFileSummary, is_git_import, parse_turn_diff_files_from_numstat};
use super::errors::{CheckpointDiffOperation, CheckpointServiceError, CheckpointSide};
use super::service::checkpoint_ref_for_scope_ordinal;
use super::store::{
    CHECKPOINT_DIFF_MAX_OUTPUT_BYTES, CheckpointStore, DiffCheckpointsInput, DiffFormat,
    ListAuthoredPathsInput,
};

/// T3's `CHECKPOINT_DIFF_TRUNCATED_MARKER`.
pub const CHECKPOINT_DIFF_TRUNCATED_MARKER: &str = "\n\n[truncated]";
/// Windows limits a command line to 32,767 characters. Leave room for the rest
/// of git's arguments.
const MAX_PATHSPEC_CHARS: usize = 24_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetTurnDiffInput {
    pub thread_id: ThreadId,
    pub from_turn_count: u32,
    pub to_turn_count: u32,
    pub ignore_whitespace: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetFullThreadDiffInput {
    pub thread_id: ThreadId,
    pub to_turn_count: u32,
    pub ignore_whitespace: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnDiffResult {
    pub thread_id: ThreadId,
    pub from_turn_count: u32,
    pub to_turn_count: u32,
    pub diff: String,
}

impl TurnDiffResult {
    pub fn empty(thread_id: ThreadId, from_turn_count: u32, to_turn_count: u32) -> Self {
        Self {
            thread_id,
            from_turn_count,
            to_turn_count,
            diff: String::new(),
        }
    }
}

/// Batches paths so one `git diff` command line stays under the Windows limit.
fn batch_file_paths(files: &[TurnDiffFileSummary]) -> Vec<Option<Vec<String>>> {
    let mut batches: Vec<Option<Vec<String>>> = Vec::new();
    let mut batch: Vec<String> = Vec::new();
    let mut length = 0usize;
    for file in files {
        let paths: Vec<String> = match &file.previous_path {
            Some(previous) => vec![previous.clone(), file.path.clone()],
            None => vec![file.path.clone()],
        };
        let size: usize = paths.iter().map(|path| path.chars().count() + 16).sum();
        if !batch.is_empty() && length + size > MAX_PATHSPEC_CHARS {
            batches.push(Some(std::mem::take(&mut batch)));
            length = 0;
        }
        batch.extend(paths);
        length += size;
    }
    if !batch.is_empty() {
        batches.push(Some(batch));
    }
    batches
}

/// T3's `getTurnDiff`.
pub fn get_turn_diff<S, C>(
    store: &S,
    contexts: &C,
    input: &GetTurnDiffInput,
) -> Result<TurnDiffResult, CheckpointServiceError>
where
    S: CheckpointStore,
    C: CheckpointContextProvider,
{
    let operation = CheckpointDiffOperation::GetTurnDiff;
    let ignore_whitespace = input.ignore_whitespace.unwrap_or(true);

    if input.from_turn_count == input.to_turn_count {
        return Ok(TurnDiffResult::empty(
            input.thread_id.clone(),
            input.from_turn_count,
            input.to_turn_count,
        ));
    }

    let context = contexts
        .get_checkpoint_context(&input.thread_id)
        .map_err(|_| CheckpointServiceError::ThreadNotFound {
            operation,
            thread_id: input.thread_id.clone(),
        })?;
    let completed_run_ids: HashSet<RunId> = context
        .runs
        .iter()
        .filter(|run| run.status == crate::contracts::RunStatus::Completed)
        .map(|run| run.id.clone())
        .collect();
    let ready_checkpoints: Vec<&Checkpoint> = context
        .checkpoints
        .iter()
        .filter(|checkpoint| {
            checkpoint.status == crate::contracts::CheckpointStatus::Ready
                && checkpoint.app_run_ordinal.is_some()
                && checkpoint
                    .run_id
                    .as_ref()
                    .is_some_and(|run| completed_run_ids.contains(run))
        })
        .collect();
    let max_turn_count = ready_checkpoints
        .iter()
        .map(|checkpoint| checkpoint.app_run_ordinal.unwrap_or(0))
        .max()
        .unwrap_or(0);
    if input.to_turn_count > max_turn_count {
        return Err(CheckpointServiceError::TurnRangeUnavailable {
            operation,
            thread_id: input.thread_id.clone(),
            requested_turn_count: input.to_turn_count,
            available_turn_count: max_turn_count,
        });
    }

    let to_checkpoint = ready_checkpoints
        .iter()
        .find(|checkpoint| checkpoint.app_run_ordinal == Some(input.to_turn_count))
        .copied()
        .ok_or_else(|| CheckpointServiceError::RefUnavailable {
            operation,
            thread_id: input.thread_id.clone(),
            turn_count: input.to_turn_count,
            checkpoint: CheckpointSide::To,
        })?;

    let to_scope = context
        .checkpoint_scopes
        .iter()
        .find(|scope| scope.id == to_checkpoint.scope_id)
        .ok_or_else(|| CheckpointServiceError::WorkspacePathMissing {
            operation,
            thread_id: input.thread_id.clone(),
        })?;

    // Capture compares against the preceding scope ordinal, even when that
    // checkpoint belongs to a stopped run or has no run.
    let parent_checkpoint = if input.from_turn_count == input.to_turn_count - 1 {
        context.checkpoints.iter().find(|checkpoint| {
            checkpoint.status == crate::contracts::CheckpointStatus::Ready
                && checkpoint.scope_id == to_checkpoint.scope_id
                && checkpoint.ordinal_within_scope + 1 == to_checkpoint.ordinal_within_scope
        })
    } else {
        None
    };

    let first_scope = if input.from_turn_count == 0 {
        context
            .checkpoint_scopes
            .iter()
            .find(|scope| scope.kind == crate::contracts::CheckpointScopeKind::RootRun)
    } else {
        None
    };
    let from_checkpoint_ref = if input.from_turn_count == 0 {
        first_scope.map(|scope| checkpoint_ref_for_scope_ordinal(&scope.id, 0))
    } else {
        parent_checkpoint
            .or_else(|| {
                ready_checkpoints
                    .iter()
                    .find(|checkpoint| checkpoint.app_run_ordinal == Some(input.from_turn_count))
                    .copied()
            })
            .map(|checkpoint| checkpoint.checkpoint_ref.clone())
    };
    let Some(from_checkpoint_ref) = from_checkpoint_ref else {
        return Err(CheckpointServiceError::RefUnavailable {
            operation,
            thread_id: input.thread_id.clone(),
            turn_count: input.from_turn_count,
            checkpoint: CheckpointSide::From,
        });
    };

    let comparison = Comparison {
        cwd: to_scope.cwd.clone(),
        from_checkpoint_ref,
        to_checkpoint_ref: to_checkpoint.checkpoint_ref.clone(),
    };
    let authored_paths = store
        .list_authored_paths(&ListAuthoredPathsInput {
            cwd: comparison.cwd.clone().into(),
            from_checkpoint_ref: comparison.from_checkpoint_ref.clone(),
            to_checkpoint_ref: comparison.to_checkpoint_ref.clone(),
        })
        .unwrap_or(None);
    let files: Vec<TurnDiffFileSummary> = match authored_paths.as_ref() {
        None => Vec::new(),
        Some(_) => store
            .diff_checkpoints(&DiffCheckpointsInput {
                cwd: comparison.cwd.clone().into(),
                from_checkpoint_ref: comparison.from_checkpoint_ref.clone(),
                to_checkpoint_ref: comparison.to_checkpoint_ref.clone(),
                fallback_from_to_head: false,
                ignore_whitespace,
                format: DiffFormat::Numstat,
                max_output_bytes: None,
                file_paths: None,
            })
            .map(|numstat| parse_turn_diff_files_from_numstat(&numstat))
            .unwrap_or_default(),
    };
    let retained_files: Vec<TurnDiffFileSummary> = files
        .iter()
        .filter(|file| !is_git_import(file, authored_paths.as_ref()))
        .cloned()
        .collect();
    // Select retained paths before generating a patch, so imported bulk cannot
    // exhaust its output limit.
    let path_batches = if retained_files.len() == files.len() {
        vec![None]
    } else {
        batch_file_paths(&retained_files)
    };
    let mut diff = String::new();
    let mut remaining_bytes = CHECKPOINT_DIFF_MAX_OUTPUT_BYTES;
    for file_paths in path_batches {
        let patch = store
            .diff_checkpoints(&DiffCheckpointsInput {
                cwd: comparison.cwd.clone().into(),
                from_checkpoint_ref: comparison.from_checkpoint_ref.clone(),
                to_checkpoint_ref: comparison.to_checkpoint_ref.clone(),
                fallback_from_to_head: false,
                ignore_whitespace,
                format: DiffFormat::Patch,
                // One extra byte distinguishes an exact fit from a truncated patch.
                max_output_bytes: Some(remaining_bytes + 1),
                file_paths,
            })
            .map_err(CheckpointServiceError::Store)?;
        let patch_bytes = patch.len();
        diff.push_str(&patch);
        if patch_bytes > remaining_bytes {
            // Reserve marker space only on overflow. Drop an incomplete final
            // UTF-8 character at the byte boundary rather than replacing it.
            let limit = CHECKPOINT_DIFF_MAX_OUTPUT_BYTES
                .saturating_sub(CHECKPOINT_DIFF_TRUNCATED_MARKER.len());
            diff = valid_prefix(&diff, limit).to_string();
            diff.push_str(CHECKPOINT_DIFF_TRUNCATED_MARKER);
            break;
        }
        remaining_bytes -= patch_bytes;
    }

    Ok(TurnDiffResult {
        thread_id: input.thread_id.clone(),
        from_turn_count: input.from_turn_count,
        to_turn_count: input.to_turn_count,
        diff,
    })
}

/// T3's `getFullThreadDiff`.
pub fn get_full_thread_diff<S, C>(
    store: &S,
    contexts: &C,
    input: &GetFullThreadDiffInput,
) -> Result<TurnDiffResult, CheckpointServiceError>
where
    S: CheckpointStore,
    C: CheckpointContextProvider,
{
    if input.to_turn_count == 0 {
        return Ok(TurnDiffResult::empty(input.thread_id.clone(), 0, 0));
    }
    get_turn_diff(
        store,
        contexts,
        &GetTurnDiffInput {
            thread_id: input.thread_id.clone(),
            from_turn_count: 0,
            to_turn_count: input.to_turn_count,
            ignore_whitespace: input.ignore_whitespace,
        },
    )
}

/// The longest byte prefix of `text` that is valid UTF-8, at most `limit`.
fn valid_prefix(text: &str, limit: usize) -> &str {
    let mut end = limit.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

struct Comparison {
    cwd: String,
    from_checkpoint_ref: CheckpointRef,
    to_checkpoint_ref: CheckpointRef,
}

/// The marker the query appends when the shared budget runs out.
pub fn truncated_marker() -> &'static str {
    CHECKPOINT_DIFF_TRUNCATED_MARKER
}

#[cfg(test)]
mod tests {
    // Ported from apps/server/src/checkpointing/CheckpointDiffQuery.test.ts
    // (T3 Code v0.1.11, MIT).
    use std::cell::RefCell;
    use std::collections::HashSet;

    use super::*;
    use crate::checkpoint::CheckpointRunRef;
    use crate::checkpoint::context::{CheckpointContext, CheckpointContextError};
    use crate::checkpoint::errors::CheckpointStoreError;
    use crate::checkpoint::service::checkpoint_ref_for_scope_ordinal;
    use crate::checkpoint::store::{
        CaptureCheckpointInput, DeleteCheckpointRefsInput, ListAuthoredPathsInput,
        RestoreCheckpointInput,
    };
    use crate::contracts::{
        CheckpointId, CheckpointScope, CheckpointScopeId, CheckpointScopeKind, CheckpointStatus,
        NodeId, RunId, RunStatus,
    };

    struct MockContext {
        context: CheckpointContext,
    }

    impl CheckpointContextProvider for MockContext {
        fn get_checkpoint_context(
            &self,
            _thread_id: &ThreadId,
        ) -> Result<CheckpointContext, CheckpointContextError> {
            Ok(self.context.clone())
        }
    }

    type DiffFn = Box<dyn Fn(&DiffCheckpointsInput) -> Result<String, CheckpointStoreError>>;

    struct MockStore {
        diff: DiffFn,
        authored: Option<HashSet<String>>,
        calls: RefCell<Vec<(CheckpointRef, CheckpointRef)>>,
    }

    impl MockStore {
        fn new(
            diff: impl Fn(&DiffCheckpointsInput) -> Result<String, CheckpointStoreError> + 'static,
        ) -> Self {
            Self {
                diff: Box::new(diff),
                authored: None,
                calls: RefCell::new(Vec::new()),
            }
        }

        fn with_authored(mut self, authored: HashSet<String>) -> Self {
            self.authored = Some(authored);
            self
        }
    }

    impl CheckpointStore for MockStore {
        fn is_git_repository(&self, _cwd: &std::path::Path) -> Result<bool, CheckpointStoreError> {
            Ok(true)
        }
        fn capture_checkpoint(
            &self,
            _input: &CaptureCheckpointInput,
        ) -> Result<(), CheckpointStoreError> {
            Ok(())
        }
        fn has_checkpoint_ref(
            &self,
            _cwd: &std::path::Path,
            _checkpoint_ref: &CheckpointRef,
        ) -> Result<bool, CheckpointStoreError> {
            Ok(true)
        }
        fn restore_checkpoint(
            &self,
            _input: &RestoreCheckpointInput,
        ) -> Result<bool, CheckpointStoreError> {
            Ok(true)
        }
        fn diff_checkpoints(
            &self,
            input: &DiffCheckpointsInput,
        ) -> Result<String, CheckpointStoreError> {
            if input.format == DiffFormat::Patch {
                self.calls.borrow_mut().push((
                    input.from_checkpoint_ref.clone(),
                    input.to_checkpoint_ref.clone(),
                ));
            }
            (self.diff)(input)
        }
        fn list_authored_paths(
            &self,
            _input: &ListAuthoredPathsInput,
        ) -> Result<Option<HashSet<String>>, CheckpointStoreError> {
            Ok(self.authored.clone())
        }
        fn delete_checkpoint_refs(
            &self,
            _input: &DeleteCheckpointRefsInput,
        ) -> Result<(), CheckpointStoreError> {
            Ok(())
        }
    }

    fn scope(
        id: &CheckpointScopeId,
        run_id: Option<RunId>,
        kind: CheckpointScopeKind,
        cwd: &str,
    ) -> CheckpointScope {
        CheckpointScope {
            id: id.clone(),
            thread_id: ThreadId::new("thread:checkpoint-diff-v2"),
            run_id,
            node_id: NodeId::new("node"),
            parent_scope_id: None,
            provider_thread_id: None,
            kind,
            ordinal_within_parent: 0,
            advances_app_run_count: true,
            cwd: cwd.into(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn checkpoint(
        scope_id: &CheckpointScopeId,
        run_id: Option<RunId>,
        ordinal_within_scope: u32,
        app_run_ordinal: Option<u32>,
        checkpoint_ref: CheckpointRef,
        status: CheckpointStatus,
    ) -> Checkpoint {
        Checkpoint {
            id: CheckpointId::new(format!(
                "{scope_id}:{ordinal_within_scope}:{}",
                checkpoint_ref.as_str()
            )),
            thread_id: ThreadId::new("thread:checkpoint-diff-v2"),
            scope_id: scope_id.clone(),
            run_id,
            node_id: NodeId::new("node"),
            parent_checkpoint_id: None,
            ordinal_within_scope,
            app_run_ordinal,
            checkpoint_ref,
            status,
            files: Vec::new(),
            files_total: None,
            captured_at: "2026-01-01T00:00:00.000Z".into(),
        }
    }

    fn make_projection() -> CheckpointContext {
        let first_run = RunId::new("run:checkpoint-diff-v2:1");
        let second_run = RunId::new("run:checkpoint-diff-v2:2");
        let first_scope = CheckpointScopeId::new("scope:checkpoint-diff-v2:1");
        let second_scope = CheckpointScopeId::new("scope:checkpoint-diff-v2:2");
        let second_ref = CheckpointRef::new("refs/t3/test/second");
        CheckpointContext {
            runs: vec![
                CheckpointRunRef {
                    id: first_run.clone(),
                    ordinal: 1,
                    status: RunStatus::Completed,
                },
                CheckpointRunRef {
                    id: second_run.clone(),
                    ordinal: 2,
                    status: RunStatus::Completed,
                },
            ],
            checkpoint_scopes: vec![
                scope(
                    &first_scope,
                    Some(first_run),
                    CheckpointScopeKind::RootRun,
                    "/repo",
                ),
                scope(
                    &second_scope,
                    Some(second_run.clone()),
                    CheckpointScopeKind::RootRun,
                    "/repo",
                ),
            ],
            checkpoints: vec![checkpoint(
                &second_scope,
                Some(second_run),
                1,
                Some(2),
                second_ref,
                CheckpointStatus::Ready,
            )],
        }
    }

    #[test]
    fn excludes_ready_checkpoints_from_rolled_back_runs() {
        let mut context = make_projection();
        context.runs[1].status = RunStatus::RolledBack;
        let first_scope = &context.checkpoint_scopes[0].id;
        let first_run = context.runs[0].id.clone();
        let mut first = context.checkpoints[0].clone();
        first.scope_id = first_scope.clone();
        first.run_id = Some(first_run);
        first.app_run_ordinal = Some(1);
        first.checkpoint_ref = CheckpointRef::new("refs/t3/test/first");
        context.checkpoints.insert(0, first);
        let contexts = MockContext { context };
        let store = MockStore::new(|_| Ok(String::new()));
        let error = get_turn_diff(
            &store,
            &contexts,
            &GetTurnDiffInput {
                thread_id: ThreadId::new("thread:checkpoint-diff-v2"),
                from_turn_count: 0,
                to_turn_count: 2,
                ignore_whitespace: None,
            },
        )
        .unwrap_err();
        assert_eq!(
            error,
            CheckpointServiceError::TurnRangeUnavailable {
                operation: CheckpointDiffOperation::GetTurnDiff,
                thread_id: ThreadId::new("thread:checkpoint-diff-v2"),
                requested_turn_count: 2,
                available_turn_count: 1,
            }
        );
    }

    fn make_projection_after_stopped_run(stopped_run_checkpoint: bool) -> CheckpointContext {
        let first_run = RunId::new("run:checkpoint-diff-v2:1");
        let second_run = RunId::new("run:checkpoint-diff-v2:2");
        let third_run = RunId::new("run:checkpoint-diff-v2:3");
        let first_scope = CheckpointScopeId::new("scope:checkpoint-diff-v2:1");
        let second_scope = CheckpointScopeId::new("scope:checkpoint-diff-v2:2");
        let ord = |ordinal: u32| CheckpointRef::new(format!("refs/t3/test/ordinal/{ordinal}"));
        let checkpoint = |ordinal: u32, run_id: Option<RunId>| {
            checkpoint(
                &first_scope,
                run_id,
                ordinal,
                if ordinal == 0 { None } else { Some(ordinal) },
                ord(ordinal),
                CheckpointStatus::Ready,
            )
        };
        let mut tool = checkpoint(2, None);
        tool.scope_id = second_scope.clone();
        tool.checkpoint_ref = CheckpointRef::new("refs/t3/test/tool/2");
        CheckpointContext {
            runs: vec![
                CheckpointRunRef {
                    id: first_run.clone(),
                    ordinal: 1,
                    status: RunStatus::Completed,
                },
                CheckpointRunRef {
                    id: second_run.clone(),
                    ordinal: 2,
                    status: RunStatus::Interrupted,
                },
                CheckpointRunRef {
                    id: third_run.clone(),
                    ordinal: 3,
                    status: RunStatus::Completed,
                },
            ],
            checkpoint_scopes: vec![
                scope(
                    &first_scope,
                    Some(third_run.clone()),
                    CheckpointScopeKind::RootRun,
                    "/repo",
                ),
                scope(
                    &second_scope,
                    Some(second_run.clone()),
                    CheckpointScopeKind::Tool,
                    "/repo",
                ),
            ],
            checkpoints: vec![
                tool,
                checkpoint(0, None),
                checkpoint(1, Some(first_run)),
                checkpoint(
                    2,
                    if stopped_run_checkpoint {
                        Some(second_run)
                    } else {
                        None
                    },
                ),
                checkpoint(3, Some(third_run)),
            ],
        }
    }

    #[test]
    fn diffs_the_turn_after_a_stopped_run_from_its_parent_checkpoint() {
        for stopped_run_checkpoint in [false, true] {
            let contexts = MockContext {
                context: make_projection_after_stopped_run(stopped_run_checkpoint),
            };
            let store = MockStore::new(|_| Ok("diff --git a/file b/file".to_string()));
            let thread_id = ThreadId::new("thread:checkpoint-diff-v2");
            let result = get_turn_diff(
                &store,
                &contexts,
                &GetTurnDiffInput {
                    thread_id: thread_id.clone(),
                    from_turn_count: 2,
                    to_turn_count: 3,
                    ignore_whitespace: None,
                },
            )
            .unwrap();
            assert_eq!(result.diff, "diff --git a/file b/file");
            get_turn_diff(
                &store,
                &contexts,
                &GetTurnDiffInput {
                    thread_id: thread_id.clone(),
                    from_turn_count: 1,
                    to_turn_count: 3,
                    ignore_whitespace: None,
                },
            )
            .unwrap();
            get_full_thread_diff(
                &store,
                &contexts,
                &GetFullThreadDiffInput {
                    thread_id: thread_id.clone(),
                    to_turn_count: 3,
                    ignore_whitespace: None,
                },
            )
            .unwrap();
            let first_scope = CheckpointScopeId::new("scope:checkpoint-diff-v2:1");
            assert_eq!(
                *store.calls.borrow(),
                vec![
                    (
                        CheckpointRef::new("refs/t3/test/ordinal/2"),
                        CheckpointRef::new("refs/t3/test/ordinal/3"),
                    ),
                    (
                        CheckpointRef::new("refs/t3/test/ordinal/1"),
                        CheckpointRef::new("refs/t3/test/ordinal/3"),
                    ),
                    (
                        checkpoint_ref_for_scope_ordinal(&first_scope, 0),
                        CheckpointRef::new("refs/t3/test/ordinal/3"),
                    ),
                ]
            );
            let error = get_turn_diff(
                &store,
                &contexts,
                &GetTurnDiffInput {
                    thread_id,
                    from_turn_count: 1,
                    to_turn_count: 2,
                    ignore_whitespace: None,
                },
            )
            .unwrap_err();
            assert_eq!(
                error,
                CheckpointServiceError::RefUnavailable {
                    operation: CheckpointDiffOperation::GetTurnDiff,
                    thread_id: ThreadId::new("thread:checkpoint-diff-v2"),
                    turn_count: 2,
                    checkpoint: CheckpointSide::To,
                }
            );
        }
    }

    #[test]
    fn truncates_a_storage_cut_utf8_character_without_replacement_text() {
        let prefix = "x".repeat(9_999_985);
        let contexts = MockContext {
            context: make_projection(),
        };
        let prefix_for_diff = prefix.clone();
        let store = MockStore::new(move |input| {
            let bytes = format!("{prefix_for_diff}😀{}\n😀tail", "x".repeat(9)).into_bytes();
            let limit = input
                .max_output_bytes
                .unwrap_or(bytes.len())
                .min(bytes.len());
            Ok(String::from_utf8_lossy(&bytes[..limit]).into_owned())
        });
        let result = get_turn_diff(
            &store,
            &contexts,
            &GetTurnDiffInput {
                thread_id: ThreadId::new("thread:checkpoint-diff-v2"),
                from_turn_count: 0,
                to_turn_count: 2,
                ignore_whitespace: None,
            },
        )
        .unwrap();
        assert!(result.diff.len() <= CHECKPOINT_DIFF_MAX_OUTPUT_BYTES);
        assert_eq!(
            result.diff,
            format!("{prefix}{CHECKPOINT_DIFF_TRUNCATED_MARKER}")
        );
    }

    #[test]
    fn preserves_an_exact_fit_filtered_patch_across_batches_without_a_marker() {
        let own_paths: Vec<String> = (0..1_000)
            .map(|index| format!("src/module-{index}/file.ts"))
            .collect();
        let patches = ["é".repeat(3_000_000), "x".repeat(4_000_000)];
        let contexts = MockContext {
            context: make_projection(),
        };
        let authored: HashSet<String> = own_paths.iter().cloned().collect();
        let own_for_diff = own_paths.clone();
        let upstream = "upstream.ts".to_string();
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let reads_for_diff = reads.clone();
        let patches_for_diff = patches.clone();
        let store = MockStore::new(move |input| {
            if input.format == DiffFormat::Numstat {
                let mut output = String::new();
                for path in own_for_diff.iter().chain(std::iter::once(&upstream)) {
                    output.push_str(&format!("1\t0\t{path}\0"));
                }
                Ok(output)
            } else {
                let index = reads_for_diff.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(patches_for_diff.get(index).cloned().unwrap_or_default())
            }
        })
        .with_authored(authored);
        let result = get_full_thread_diff(
            &store,
            &contexts,
            &GetFullThreadDiffInput {
                thread_id: ThreadId::new("thread:checkpoint-diff-v2"),
                to_turn_count: 2,
                ignore_whitespace: None,
            },
        )
        .unwrap();
        assert_eq!(result.diff, patches.concat());
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}
