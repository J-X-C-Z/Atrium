//! The notice cursor pauses while a capture runs, so later phases keep their turn.
use super::Workspace;
use atrium_project::{
    Project,
    preview::{Preview, PreviewCommands},
};
use gpui::Context;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};

#[derive(Default)]
pub(super) struct ProjectPreviews {
    seen: usize,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<anyhow::Result<Option<PathBuf>>>>,
}

impl Drop for ProjectPreviews {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl ProjectPreviews {
    pub(super) fn tick(
        &mut self,
        project: &Project,
        approved: &PreviewCommands,
        cx: &mut Context<Workspace>,
    ) {
        if let Some(worker) = &self.worker {
            if !worker.is_finished() {
                return;
            }
            match self.worker.take().unwrap().join() {
                Ok(Ok(Some(path))) => {
                    gpui::ImageSource::from(path).remove_asset(cx);
                    cx.notify();
                }
                Ok(Ok(None)) => (),
                Ok(Err(e)) => log::warn!("preview in {}: {e:#}", project.root.display()),
                Err(_) => log::error!("preview worker panicked in {}", project.root.display()),
            }
        }
        if self.seen > project.notices.len() {
            self.seen = 0;
        }
        while let Some(n) = project.notices.get(self.seen) {
            self.seen += 1;
            if let Some(request) = Preview::from_notice(project, n) {
                let approved = approved.clone();
                let cancel = self.cancel.clone();
                self.worker = Some(std::thread::spawn(move || {
                    let path = agent_rpc::process::effective_path();
                    Ok(request
                        .capture(&approved, &path, &cancel)?
                        .then_some(request.output))
                }));
                break;
            }
        }
    }
}

impl Workspace {
    pub fn set_preview_commands(
        &mut self,
        root: &Path,
        commands: Option<PreviewCommands>,
        cx: &mut Context<Self>,
    ) {
        self.previews.remove(root);
        if let Some(commands) = commands {
            if !self.projects.iter().any(|p| p.root == root) {
                return;
            }
            self.settings
                .preview_commands
                .insert(root.to_owned(), commands);
        } else {
            self.settings.preview_commands.remove(root);
        }
        self.settings.save();
        cx.notify();
    }
}
