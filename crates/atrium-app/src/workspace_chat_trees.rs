//! A new chat can start in its own git worktree, on a branch of its own, so
//! several agents can change the same repository at once without stepping
//! on each other. The worktrees live under Atrium's data folder.

use std::path::{Path, PathBuf};

use atrium_project::repo;
use gpui::{AppContext, Context, Task};

use super::{Notice, Workspace};
use crate::agents;
use crate::i18n::{t, tf};

/// Where chat worktrees go: `<data>/Atrium/worktrees/<repo>/<name>`.
pub fn worktrees_base() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("Atrium")
        .join("worktrees")
}

/// Whether `dir` is a chat worktree Atrium made.
pub fn is_chat_tree(dir: &Path) -> bool {
    dir.starts_with(worktrees_base())
}

impl Workspace {
    /// The folder a new chat with `backend` starts in. With worktrees on and
    /// a local repository, that is a new worktree named after `title`; the
    /// task gives the folder, or an error to show.
    pub(super) fn new_chat_folder(
        &self,
        backend: &str,
        title: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<PathBuf, String>> {
        let folder = PathBuf::from(self.folder_for(backend));
        if !self.settings.chat_worktrees || agents::split_backend(backend).1.is_some() {
            return Task::ready(Ok(folder));
        }
        let title = title.chars().take(60).collect::<String>();
        cx.background_spawn(async move {
            if repo::root(&folder).is_none() {
                return Ok(folder);
            }
            repo::add_worktree(&folder, &worktrees_base(), &title).map_err(|e| {
                tf("Could not make a worktree: {error}", &[("error", &format!("{e:#}"))])
            })
        })
    }

    /// The open chat's folder when it is a chat worktree.
    pub fn chat_tree(&self) -> Option<PathBuf> {
        let id = self.active_thread_id()?;
        self.local_dir(id).filter(|dir| is_chat_tree(dir))
    }

    /// Merges the open chat's worktree branch into the main checkout.
    pub fn merge_chat_tree(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.chat_tree() else {
            return;
        };
        let job = cx.background_spawn(async move { repo::merge_worktree(&dir) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |_, cx| match result {
                Ok(into) => cx.emit(Notice::Success(tf("Merged into {branch}.", &[("branch", &into)]))),
                Err(e) => cx.emit(Notice::Error(format!("{e:#}"))),
            });
        })
        .detach();
    }

    /// Deletes the open chat's worktree folder; its branch stays.
    pub fn remove_chat_tree(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.chat_tree() else {
            return;
        };
        if self.is_running() {
            cx.emit(Notice::Info(t("Stop the agent before removing its worktree.").into()));
            return;
        }
        let job = cx.background_spawn(async move { repo::remove_worktree(&dir) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |_, cx| {
                match result {
                    Ok(()) => cx.emit(Notice::Success(
                        t("Worktree removed. Its branch is kept.").into(),
                    )),
                    Err(e) => cx.emit(Notice::Error(format!("{e:#}"))),
                }
                cx.notify();
            });
        })
        .detach();
    }
}
