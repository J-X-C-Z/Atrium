//! A scripted stand-in for the `claude` CLI, speaking its stream-JSON and
//! control-channel protocol. It exists so Atrium can develop and test the
//! Claude transport without an account.
//!
//! Scripts, picked by words in the prompt:
//!
//! * plain: a streamed text turn and a successful result.
//! * `run`/`tool`: an assistant `Bash` call, a `can_use_tool` request, then a
//!   tool result once the host answers.
//! * `interrupt`: a turn that keeps working until an `interrupt` control
//!   request arrives, then emits an aborted result.
//! * `usage`/`limit`: a rejected `rate_limit_event` and a 429 result with a
//!   reset time.
//!
//! Usage: `mock-claude [--model=...] [--resume=...] [--session-id=...]`.
//! All other SDK launch flags are accepted and ignored.

use std::{
    io::{self, BufRead, Write},
    process,
};

use serde_json::{Value, json};

struct State {
    session_id: String,
    model: String,
    permission_mode: String,
    initialized: bool,
    turn: u64,
    request_seq: u64,
    pending: Option<Pending>,
}

enum Pending {
    Permission { prompt: String, tool_use_id: String },
    Interrupt,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |name: &str| -> Option<String> {
        let prefix = format!("--{name}=");
        args.iter()
            .find_map(|arg| arg.strip_prefix(&prefix).map(str::to_owned))
    };
    let session_id = arg("session-id")
        .or_else(|| arg("resume"))
        .unwrap_or_else(|| format!("00000000-0000-4000-8000-{:012}", process::id()));
    let mut state = State {
        session_id,
        model: arg("model").unwrap_or_else(|| "claude-sonnet-4-6".into()),
        permission_mode: arg("permission-mode").unwrap_or_else(|| "default".into()),
        initialized: false,
        turn: 0,
        request_seq: 0,
        pending: None,
    };

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("control_request") => control_request(&mut state, &value),
            Some("control_response") => control_response(&mut state, &value),
            Some("user") => user_turn(&mut state, &value),
            _ => {}
        }
    }
}

fn emit(value: Value) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{value}");
    let _ = stdout.flush();
}

fn uuid(state: &State, offset: u64) -> String {
    let turn = state.turn.to_string();
    let tail = format!("{turn:0>6}{offset:0>6}");
    format!("00000000-0000-4000-9000-{tail:0>12}")
}

fn respond(request_id: &str, payload: Value) {
    emit(json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": payload,
        }
    }));
}

