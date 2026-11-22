//! Finished conversations waiting for a memory service: one small JSON
//! file each. The service takes a request, reviews the session file it
//! names, changes notes and skills, then deletes the request.

use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::store::{Store, write_atomic};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub version: u32,
    pub thread: String,
    pub agent: String,
    pub cwd: String,
    pub project: Option<String>,
    /// The exported conversation.
    pub session: PathBuf,
    pub turns: usize,
    /// `idle`, `archived` or `asked` (the user asked from the UI).
    pub reason: String,
    pub at_ms: i64,
}

impl Store {
    fn inbox(&self) -> PathBuf {
        self.root().join("inbox")
    }

    /// Queues a review; a request already waiting for the same
    /// conversation is replaced, so a service sees it once.
    pub fn enqueue(&self, request: &ReviewRequest) -> std::io::Result<PathBuf> {
        let dir = self.inbox();
        fs::create_dir_all(&dir)?;
        for (path, old) in self.pending() {
            if old.thread == request.thread {
                let _ = fs::remove_file(path);
            }
        }
        let safe: String = request
            .thread
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let path = dir.join(format!("{}-{safe}.json", request.at_ms));
        write_atomic(
            &path,
            &serde_json::to_string_pretty(request).unwrap_or_default(),
        )?;
        Ok(path)
    }

    /// Requests waiting, oldest first.
    pub fn pending(&self) -> Vec<(PathBuf, ReviewRequest)> {
        let mut out: Vec<_> = fs::read_dir(self.inbox())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .filter_map(|p| {
                let request = serde_json::from_str(&fs::read_to_string(&p).ok()?).ok()?;
                Some((p, request))
            })
            .collect();
        out.sort_by_key(|(_, r): &(PathBuf, ReviewRequest)| r.at_ms);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(thread: &str, at: i64) -> ReviewRequest {
        ReviewRequest {
            version: crate::VERSION,
            thread: thread.into(),
            agent: "claude".into(),
            cwd: "/code/app".into(),
            project: None,
            session: PathBuf::from("/x.jsonl"),
            turns: 3,
            reason: "idle".into(),
            at_ms: at,
        }
    }

    #[test]
    fn one_request_per_conversation_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        store.enqueue(&request("a", 10)).unwrap();
        store.enqueue(&request("b", 5)).unwrap();
        store.enqueue(&request("a", 20)).unwrap();
        let pending = store.pending();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].1.thread, "b");
        assert_eq!(pending[1].1.at_ms, 20);
    }
}
