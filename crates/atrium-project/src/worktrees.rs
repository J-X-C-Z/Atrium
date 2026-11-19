//! Git coordination shared by the desktop scheduler and the MCP claim boundary.
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    ATRIUM, Project, Role, Task, TaskStatus, Workstream, YOU, atomic_write,
    git::{self, Cleanup, Integration},
};

/// Saved in the worker's private Git admin directory. Removing or recreating
/// a checkout thus never grants a new worker an old claim.
#[derive(Default, Serialize, Deserialize)]
struct Handoff {
    merged: BTreeSet<String>,
    committed: Option<String>,
    conflict: Option<(String, String, String)>,
    ready: Option<(String, String, String)>,
}

impl Handoff {
    fn load(tree: &Path) -> Result<(Self, PathBuf)> {
        let path = git::git_dir(tree, "--absolute-git-dir")?.join("atrium-handoff.json");
        let state = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e.into()),
        };
        Ok((state, path))
    }

    fn save(&self, path: &Path) -> Result<()> {
        atomic_write(path, &serde_json::to_vec(self)?)
    }
}

fn lock(root: &Path) -> Result<File> {
    let dir = root.join(".atrium");
    ensure!(git::require_dir(&dir, false)?, "missing project directory");
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("worktree.lock"))?;
    file.lock()?;
    Ok(file)
}

fn checkout(project: &Project, id: &str) -> Result<PathBuf> {
    let name = git::branch(id)?;
    let dir = project.worktree_dir(id);
    git::check_tree(&project.root, &dir, &name)?;
    Ok(dir)
}

fn tips(tree: &Path, lead: &str, key: String) -> Result<(String, String, String)> {
    Ok((
        key,
        git::commit_id(tree, "HEAD")?,
        git::commit_id(tree, &format!("refs/heads/{}", git::branch(lead)?))?,
    ))
}

fn files(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.to_string_lossy())
        .collect::<Vec<_>>()
        .join(", ")
}

impl Project {
    /// Existing sessions keep their checkout when the default for new ones changes.
    pub fn session_checkout(&self, id: &str) -> PathBuf {
        let tree = self.worktree_dir(id);
        if self.stream(id).is_some_and(|s| s.role != Role::Controller)
            && git::branch(id).is_ok()
            && tree.join(".git").is_file()
        {
            tree
        } else {
            self.root.clone()
        }
    }

    fn tree_notice(&mut self, to: &str, text: String) -> Result<()> {
        if !self
            .notices
            .iter()
            .any(|n| n.from == ATRIUM && n.to == [to] && n.text == text)
        {
            self.post(ATRIUM, vec![to.to_owned()], &text)?;
        }
        Ok(())
    }

    /// None means that the caller must persist the non-Git fallback on the UI thread.
    pub fn prepare_stream_tree(&self, stream: &Workstream) -> Result<Option<PathBuf>> {
        if !self.file.worktrees {
            return Ok(Some(self.root.clone()));
        }
        let _lock = lock(&self.root)?;
        if git::top_level(&self.root)?.is_none() {
            return Ok(None);
        }
        if stream.role == Role::Controller {
            return Ok(Some(self.root.clone()));
        }
        let parent = if stream.role == Role::Worker {
            stream.parent.as_deref()
        } else {
            None
        };
        if let Some(id) = parent {
            ensure!(
                self.stream(id).is_some_and(|s| s.role == Role::Lead),
                "missing worker lead"
            );
            git::ensure_worktree(&self.root, id, None)?;
        }
        git::ensure_worktree(&self.root, &stream.id, parent)
    }

    pub fn disable_non_git_worktrees(&mut self) -> Result<()> {
        self.tree_notice(
            YOU,
            "Worktree isolation requires a Git repository. Sessions will use the project folder."
                .into(),
        )?;
        self.file.worktrees = false;
        self.save()
    }

    // Called under TaskLock by both the scheduler and agent MCP process. Only
    // read Git here: the background scheduler does the merge/rebase before permitting a claim.
    pub(crate) fn worktree_allows_claim(&self, worker: &str, task: &Task) -> Result<bool> {
        if !self.worktree_dir(worker).join(".git").is_file() {
            return Ok(true);
        }
        let tree = checkout(self, worker)?;
        let (state, _) = Handoff::load(&tree)?;
        Ok(self
            .done_by(worker)
            .all(|t| state.merged.contains(&t.key()))
            && state.ready == Some(tips(&tree, &task.owner, task.key())?))
    }

    fn done_by<'a>(&'a self, worker: &'a str) -> impl Iterator<Item = &'a Task> {
        self.tasks.iter().filter(move |t| {
            t.claimed_by.as_deref() == Some(worker) && t.status == TaskStatus::Done
        })
    }