fn control_request(state: &mut State, value: &Value) {
    let Some(request_id) = value.get("request_id").and_then(Value::as_str) else {
        return;
    };
    let request = value.get("request").cloned().unwrap_or_default();
    match request.get("subtype").and_then(Value::as_str) {
        Some("initialize") => respond(
            request_id,
            json!({
                "commands": [
                    {"name": "review", "description": "Review changes", "argumentHint": "<path>", "builtin": true},
                    {"name": "compact", "description": "Compact the conversation", "argumentHint": "", "builtin": true}
                ],
                "agents": [{"name": "Explore", "description": "Explore the codebase"}],
                "models": [
                    {"value": "claude-sonnet-4-6", "displayName": "Sonnet 4.6", "description": "balanced",
                     "supportsEffort": true, "supportedEffortLevels": ["low", "medium", "high", "xhigh", "max"]}
                ],
                "output_style": "default",
                "available_output_styles": ["default"],
                "account": {"subscriptionType": "max", "apiProvider": "firstParty"},
                "pending_permission_requests": []
            }),
        ),
        Some("interrupt") => {
            respond(request_id, json!({}));
            if matches!(state.pending, Some(Pending::Interrupt)) {
                state.pending = None;
                emit_result(state, true, "aborted_streaming", "", None);
            }
        }
        Some("set_model") => {
            if let Some(model) = request.get("model").and_then(Value::as_str) {
                state.model = model.to_owned();
            }
            respond(request_id, json!({}));
        }
        Some("set_permission_mode") => {
            if let Some(mode) = request.get("mode").and_then(Value::as_str) {
                state.permission_mode = mode.to_owned();
            }
            respond(request_id, json!({}));
        }
        Some("set_max_thinking_tokens") => respond(request_id, json!({})),
        Some("get_usage") => respond(
            request_id,
            json!({
                "session": {
                    "total_cost_usd": 0.01,
                    "total_api_duration_ms": 1000,
                    "total_duration_ms": 1200,
                    "total_lines_added": 0,
                    "total_lines_removed": 0,
                    "model_usage": {}
                },
                "subscription_type": "max",
                "rate_limits_available": true,
                "rate_limits": {
                    "five_hour": {"utilization": 54, "resets_at": "2026-07-18T14:39:00Z"},
                    "seven_day": {"utilization": 18.4, "resets_at": "2026-07-24T08:59:00+00:00"}
                }
            }),
        ),
        Some("get_context_usage") => respond(
            request_id,
            json!({
                "categories": [
                    {"id": "messages", "name": "Messages", "tokens": 1000, "color": "blue", "kind": "used"},
                    {"id": "free_space", "name": "Free space", "tokens": 199000, "color": "gray", "kind": "free"}
                ],
                "totalTokens": 1000,
                "maxTokens": 200000,
                "rawMaxTokens": 200000,
                "percentage": 0.5,
                "gridRows": [],
                "model": state.model.clone(),
                "memoryFiles": [],
                "mcpTools": [],
                "agents": []
            }),
        ),
        Some("list_models") => respond(
            request_id,
            json!([{"value": "claude-sonnet-4-6", "displayName": "Sonnet 4.6"}]),
        ),
        Some(other) => {
            let _ = other;
            respond(request_id, json!({}));
        }
        None => respond(request_id, json!({})),
    }
}

fn control_response(state: &mut State, value: &Value) {
    let response = value.get("response").cloned().unwrap_or_default();
    let Some(Pending::Permission {
        prompt,
        tool_use_id,
    }) = state.pending.take()
    else {
        return;
    };
    let decision = response.get("response").cloned().unwrap_or_default();
    if decision.get("behavior").and_then(Value::as_str) == Some("allow") {
        emit(json!({
            "type": "user",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": tool_use_id, "content": "mock output"}
            ]},
            "parent_tool_use_id": null,
            "session_id": state.session_id.clone(),
            "uuid": uuid(state, 7),
        }));
        emit_text_assistant(state, &format!("Command finished. ({prompt})"));
        emit_result(state, false, "completed", "Command finished.", None);
    } else {
        emit(json!({
            "type": "user",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": tool_use_id, "content": "Permission denied.", "is_error": true}
            ]},
            "parent_tool_use_id": null,
            "session_id": state.session_id.clone(),
            "uuid": uuid(state, 9),
        }));
        emit_text_assistant(state, "Permission denied.");
        emit_result(state, false, "completed", "Permission denied.", None);
    }
}

