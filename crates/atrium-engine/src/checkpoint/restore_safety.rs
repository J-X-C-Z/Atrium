// Ported from apps/server/src/orchestration-v2/CheckpointRestoreSafety.ts
// (T3 Code v0.1.11, MIT).
//!
//! A checkpoint restores a whole checkout. Before rewriting files, decide
//! whether another thread's work could share that checkout. The answer is
//! checked at admission and again at rollback because a workspace can become
//! shared in between.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::contracts::{
    AppThreadLineage, CreationSource, ForkSource, NodeId, ProjectId, ProviderInstanceId,
    ProviderThreadId, RelationshipToParent, RunId, RunStatus, ThreadId,
};

use super::context::{CheckpointContext, CheckpointContextError};

pub const UNFINISHED_RUN_STATUSES: [RunStatus; 5] = [
    RunStatus::Queued,
    RunStatus::Preparing,
    RunStatus::Starting,
    RunStatus::Running,
    RunStatus::Waiting,
];

pub const SHARED_WORKSPACE_RESTORE_MESSAGE: &str = "File restore requires an isolated worktree. This workspace may contain changes from another thread or a side chat. Rewind the conversation without restoring files instead, or delete side chats you no longer need.";

const LIVE_SHELL_STATUSES: [&str; 5] = ["queued", "preparing", "starting", "running", "waiting"];

/// The thread whose files are about to be restored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreThread {
    pub id: ThreadId,
    pub worktree_path: Option<String>,
}

/// The shell fields restore safety reads.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadShell {
    pub id: ThreadId,
    pub project_id: ProjectId,
    pub deleted_at: Option<String>,
    pub worktree_path: Option<String>,
    pub active_run_id: Option<RunId>,
    pub status: String,
    pub activity_run_status: Option<String>,
    pub pending_runtime_request: Option<serde_json::Value>,
    pub pending_background_tasks: Vec<serde_json::Value>,
    pub lineage: AppThreadLineage,
    pub forked_from: Option<ForkSource>,
    pub creation_source: CreationSource,
}

impl ThreadShell {
    pub fn is_provider_native_subagent(&self) -> bool {
        self.lineage.relationship_to_parent == Some(RelationshipToParent::Subagent)
            && self.creation_source == CreationSource::Provider
    }