    /// Called off the UI thread for workers between turns (including retired ones).
    pub fn reconcile_worker_tree(&mut self, id: &str) -> Result<()> {
        let _lock = lock(&self.root)?;
        self.reload();
        let worker = self.stream(id).cloned().context("unknown worker")?;
        ensure!(
            worker.role == Role::Worker && worker.task.is_none(),
            "expected a pool worker"
        );
        if !self.worktree_dir(id).join(".git").is_file() || self.task_of(&worker).is_some() {
            return Ok(());
        }
        let tree = checkout(self, id)?;
        let lead = worker.parent.as_deref().context("worker has no lead")?;
        let receiver = checkout(self, lead)?;
        let (mut state, path) = Handoff::load(&tree)?;
        let done: Vec<_> = self
            .done_by(id)
            .filter(|t| !state.merged.contains(&t.key()))
            .cloned()
            .collect();
        for task in done {
            let key = task.key();
            ensure!(task.owner == lead, "task owner differs from worker's lead");
            if state.committed.as_ref() != Some(&key) {
                git::commit_task(&tree, &format!("{} ({key})", task.title))?;
                state.committed = Some(key.clone());
                state.ready = None;
                state.save(&path)?;
            }
            let identity = tips(&tree, lead, key.clone())?;
            if state.conflict.as_ref() == Some(&identity) {
                return Ok(());
            }
            match git::merge_into(&receiver, id)? {
                Integration::Applied => {
                    state.merged.insert(key);
                    state.committed = None;
                    state.conflict = None;
                    state.save(&path)?;
                }
                Integration::Dirty => return Ok(()),
                Integration::Conflicts(paths) => {
                    self.tree_notice(
                        lead,
                        format!(
                            "Merge conflict for {key} from {}: {}",
                            Project::branch_of(id),
                            files(&paths)
                        ),
                    )?;
                    state.conflict = Some(identity);
                    state.save(&path)?;
                    return Ok(());
                }
            }
        }
        if worker.retired {
            return self.clean_stream_tree_locked(&worker);
        }
        if let Some(next) = self.claimable(lead).first() {
            let key = next.key();
            let identity = tips(&tree, lead, key.clone())?;
            if state.ready.as_ref() != Some(&identity) && state.conflict.as_ref() != Some(&identity)
            {
                match git::rebase_onto(&tree, lead)? {
                    Integration::Applied => {
                        state.ready = Some(tips(&tree, lead, key)?);
                        state.conflict = None;
                        state.save(&path)?;
                    }
                    Integration::Conflicts(paths) => {
                        self.tree_notice(
                            lead,
                            format!(
                                "Rebase conflict before {key} in {}: {}",
                                Project::branch_of(id),
                                files(&paths)
                            ),
                        )?;
                        state.conflict = Some(identity);
                        state.save(&path)?;
                    }
                    Integration::Dirty => {}
                }
            }
        }
        Ok(())
    }

    pub fn clean_stream_tree(&mut self, stream: &Workstream) -> Result<()> {
        let _lock = lock(&self.root)?;
        self.clean_stream_tree_locked(stream)
    }

    fn clean_stream_tree_locked(&mut self, stream: &Workstream) -> Result<()> {
        git::branch(&stream.id)?;
        if !self.worktree_dir(&stream.id).exists() {
            return Ok(());
        }
        let receiver = match (stream.role, stream.parent.as_deref()) {
            (Role::Worker, Some(id)) => checkout(self, id)?,
            _ => self.root.clone(),
        };
        let result = git::remove_worktree(
            &self.root,
            &stream.id,
            &receiver,
            stream.role == Role::Worker && stream.retired,
        )?;
        if result != Cleanup::Removed {
            self.tree_notice(YOU, format!("Kept worktree {}: {result:?}.", stream.id))?;
        }
        Ok(())
    }

    pub fn merge_lead_tree(&mut self, id: &str) -> Result<()> {
        let _lock = lock(&self.root)?;
        ensure!(
            self.stream(id).is_some_and(|s| s.role == Role::Lead),
            "expected a lead"
        );
        checkout(self, id)?;
        match git::merge_into(&self.root, id)? {
            Integration::Applied => Ok(()),
            Integration::Dirty => self.tree_notice(
                YOU,
                format!(
                    "Kept {} separate because the destination checkout has local changes.",
                    Project::branch_of(id)
                ),
            ),
            Integration::Conflicts(paths) => self.tree_notice(
                YOU,
                format!(
                    "Merge conflict for {}: {}",
                    Project::branch_of(id),
                    files(&paths)
                ),
            ),
        }
    }
}
