//! Conversations as Atrium exports them: one JSON Lines file per
//! conversation, the same for every agent. The first line describes the
//! conversation; each further line is one message or one tool step.

use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::store::{Store, write_atomic};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Header {
    /// Always `"session"`.
    #[serde(rename = "type")]
    pub kind: String,
    pub version: u32,
    pub thread: String,
    /// The backend id: `codex`, `claude`, `codex@studio`…
    pub agent: String,
    pub cwd: String,
    /// The project key, when the folder is in a project.
    pub project: Option<String>,
    pub title: String,
    pub started_ms: i64,
    pub updated_ms: i64,
}

/// One line after the header.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Line {
    /// `user`, `agent` or `tool`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Which turn of the conversation, from 0.
    pub turn: usize,
    pub text: String,
}

impl Line {
    pub fn new(kind: &str, turn: usize, text: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            turn,
            text: text.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub header: Header,
    pub lines: Vec<Line>,
}

impl Session {
    pub fn turns(&self) -> usize {
        self.lines.iter().map(|l| l.turn + 1).max().unwrap_or(0)
    }

    /// The conversation as plain text, for a memory service that takes
    /// text, such as Hindsight's `retain`. Long tool output is cut.
    pub fn transcript(&self) -> String {
        let mut out = format!(
            "Conversation \"{}\" with {} in {}\n",
            self.header.title, self.header.agent, self.header.cwd
        );
        for line in &self.lines {
            let who = match line.kind.as_str() {
                "user" => "User",
                "agent" => "Agent",
                _ => "Tool",
            };
            let text: String = if line.kind == "tool" {
                line.text.chars().take(300).collect()
            } else {
                line.text.clone()
            };
            out.push_str(&format!("\n{who}: {text}\n"));
        }
        out
    }
}

impl Store {
    pub fn session_file(&self, thread: &str) -> PathBuf {
        let safe: String = thread
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.root().join("sessions").join(format!("{safe}.jsonl"))
    }

    /// Writes the whole conversation again; returns its file.
    pub fn export_session(&self, session: &Session) -> std::io::Result<PathBuf> {
        let path = self.session_file(&session.header.thread);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut text = serde_json::to_string(&session.header).unwrap_or_default();
        text.push('\n');
        for line in &session.lines {
            text.push_str(&serde_json::to_string(line).unwrap_or_default());
            text.push('\n');
        }
        write_atomic(&path, &text)?;
        Ok(path)
    }

    pub fn read_session(&self, thread: &str) -> Option<Session> {
        let text = fs::read_to_string(self.session_file(thread)).ok()?;
        let mut lines = text.lines();
        let header: Header = serde_json::from_str(lines.next()?).ok()?;
        Some(Session {
            header,
            lines: lines.filter_map(|l| serde_json::from_str(l).ok()).collect(),
        })
    }

    /// Conversations whose messages hold every word of `query`, newest
    /// first, with the first matching line of each.
    pub fn search_sessions(&self, query: &str, limit: usize) -> Vec<(Header, String)> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if words.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<(Header, String)> = fs::read_dir(self.root().join("sessions"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let thread = e.path().file_stem()?.to_string_lossy().to_string();
                let session = self.read_session(&thread)?;
                let all = session
                    .lines
                    .iter()
                    .map(|l| l.text.to_lowercase())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !words.iter().all(|w| all.contains(w)) {
                    return None;
                }
                let line = session
                    .lines
                    .iter()
                    .find(|l| words.iter().any(|w| l.text.to_lowercase().contains(w)))
                    .map(|l| l.text.chars().take(240).collect())
                    .unwrap_or_default();
                Some((session.header, line))
            })
            .collect();
        hits.sort_by_key(|h| std::cmp::Reverse(h.0.updated_ms));
        hits.truncate(limit);
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample(thread: &str, text: &str, at: i64) -> Session {
        Session {
            header: Header {
                kind: "session".into(),
                version: crate::VERSION,
                thread: thread.into(),
                agent: "codex".into(),
                cwd: "/code/app".into(),
                project: Some("app-1".into()),
                title: "Fix the build".into(),
                started_ms: at,
                updated_ms: at,
            },
            lines: vec![
                Line::new("user", 0, text),
                Line::new("tool", 0, "$ cargo build\nerror: linker cc not found"),
                Line::new("agent", 0, "Installed build-essential; it builds now."),
            ],
        }
    }

    #[test]
    fn sessions_export_read_back_and_search() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        store
            .export_session(&sample("t-1", "The build fails on Linux", 1))
            .unwrap();
        store
            .export_session(&sample("t/2", "Add a dark theme", 2))
            .unwrap();
        let back = store.read_session("t-1").unwrap();
        assert_eq!(back.turns(), 1);
        assert!(back.transcript().contains("User: The build fails"));
        let hits = store.search_sessions("linker build", 5);
        assert_eq!(hits.len(), 2);
        let hits = store.search_sessions("dark", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.thread, "t/2");
    }
}