    /// A subagent child that can still need the user or hold its parent.
    pub fn is_live_subagent_shell(&self) -> bool {
        self.activity_run_status.is_some()
            || LIVE_SHELL_STATUSES.contains(&self.status.as_str())
            || self.pending_runtime_request.is_some()
            || !self.pending_background_tasks.is_empty()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShellSnapshot {
    pub threads: Vec<ThreadShell>,
    pub archived_threads: Vec<ThreadShell>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderSessionRef {
    pub cwd: String,
    pub status: String,
    pub supports_multiple_provider_threads_per_session: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThreadProviderContext {
    pub provider_threads: Vec<ProviderThreadRef>,
    pub provider_sessions: Vec<ProviderSessionRef>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderThreadRef {
    pub provider_instance_id: ProviderInstanceId,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub status: String,
    pub pending_background_tasks: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnItemRef {
    pub item_type: String,
    pub status: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentRef {
    pub id: NodeId,
    pub provider_instance_id: ProviderInstanceId,
    pub child_thread_id: Option<ThreadId>,
    pub origin: String,
    pub status: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadRecordsFilter {
    pub turn_item_types: Vec<String>,
    pub turn_item_statuses: Vec<String>,
    pub subagent_ids: Vec<NodeId>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThreadRecords {
    pub provider_threads: Vec<ProviderThreadRef>,
    pub turn_items: Vec<TurnItemRef>,
    pub subagents: Vec<SubagentRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum RestoreSafetyError {
    #[error("could not read the workspace projection: {0}")]
    Projection(String),
    #[error("could not resolve {path}: {cause}")]
    Resolve { path: String, cause: String },
}

/// The projections restore safety consults.
pub trait RestoreSafetyContext {
    fn shell_snapshot(&self) -> Result<ShellSnapshot, RestoreSafetyError>;
    fn checkpoint_context(
        &self,
        thread_id: &ThreadId,
    ) -> Result<CheckpointContext, CheckpointContextError>;
    fn thread_provider_context(
        &self,
        thread_id: &ThreadId,
    ) -> Result<ThreadProviderContext, RestoreSafetyError>;
    fn thread_records(
        &self,
        thread_id: &ThreadId,
        filter: &ThreadRecordsFilter,
    ) -> Result<ThreadRecords, RestoreSafetyError>;
    fn project_workspace_root(
        &self,
        project_id: &ProjectId,
        include_deleted: bool,
    ) -> Option<String>;
}

/// T3's `isCheckpointRestoreIsolated`.
pub fn is_checkpoint_restore_isolated(
    thread: &RestoreThread,
    scope_cwd: &str,
    context: &impl RestoreSafetyContext,
) -> Result<bool, RestoreSafetyError> {
    let worktree_path = thread.worktree_path.as_deref();
    let mut shared = worktree_path.is_none();
    let Some(worktree_path) = worktree_path else {
        return Ok(!shared);
    };

    // Storage cleanup may have removed the worktree; rollback recreates it.
    let cwd = resolve_or_absolute(Path::new(scope_cwd))?;
    let worktree_cwd = resolve_or_absolute(Path::new(worktree_path))?;
    shared = cwd != worktree_cwd;
    if shared {
        return Ok(false);
    }

    let shell = context.shell_snapshot()?;
    let threads: Vec<ThreadShell> = shell
        .threads
        .into_iter()
        .chain(shell.archived_threads)
        .collect();
    let threads_by_id: HashMap<ThreadId, ThreadShell> = threads
        .iter()
        .cloned()
        .map(|thread| (thread.id.clone(), thread))
        .collect();

    let mut ancestor_providers: HashMap<ThreadId, Vec<ProviderThreadRef>> = HashMap::new();
    let mut checked_paths: HashSet<String> = HashSet::new();

    for other_thread in &threads {
        if other_thread.id == thread.id || other_thread.deleted_at.is_some() {
            continue;
        }
        // An idle side chat that only answered questions never touched the
        // shared worktree.
        if other_thread.lineage.side_chat == Some(true)
            && other_thread.active_run_id.is_none()
            && other_thread.worktree_path.as_deref() == Some(worktree_path)
        {
            let records = context.thread_records(
                &other_thread.id,
                &ThreadRecordsFilter {
                    turn_item_types: vec![
                        "command_execution".into(),
                        "file_change".into(),
                        "dynamic_tool".into(),
                    ],
                    ..Default::default()
                },
            )?;
            if records.turn_items.is_empty() {
                continue;
            }
        }
        let other = context
            .checkpoint_context(&other_thread.id)
            .map_err(|error| RestoreSafetyError::Projection(error.to_string()))?;
        let native_subagent = other_thread.is_provider_native_subagent();
        let ancestors = subagent_ancestors(thread, other_thread, &threads_by_id);
        // Native children can be runless; their completion lives on the
        // parent's task record.
        if other_thread.worktree_path.as_deref() == Some(worktree_path)
            && (other_thread.status == "completed"
                || (native_subagent && other_thread.status == "idle" && other.runs.is_empty()))
            && other_thread.active_run_id.is_none()
            && !other_thread.is_live_subagent_shell()
            && ancestors.is_some()
            && !other
                .runs
                .iter()
                .any(|run| UNFINISHED_RUN_STATUSES.contains(&run.status))
            && other_thread.lineage.parent_thread_id.is_some()
            && matches!(other_thread.forked_from, Some(ForkSource::Node { .. }))
        {
            let Some(ForkSource::Node {
                node_id: task_node_id,
            }) = other_thread.forked_from.clone()
            else {
                unreachable!("checked above");
            };
            let parent_id = other_thread
                .lineage
                .parent_thread_id
                .clone()
                .expect("checked above");
            let SubagentTask { task } =
                find_task(context, &parent_id, &task_node_id, &other_thread.id)?;
            if task.as_ref().is_some_and(|task| {
                task.status == "completed"
                    && task.origin
                        == if native_subagent {
                            "provider_native"
                        } else {
                            "app_owned"
                        }
            }) {
                if !native_subagent {
                    continue;
                }
                // Runless shells omit background work; inspect the raw records
                // before restoring files.
                let records = context.thread_records(
                    &other_thread.id,
                    &ThreadRecordsFilter {
                        turn_item_types: vec![
                            "command_execution".into(),
                            "file_change".into(),
                            "dynamic_tool".into(),
                            "subagent".into(),
                        ],
                        turn_item_statuses: vec![
                            "pending".into(),
                            "running".into(),
                            "waiting".into(),
                        ],
                        ..Default::default()
                    },
                )?;
                let has_provider_activity = records.provider_threads.iter().any(|provider| {
                    provider.status == "active" || !provider.pending_background_tasks.is_empty()
                });
                if records.turn_items.is_empty() && !has_provider_activity {
                    // Claude can keep a child's unattributed background commands
                    // on an ancestor session.
                    let mut pending_ancestor_work = false;
                    for ancestor_id in ancestors.iter().flatten() {
                        let providers = if let Some(providers) = ancestor_providers.get(ancestor_id)
                        {
                            providers.clone()
                        } else {
                            let providers = context
                                .thread_records(ancestor_id, &ThreadRecordsFilter::default())?
                                .provider_threads;
                            ancestor_providers.insert(ancestor_id.clone(), providers.clone());
                            providers
                        };
                        if providers.iter().any(|provider| {
                            Some(&provider.provider_instance_id)
                                == task.as_ref().map(|task| &task.provider_instance_id)
                                && !provider.pending_background_tasks.is_empty()
                        }) {
                            pending_ancestor_work = true;
                            break;
                        }
                    }
                    if !pending_ancestor_work {
                        continue;
                    }
                }
            }
        }

        let provider_context = context.thread_provider_context(&other_thread.id)?;
        let mut paths: Vec<String> = Vec::new();
        if let Some(path) = other_thread.worktree_path.clone() {
            paths.push(path);
        }
        paths.extend(
            other
                .checkpoint_scopes
                .iter()
                .map(|scope| scope.cwd.clone()),
        );
        // Errored sessions can still stream. Shared sessions retain another
        // thread's opening cwd.
        paths.extend(
            provider_context
                .provider_sessions
                .iter()
                .filter(|session| {
                    session.status != "stopped"
                        && !session.supports_multiple_provider_threads_per_session
                })
                .map(|session| session.cwd.clone()),
        );
        if other_thread.worktree_path.is_none() {
            let Some(project_root) = context.project_workspace_root(&other_thread.project_id, true)
            else {
                return Ok(false);
            };
            paths.push(project_root);
        }
        for candidate in paths {
            if !checked_paths.insert(candidate.clone()) {
                continue;
            }
            let Some(other_cwd) = resolve_existing(Path::new(&candidate))? else {
                continue;
            };
            if contains(&cwd, &other_cwd) || contains(&other_cwd, &cwd) {
                shared = true;
                break;
            }
        }
        if shared {
            break;
        }
    }
    Ok(!shared)
}

#[derive(Clone, Debug)]
struct SubagentTask {
    task: Option<SubagentRef>,
}

fn find_task(
    context: &impl RestoreSafetyContext,
    parent_id: &ThreadId,
    task_node_id: &NodeId,
    child_id: &ThreadId,
) -> Result<SubagentTask, RestoreSafetyError> {
    let records = context.thread_records(
        parent_id,
        &ThreadRecordsFilter {
            subagent_ids: vec![task_node_id.clone()],
            ..Default::default()
        },
    )?;
    Ok(SubagentTask {
        task: records.subagents.into_iter().find(|candidate| {
            &candidate.id == task_node_id && candidate.child_thread_id.as_ref() == Some(child_id)
        }),
    })
}

fn subagent_ancestors(
    thread: &RestoreThread,
    candidate: &ThreadShell,
    threads_by_id: &HashMap<ThreadId, ThreadShell>,
) -> Option<Vec<ThreadId>> {
    let mut ancestors = Vec::new();
    let mut seen = HashSet::new();
    let mut current = Some(candidate.clone());
    while let Some(node) = current {
        if !seen.insert(node.id.clone()) {
            return None;
        }
        if node.lineage.relationship_to_parent != Some(RelationshipToParent::Subagent) {
            return None;
        }
        let parent_id = node.lineage.parent_thread_id.clone()?;
        ancestors.push(parent_id.clone());
        if parent_id == thread.id {
            return Some(ancestors);
        }
        current = threads_by_id.get(&parent_id).cloned();
    }
    None
}

fn resolve_existing(path: &Path) -> Result<Option<PathBuf>, RestoreSafetyError> {
    match std::fs::canonicalize(path) {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(RestoreSafetyError::Resolve {
            path: path.display().to_string(),
            cause: error.to_string(),
        }),
    }
}

fn resolve_or_absolute(path: &Path) -> Result<PathBuf, RestoreSafetyError> {
    if let Some(path) = resolve_existing(path)? {
        return Ok(path);
    }
    std::path::absolute(path).map_err(|error| RestoreSafetyError::Resolve {
        path: path.display().to_string(),
        cause: error.to_string(),
    })
}

fn contains(parent: &Path, child: &Path) -> bool {
    child == parent || child.starts_with(parent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::CheckpointRunRef;
    use crate::contracts::{CheckpointScope, CheckpointScopeId, CheckpointScopeKind};
    use std::fs;

    // Ported from apps/server/src/orchestration-v2/CheckpointRestoreSafety.test.ts
    // (T3 Code v0.1.11, MIT).

    type RecordsFn = Box<dyn Fn(&ThreadId, &ThreadRecordsFilter) -> ThreadRecords>;

    struct TestContext {
        shell: ShellSnapshot,
        contexts: HashMap<ThreadId, CheckpointContext>,
        provider_contexts: HashMap<ThreadId, ThreadProviderContext>,
        records: RecordsFn,
        project_root: Option<String>,
    }

    impl RestoreSafetyContext for TestContext {
        fn shell_snapshot(&self) -> Result<ShellSnapshot, RestoreSafetyError> {
            Ok(self.shell.clone())
        }
        fn checkpoint_context(
            &self,
            thread_id: &ThreadId,
        ) -> Result<CheckpointContext, CheckpointContextError> {
            Ok(self.contexts.get(thread_id).cloned().unwrap_or_default())
        }
        fn thread_provider_context(
            &self,
            thread_id: &ThreadId,
        ) -> Result<ThreadProviderContext, RestoreSafetyError> {
            Ok(self
                .provider_contexts
                .get(thread_id)
                .cloned()
                .unwrap_or_default())
        }
        fn thread_records(
            &self,
            thread_id: &ThreadId,
            filter: &ThreadRecordsFilter,
        ) -> Result<ThreadRecords, RestoreSafetyError> {
            Ok((self.records)(thread_id, filter))
        }
        fn project_workspace_root(
            &self,
            _project_id: &ProjectId,
            _include_deleted: bool,
        ) -> Option<String> {
            self.project_root.clone()
        }
    }

    fn thread(id: &str, worktree_path: Option<&str>) -> RestoreThread {
        RestoreThread {
            id: ThreadId::new(id),
            worktree_path: worktree_path.map(str::to_string),
        }
    }

    fn shell_thread(id: &str, worktree_path: Option<&str>) -> ThreadShell {
        ThreadShell {
            id: ThreadId::new(id),
            project_id: ProjectId::new("restore-project"),
            deleted_at: None,
            worktree_path: worktree_path.map(str::to_string),
            active_run_id: None,
            status: String::new(),
            activity_run_status: None,
            pending_runtime_request: None,
            pending_background_tasks: Vec::new(),
            lineage: AppThreadLineage {
                parent_thread_id: None,
                relationship_to_parent: None,
                root_thread_id: ThreadId::new(id),
                side_chat: None,
            },
            forked_from: None,
            creation_source: CreationSource::Web,
        }
    }

    fn context_with(
        shell: ShellSnapshot,
        records: impl Fn(&ThreadId, &ThreadRecordsFilter) -> ThreadRecords + 'static,
    ) -> TestContext {
        TestContext {
            shell,
            contexts: HashMap::new(),
            provider_contexts: HashMap::new(),
            records: Box::new(records),
            project_root: None,
        }
    }

    #[test]
    fn preserves_overlapping_workspace_files_by_owner() {
        let parent_dir = tempfile::tempdir().unwrap();
        let parent = parent_dir.path();
        let cwd = parent.join("worktree");
        let nested = cwd.join("nested");
        let sibling = parent.join("worktree2");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(&sibling).unwrap();
        let other_file = nested.join("other-thread.txt");
        fs::write(&other_file, "other thread's uncommitted work").unwrap();

        for owner in [
            "nested",
            "ancestor",
            "provider",
            "sibling",
            "shared-provider",
            "conversation",
        ] {
            let other_worktree = match owner {
                "ancestor" | "conversation" => parent.to_path_buf(),
                "nested" => nested.clone(),
                _ => sibling.clone(),
            };
            let mut other_thread = shell_thread("restore-other", other_worktree.to_str());
            other_thread.lineage.parent_thread_id = None;
            let provider_cwd = if owner == "shared-provider" {
                cwd.to_str().unwrap()
            } else {
                nested.to_str().unwrap()
            };
            let mut context = context_with(
                ShellSnapshot {
                    threads: vec![other_thread.clone()],
                    archived_threads: vec![],
                },
                |_id, _filter| ThreadRecords::default(),
            );
            if owner == "provider" || owner == "shared-provider" {
                context.provider_contexts.insert(
                    other_thread.id.clone(),
                    ThreadProviderContext {
                        provider_threads: vec![],
                        provider_sessions: vec![ProviderSessionRef {
                            cwd: provider_cwd.to_string(),
                            status: "running".into(),
                            supports_multiple_provider_threads_per_session: owner
                                == "shared-provider",
                        }],
                    },
                );
            }
            let restore_files = owner != "conversation";
            let rejected = restore_files
                && !is_checkpoint_restore_isolated(
                    &thread("restore-current", cwd.to_str()),
                    cwd.to_str().unwrap(),
                    &context,
                )
                .unwrap();
            let expected_rejected =
                !["sibling", "shared-provider", "conversation"].contains(&owner);
            assert_eq!(rejected, expected_rejected, "owner={owner}");
            if !restore_files {
                assert_eq!(
                    fs::read_to_string(&other_file).unwrap(),
                    "other thread's uncommitted work"
                );
            }
        }
    }

    #[test]
    fn lets_rollback_recreate_a_worktree_storage_cleanup_removed() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("worktree");
        let context = context_with(ShellSnapshot::default(), |_id, _filter| {
            ThreadRecords::default()
        });
        assert!(
            is_checkpoint_restore_isolated(
                &thread("restore-removed", worktree.to_str()),
                worktree.to_str().unwrap(),
                &context,
            )
            .unwrap()
        );
    }

    #[test]
    fn a_side_chat_shares_the_parent_worktree_only_when_it_ran_tools() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().to_path_buf();
        let parent_id = ThreadId::new("restore-side-chat-parent");
        for (running, tool_items, isolated) in
            [(false, 0usize, true), (true, 0, false), (false, 1, false)]
        {
            let side_chat = ThreadShell {
                id: ThreadId::new("restore-side-chat"),
                project_id: ProjectId::new("restore-project"),
                deleted_at: None,
                worktree_path: Some(worktree.to_str().unwrap().to_string()),
                active_run_id: running.then(|| RunId::new("side-chat-run")),
                status: String::new(),
                activity_run_status: None,
                pending_runtime_request: None,
                pending_background_tasks: Vec::new(),
                lineage: AppThreadLineage {
                    parent_thread_id: Some(parent_id.clone()),
                    relationship_to_parent: Some(RelationshipToParent::Fork),
                    root_thread_id: parent_id.clone(),
                    side_chat: Some(true),
                },
                forked_from: None,
                creation_source: CreationSource::Web,
            };
            let context = context_with(
                ShellSnapshot {
                    threads: vec![side_chat],
                    archived_threads: vec![],
                },
                move |_id, filter| {
                    if filter.turn_item_types.is_empty() {
                        ThreadRecords::default()
                    } else {
                        ThreadRecords {
                            turn_items: (0..tool_items)
                                .map(|_| TurnItemRef {
                                    item_type: "command_execution".into(),
                                    status: None,
                                })
                                .collect(),
                            ..Default::default()
                        }
                    }
                },
            );
            assert_eq!(
                is_checkpoint_restore_isolated(
                    &thread("restore-side-chat-parent", worktree.to_str()),
                    worktree.to_str().unwrap(),
                    &context,
                )
                .unwrap(),
                isolated
            );
        }
    }

    #[test]
    fn parent_file_restore_respects_child_completion() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().to_path_buf();
        let parent_id = ThreadId::new("restore-parent");
        let child_id = ThreadId::new("restore-child");
        let ancestor_id = ThreadId::new("restore-ancestor");

        let cases: Vec<(&str, bool)> = vec![
            ("completed child", true),
            ("completed native child", true),
            ("native child with running tool", false),
            ("native child with provider background task", false),
            ("native child with parent background task", false),
            ("native child with another provider parent task", true),
            ("native child grandchild with root background task", false),
            ("missing task record", false),
            ("task belongs to another child", false),
            ("completed child with pending task", false),
            ("grandchild through fork", false),
            ("completed grandchild", true),
            ("unrelated child", false),
            ("running child", false),
            ("held queue", false),
            ("failed child", false),
            ("cyclic ancestry", false),
        ];

        for (name, isolated) in cases {
            let native = name.contains("native child");
            let child = ThreadShell {
                id: child_id.clone(),
                project_id: ProjectId::new("restore-project"),
                deleted_at: None,
                worktree_path: Some(worktree.to_str().unwrap().to_string()),
                active_run_id: (name == "running child").then(|| RunId::new("child-run")),
                status: if native {
                    "idle".to_string()
                } else if name == "running child" {
                    "running".to_string()
                } else if name == "failed child" {
                    "failed".to_string()
                } else {
                    "completed".to_string()
                },
                activity_run_status: None,
                pending_runtime_request: None,
                pending_background_tasks: Vec::new(),
                forked_from: Some(ForkSource::Node {
                    node_id: NodeId::new("child-task"),
                }),
                creation_source: if native {
                    CreationSource::Provider
                } else {
                    CreationSource::Web
                },
                lineage: AppThreadLineage {
                    parent_thread_id: Some(
                        if matches!(
                            name,
                            "completed grandchild"
                                | "native child grandchild with root background task"
                                | "grandchild through fork"
                                | "cyclic ancestry"
                        ) {
                            ancestor_id.clone()
                        } else if name == "unrelated child" {
                            ThreadId::new("another-parent")
                        } else {
                            parent_id.clone()
                        },
                    ),
                    relationship_to_parent: Some(RelationshipToParent::Subagent),
                    root_thread_id: parent_id.clone(),
                    side_chat: None,
                },
            };
            let ancestor = ThreadShell {
                id: ancestor_id.clone(),
                project_id: child.project_id.clone(),
                deleted_at: None,
                worktree_path: child.worktree_path.clone(),
                active_run_id: None,
                status: child.status.clone(),
                activity_run_status: None,
                pending_runtime_request: None,
                pending_background_tasks: Vec::new(),
                forked_from: Some(ForkSource::Node {
                    node_id: NodeId::new("ancestor-task"),
                }),
                creation_source: child.creation_source,
                lineage: AppThreadLineage {
                    parent_thread_id: Some(if name == "cyclic ancestry" {
                        child_id.clone()
                    } else {
                        parent_id.clone()
                    }),
                    relationship_to_parent: Some(if name == "grandchild through fork" {
                        RelationshipToParent::Fork
                    } else {
                        RelationshipToParent::Subagent
                    }),
                    root_thread_id: parent_id.clone(),
                    side_chat: None,
                },
            };
            let children = if matches!(
                name,
                "completed grandchild"
                    | "native child grandchild with root background task"
                    | "grandchild through fork"
                    | "cyclic ancestry"
            ) {
                vec![child, ancestor]
            } else {
                vec![child]
            };
            let name_owned = name.to_string();
            let parent_in_closure = parent_id.clone();
            let child_in_closure = child_id.clone();
            let ancestor_in_closure = ancestor_id.clone();
            let mut context = context_with(
                ShellSnapshot {
                    threads: children,
                    archived_threads: vec![],
                },
                move |thread_id, filter| {
                    let name = name_owned.as_str();
                    let provider_threads = if thread_id == &parent_in_closure
                        && (name.contains("parent")
                            || name == "native child grandchild with root background task")
                        && name.contains("native child")
                    {
                        vec![ProviderThreadRef {
                            provider_instance_id: ProviderInstanceId::new(
                                if name == "native child with another provider parent task" {
                                    "other-provider"
                                } else {
                                    "claude"
                                },
                            ),
                            provider_thread_id: None,
                            status: "idle".into(),
                            pending_background_tasks: vec![serde_json::json!({
                                "taskId": "background",
                                "kind": "command"
                            })],
                        }]
                    } else if name == "native child with provider background task"
                        && thread_id == &child_in_closure
                    {
                        vec![ProviderThreadRef {
                            provider_instance_id: ProviderInstanceId::new("claude"),
                            provider_thread_id: None,
                            status: "idle".into(),
                            pending_background_tasks: vec![serde_json::json!({
                                "taskId": "background"
                            })],
                        }]
                    } else {
                        vec![]
                    };
                    let turn_items = if !filter.turn_item_statuses.is_empty()
                        && name == "native child with running tool"
                    {
                        vec![TurnItemRef {
                            item_type: "command_execution".into(),
                            status: Some("running".into()),
                        }]
                    } else {
                        vec![]
                    };
                    let subagents = if !filter.subagent_ids.is_empty() {
                        if name == "missing task record" {
                            vec![]
                        } else {
                            vec![SubagentRef {
                                id: filter.subagent_ids[0].clone(),
                                provider_instance_id: ProviderInstanceId::new("claude"),
                                child_thread_id: Some(if name == "task belongs to another child" {
                                    ThreadId::new("another-child")
                                } else if filter.subagent_ids[0].as_str() == "ancestor-task" {
                                    ancestor_in_closure.clone()
                                } else {
                                    child_in_closure.clone()
                                }),
                                origin: if native {
                                    "provider_native".into()
                                } else {
                                    "app_owned".into()
                                },
                                status: if name == "completed child with pending task" {
                                    "running".into()
                                } else {
                                    "completed".into()
                                },
                            }]
                        }
                    } else {
                        vec![]
                    };
                    ThreadRecords {
                        provider_threads,
                        turn_items,
                        subagents,
                    }
                },
            );
            let held_runs = if name == "held queue" {
                vec![CheckpointRunRef {
                    id: RunId::new("held-run"),
                    ordinal: 1,
                    status: RunStatus::Queued,
                }]
            } else {
                vec![]
            };
            context.contexts.insert(
                child_id.clone(),
                CheckpointContext {
                    runs: held_runs.clone(),
                    ..Default::default()
                },
            );
            context.contexts.insert(
                ancestor_id.clone(),
                CheckpointContext {
                    runs: held_runs.clone(),
                    ..Default::default()
                },
            );
            context.contexts.insert(
                parent_id.clone(),
                CheckpointContext {
                    runs: held_runs,
                    ..Default::default()
                },
            );
            assert_eq!(
                is_checkpoint_restore_isolated(
                    &thread("restore-parent", worktree.to_str()),
                    worktree.to_str().unwrap(),
                    &context,
                )
                .unwrap(),
                isolated,
                "{name}"
            );
        }
    }

    #[test]
    fn unrelated_scope_types_are_projected_for_completeness() {
        let scope = CheckpointScope {
            id: CheckpointScopeId::new("scope"),
            thread_id: ThreadId::new("thread"),
            run_id: None,
            node_id: NodeId::new("node"),
            parent_scope_id: None,
            provider_thread_id: None,
            kind: CheckpointScopeKind::RootRun,
            ordinal_within_parent: 0,
            advances_app_run_count: true,
            cwd: "/repo".into(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
        };
        assert_eq!(scope.kind, CheckpointScopeKind::RootRun);
    }
}
