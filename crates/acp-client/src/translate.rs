//! ACP `session/update`s to AgentDesk events.

use std::collections::HashMap;

use agentdesk_core::{
    AgentCommand, AgentEvent, ApprovalKind, ApprovalRequest, ChangeKind, ConfigChoice,
    ConfigOption, Cost, FileChange, Item, ItemStatus, PlanStep, ThreadMeta, TokenCounts,
    TokenUsage, TurnStatus,
};
use serde_json::Value;

fn s(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
}

fn opt_s(value: &Value, key: &str) -> Option<String> {
    value[key]
        .as_str()
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

fn pretty(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// The text of a content block, or a short stand-in for non-text blocks.
fn block_text(block: &Value) -> String {
    match block["type"].as_str() {
        Some("text") => s(block, "text"),
        Some("resource_link") => format!("[{}]({})", s(block, "name"), s(block, "uri")),
        Some("resource") => block["resource"]["text"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| s(&block["resource"], "uri")),
        Some("image") => "[image]".into(),
        Some("audio") => "[audio]".into(),
        _ => String::new(),
    }
}

/// Settings from a `session/new` (or load/resume) response: `configOptions`,
/// plus the older `modes`, offered as a `mode` option when no option covers it.
pub fn config_options(result: &Value) -> Vec<ConfigOption> {
    let mut options: Vec<ConfigOption> = result["configOptions"]
        .as_array()
        .map(|options| options.iter().filter_map(config_option).collect())
        .unwrap_or_default();
    let modes = &result["modes"];
    let has_mode = options
        .iter()
        .any(|o| o.category.as_deref() == Some("mode"));
    if !has_mode && modes["availableModes"].is_array() {
        options.insert(
            0,
            ConfigOption {
                id: MODE_OPTION.into(),
                name: "Mode".into(),
                category: Some("mode".into()),
                current: s(modes, "currentModeId"),
                choices: modes["availableModes"]
                    .as_array()
                    .map(|modes| {
                        modes
                            .iter()
                            .map(|m| ConfigChoice {
                                value: s(m, "id"),
                                name: s(m, "name"),
                                description: opt_s(m, "description"),
                                group: None,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            },
        );
    }
    options
}

/// The id AgentDesk gives an agent's session modes when it has no mode option.
pub const MODE_OPTION: &str = "__mode";

fn config_option(option: &Value) -> Option<ConfigOption> {
    if option["type"].as_str().is_some_and(|t| t != "select") {
        return None;
    }
    let mut choices = Vec::new();
    for entry in option["options"].as_array()? {
        // Options may be grouped: `{ group, name, options: [...] }`.
        if let Some(inner) = entry["options"].as_array() {
            let group = opt_s(entry, "name").or_else(|| opt_s(entry, "group"));
            for choice in inner {
                choices.push(ConfigChoice {
                    value: s(choice, "value"),
                    name: s(choice, "name"),
                    description: opt_s(choice, "description"),
                    group: group.clone(),
                });
            }
        } else {
            choices.push(ConfigChoice {
                value: s(entry, "value"),
                name: s(entry, "name"),
                description: opt_s(entry, "description"),
                group: None,
            });
        }
    }
    Some(ConfigOption {
        id: s(option, "id"),
        name: s(option, "name"),
        category: opt_s(option, "category"),
        current: match &option["currentValue"] {
            Value::String(value) => value.clone(),
            other => other.to_string(),
        },
        choices,
    })
}

/// An ACP `SessionInfo` as a sidebar entry.
pub fn session_meta(backend: &str, info: &Value) -> Option<ThreadMeta> {
    let id = info["sessionId"].as_str()?.to_string();
    let updated = info["updatedAt"]
        .as_str()
        .and_then(|at| at.parse::<jiff::Timestamp>().ok())
        .map(|at| at.as_second())
        .unwrap_or_default();
    let title = opt_s(info, "title");
    Some(ThreadMeta {
        id,
        backend: backend.to_string(),
        name: title.clone(),
        preview: title.unwrap_or_default(),
        cwd: s(info, "cwd"),
        model: None,
        created_at: updated,
        updated_at: updated,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Open {
    User,
    Agent,
    Thought,
}

#[derive(Default)]
struct Session {
    turn: Option<String>,
    turns: u64,
    items: u64,
    /// The text item being streamed, its kind and its text so far.
    open: Option<(String, Open, String)>,
    /// Tool calls by ACP id: the last state, merged from updates.
    tools: HashMap<String, Value>,
    replaying: bool,
    options: Vec<ConfigOption>,
    /// Usage as reported so far: context from `usage_update`, counts from
    /// each prompt's response.
    usage: TokenUsage,
}

/// Keeps per-session state to give ACP's anonymous updates stable ids.
pub struct Translator {
    backend: String,
    sessions: HashMap<String, Session>,
}

impl Translator {
    pub fn new(backend: impl Into<String>) -> Self {
        Self {
            backend: backend.into(),
            sessions: HashMap::new(),
        }
    }

    fn session(&mut self, id: &str) -> &mut Session {
        self.sessions.entry(id.to_string()).or_default()
    }

    /// Remembers a session's settings, so mode updates can patch them.
    pub fn set_options(&mut self, session: &str, options: Vec<ConfigOption>) {
        self.session(session).options = options;
    }

    pub fn active_turn(&self, session: &str) -> Option<&str> {
        self.sessions.get(session).and_then(|s| s.turn.as_deref())
    }

    /// Starts a turn for a prompt about to be sent.
    pub fn begin_turn(&mut self, session: &str) -> (String, Vec<AgentEvent>) {
        let mut events = Vec::new();
        if self.session(session).turn.is_some() {
            events.extend(self.end_turn(session, TurnStatus::Completed, None));
        }
        let state = self.session(session);
        state.turns += 1;
        let turn = format!("{session}#t{}", state.turns);
        state.turn = Some(turn.clone());
        events.push(AgentEvent::TurnStarted {
            thread_id: session.to_string(),
            turn_id: turn.clone(),
        });
        (turn, events)
    }

    /// Ends the running turn: open items complete, then the turn.
    pub fn end_turn(
        &mut self,
        session: &str,
        status: TurnStatus,
        error: Option<String>,
    ) -> Vec<AgentEvent> {
        let mut events = self.close_open(session);
        let state = self.session(session);
        let Some(turn) = state.turn.take() else {
            return events;
        };
        // Tool calls still running when the turn ends did not finish.
        let unfinished: Vec<Value> = state
            .tools
            .drain()
            .map(|(_, call)| call)
            .filter(|call| !matches!(call["status"].as_str(), Some("completed") | Some("failed")))
            .collect();
        for mut call in unfinished {
            call["status"] = Value::from(if status == TurnStatus::Completed {
                "completed"
            } else {
                "failed"
            });
            events.push(AgentEvent::ItemCompleted {
                thread_id: session.to_string(),
                turn_id: turn.clone(),
                item: tool_item(session, &call),
            });
        }
        events.push(AgentEvent::TurnCompleted {
            thread_id: session.to_string(),
            turn_id: turn,
            status,
            error,
            duration_ms: None,
        });
        events
    }

    /// The prompt's response: how the turn ended.
    pub fn finish_prompt(
        &mut self,
        session: &str,
        result: Result<String, String>,
        usage: Option<&Value>,
    ) -> Vec<AgentEvent> {
        // The turn's own counts (ACP's unstable `usage`): input there
        // excludes the cache counters.
        let mut reported = Vec::new();
        if let Some(usage) = usage.filter(|u| u.is_object()) {
            let n = |key: &str| usage[key].as_u64().unwrap_or_default();
            let (read, write) = (n("cachedReadTokens"), n("cachedWriteTokens"));
            let last = TokenCounts {
                total: n("totalTokens"),
                input: n("inputTokens") + read + write,
                cached_input: read,
                output: n("outputTokens"),
                reasoning: n("thoughtTokens"),
            };
            if last.total > 0 || last.output > 0 {
                let state = self.session(session);
                state.usage.last = last;
                state.usage.total.add(&last);
                reported.push(AgentEvent::TokenUsage {
                    thread_id: session.to_string(),
                    usage: state.usage.clone(),
                });
            }
        }
        let (status, error) = match result.as_deref() {
            Ok("end_turn") => (TurnStatus::Completed, None),
            Ok("cancelled") => (TurnStatus::Interrupted, None),
            Ok("max_tokens") => (
                TurnStatus::Failed,
                Some("The answer hit the token limit.".to_string()),
            ),
            Ok("max_turn_requests") => (
                TurnStatus::Failed,
                Some("The agent hit its step limit.".to_string()),
            ),
            Ok("refusal") => (
                TurnStatus::Failed,
                Some("The agent declined to continue.".to_string()),
            ),
            Ok(_) => (TurnStatus::Completed, None),
            Err(message) => (TurnStatus::Failed, Some(message.to_string())),
        };
        reported.extend(self.end_turn(session, status, error));
        reported
    }

    /// History is about to be replayed through updates (`session/load`).
    pub fn begin_replay(&mut self, session: &str) {
        self.session(session).replaying = true;
    }

    /// The replay is over: the last replayed turn ends.
    pub fn end_replay(&mut self, session: &str) -> Vec<AgentEvent> {
        self.session(session).replaying = false;
        self.end_turn(session, TurnStatus::Completed, None)
    }

    fn next_item(&mut self, session: &str) -> String {
        let state = self.session(session);
        state.items += 1;
        format!("{session}#i{}", state.items)
    }

    /// Completes the text item being streamed, if any.
    fn close_open(&mut self, session: &str) -> Vec<AgentEvent> {
        let state = self.session(session);
        let (Some((id, kind, text)), Some(turn)) = (state.open.take(), state.turn.clone()) else {
            return Vec::new();
        };
        let item = match kind {
            Open::User => Item::User {
                id,
                text,
                attachments: Vec::new(),
            },
            Open::Agent => Item::Agent { id, text },
            Open::Thought => Item::Reasoning { id, text },
        };
        vec![AgentEvent::ItemCompleted {
            thread_id: session.to_string(),
            turn_id: turn,
            item,
        }]
    }

    /// Text arriving for `kind`: continues the open item or starts one.
    fn chunk(&mut self, session: &str, kind: Open, text: String) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        let replaying = self.session(session).replaying;
        // In a replay, each user message starts the next turn.
        if kind == Open::User
            && replaying
            && !matches!(self.session(session).open, Some((_, Open::User, _)))
        {
            events.extend(self.end_turn(session, TurnStatus::Completed, None));
            let (_, started) = self.begin_turn(session);
            events.extend(started);
        }
        if self.session(session).turn.is_none() {
            let (_, started) = self.begin_turn(session);
            events.extend(started);
        }
        let same = matches!(&self.session(session).open, Some((_, open, _)) if *open == kind);
        if !same {
            events.extend(self.close_open(session));
            let id = self.next_item(session);
            let turn = self.session(session).turn.clone().unwrap_or_default();
            let item = match kind {
                Open::User => Item::User {
                    id: id.clone(),
                    text: String::new(),
                    attachments: Vec::new(),
                },
                Open::Agent => Item::Agent {
                    id: id.clone(),
                    text: String::new(),
                },
                Open::Thought => Item::Reasoning {
                    id: id.clone(),
                    text: String::new(),
                },
            };
            // User text is shown whole on completion; agents' streams live.
            if kind != Open::User {
                events.push(AgentEvent::ItemStarted {
                    thread_id: session.to_string(),
                    turn_id: turn,
                    item,
                });
            }
            self.session(session).open = Some((id, kind, String::new()));
        }
        let state = self.session(session);
        let turn = state.turn.clone().unwrap_or_default();
        let Some((id, _, buffer)) = state.open.as_mut() else {
            return events;
        };
        buffer.push_str(&text);
        let (thread_id, item_id) = (session.to_string(), id.clone());
        match kind {
            Open::Agent => events.push(AgentEvent::AgentMessageDelta {
                thread_id,
                turn_id: turn,
                item_id,
                delta: text,
            }),
            Open::Thought => events.push(AgentEvent::ReasoningDelta {
                thread_id,
                turn_id: turn,
                item_id,
                delta: text,
            }),
            Open::User => {}
        }
        events
    }

    /// One `session/update` notification's params.
    pub fn update(&mut self, params: &Value) -> Vec<AgentEvent> {
        let session = s(params, "sessionId");
        if session.is_empty() {
            return Vec::new();
        }
        let update = &params["update"];
        match update["sessionUpdate"].as_str().unwrap_or_default() {
            "user_message_chunk" => {
                // Live prompts are already shown; only replays carry new user text.
                if !self.session(&session).replaying {
                    return Vec::new();
                }
                self.chunk(&session, Open::User, block_text(&update["content"]))
            }
            "agent_message_chunk" => {
                self.chunk(&session, Open::Agent, block_text(&update["content"]))
            }
            "agent_thought_chunk" => {
                self.chunk(&session, Open::Thought, block_text(&update["content"]))
            }
            "tool_call" | "tool_call_update" => self.tool(&session, update),
            "plan" => {
                let Some(turn) = self.ensure_turn(&session) else {
                    return Vec::new();
                };
                vec![AgentEvent::PlanUpdated {
                    thread_id: session,
                    turn_id: turn,
                    explanation: None,
                    steps: update["entries"]
                        .as_array()
                        .map(|entries| {
                            entries
                                .iter()
                                .map(|e| PlanStep {
                                    step: s(e, "content"),
                                    done: e["status"] == "completed",
                                    active: e["status"] == "in_progress",
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                }]
            }
            "usage_update" => {
                let state = self.session(&session);
                state.usage.context_used = update["used"].as_u64();
                if let Some(size) = update["size"].as_u64().filter(|s| *s > 0) {
                    state.usage.context_window = Some(size);
                }
                if let Some(amount) = update["cost"]["amount"].as_f64() {
                    state.usage.cost = Some(Cost {
                        amount,
                        currency: s(&update["cost"], "currency"),
                    });
                }
                vec![AgentEvent::TokenUsage {
                    usage: state.usage.clone(),
                    thread_id: session,
                }]
            }
            "session_info_update" => match opt_s(update, "title") {
                Some(title) => vec![AgentEvent::ThreadNameUpdated {
                    thread_id: session,
                    name: Some(title),
                }],
                None => Vec::new(),
            },
            "available_commands_update" => vec![AgentEvent::CommandsUpdated {
                thread_id: session,
                commands: update["availableCommands"]
                    .as_array()
                    .map(|commands| {
                        commands
                            .iter()
                            .map(|c| AgentCommand {
                                name: s(c, "name"),
                                description: s(c, "description"),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            }],
            "current_mode_update" => {
                let mode = s(update, "currentModeId");
                let state = self.session(&session);
                for option in &mut state.options {
                    if option.category.as_deref() == Some("mode") || option.id == MODE_OPTION {
                        option.current = mode.clone();
                    }
                }
                vec![AgentEvent::ConfigUpdated {
                    thread_id: session.clone(),
                    options: self.session(&session).options.clone(),
                }]
            }
            "config_option_update" => {
                let options = config_options(update);
                if options.is_empty() {
                    return Vec::new();
                }
                self.set_options(&session, options.clone());
                vec![AgentEvent::ConfigUpdated {
                    thread_id: session,
                    options,
                }]
            }
            "notice" => vec![AgentEvent::Warning(
                opt_s(update, "title").unwrap_or_else(|| "The agent sent a notice".into()),
            )],
            _ => Vec::new(),
        }
    }

    fn ensure_turn(&mut self, session: &str) -> Option<String> {
        self.session(session).turn.clone()
    }

    fn tool(&mut self, session: &str, update: &Value) -> Vec<AgentEvent> {
        let mut events = self.close_open(session);
        if self.session(session).turn.is_none() {
            let (_, started) = self.begin_turn(session);
            events.extend(started);
        }
        let id = s(update, "toolCallId");
        let state = self.session(session);
        let call = state
            .tools
            .entry(id)
            .or_insert_with(|| Value::Object(Default::default()));
        // Updates carry only what changed: merge them in.
        if let (Some(call), Some(fields)) = (call.as_object_mut(), update.as_object()) {
            for (key, value) in fields {
                if key != "sessionUpdate" && !value.is_null() {
                    call.insert(key.clone(), value.clone());
                }
            }
        }
        let call = call.clone();
        let turn = state.turn.clone().unwrap_or_default();
        let item = tool_item(session, &call);
        let done = matches!(call["status"].as_str(), Some("completed") | Some("failed"));
        events.push(if done {
            AgentEvent::ItemCompleted {
                thread_id: session.to_string(),
                turn_id: turn,
                item,
            }
        } else {
            AgentEvent::ItemStarted {
                thread_id: session.to_string(),
                turn_id: turn,
                item,
            }
        });
        events
    }

    /// A `session/request_permission` request as an approval.
    pub fn permission(&mut self, request_id: &Value, params: &Value) -> AgentEvent {
        let session = s(params, "sessionId");
        let call = &params["toolCall"];
        let turn = self.session(&session).turn.clone().unwrap_or_default();
        let title = opt_s(call, "title").unwrap_or_else(|| "The agent wants to use a tool".into());
        let kind = match call["kind"].as_str() {
            Some("execute") => ApprovalKind::Command {
                command: command_of(call).unwrap_or_else(|| title.clone()),
                cwd: s(&call["rawInput"], "cwd"),
                reason: Some(title),
            },
            Some("edit") | Some("delete") | Some("move") => ApprovalKind::FileChange {
                reason: Some(title),
                grant_root: None,
            },
            _ => ApprovalKind::Tool {
                title,
                detail: Some(pretty(&call["rawInput"])).filter(|d| !d.is_empty()),
            },
        };
        AgentEvent::ApprovalRequested(ApprovalRequest {
            request_id: request_id.clone(),
            thread_id: session.clone(),
            turn_id: turn,
            item_id: format!("{session}#{}", s(call, "toolCallId")),
            kind,
        })
    }

    pub fn backend(&self) -> &str {
        &self.backend
    }
}

/// A shell command from a tool call's raw input, if it has one.
fn command_of(call: &Value) -> Option<String> {
    let input = &call["rawInput"];
    match &input["command"] {
        Value::String(command) => Some(command.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    }
}

fn status_of(call: &Value) -> ItemStatus {
    match call["status"].as_str() {
        Some("completed") => ItemStatus::Completed,
        Some("failed") => ItemStatus::Failed,
        _ => ItemStatus::InProgress,
    }
}

/// The text a tool call's content carries.
fn content_text(call: &Value) -> String {
    call["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| match block["type"].as_str() {
                    Some("content") => Some(block_text(&block["content"])),
                    Some("terminal") => Some("(output in a terminal)".into()),
                    _ => None,
                })
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// A before/after pair written as a diff the UI splits back apart.
fn as_diff(old: &str, new: &str) -> String {
    let mut out = String::from("@@ @@\n");
    for line in old.lines() {
        out.push('-');
        out.push_str(line);
        out.push('\n');
    }
    for line in new.lines() {
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// One tool call, in its latest merged state, as an item.
fn tool_item(session: &str, call: &Value) -> Item {
    let id = format!("{session}#{}", s(call, "toolCallId"));
    let status = status_of(call);
    let title = opt_s(call, "title").unwrap_or_else(|| "Tool".into());
    let diffs: Vec<FileChange> = call["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b["type"] == "diff")
                .map(|b| {
                    let old = b["oldText"].as_str();
                    let new = s(b, "newText");
                    FileChange {
                        path: s(b, "path"),
                        kind: match old {
                            None => ChangeKind::Add,
                            Some(_) if new.is_empty() => ChangeKind::Delete,
                            Some(_) => ChangeKind::Update { move_to: None },
                        },
                        diff: as_diff(old.unwrap_or_default(), &new),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    if !diffs.is_empty() {
        return Item::FileChange {
            id,
            changes: diffs,
            status,
        };
    }
    match call["kind"].as_str() {
        Some("execute") => Item::Command {
            id,
            command: command_of(call).unwrap_or(title),
            cwd: s(&call["rawInput"], "cwd"),
            status,
            output: {
                let text = content_text(call);
                if text.is_empty() {
                    pretty(&call["rawOutput"])
                } else {
                    text
                }
            },
            exit_code: call["rawOutput"]["exit_code"]
                .as_i64()
                .or_else(|| call["rawOutput"]["exitCode"].as_i64())
                .map(|code| code as i32),
            duration_ms: None,
        },
        Some("fetch") if call["rawInput"]["query"].is_string() => Item::WebSearch {
            id,
            query: s(&call["rawInput"], "query"),
            detail: Some(content_text(call)).filter(|t| !t.is_empty()),
        },
        _ => {
            let mut result = content_text(call);
            if result.is_empty() {
                result = pretty(&call["rawOutput"]);
            }
            if let Some(locations) = call["locations"].as_array().filter(|l| !l.is_empty()) {
                let paths: Vec<String> = locations
                    .iter()
                    .map(|l| match l["line"].as_u64() {
                        Some(line) => format!("{}:{line}", s(l, "path")),
                        None => s(l, "path"),
                    })
                    .collect();
                if result.is_empty() {
                    result = paths.join("\n");
                }
            }
            Item::Tool {
                id,
                name: title,
                status,
                arguments: pretty(&call["rawInput"]),
                result,
                duration_ms: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use agentdesk_core::{EntryKind, Transcript};
    use serde_json::json;

    use super::*;

    fn update(session: &str, update: Value) -> Value {
        json!({"sessionId": session, "update": update})
    }

    fn fold(transcript: &mut Transcript, events: Vec<AgentEvent>) {
        for event in &events {
            transcript.apply(event);
        }
    }

    #[test]
    fn a_live_turn_streams_text_then_tools_then_more_text() {
        let mut t = Translator::new("claude");
        let mut transcript = Transcript::new();
        transcript.push_local_user("hi", vec![]);
        let (turn, events) = t.begin_turn("s");
        fold(&mut transcript, events);
        for chunk in ["Let me ", "look."] {
            let events = t.update(&update("s", json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": chunk}})));
            fold(&mut transcript, events);
        }
        let events = t.update(&update(
            "s",
            json!({"sessionUpdate": "tool_call", "toolCallId": "c1", "title": "Run tests",
            "kind": "execute", "status": "pending", "rawInput": {"command": ["cargo", "test"]}}),
        ));
        fold(&mut transcript, events);
        let events = t.update(&update(
            "s",
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed",
            "content": [{"type": "content", "content": {"type": "text", "text": "ok"}}]}),
        ));
        fold(&mut transcript, events);
        let events = t.update(&update("s", json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "All good."}})));
        fold(&mut transcript, events);
        let events = t.finish_prompt("s", Ok("end_turn".into()), None);
        fold(&mut transcript, events);

        let kinds: Vec<String> = transcript
            .entries()
            .iter()
            .map(|e| match &e.kind {
                EntryKind::Item(Item::User { .. }) => "user".into(),
                EntryKind::Item(Item::Agent { text, .. }) => format!("agent:{text}"),
                EntryKind::Item(Item::Command {
                    command,
                    output,
                    status,
                    ..
                }) => {
                    format!("cmd:{command}:{output}:{status:?}")
                }
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "user",
                "agent:Let me look.",
                "cmd:cargo test:ok:Completed",
                "agent:All good."
            ]
        );
        assert!(!transcript.is_running());
        assert!(transcript.entries().iter().all(|e| e.turn_id == turn));
    }

    #[test]
    fn a_replay_splits_turns_at_user_messages() {
        let mut t = Translator::new("claude");
        t.begin_replay("s");
        let mut events = Vec::new();
        for (kind, text) in [
            ("user_message_chunk", "first"),
            ("agent_message_chunk", "one"),
            ("user_message_chunk", "second"),
            ("agent_message_chunk", "two"),
        ] {
            events.extend(t.update(&update(
                "s",
                json!({"sessionUpdate": kind, "content": {"type": "text", "text": text}}),
            )));
        }
        events.extend(t.end_replay("s"));
        let mut transcript = Transcript::new();
        fold(&mut transcript, events);
        let turns: Vec<&str> = transcript
            .entries()
            .iter()
            .map(|e| e.turn_id.as_str())
            .collect();
        assert_eq!(turns.len(), 4);
        assert_eq!(turns[0], turns[1]);
        assert_ne!(turns[1], turns[2]);
        assert!(!transcript.is_running());
        assert!(
            matches!(&transcript.entries()[2].kind, EntryKind::Item(Item::User { text, .. }) if text == "second")
        );
    }

    #[test]
    fn diffs_become_file_changes_and_permissions_become_approvals() {
        let mut t = Translator::new("deepseek");
        t.begin_turn("s");
        let events = t.update(&update("s", json!({"sessionUpdate": "tool_call", "toolCallId": "e1", "title": "Edit lib.rs", "kind": "edit",
            "status": "completed", "content": [{"type": "diff", "path": "/r/lib.rs", "oldText": "a\nb", "newText": "a\nB"}]})));
        assert!(
            matches!(events.last(), Some(AgentEvent::ItemCompleted { item: Item::FileChange { changes, .. }, .. })
            if changes[0].diff == "@@ @@\n-a\n-b\n+a\n+B\n")
        );
        let approval = t.permission(&json!(4), &json!({"sessionId": "s", "toolCall": {"toolCallId": "x", "title": "Run rm", "kind": "execute",
            "rawInput": {"command": "rm -rf build"}}, "options": []}));
        assert!(
            matches!(approval, AgentEvent::ApprovalRequested(ApprovalRequest { kind: ApprovalKind::Command { command, .. }, .. }) if command == "rm -rf build")
        );
    }

    #[test]
    fn grouped_options_and_modes_read_as_config() {
        let options = config_options(&json!({
            "modes": {"currentModeId": "plan", "availableModes": [{"id": "plan", "name": "Plan"}, {"id": "auto", "name": "Auto"}]},
            "configOptions": [{"id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": "v4",
                "options": [{"group": "deepseek", "name": "DeepSeek", "options": [{"value": "v4", "name": "deepseek-v4"}]}]}]
        }));
        assert_eq!(options[0].id, MODE_OPTION);
        assert_eq!(options[0].current_name(), "Plan");
        assert_eq!(options[1].choices[0].group.as_deref(), Some("DeepSeek"));
        let meta = session_meta("claude", &json!({"sessionId": "a", "cwd": "/r", "title": "Fix it", "updatedAt": "2026-11-16T05:11:35.756Z"})).unwrap();
        assert_eq!(meta.title(), "Fix it");
        assert!(meta.updated_at > 0);
    }
}
