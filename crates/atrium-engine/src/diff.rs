//! Normalized event streams for `tools/t3-diff`.
//!
//! A differential scenario runs once under T3 Code and once under the engine;
//! both sides are reduced to the same JSON lines here, then compared. Keep the
//! normalization independent of provider noise (ids, timings) and stable
//! across runs.
//!
//! The schema is deliberately small. One line is one of:
//!
//! ```text
//! {"kind":"thread.created"}
//! {"kind":"run.started","ordinal":1}
//! {"kind":"item","ordinal":1,"role":"user","text":"say hi"}
//! {"kind":"item","ordinal":1,"role":"assistant","text":"..."}
//! {"kind":"approval.requested","ordinal":1}
//! {"kind":"run.finished","ordinal":1,"status":"completed"}
//! {"kind":"thread.finished","status":"completed"}
//! ```
//!
//! Ignored on purpose: thread/run/item ids, timestamps, durations, provider
//! names, hosted links, reasoning and tool items, and T3's sender note.

use serde::{Deserialize, Serialize};

/// The note T3 Code puts in front of a user message an agent sends through
/// its tools (T3 `withMessageSenderNote`, `packages/contracts`).
///
/// The driver sends the same wrapper so the scripted mock answers both sides
/// identically; the normalizer removes it from user-facing text.
pub const T3_SENDER_NOTE: &str =
    "[T3 Code: this message came through T3 Code's tools, not from the user typing in this chat.]";

/// Who wrote a normalized item. Only the two message roles that both sides
/// expose; reasoning, commands, file changes and plans are dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// One normalized line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum NormalLine {
    #[serde(rename = "thread.created")]
    ThreadCreated,
    #[serde(rename = "run.started")]
    RunStarted { ordinal: u32 },
    #[serde(rename = "item")]
    Item {
        ordinal: u32,
        role: Role,
        text: String,
    },
    #[serde(rename = "approval.requested")]
    ApprovalRequested { ordinal: u32 },
    #[serde(rename = "run.finished")]
    RunFinished { ordinal: u32, status: String },
    #[serde(rename = "thread.finished")]
    ThreadFinished { status: String },
}

impl NormalLine {
    pub fn thread_created() -> Self {
        Self::ThreadCreated
    }

    pub fn run_started(ordinal: u32) -> Self {
        Self::RunStarted { ordinal }
    }

    pub fn item(ordinal: u32, role: Role, text: impl Into<String>) -> Self {
        Self::Item {
            ordinal,
            role,
            text: normalize_text(&text.into()),
        }
    }

    pub fn approval_requested(ordinal: u32) -> Self {
        Self::ApprovalRequested { ordinal }
    }

    pub fn run_finished(ordinal: u32, status: impl AsRef<str>) -> Self {
        Self::RunFinished {
            ordinal,
            status: normalize_status(status.as_ref()),
        }
    }

    pub fn thread_finished(status: impl AsRef<str>) -> Self {
        Self::ThreadFinished {
            status: normalize_status(status.as_ref()),
        }
    }

    /// The line as the harness writes it: one compact JSON object.
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("normalized lines always serialize")
    }
}

/// Statuses are compared case-insensitively; both sides use the same
/// lowercase words (`completed`, `failed`, `waiting`, `cancelled`, ...).
pub fn normalize_status(status: &str) -> String {
    status.trim().to_ascii_lowercase()
}

/// Text as it is compared: line endings normalized, T3's sender note removed
/// from the front, surrounding whitespace trimmed.
pub fn normalize_text(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let text = text.strip_prefix(T3_SENDER_NOTE).unwrap_or(text.as_str());
    text.trim().to_string()
}

/// Render a whole stream, one JSON line per entry, ending in a newline.
pub fn render(lines: &[NormalLine]) -> String {
    let mut out = String::new();
    for line in lines {
        out.push_str(&line.to_line());
        out.push('\n');
    }
    out
}

/// The first line where two rendered streams differ.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    /// 1-based line number.
    pub line: usize,
    pub t3: Option<String>,
    pub engine: Option<String>,
}

