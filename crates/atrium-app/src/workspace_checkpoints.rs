//! Checkpoints: before each turn in a git folder, Atrium records the files
//! as they are, so a turn's changes can be undone from its footer. They are
//! kept as refs under `refs/atrium/checkpoints/`, outside every branch.

use std::path::PathBuf;

use atrium_core::{EntryKind, Item};
use atrium_project::repo;
use gpui::{AppContext, Context, Task};

use super::{Notice, Workspace};
use crate::agents;
use crate::i18n::{t, tf};

/// Checkpoints taken when a restore is undone are numbered from here, so
/// they never collide with a turn's.
const UNDO_BASE: usize = 1_000_000;

/// A fresh number for the checkpoint a restore leaves behind.
fn undo_slot() -> usize {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let now = jiff::Timestamp::now().as_second().unsigned_abs() as usize;
    UNDO_BASE + (now % 1_000_000) * 100 + NEXT.fetch_add(1, Ordering::Relaxed) % 100
}

impl Workspace {
    /// The folder of conversation `thread` when it is on this computer.
    pub fn local_dir(&self, thread: &str) -> Option<PathBuf> {
        let meta = self.threads.iter().find(|t| t.id == thread)?;
        if agents::split_backend(&meta.backend).1.is_some() {
            return None;
        }
        let dir = PathBuf::from(&meta.cwd);
        dir.is_dir().then_some(dir)
    }

    /// How many of your messages `thread` has: the number of the next turn.
    fn turns_so_far(&self, thread: &str) -> usize {
        self.transcripts.get(thread).map_or(0, |t| {
            t.entries()
                .iter()
                .filter(|e| matches!(e.kind, EntryKind::Item(Item::User { .. })))
                .count()
        })
    }

    /// Records the files before the next turn of `thread`. Await the task
    /// before sending, so the agent cannot change anything first.
    pub(super) fn checkpoint_next_turn(
        &mut self,
        thread: &str,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let Some(dir) = self.local_dir(thread) else {
            return Task::ready(());
        };
        let turn = self.turns_so_far(thread);
        let key = thread.to_string();
        self.checkpoint_in(dir, key, turn, cx)
    }

    /// Records the files in `dir` before turn `turn` under `key`.
    pub(super) fn checkpoint_in(
        &mut self,
        dir: PathBuf,
        key: String,
        turn: usize,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let job = cx.background_spawn({
            let key = key.clone();
            async move {
                if repo::root(&dir).is_none() {
                    return false;
                }
                match repo::checkpoint(&dir, &key, turn) {
                    Ok(()) => true,
                    Err(e) => {
                        log::warn!("checkpoint before turn {turn}: {e:#}");
                        false
                    }
                }
            }
        });
        cx.spawn(async move |this, cx| {
            if job.await {
                let _ = this.update(cx, |ws, _| {
                    let turns = ws.checkpoints.entry(key).or_default();
                    if !turns.contains(&turn) {
                        turns.push(turn);
                        turns.sort_unstable();
                    }
                });
            }
        })
    }

    /// Records the files before a new chat's first turn, under a key that
    /// [`Self::adopt_checkpoints`] later swaps for the chat's id.
    pub(super) fn checkpoint_new_chat(
        &mut self,
        backend: &str,
        cx: &mut Context<Self>,
    ) -> (Task<()>, Option<(PathBuf, String)>) {
        // A chat worktree starts clean from HEAD; its folder is the safety net.
        if agents::split_backend(backend).1.is_some() || self.settings.chat_worktrees {
            return (Task::ready(()), None);
        }
        let dir = PathBuf::from(self.folder_for(backend));
        if !dir.is_dir() {
            return (Task::ready(()), None);
        }
        let key = format!("draft-{}", uuid::Uuid::new_v4().simple());
        let task = self.checkpoint_in(dir.clone(), key.clone(), 0, cx);
        (task, Some((dir, key)))
    }