fn user_turn(state: &mut State, value: &Value) {
    state.turn += 1;
    let message = value.get("message").cloned().unwrap_or_default();
    let prompt = message_text(&message);
    if !state.initialized {
        state.initialized = true;
        emit_hooks(state);
        emit_init(state);
    }
    if prompt.contains("interrupt") {
        emit_stream_text(state, "working...");
        state.pending = Some(Pending::Interrupt);
        return;
    }
    if prompt.contains("usage") || prompt.contains("limit") {
        emit(json!({
            "type": "rate_limit_event",
            "rate_limit_info": {
                "status": "rejected",
                "rateLimitType": "five_hour",
                "resetsAt": 1_790_298_600,
                "utilization": 1.0
            },
            "uuid": uuid(state, 20),
            "session_id": state.session_id.clone(),
        }));
        emit_result(
            state,
            true,
            "api_error",
            "API Error: 429 rate limit reached (reset after 1h 0m 0s)",
            Some(429),
        );
        return;
    }
    if prompt.contains("plan") || prompt.contains("todo") {
        plan_turn(state, &prompt);
        return;
    }
    if prompt.contains("subagent") || prompt.contains("spawn") {
        subagent_turn(state, &prompt);
        return;
    }
    if prompt.contains("think about") {
        thinking_turn(state, &prompt);
        return;
    }
    if prompt.contains("run") || prompt.contains("tool") {
        state.request_seq += 1;
        let tool_use_id = format!("toolu_mock_{}", state.request_seq);
        emit_text_assistant(state, "I'll run that.");
        emit(json!({
            "type": "assistant",
            "message": {
                "id": format!("msg_mock_{}", state.request_seq),
                "model": state.model.clone(),
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": tool_use_id,
                    "name": "Bash",
                    "input": {"command": "echo mock", "description": "Run the mock command"},
                    "caller": {"type": "direct"}
                }],
                "stop_reason": null,
                "usage": {"input_tokens": 3, "output_tokens": 5}
            },
            "parent_tool_use_id": null,
            "session_id": state.session_id.clone(),
            "uuid": uuid(state, 31),
        }));
        state.request_seq += 1;
        let request_id = format!("perm-{}", state.request_seq);
        emit(json!({
            "type": "control_request",
            "request_id": request_id,
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "Bash",
                "input": {"command": "echo mock", "description": "Run the mock command"},
                "permission_suggestions": [{
                    "type": "addRules",
                    "rules": [{"toolName": "Bash", "ruleContent": "echo mock"}],
                    "behavior": "allow",
                    "destination": "session"
                }],
                "tool_use_id": tool_use_id,
                "decision_reason": "Bash is not pre-approved",
                "decision_reason_type": "mode"
            }
        }));
        state.pending = Some(Pending::Permission {
            prompt,
            tool_use_id,
        });
        return;
    }
    emit_stream_text(state, &format!("echo: {prompt}"));
    emit_assistant(state, &format!("echo: {prompt}"));
    emit(json!({
        "type": "rate_limit_event",
        "rate_limit_info": {"status": "allowed"},
        "uuid": uuid(state, 40),
        "session_id": state.session_id.clone(),
    }));
    emit_result(state, false, "completed", &format!("echo: {prompt}"), None);
}

/// A TodoWrite call that the adapter projects as a plan card.
fn plan_turn(state: &mut State, prompt: &str) {
    let message_id = format!("msg_mock_{}_plan", state.turn);
    emit(json!({
        "type": "assistant",
        "message": {
            "id": message_id,
            "model": state.model.clone(),
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": format!("toolu_plan_{}", state.turn),
                "name": "TodoWrite",
                "input": {"todos": [
                    {"content": "Inspect the request", "status": "completed", "activeForm": "Inspecting"},
                    {"content": "Write the plan", "status": "in_progress", "activeForm": "Writing"},
                    {"content": "Verify the result", "status": "pending", "activeForm": "Verifying"}
                ]},
                "caller": {"type": "direct"}
            }],
            "stop_reason": null,
            "usage": {"input_tokens": 3, "output_tokens": 7}
        },
        "parent_tool_use_id": null,
        "session_id": state.session_id.clone(),
        "uuid": uuid(state, 70),
    }));
    emit(json!({
        "type": "user",
        "message": {"role": "user", "content": [{
            "type": "tool_result",
            "tool_use_id": format!("toolu_plan_{}", state.turn),
            "content": "Todos updated"
        }]},
        "parent_tool_use_id": null,
        "session_id": state.session_id.clone(),
        "uuid": uuid(state, 71),
    }));
    emit_assistant(state, &format!("Plan recorded for: {prompt}"));
    emit_result(state, false, "completed", "Plan recorded.", None);
}