/// Compare two rendered streams line by line.
///
/// The harness writes a fuller `difflib` diff; this is the shared rule for
/// "do these two streams say the same thing", used by unit tests and by
/// `--no-t3` sanity checks.
pub fn first_mismatch(t3: &str, engine: &str) -> Option<Mismatch> {
    let t3: Vec<&str> = t3.lines().collect();
    let engine: Vec<&str> = engine.lines().collect();
    let count = t3.len().max(engine.len());
    for ix in 0..count {
        let left = t3.get(ix).copied();
        let right = engine.get(ix).copied();
        if left != right {
            return Some(Mismatch {
                line: ix + 1,
                t3: left.map(str::to_owned),
                engine: right.map(str::to_owned),
            });
        }
    }
    None
}

/// The final thread status in a rendered stream, when one is present.
pub fn final_thread_status(stream: &str) -> Option<String> {
    stream.lines().rev().find_map(|line| {
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        if value.get("kind")?.as_str()? != "thread.finished" {
            return None;
        }
        value.get("status")?.as_str().map(str::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_have_the_documented_shape() {
        assert_eq!(
            NormalLine::thread_created().to_line(),
            r#"{"kind":"thread.created"}"#
        );
        assert_eq!(
            NormalLine::run_started(2).to_line(),
            r#"{"kind":"run.started","ordinal":2}"#
        );
        assert_eq!(
            NormalLine::item(1, Role::User, "say hi").to_line(),
            r#"{"kind":"item","ordinal":1,"role":"user","text":"say hi"}"#
        );
        assert_eq!(
            NormalLine::approval_requested(1).to_line(),
            r#"{"kind":"approval.requested","ordinal":1}"#
        );
        assert_eq!(
            NormalLine::run_finished(1, "COMPLETED").to_line(),
            r#"{"kind":"run.finished","ordinal":1,"status":"completed"}"#
        );
        assert_eq!(
            NormalLine::thread_finished("waiting").to_line(),
            r#"{"kind":"thread.finished","status":"waiting"}"#
        );
    }

    #[test]
    fn sender_note_and_line_endings_are_removed() {
        let text = format!("{T3_SENDER_NOTE}\r\n\r\n  say hi  \r\n");
        assert_eq!(normalize_text(&text), "say hi");
    }

    #[test]
    fn plain_text_is_only_trimmed() {
        assert_eq!(normalize_text("  now\nsay bye \n"), "now\nsay bye");
        assert_eq!(normalize_text(""), "");
    }

    #[test]
    fn a_note_in_the_middle_is_left_alone() {
        let text = format!("before {T3_SENDER_NOTE} after");
        assert_eq!(normalize_text(&text), text);
    }

    #[test]
    fn render_ends_every_line() {
        let stream = render(&[
            NormalLine::thread_created(),
            NormalLine::thread_finished("completed"),
        ]);
        assert_eq!(
            stream,
            "{\"kind\":\"thread.created\"}\n{\"kind\":\"thread.finished\",\"status\":\"completed\"}\n"
        );
    }

    #[test]
    fn mismatch_reports_the_first_difference() {
        let left = "{\"kind\":\"thread.created\"}\n{\"kind\":\"run.started\",\"ordinal\":1}\n";
        let right = "{\"kind\":\"thread.created\"}\n{\"kind\":\"run.started\",\"ordinal\":2}\n";
        let mismatch = first_mismatch(left, right).expect("streams differ");
        assert_eq!(mismatch.line, 2);
        assert_eq!(
            mismatch.t3.as_deref(),
            Some(r#"{"kind":"run.started","ordinal":1}"#)
        );
        assert!(first_mismatch(left, left).is_none());
    }

    #[test]
    fn a_short_stream_is_a_mismatch() {
        let left = "{\"kind\":\"thread.created\"}\n{\"kind\":\"thread.finished\",\"status\":\"completed\"}\n";
        let right = "{\"kind\":\"thread.created\"}\n";
        let mismatch = first_mismatch(left, right).expect("streams differ");
        assert_eq!(mismatch.line, 2);
        assert!(mismatch.engine.is_none());
    }

    #[test]
    fn final_status_is_read_from_thread_finished() {
        let stream = render(&[
            NormalLine::thread_created(),
            NormalLine::run_finished(1, "completed"),
            NormalLine::thread_finished("waiting"),
        ]);
        assert_eq!(final_thread_status(&stream).as_deref(), Some("waiting"));
        assert!(final_thread_status(&render(&[NormalLine::thread_created()])).is_none());
    }
}
