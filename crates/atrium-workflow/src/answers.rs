//! Reading a workflow agent's transcript: which lines count, the message id
//! and effort/speed metadata a split assistant reply carries, whether its
//! turn ended, and the answer it gave.
//!
//! Ported from T3 Code v0.1.11:
//! `apps/server/src/orchestration-v2/workflowAgentAnswers.ts` and
//! `apps/server/src/orchestration-v2/workflowAgentTranscript.ts`.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::files::{self, Cursor, FileError, ReadOptions};

/// The transcript read T3 caps each agent read at.
pub const TRANSCRIPT_BYTE_CAP: usize = 512 * 1024;

/// What an agent's assistant messages say about the model settings used.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<String>,
}

/// A non-empty, trimmed string field, as T3's `text` helper reads it.
fn text(value: Option<&Value>) -> Option<&str> {
    let value = value?.as_str()?.trim();
    (!value.is_empty()).then_some(value)
}

/// The complete lines of a transcript, skipping harness bookkeeping and
/// lines without the uuid the projection needs.
pub fn parse_lines(contents: &str) -> Vec<Value> {
    contents
        .lines()
        .filter_map(|line| {
            if line.trim().is_empty() {
                return None;
            }
            let entry: Value = serde_json::from_str(line).ok()?;
            let kind = entry.get("type").and_then(Value::as_str)?;
            if kind != "assistant" && kind != "user" {
                return None;
            }
            if entry.get("isMeta") == Some(&Value::Bool(true)) {
                return None;
            }
            text(entry.get("uuid"))?;
            Some(entry)
        })
        .collect()
}

/// Folds the metadata of assistant lines into `previous`. A reply split
/// over several lines keeps its effort and speed; a new reply (a different
/// message id, or none) starts over. Bookkeeping and API-error lines are
/// skipped without clearing anything.
pub fn merge_metadata(lines: &[Value], previous: &AgentMetadata) -> AgentMetadata {
    let mut metadata = previous.clone();
    for entry in lines {
        if entry.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        if entry.get("isMeta") == Some(&Value::Bool(true))
            || entry.get("isApiErrorMessage") == Some(&Value::Bool(true))
        {
            continue;
        }
        let message = entry.get("message");
        let message_id = text(message.and_then(|message| message.get("id")));
        let effort = text(entry.get("effort"));
        let speed = text(
            message
                .and_then(|message| message.get("usage"))
                .and_then(|usage| usage.get("speed")),
        );
        let same_message = message_id.is_some() && message_id == metadata.message_id.as_deref();
        let mut next = if same_message {
            metadata
        } else {
            AgentMetadata::default()
        };
        if let Some(id) = message_id {
            next.message_id = Some(id.to_string());
        }
        if let Some(effort) = effort {
            next.effort = Some(effort.to_string());
        }
        if let Some(speed) = speed {
            next.speed = Some(speed.to_string());
        }
        metadata = next;
    }
    metadata
}

/// Whether an assistant line ended its turn. Blocks arrive separately with
/// no stop reason until the last; pre-tool text is not the reply.
pub fn turn_ended(entry: &Value) -> bool {
    if entry.get("type").and_then(Value::as_str) != Some("assistant") {
        return false;
    }
    entry
        .get("message")
        .and_then(|message| message.get("stop_reason"))
        .and_then(Value::as_str)
        .is_some_and(|reason| reason != "tool_use")
}