/// Claude's native subagent lifecycle frames.
fn subagent_turn(state: &mut State, prompt: &str) {
    let task_id = format!("task-{}", state.turn);
    emit(json!({
        "type": "system", "subtype": "task_started",
        "task_id": task_id,
        "description": "Explore the repository",
        "subagent_type": "Explore",
        "task_type": "local_agent",
        "prompt": prompt,
        "is_backgrounded": false,
        "spawn_depth": 0,
        "uuid": uuid(state, 72),
        "session_id": state.session_id.clone(),
    }));
    emit(json!({
        "type": "system", "subtype": "task_progress",
        "task_id": task_id,
        "description": "Explore the repository",
        "subagent_type": "Explore",
        "last_tool_name": "Read",
        "summary": "read 2 files",
        "usage": {"total_tokens": 512, "tool_uses": 2, "duration_ms": 1200},
        "uuid": uuid(state, 73),
        "session_id": state.session_id.clone(),
    }));
    emit_assistant(state, "The sub-agent finished.");
    emit(json!({
        "type": "system", "subtype": "task_notification",
        "task_id": task_id,
        "status": "completed",
        "summary": "read 2 files",
        "output_file": "",
        "usage": {"total_tokens": 512, "tool_uses": 2, "duration_ms": 1200},
        "uuid": uuid(state, 74),
        "session_id": state.session_id.clone(),
    }));
    emit_result(state, false, "completed", "Sub-agent finished.", None);
}

/// Streamed thinking, then a text answer.
fn thinking_turn(state: &mut State, prompt: &str) {
    let message_id = format!("msg_mock_{}_think", state.turn);
    emit(json!({
        "type": "stream_event",
        "event": {
            "type": "message_start",
            "message": {
                "model": state.model.clone(), "id": message_id, "type": "message", "role": "assistant",
                "content": [], "stop_reason": null,
                "usage": {"input_tokens": 3, "output_tokens": 1}
            }
        },
        "session_id": state.session_id.clone(), "parent_tool_use_id": null, "uuid": uuid(state, 80),
    }));
    emit(json!({
        "type": "stream_event",
        "event": {"type": "content_block_start", "index": 0,
                  "content_block": {"type": "thinking", "thinking": "", "signature": ""}},
        "session_id": state.session_id.clone(), "parent_tool_use_id": null, "uuid": uuid(state, 81),
    }));
    for (offset, chunk) in ["Weighing the ", "options."].iter().enumerate() {
        emit(json!({
            "type": "stream_event",
            "event": {"type": "content_block_delta", "index": 0,
                      "delta": {"type": "thinking_delta", "thinking": chunk}},
            "session_id": state.session_id.clone(), "parent_tool_use_id": null,
            "uuid": uuid(state, 82 + offset as u64),
        }));
    }
    emit(json!({
        "type": "stream_event",
        "event": {"type": "content_block_stop", "index": 0},
        "session_id": state.session_id.clone(), "parent_tool_use_id": null, "uuid": uuid(state, 84),
    }));
    emit(json!({
        "type": "assistant",
        "message": {
            "id": message_id,
            "model": state.model.clone(),
            "type": "message",
            "role": "assistant",
            "content": [{"type": "thinking", "thinking": "Weighing the options.", "signature": "sig"}],
            "stop_reason": null,
            "usage": {"input_tokens": 3, "output_tokens": 4}
        },
        "parent_tool_use_id": null,
        "session_id": state.session_id.clone(),
        "uuid": uuid(state, 85),
    }));
    emit_assistant(state, &format!("Thinking about: {prompt}"));
    emit_result(state, false, "completed", "Thought it through.", None);
}

fn message_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn emit_hooks(state: &State) {
    emit(json!({
        "type": "system", "subtype": "hook_started",
        "hook_id": "hook-1", "hook_name": "SessionStart:startup", "hook_event": "SessionStart",
        "uuid": uuid(state, 1), "session_id": state.session_id.clone(),
    }));
    emit(json!({
        "type": "system", "subtype": "hook_response",
        "hook_id": "hook-1", "hook_name": "SessionStart:startup", "hook_event": "SessionStart",
        "output": "", "stdout": "", "stderr": "", "exit_code": 0, "outcome": "success",
        "uuid": uuid(state, 2), "session_id": state.session_id.clone(),
    }));
}

fn emit_init(state: &State) {
    emit(json!({
        "type": "system", "subtype": "init",
        "agents": ["Explore"],
        "apiKeySource": "none",
        "claude_code_version": "2.1.999-mock",
        "cwd": std::env::current_dir().map(|path| path.display().to_string()).unwrap_or_default(),
        "tools": ["Read", "Bash"],
        "mcp_servers": [],
        "model": state.model.clone(),
        "permissionMode": state.permission_mode.clone(),
        "slash_commands": ["review", "compact"],
        "output_style": "default",
        "skills": ["review", "deploy"],
        "plugins": [],
        "fast_mode_state": "off",
        "uuid": uuid(state, 3),
        "session_id": state.session_id.clone(),
    }));
}

