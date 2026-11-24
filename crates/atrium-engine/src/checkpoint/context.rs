// Ported from apps/server/src/orchestration-v2/ProjectionStore.ts
// (`getCheckpointContext`) and the projection's scope-ownership rule
// (T3 Code v0.1.11, MIT).
//!
//! The checkpoint projection is small: the runs needed to know a turn was
//! completed, the scopes the thread owns, and the checkpoints captured in
//! them. A scope id may be reused when a second root run takes the thread
//! over; the newest event owns the scope, which is what makes the baseline of
//! a two-run thread resolve against the run's own workspace.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::contracts::{
    Checkpoint, CheckpointScope, CheckpointScopeKind, RunId, RunStatus, ThreadId,
};

/// The run fields checkpoint diffs need.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointRunRef {
    pub id: RunId,
    pub ordinal: u32,
    pub status: RunStatus,
}

/// What `getCheckpointContext` returns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CheckpointContext {
    pub runs: Vec<CheckpointRunRef>,
    pub checkpoint_scopes: Vec<CheckpointScope>,
    pub checkpoints: Vec<Checkpoint>,
}

impl CheckpointContext {
    /// Applies `checkpoint-scope.created`: a scope is keyed by id, so a second
    /// root run that reuses the scope replaces its owner and cwd.
    pub fn apply_scope(&mut self, scope: CheckpointScope) {
        if let Some(existing) = self
            .checkpoint_scopes
            .iter_mut()
            .find(|existing| existing.id == scope.id)
        {
            *existing = scope;
        } else {
            self.checkpoint_scopes.push(scope);
        }
    }

    /// Applies `checkpoint.captured`: a checkpoint is keyed by id.
    pub fn apply_checkpoint(&mut self, checkpoint: Checkpoint) {
        if let Some(existing) = self
            .checkpoints
            .iter_mut()
            .find(|existing| existing.id == checkpoint.id)
        {
            *existing = checkpoint;
        } else {
            self.checkpoints.push(checkpoint);
        }
    }

    /// Applies a `run.updated` snapshot.
    pub fn apply_run(&mut self, run: CheckpointRunRef) {
        if let Some(existing) = self.runs.iter_mut().find(|existing| existing.id == run.id) {
            *existing = run;
        } else {
            self.runs.push(run);
        }
    }

    pub fn root_scope(&self) -> Option<&CheckpointScope> {
        self.checkpoint_scopes
            .iter()
            .find(|scope| scope.kind == CheckpointScopeKind::RootRun)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum CheckpointContextError {
    #[error("thread '{0}' not found")]
    ThreadNotFound(ThreadId),
    #[error("{0}")]
    Read(String),
}

/// The read model `CheckpointDiffQuery` and the restore services consult.
pub trait CheckpointContextProvider {
    fn get_checkpoint_context(
        &self,
        thread_id: &ThreadId,
    ) -> Result<CheckpointContext, CheckpointContextError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::service::checkpoint_ref_for_scope_ordinal;
    use crate::contracts::{
        CheckpointId, CheckpointRef, CheckpointScopeId, CheckpointStatus, NodeId,
    };

    // Ported from apps/server/src/orchestration-v2/CheckpointScopeOwnership.test.ts
    // (T3 Code v0.1.11, MIT).

    #[test]
    fn a_second_root_run_replaces_scope_ownership() {
        let thread_id = ThreadId::new("thread:audit-root-scope");
        let first_run = RunId::new("run:audit-root-scope:1");
        let second_run = RunId::new("run:audit-root-scope:2");
        let first_node = NodeId::new("node:audit-root-scope:1");
        let second_node = NodeId::new("node:audit-root-scope:2");
        let scope_id = CheckpointScopeId::new("scope:audit-root-scope");
        let refs: Vec<CheckpointRef> = [0, 1, 2]
            .into_iter()
            .map(|ordinal| checkpoint_ref_for_scope_ordinal(&scope_id, ordinal))
            .collect();
        let baseline_id = CheckpointId::new("checkpoint:audit-root-scope:0");
        let first_checkpoint_id = CheckpointId::new("checkpoint:audit-root-scope:1");
        let second_checkpoint_id = CheckpointId::new("checkpoint:audit-root-scope:2");
        let now = "2026-09-08T12:00:00.000Z";

        let scope = |run_id: RunId, node_id: NodeId, cwd: &str| CheckpointScope {
            id: scope_id.clone(),
            thread_id: thread_id.clone(),
            run_id: Some(run_id),
            node_id,
            parent_scope_id: None,
            provider_thread_id: None,
            kind: CheckpointScopeKind::RootRun,
            ordinal_within_parent: 0,
            advances_app_run_count: true,
            cwd: cwd.to_string(),
            created_at: now.to_string(),
        };
        let checkpoint = |id: CheckpointId,
                          run_id: Option<RunId>,
                          node_id: NodeId,
                          ordinal: u32,
                          app_run_ordinal: Option<u32>| Checkpoint {
            id,
            thread_id: thread_id.clone(),
            scope_id: scope_id.clone(),
            run_id,
            node_id,
            parent_checkpoint_id: None,
            ordinal_within_scope: ordinal,
            app_run_ordinal,
            checkpoint_ref: refs[ordinal as usize].clone(),
            status: CheckpointStatus::Ready,
            files: Vec::new(),
            files_total: None,
            captured_at: now.to_string(),
        };

        let mut context = CheckpointContext::default();
        context.apply_run(CheckpointRunRef {
            id: first_run.clone(),
            ordinal: 1,
            status: RunStatus::Completed,
        });
        context.apply_scope(scope(first_run.clone(), first_node.clone(), "/repo"));
        context.apply_checkpoint(checkpoint(
            baseline_id.clone(),
            None,
            first_node.clone(),
            0,
            None,
        ));
        context.apply_checkpoint(checkpoint(
            first_checkpoint_id.clone(),
            Some(first_run),
            first_node,
            1,
            Some(1),
        ));
        context.apply_run(CheckpointRunRef {
            id: second_run.clone(),
            ordinal: 2,
            status: RunStatus::Completed,
        });
        context.apply_scope(scope(
            second_run.clone(),
            second_node.clone(),
            "/prepared-repo",
        ));
        context.apply_checkpoint(checkpoint(
            second_checkpoint_id,
            Some(second_run),
            second_node,
            2,
            Some(2),
        ));

        assert_eq!(
            context
                .checkpoints
                .iter()
                .map(|checkpoint| checkpoint.app_run_ordinal)
                .collect::<Vec<_>>(),
            vec![None, Some(1), Some(2)]
        );
        assert_eq!(context.checkpoint_scopes.len(), 1);
        assert_eq!(context.root_scope().unwrap().cwd, "/prepared-repo");
    }
}