    /// Gives a new chat's checkpoints, taken under `draft`, its id.
    pub(super) fn adopt_checkpoints(
        &mut self,
        dir: PathBuf,
        draft: String,
        thread: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(turns) = self.checkpoints.remove(&draft) {
            self.checkpoints.insert(thread.clone(), turns);
        }
        cx.background_spawn(async move {
            if repo::root(&dir).is_some()
                && let Err(e) = repo::rename_checkpoints(&dir, &draft, &thread)
            {
                log::warn!("checkpoints of a new chat: {e:#}");
            }
        })
        .detach();
    }

    /// The turns of the open conversation with a checkpoint, read from git
    /// the first time.
    pub fn checkpoint_turns(&mut self, cx: &mut Context<Self>) -> Vec<usize> {
        let Some(thread) = self.active_thread_id().map(str::to_owned) else {
            return Vec::new();
        };
        if let Some(turns) = self.checkpoints.get(&thread) {
            return turns.clone();
        }
        self.checkpoints.insert(thread.clone(), Vec::new());
        if let Some(dir) = self.local_dir(&thread) {
            let key = thread.clone();
            let job = cx.background_spawn(async move { repo::checkpoints(&dir, &key) });
            cx.spawn(async move |this, cx| {
                let turns = job.await;
                let _ = this.update(cx, |ws, cx| {
                    ws.checkpoints.insert(thread, turns);
                    cx.notify();
                });
            })
            .detach();
        }
        Vec::new()
    }

    /// Whether turn `turn` (counting your messages from 0) of the open
    /// conversation can be undone.
    pub fn has_checkpoint(&self, turn: usize) -> bool {
        self.active_thread_id()
            .and_then(|id| self.checkpoints.get(id))
            .is_some_and(|turns| turns.contains(&turn))
    }

    /// Puts the files back as they were before turn `turn` of the open conversation.
    pub fn restore_turn(&mut self, turn: usize, cx: &mut Context<Self>) {
        let Some(thread) = self.active_thread_id().map(str::to_owned) else {
            return;
        };
        if self.is_running() {
            cx.emit(Notice::Info(
                t("Stop the agent before undoing files.").into(),
            ));
            return;
        }
        let Some(dir) = self.local_dir(&thread) else {
            return;
        };
        let undo = undo_slot();
        let key = thread.clone();
        let job = cx.background_spawn(async move { repo::restore(&dir, &key, turn, undo) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |ws, cx| {
                match result {
                    Ok(()) => {
                        ws.restores.insert(thread, undo);
                        cx.emit(Notice::Success(tf(
                            "Files are back as they were before message {n}. Undo it from the chat menu.",
                            &[("n", &(turn + 1))],
                        )));
                    }
                    Err(e) => cx.emit(Notice::Error(format!("{e:#}"))),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Whether the open conversation has a restore that can be undone.
    pub fn can_undo_restore(&self) -> bool {
        self.active_thread_id()
            .is_some_and(|id| self.restores.contains_key(id))
    }

    /// Puts the files back as they were just before the last restore.
    pub fn undo_restore(&mut self, cx: &mut Context<Self>) {
        let Some(thread) = self.active_thread_id().map(str::to_owned) else {
            return;
        };
        let (Some(dir), Some(undo)) =
            (self.local_dir(&thread), self.restores.get(&thread).copied())
        else {
            return;
        };
        let key = thread.clone();
        // The files as they are now are kept too, under the next number.
        let job = cx.background_spawn(async move { repo::restore(&dir, &key, undo, undo_slot()) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |ws, cx| {
                match result {
                    Ok(()) => {
                        ws.restores.remove(&thread);
                        cx.emit(Notice::Success(t("Restore undone.").into()));
                    }
                    Err(e) => cx.emit(Notice::Error(format!("{e:#}"))),
                }
                cx.notify();
            });
        })
        .detach();
    }
}