fn emit_stream_text(state: &State, text: &str) {
    let message_id = format!("msg_mock_{}", state.turn);
    emit(json!({
        "type": "stream_event",
        "event": {
            "type": "message_start",
            "message": {
                "model": state.model.clone(), "id": message_id, "type": "message", "role": "assistant",
                "content": [], "stop_reason": null,
                "usage": {"input_tokens": 3, "output_tokens": 1}
            }
        },
        "session_id": state.session_id.clone(), "parent_tool_use_id": null, "uuid": uuid(state, 4),
    }));
    emit(json!({
        "type": "stream_event",
        "event": {"type": "content_block_start", "index": 0,
                  "content_block": {"type": "text", "text": ""}},
        "session_id": state.session_id.clone(), "parent_tool_use_id": null, "uuid": uuid(state, 5),
    }));
    emit(json!({
        "type": "stream_event",
        "event": {"type": "content_block_delta", "index": 0,
                  "delta": {"type": "text_delta", "text": text}},
        "session_id": state.session_id.clone(), "parent_tool_use_id": null, "uuid": uuid(state, 6),
    }));
    emit(json!({
        "type": "stream_event",
        "event": {"type": "content_block_stop", "index": 0},
        "session_id": state.session_id.clone(), "parent_tool_use_id": null, "uuid": uuid(state, 7),
    }));
    emit(json!({
        "type": "stream_event",
        "event": {"type": "message_delta",
                  "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                  "usage": {"output_tokens": 2}},
        "session_id": state.session_id.clone(), "parent_tool_use_id": null, "uuid": uuid(state, 8),
    }));
    emit(json!({
        "type": "stream_event",
        "event": {"type": "message_stop"},
        "session_id": state.session_id.clone(), "parent_tool_use_id": null, "uuid": uuid(state, 9),
    }));
}

fn emit_text_assistant(state: &State, text: &str) {
    emit_assistant(state, text);
}

fn emit_assistant(state: &State, text: &str) {
    emit(json!({
        "type": "assistant",
        "message": {
            "id": format!("msg_mock_{}", state.turn),
            "model": state.model.clone(),
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
            "stop_reason": null,
            "usage": {"input_tokens": 3, "output_tokens": 2}
        },
        "parent_tool_use_id": null,
        "session_id": state.session_id.clone(),
        "uuid": uuid(state, 50),
    }));
}

fn emit_result(
    state: &State,
    is_error: bool,
    terminal_reason: &str,
    result: &str,
    api_error_status: Option<i64>,
) {
    let mut model_usage = serde_json::Map::new();
    model_usage.insert(
        state.model.clone(),
        json!({
            "inputTokens": 3, "outputTokens": 2, "cacheReadInputTokens": 0,
            "cacheCreationInputTokens": 0, "webSearchRequests": 0, "costUSD": 0.0001,
            "contextWindow": 200000, "maxOutputTokens": 32000
        }),
    );
    let mut value = json!({
        "type": "result",
        "subtype": "success",
        "is_error": is_error,
        "api_error_status": api_error_status,
        "duration_ms": 10,
        "duration_api_ms": 8,
        "num_turns": 1,
        "result": result,
        "stop_reason": if is_error { Value::Null } else { json!("end_turn") },
        "session_id": state.session_id.clone(),
        "total_cost_usd": 0.0001,
        "usage": {"input_tokens": 3, "output_tokens": 2, "cache_creation_input_tokens": 0,
                  "cache_read_input_tokens": 0, "service_tier": "standard"},
        "modelUsage": Value::Object(model_usage),
        "permission_denials": [],
        "terminal_reason": terminal_reason,
        "fast_mode_state": "off",
        "uuid": uuid(state, 60),
    });
    if is_error {
        value["errors"] = json!([]);
    }
    emit(value);
}
