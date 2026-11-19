//! Keep checkout preparation and Git integration off the GPUI thread.
use super::*;
use crate::project::YOU;
use gpui::AppContext;

impl Workspace {
    pub fn set_worktrees(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(root) = self.project_root()
            && let Some(project) = self.project_mut(&root)
        {
            let old = project.file.worktrees;
            project.file.worktrees = on;
            if let Err(error) = project.save() {
                project.file.worktrees = old;
                tree_error(error, cx);
            }
            cx.notify();
        }
    }

    pub(super) fn prepare_stream_checkout(
        &mut self,
        root: PathBuf,
        stream: Workstream,
        title: String,
        kickoff: String,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.project_mut(&root).cloned() else {
            return;
        };
        if stream.role != Role::Controller && agents::split_backend(&stream.backend).1.is_some() {
            self.stream_failed(
                &root,
                &stream.id,
                anyhow::anyhow!("The remote agent needs a worktree on its own device"),
                cx,
            );
            return;
        }
        let work = cx.background_spawn({
            let stream = stream.clone();
            async move { project.prepare_stream_tree(&stream) }
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |ws, cx| {
                let Some(project) = ws.project_mut(&root) else {
                    return;
                };
                if project.stream(&stream.id) != Some(&stream) {
                    return;
                }
                let cwd = result.and_then(|dir| match dir {
                    Some(dir) => Ok(dir),
                    None => {
                        project.disable_non_git_worktrees()?;
                        Ok(root.clone())
                    }
                });
                match cwd {
                    Ok(cwd) => ws.launch_stream(root, stream, title, kickoff, cwd, cx),
                    Err(e) => ws.stream_failed(&root, &stream.id, e, cx),
                }
            });
        })
        .detach();
    }

    pub(super) fn tick_worktrees(&mut self, root: &PathBuf, cx: &mut Context<Self>) {
        if self.project_schedule.tree_jobs.contains(root) {
            return;
        }
        let Some(project) = self.projects.iter().find(|p| p.root == *root) else {
            return;
        };
        let idle = |s: &Workstream| {
            s.thread_id
                .as_ref()
                .and_then(|t| self.transcripts.get(t))
                .is_some_and(|t| !t.is_running() && waiting_on_user(t) == 0)
        };
        let workers: Vec<_> = project
            .file
            .streams
            .iter()
            .filter(|s| {
                s.role == Role::Worker
                    && s.task.is_none()
                    && (s.retired || idle(s))
                    && project.task_of(s).is_none()
                    && project.worktree_dir(&s.id).join(".git").is_file()
                    && s.parent
                        .as_deref()
                        .and_then(|id| project.stream(id))
                        .is_some_and(|lead| idle(lead) || lead.retired)
            })
            .map(|s| s.id.clone())
            .collect();
        if workers.is_empty() {
            return;
        }
        self.project_schedule.tree_jobs.insert(root.clone());
        let mut project = project.clone();
        let root = root.clone();
        let work = cx.background_spawn(async move {
            for id in workers {
                if let Err(e) = project.reconcile_worker_tree(&id) {
                    log::warn!("worktree integration for {id}: {e:#}");
                }
            }
        });
        cx.spawn(async move |this, cx| {
            work.await;
            let _ = this.update(cx, |ws, _| {
                ws.project_schedule.tree_jobs.remove(&root);
            });
        })
        .detach();
    }

    pub(super) fn clean_removed_checkout(
        &mut self,
        root: &PathBuf,
        id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.iter().find(|p| p.root == *root) else {
            return;
        };
        let Some(stream) = project.stream(id).cloned() else {
            return;
        };
        if !project.worktree_dir(id).exists() {
            return;
        }
        let mut project = project.clone();
        if stream
            .thread_id
            .as_ref()
            .and_then(|t| self.transcripts.get(t))
            .is_none_or(|t| t.is_running() || waiting_on_user(t) > 0)
        {
            if let Err(e) = project.post(
                ATRIUM,
                vec![YOU.into()],
                &format!("Kept worktree {id}: its session may still be active."),
            ) {
                tree_error(e, cx);
            }
            return;
        }
        let job = cx.background_spawn(async move { project.clean_stream_tree(&stream) });
        self.finish_tree_job(job, cx);
    }

    #[expect(
        dead_code,
        reason = "public hook for the lead card specified in the worktree brief"
    )]
    pub fn merge_lead(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(mut project) = self.active_project().cloned() else {
            return;
        };
        let id = id.to_owned();
        let job = cx.background_spawn(async move { project.merge_lead_tree(&id) });
        self.finish_tree_job(job, cx);
    }

    fn finish_tree_job(&self, job: gpui::Task<anyhow::Result<()>>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |_, cx| {
                if let Err(error) = result {
                    tree_error(error, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

fn tree_error(error: anyhow::Error, cx: &mut Context<Workspace>) {
    cx.emit(Notice::Error(tf(
        "Worktree operation failed: {error}",
        &[("error", &format!("{error:#}"))],
    )));
}
