//! Document edits you saved in the side panel, handed to the agent with
//! your next message in the conversation, so it carries on from your version.

use std::path::Path;

use gpui::Context;

use super::{Composed, View, Workspace};

impl Workspace {
    /// The key reports for the open conversation are kept under.
    fn doc_key(&self) -> String {
        match &self.view {
            View::Thread(id) => id.clone(),
            View::Draft => String::new(),
        }
    }

    /// Queues `report` about `path` for the next message here, replacing an
    /// earlier one about the same file.
    pub fn queue_doc_report(&mut self, path: &Path, report: String, cx: &mut Context<Self>) {
        let key = self.doc_key();
        let reports = self.doc_reports.entry(key).or_default();
        // A second save before sending: the agent needs only the latest
        // report, but against what it last saw, so both stay in order.
        reports.push((path.to_path_buf(), report));
        cx.notify();
    }

    /// Forgets reports about `path` (its save was undone).
    pub fn drop_doc_report(&mut self, path: &Path, cx: &mut Context<Self>) {
        let key = self.doc_key();
        if let Some(reports) = self.doc_reports.get_mut(&key) {
            reports.retain(|(p, _)| p != path);
        }
        cx.notify();
    }

    /// The files whose changes go with the next message here.
    pub fn pending_doc_reports(&self) -> Vec<&Path> {
        let mut files: Vec<&Path> = self
            .doc_reports
            .get(&self.doc_key())
            .map(|r| r.iter().map(|(p, _)| p.as_path()).collect())
            .unwrap_or_default();
        files.dedup();
        files
    }

    /// `composed` with the queued reports before your text.
    pub(super) fn with_doc_reports(&mut self, composed: Composed) -> Composed {
        let Some(reports) = self.doc_reports.remove(&self.doc_key()) else {
            return composed;
        };
        if reports.is_empty() {
            return composed;
        }
        let mut text: Vec<String> = reports.into_iter().map(|(_, r)| r).collect();
        text.push(composed.text.clone());
        Composed {
            text: text.join("\n\n"),
            ..composed
        }
    }
}