/// The last assistant answer in a transcript: its text blocks joined, with
/// bookkeeping and API errors left out. T3 shows this as the agent's result
/// preview; Atrium keeps it as the step's report for later steps.
pub fn last_answer(lines: &[Value]) -> Option<String> {
    lines.iter().rev().find_map(|entry| {
        if entry.get("type").and_then(Value::as_str) != Some("assistant") {
            return None;
        }
        if entry.get("isMeta") == Some(&Value::Bool(true))
            || entry.get("isApiErrorMessage") == Some(&Value::Bool(true))
        {
            return None;
        }
        let content = entry.get("message")?.get("content")?;
        let text = match content {
            Value::String(text) => text.clone(),
            Value::Array(blocks) => blocks
                .iter()
                .filter_map(|block| {
                    (block.get("type").and_then(Value::as_str) == Some("text"))
                        .then(|| block.get("text").and_then(Value::as_str))
                        .flatten()
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => return None,
        };
        (!text.trim().is_empty()).then_some(text)
    })
}

/// The lines and next cursor of one agent's transcript read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentTranscript {
    pub lines: Vec<Value>,
    pub cursor: Cursor,
    pub truncated: bool,
}

fn valid_agent_id(agent_id: &str) -> bool {
    (1..=128).contains(&agent_id.len())
        && agent_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Reads the complete lines `agent-<id>.jsonl` gained since `cursor`, under
/// `root`. A missing file is an error the caller may treat as "not yet".
pub fn read_agent_transcript(
    root: &Path,
    transcript_dir: &Path,
    agent_id: &str,
    cursor: Cursor,
) -> Result<AgentTranscript, FileError> {
    let path = transcript_dir.join(format!("agent-{agent_id}.jsonl"));
    if !valid_agent_id(agent_id) {
        return Err(FileError::InvalidPath(path));
    }
    let file = files::read_contained_file(
        root,
        &path,
        ReadOptions {
            extension: Some("jsonl"),
            byte_cap: TRANSCRIPT_BYTE_CAP,
            from: Some(cursor),
            ..ReadOptions::default()
        },
    )?;
    Ok(AgentTranscript {
        lines: parse_lines(&file.contents),
        cursor: Cursor {
            offset: file.end_offset,
            identity: Some(file.identity),
        },
        truncated: file.truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keeps_effort_and_speed_across_reads_of_a_split_assistant_message() {
        let metadata = merge_metadata(
            &[json!({
                "type": "assistant",
                "effort": "xhigh",
                "message": {"id": "a", "content": [], "usage": {"speed": "standard"}}
            })],
            &AgentMetadata::default(),
        );
        let expected = AgentMetadata {
            message_id: Some("a".into()),
            effort: Some("xhigh".into()),
            speed: Some("standard".into()),
        };
        assert_eq!(metadata, expected);
        assert_eq!(
            merge_metadata(
                &[json!({
                    "type": "assistant",
                    "message": {"id": "a", "content": [{"type": "text", "text": "Done"}]}
                })],
                &metadata,
            ),
            expected
        );
        assert_eq!(merge_metadata(&[], &metadata), expected);
    }

    #[test]
    fn ignores_bookkeeping_and_api_errors_without_clearing_prior_metadata() {
        let metadata = AgentMetadata {
            message_id: Some("a".into()),
            effort: Some("max".into()),
            speed: Some("standard".into()),
        };
        assert_eq!(
            merge_metadata(
                &[
                    json!({"type": "user", "effort": "high", "message": {"usage": {"speed": "fast"}}}),
                    json!({"type": "assistant", "isMeta": true, "effort": "high"}),
                    json!({"type": "assistant", "isApiErrorMessage": true, "message": {"id": "error"}}),
                    json!({"type": "assistant", "effort": 1, "message": {"id": "a", "usage": {"speed": false}}}),
                ],
                &metadata,
            ),
            metadata
        );
    }

    #[test]
    fn clears_omitted_metadata_for_a_new_response() {
        let previous = AgentMetadata {
            message_id: Some("a".into()),
            effort: Some("max".into()),
            speed: Some("standard".into()),
        };
        assert_eq!(
            merge_metadata(
                &[json!({"type": "assistant", "message": {"id": "b", "content": []}})],
                &previous,
            ),
            AgentMetadata {
                message_id: Some("b".into()),
                ..AgentMetadata::default()
            }
        );
        // An assistant line with no message id is a new response too.
        assert_eq!(
            merge_metadata(
                &[json!({"type": "assistant", "message": {"content": []}})],
                &previous,
            ),
            AgentMetadata::default()
        );
    }

    #[test]
    fn skips_harness_bookkeeping_and_lines_without_uuids() {
        let contents = [
            json!({"type": "attachment", "uuid": "att", "attachment": {"type": "skill_listing"}}),
            json!({"type": "user", "uuid": "meta", "isMeta": true, "message": {"content": "context"}}),
            json!({"type": "user", "uuid": "u1", "message": {"content": "go"}}),
            json!({"type": "assistant", "message": {"content": []}}),
            json!({"type": "assistant", "uuid": "a", "message": {"content": []}}),
        ]
        .map(|line| line.to_string())
        .join("\n")
            + "\n";
        let lines = parse_lines(&contents);
        let ids: Vec<&str> = lines
            .iter()
            .filter_map(|entry| entry["uuid"].as_str())
            .collect();
        assert_eq!(ids, ["u1", "a"]);
    }

    #[test]
    fn a_turn_ends_only_on_an_assistant_stop_reason_other_than_tool_use() {
        assert!(turn_ended(&json!({
            "type": "assistant",
            "message": {"stop_reason": "end_turn"}
        })));
        assert!(!turn_ended(&json!({
            "type": "assistant",
            "message": {"stop_reason": "tool_use"}
        })));
        assert!(!turn_ended(&json!({
            "type": "assistant",
            "message": {"stop_reason": null, "content": [{"type": "text", "text": "pre-tool"}]}
        })));
        assert!(!turn_ended(
            &json!({"type": "user", "message": {"content": "go"}})
        ));
    }

    #[test]
    fn the_last_answer_joins_text_blocks_and_skips_bookkeeping() {
        let lines = parse_lines(
            &[
                json!({
                    "type": "assistant", "uuid": "a",
                    "message": {"content": [{"type": "text", "text": "first"}]}
                }),
                json!({
                    "type": "assistant", "uuid": "b", "isApiErrorMessage": true,
                    "message": {"content": [{"type": "text", "text": "error"}]}
                }),
                json!({
                    "type": "assistant", "uuid": "c",
                    "message": {"content": [
                        {"type": "text", "text": "done"},
                        {"type": "tool_use", "id": "t", "name": "edit", "input": {}}
                    ]}
                }),
            ]
            .map(|line| line.to_string())
            .join("\n"),
        );
        assert_eq!(last_answer(&lines).as_deref(), Some("done"));
        assert_eq!(last_answer(&[]), None);
    }

    #[test]
    fn reads_an_agent_transcript_under_its_root() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("projects/test");
        std::fs::create_dir_all(&transcript).unwrap();
        std::fs::write(
            transcript.join("agent-a1.jsonl"),
            json!({"type": "assistant", "uuid": "a", "message": {"content": []}}).to_string()
                + "\n",
        )
        .unwrap();
        let first = read_agent_transcript(dir.path(), &transcript, "a1", Cursor::new()).unwrap();
        assert_eq!(first.lines.len(), 1);
        assert!(first.cursor.identity.is_some());
        assert!(matches!(
            read_agent_transcript(dir.path(), &transcript, "../a1", Cursor::new()),
            Err(FileError::InvalidPath(_))
        ));
        // A directory outside the root cannot be reached.
        let outside = tempfile::tempdir().unwrap();
        assert!(read_agent_transcript(dir.path(), outside.path(), "a1", Cursor::new()).is_err());
    }
}
