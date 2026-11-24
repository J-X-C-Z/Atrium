//! A stand-in ACP agent with a scripted brain, for developing and testing
//! Atrium without accounts. `--name` picks whose capabilities it mimics,
//! as their real handshakes report them:
//!
//! * `claude`: loads sessions with history, lists them, steers, five modes
//!   and a model picker, takes images.
//! * `deepseek`: lists and resumes (no history), grouped models and a
//!   reasoning level, no images.
//! * `antigravity`: lists and loads sessions, a mode and a model.
//!
//! A message with "run" asks permission for a command; "edit" proposes a
//! diff; "plan" streams a plan; anything else gets a streamed answer.

use std::{
    collections::HashMap,
    io::{self, BufRead, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use serde_json::{Value, json};

type Out = Arc<Mutex<io::Stdout>>;

fn send(out: &Out, mut message: Value) {
    message["jsonrpc"] = json!("2.0");
    let mut out = out.lock().unwrap();
    let _ = writeln!(out, "{message}");
    let _ = out.flush();
}

fn update(out: &Out, session: &str, update: Value) {
    send(
        out,
        json!({"method": "session/update", "params": {"sessionId": session, "update": update}}),
    );
}

fn pause(ms: u64) {
    thread::sleep(Duration::from_millis(ms));
}

static SEQ: AtomicU64 = AtomicU64::new(1);
fn next(prefix: &str) -> String {
    // Unique across mock processes, as real session ids are.
    format!(
        "{prefix}-{:x}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

#[derive(Clone)]
struct Session {
    cwd: String,
    title: Option<String>,
    /// `(user, agent)` exchanges, for replay.
    history: Vec<(String, String)>,
    mode: String,
    model: String,
    effort: String,
}

struct State {
    name: String,
    sessions: Mutex<HashMap<String, Session>>,
    answers: Mutex<HashMap<i64, mpsc::Sender<Value>>>,
    next_request: AtomicU64,
    cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let name = args
        .iter()
        .position(|a| a == "--name")
        .and_then(|ix| args.get(ix + 1))
        .cloned()
        .unwrap_or_else(|| "claude".into());
    let out: Out = Arc::new(Mutex::new(io::stdout()));
    let state = Arc::new(State {
        name: name.clone(),
        sessions: Mutex::new(HashMap::new()),
        answers: Mutex::new(HashMap::new()),
        next_request: AtomicU64::new(100),
        cancels: Mutex::new(HashMap::new()),
    });
    {
        let mut sessions = state.sessions.lock().unwrap();
        let cwd = std::env::current_dir().unwrap().display().to_string();
        let (question, answer) = match name.as_str() {
            "deepseek" => (
                "Summarize the build setup",
                "It is a cargo workspace with five crates.",
            ),
            "antigravity" => (
                "Sketch the settings page",
                "Three sections: appearance, agents, account.",
            ),
            _ => (
                "Why is the list virtualized?",
                "So only rows near the view are drawn.",
            ),
        };
        sessions.insert(
            format!("{name}-past-1"),
            Session {
                cwd,
                title: Some(question.into()),
                history: vec![(question.into(), answer.into())],
                mode: default_mode(&name).into(),
                model: "default".into(),
                effort: "high".into(),
            },
        );
    }
    eprintln!("mock-acp ({name}): ready");
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = message.get("id").cloned();
        let method = message["method"].as_str().map(str::to_owned);
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match (method, id) {
            (Some(method), Some(id)) => {
                let (out, state) = (out.clone(), state.clone());
                // Prompts run long: answer them on their own thread.
                if method == "session/prompt" {
                    thread::spawn(move || prompt(&out, &state, id, &params));
                } else {
                    handle(&method, id, &params, &out, &state);
                }
            }
            (Some(method), None) => {
                if method == "session/cancel"
                    && let Some(flag) = state
                        .cancels
                        .lock()
                        .unwrap()
                        .get(params["sessionId"].as_str().unwrap_or_default())
                {
                    flag.store(true, Ordering::Relaxed);
                }
            }
            (None, Some(id)) => {
                if let Some(key) = id.as_i64()
                    && let Some(waiter) = state.answers.lock().unwrap().remove(&key)
                {
                    let _ = waiter.send(message.get("result").cloned().unwrap_or(Value::Null));
                }
            }
            _ => {}
        }
    }
}

fn default_mode(name: &str) -> &'static str {
    match name {
        "claude" => "default",
        "antigravity" => "agent",
        _ => "",
    }
}

fn options(name: &str, session: &Session) -> Value {
    match name {
        "deepseek" => json!({"configOptions": [
            {"id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": session.model,
             "options": [{"group": "deepseek-official", "name": "DeepSeek", "options": [
                {"value": "default", "name": "deepseek-v4-flash"}, {"value": "pro", "name": "deepseek-v4-pro"}]}]},
            {"id": "reasoning_effort", "name": "Reasoning effort", "category": "thought_level", "type": "select", "currentValue": session.effort,
             "options": [{"value": "off", "name": "Off"}, {"value": "low", "name": "Low"}, {"value": "high", "name": "High"}, {"value": "max", "name": "Max"}]}]}),
        "antigravity" => json!({
            "modes": {"currentModeId": session.mode, "availableModes": [
                {"id": "agent", "name": "Agent", "description": "Plans and acts"}, {"id": "ask", "name": "Ask", "description": "Answers without changes"}]},
            "configOptions": [
                {"id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": session.model,
                 "options": [
                    {"group": "Gemini", "name": "Gemini", "options": [
                        {"value": "default", "name": "Gemini 3 Pro"}, {"value": "flash", "name": "Gemini 3 Flash"}]},
                    {"value": "gemini-3.8-flash-high", "name": "Gemini 3.8 Flash (high)"}]},
                {"id": "thought_level", "name": "Thinking", "category": "thought_level", "type": "select", "currentValue": session.effort,
                 "options": [{"value": "off", "name": "Off"}, {"value": "low", "name": "Low"}, {"value": "high", "name": "High"}]},
                {"id": "contextWindow", "name": "Context window", "category": "context_window", "type": "select", "currentValue": "200k",
                 "options": [{"value": "200k", "name": "200K"}, {"value": "1m", "name": "1M"}]},
                {"id": "autoCompact", "name": "Auto compact", "type": "boolean", "currentValue": true}]}),
        _ => json!({
            "modes": {"currentModeId": session.mode, "availableModes": [
                {"id": "default", "name": "Manual", "description": "Always ask before making changes"},
                {"id": "acceptEdits", "name": "Accept edits", "description": "Automatically accept all file edits"},
                {"id": "plan", "name": "Plan", "description": "Create a plan before making changes"}]},
            "configOptions": [
                {"id": "mode", "name": "Mode", "category": "mode", "type": "select", "currentValue": session.mode, "options": [
                    {"value": "default", "name": "Manual"}, {"value": "acceptEdits", "name": "Accept edits"}, {"value": "plan", "name": "Plan"}]},
                {"id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": session.model, "options": [
                    {"value": "default", "name": "Default (recommended)"}, {"value": "sonnet", "name": "Sonnet"}, {"value": "haiku", "name": "Haiku"}]}]}),
    }
}

fn reply(out: &Out, id: Value, result: Value) {
    send(out, json!({"id": id, "result": result}));
}

fn fail(out: &Out, id: Value, code: i64, message: &str) {
    send(
        out,
        json!({"id": id, "error": {"code": code, "message": message}}),
    );
}

fn handle(method: &str, id: Value, params: &Value, out: &Out, state: &Arc<State>) {
    let name = state.name.as_str();
    let session_id = params["sessionId"].as_str().unwrap_or_default().to_string();
    match method {
        "initialize" => {
            let (title, caps) = match name {
                "deepseek" => (
                    "DeepSeek Harness",
                    json!({"promptCapabilities": {"image": false},
                    "sessionCapabilities": {"close": {}, "list": {}, "resume": {}}}),
                ),
                "antigravity" => (
                    "Google Antigravity",
                    json!({"loadSession": true,
                    "promptCapabilities": {"image": true, "audio": true, "embeddedContext": true},
                    "sessionCapabilities": {"list": {}, "resume": {}},
                    "auth": {"logout": {}}}),
                ),
                _ => (
                    "Claude Agent",
                    json!({"loadSession": true, "promptCapabilities": {"image": true, "embeddedContext": true},
                    "sessionCapabilities": {"list": {}, "close": {}, "resume": {}}}),
                ),
            };
            let mut result = json!({"protocolVersion": if name == "antigravity" { 2 } else { 1 },
                "agentCapabilities": caps, "authMethods": [],
                "agentInfo": {"name": if name == "antigravity" { "antigravity-acp".to_string() } else { format!("mock-{name}") },
                    "title": title,
                    "version": std::env::var("ATRIUM_MOCK_ACP_VERSION").unwrap_or_else(|_| "0.0.0".into())}});
            if name == "claude" {
                result["_meta"] = json!({"steering": {"supported": true}});
            }
            if name == "antigravity" {
                // The methods the installed agy advertises (T3's fixture).
                result["authMethods"] = json!([
                    {"id": "oauth-personal", "name": "Log in with Google", "description": "Log in with your Google account"},
                    {"id": "oauth-business", "name": "Log in with Gemini Enterprise", "description": "Log in with your Gemini Enterprise account"},
                    {"id": "gemini-api-key", "name": "Gemini API key", "description": "Use an API key with Gemini Developer API"},
                    {"id": "agent-platform", "name": "Gemini Enterprise Agent Platform", "description": "Use Gemini Enterprise Agent Platform with Application Default Credentials or an API key"}
                ]);
            }
            reply(out, id, result);
        }
        "authenticate" => {
            // No account is needed: the mock records the method so a test can
            // assert the sign-in path, and answers success. Antigravity's
            // browser flow announces its authorization link on stderr, as
            // `agy` does; the client captures it without reading credentials.
            if name == "antigravity"
                && params["methodId"]
                    .as_str()
                    .unwrap_or_default()
                    .starts_with("oauth-")
            {
                eprintln!(
                    "Open the following link to authenticate the ACP server: \
                     https://accounts.google.com/o/oauth2/v2/auth?response_type=code&state=mock-state&redirect_uri=http%3A%2F%2F127.0.0.1%3A4567%2F"
                );
            }
            reply(out, id, json!({}));
        }
        "logout" => {
            // ACP's `logout`; only advertised when `auth.logout` is present.
            if name == "antigravity" {
                reply(out, id, json!({}));
            } else {
                fail(out, id, -32601, "mock-acp has no credentials to clear");
            }
        }
        "session/new" => {
            let id_new = next(name);
            let session = Session {
                cwd: params["cwd"].as_str().unwrap_or("/").into(),
                title: None,
                history: Vec::new(),
                mode: default_mode(name).into(),
                model: "default".into(),
                effort: "high".into(),
            };
            let mut result = options(name, &session);
            result["sessionId"] = json!(id_new);
            state
                .sessions
                .lock()
                .unwrap()
                .insert(id_new.clone(), session);
            reply(out, id, result);
            update(
                out,
                &id_new,
                json!({"sessionUpdate": "available_commands_update", "availableCommands": [
                {"name": "review", "description": "Review the uncommitted changes"},
                {"name": "init", "description": "Write an instructions file for this repository"}]}),
            );
        }
        "session/list" => {
            let sessions = state.sessions.lock().unwrap();
            let list: Vec<Value> = sessions
                .iter()
                .filter(|(_, s)| !s.history.is_empty())
                .map(|(id, s)| json!({"sessionId": id, "cwd": s.cwd, "title": s.title, "updatedAt": "2026-11-15T10:00:00Z"}))
                .collect();
            reply(out, id, json!({"sessions": list}));
        }
        "session/load" | "session/resume" => {
            let found = state.sessions.lock().unwrap().get(&session_id).cloned();
            let Some(session) = found else {
                return fail(out, id, -32602, "no such session");
            };
            if method == "session/load" {
                // The history comes back as updates before the response.
                for (user, agent) in &session.history {
                    update(
                        out,
                        &session_id,
                        json!({"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": user}}),
                    );
                    update(
                        out,
                        &session_id,
                        json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": agent}}),
                    );
                }
            }
            reply(out, id, options(name, &session));
        }
        "session/set_mode" => {
            if let Some(s) = state.sessions.lock().unwrap().get_mut(&session_id) {
                s.mode = params["modeId"].as_str().unwrap_or_default().into();
            }
            reply(out, id, json!({}));
            update(
                out,
                &session_id,
                json!({"sessionUpdate": "current_mode_update", "currentModeId": params["modeId"]}),
            );
        }
        "session/set_config_option" => {
            let mut sessions = state.sessions.lock().unwrap();
            let Some(session) = sessions.get_mut(&session_id) else {
                return fail(out, id, -32602, "no such session");
            };
            let value = params["value"].as_str().unwrap_or_default().to_string();
            match params["configId"].as_str() {
                Some("mode") => session.mode = value,
                Some("model") => session.model = value,
                Some("reasoning_effort" | "thought_level" | "thinking") => session.effort = value,
                _ => {}
            }
            let result = options(name, session);
            drop(sessions);
            reply(out, id, result);
        }
        "session/close" => reply(out, id, json!({})),
        other => fail(
            out,
            id,
            -32601,
            &format!("mock-acp does not implement {other}"),
        ),
    }
}

/// Asks the client and waits for the answer.
fn ask(out: &Out, state: &State, method: &str, params: Value) -> Value {
    let key = state.next_request.fetch_add(1, Ordering::Relaxed) as i64;
    let (tx, rx) = mpsc::channel();
    state.answers.lock().unwrap().insert(key, tx);
    send(out, json!({"id": key, "method": method, "params": params}));
    rx.recv().unwrap_or(Value::Null)
}

fn stream(out: &Out, session: &str, kind: &str, text: &str, stop: &AtomicBool) -> bool {
    for chunk in text.split_inclusive(' ').collect::<Vec<_>>().chunks(2) {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        update(
            out,
            session,
            json!({"sessionUpdate": kind, "content": {"type": "text", "text": chunk.concat()}}),
        );
        pause(30);
    }
    true
}

/// What the person asked: T3 wraps a turn's first prompt in
/// `<t3_code_instructions>`/`<user_request>` and prefixes later prompts with
/// its sender note. The mock answers the same subject for both.
fn prompt_subject(text: &str) -> String {
    let body = match (text.find("<user_request>"), text.find("</user_request>")) {
        (Some(start), Some(end)) if end > start => &text[start + "<user_request>".len()..end],
        _ => text,
    };
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("[T3 Code: this message came through"))
        .unwrap_or_default()
        .to_string()
}

fn prompt(out: &Out, state: &State, id: Value, params: &Value) {
    let session_id = params["sessionId"].as_str().unwrap_or_default().to_string();
    let text = params["prompt"]
        .as_array()
        .and_then(|blocks| blocks.iter().find(|b| b["type"] == "text"))
        .and_then(|b| b["text"].as_str())
        .unwrap_or_default()
        .to_string();
    let attached = params["prompt"]
        .as_array()
        .map_or(0, |b| b.len().saturating_sub(1));
    // What the client actually sent, so a test can assert image/file blocks.
    let block_types = params["prompt"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .map(|block| match block["type"].as_str() {
                    Some("image") => {
                        format!("image:{}", block["mimeType"].as_str().unwrap_or_default())
                    }
                    Some("audio") => "audio".to_string(),
                    Some("resource") => "resource".to_string(),
                    Some("resource_link") => "resource_link".to_string(),
                    Some(other) => other.to_string(),
                    None => "unknown".to_string(),
                })
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    let Some(session) = state.sessions.lock().unwrap().get(&session_id).cloned() else {
        return fail(out, id, -32602, "no such session");
    };
    let stop = Arc::new(AtomicBool::new(false));
    state
        .cancels
        .lock()
        .unwrap()
        .insert(session_id.clone(), stop.clone());
    let hierarchy = std::env::var_os("ATRIUM_HIERARCHY_MOCK").is_some();
    // The subject is what the person asked, after T3's instruction
    // wrappers and sender note; the mock's answer echoes it.
    let subject = prompt_subject(&text);
    // Kickoff prose contains demo trigger words (for example "failed");
    // the hierarchy fixture exercises the protocol without triggering them.
    let lower = if hierarchy {
        String::new()
    } else {
        subject.to_lowercase()
    };
    let who = match state.name.as_str() {
        "deepseek" => "DeepSeek",
        "antigravity" => "Antigravity",
        _ => "Claude",
    };
    let thought = if lower.contains("attach") || block_types.matches(':').count() > 0 {
        format!(
            "The user asked about \"{}\" with {attached} attachment(s): {block_types}. I'll look at the code first.",
            subject
        )
    } else {
        format!(
            "The user asked about \"{}\" with {attached} attachment(s). I'll look at the code first.",
            subject
        )
    };
    let mut cancelled = !stream(out, &session_id, "agent_thought_chunk", &thought, &stop);
    if !cancelled && (lower.contains("plan") || session.mode == "plan") {
        let steps = ["Read the module", "Make the change", "Run the tests"];
        for done in 0..=steps.len() {
            let entries: Vec<Value> = steps
                .iter()
                .enumerate()
                .map(|(ix, s)| json!({"content": s, "priority": "medium",
                    "status": if ix < done { "completed" } else if ix == done { "in_progress" } else { "pending" }}))
                .collect();
            update(
                out,
                &session_id,
                json!({"sessionUpdate": "plan", "entries": entries}),
            );
            pause(250);
        }
    }
    if !cancelled && state.name == "antigravity" && lower.contains("question") {
        // agy shares `session/request_permission` for a native question.
        let call = json!({"toolCallId": "interaction_mock1", "title": "Which approach?",
            "kind": "other", "status": "pending"});
        let mut first = call.clone();
        first["sessionUpdate"] = json!("tool_call");
        update(out, &session_id, first);
        let answer = ask(
            out,
            state,
            "session/request_permission",
            json!({"sessionId": session_id, "toolCall": call, "options": [
            {"optionId": "stable-choice", "name": "Plan first", "kind": "allow_once"},
            {"optionId": "deny", "name": "Deny", "kind": "reject_once"}]}),
        );
        update(
            out,
            &session_id,
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "interaction_mock1", "status": "completed"}),
        );
        if answer["outcome"]["outcome"] != "selected" {
            cancelled = true;
        }
    }
    if !cancelled && lower.contains("subagent") {
        update(
            out,
            &session_id,
            json!({"sessionUpdate": "tool_call", "toolCallId": "sub-1", "title": "Run invoke_subagent",
                "kind": "other", "status": "in_progress",
                "rawInput": {"Role": "Researcher", "Prompt": "Read the README"}}),
        );
    }
    if !cancelled && lower.contains("read") {
        update(
            out,
            &session_id,
            json!({"sessionUpdate": "tool_call", "toolCallId": "read-1", "title": "Read src/lib.rs",
            "kind": "read", "status": "in_progress", "locations": [{"path": "src/lib.rs", "line": 1}], "rawInput": {"path": "src/lib.rs"}}),
        );
        pause(300);
        update(
            out,
            &session_id,
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "read-1", "status": "completed",
            "content": [{"type": "content", "content": {"type": "text", "text": "//! A library.\npub fn add(a: i32, b: i32) -> i32 { a + b }"}}]}),
        );
    }
    if !cancelled && lower.contains("run") {
        let call = json!({"toolCallId": "run-1", "title": "Run the test suite", "kind": "execute", "status": "pending",
            "rawInput": {"command": "cargo test --workspace", "cwd": session.cwd}});
        let mut first = call.clone();
        first["sessionUpdate"] = json!("tool_call");
        update(out, &session_id, first);
        let answer = ask(
            out,
            state,
            "session/request_permission",
            json!({"sessionId": session_id, "toolCall": call, "options": [
            {"optionId": "yes", "name": "Allow", "kind": "allow_once"},
            {"optionId": "always", "name": "Always allow", "kind": "allow_always"},
            {"optionId": "no", "name": "Reject", "kind": "reject_once"}]}),
        );
        let allowed =
            answer["outcome"]["outcome"] == "selected" && answer["outcome"]["optionId"] != "no";
        if allowed {
            update(
                out,
                &session_id,
                json!({"sessionUpdate": "tool_call_update", "toolCallId": "run-1", "status": "in_progress"}),
            );
            pause(500);
            update(
                out,
                &session_id,
                json!({"sessionUpdate": "tool_call_update", "toolCallId": "run-1", "status": "completed",
                "content": [{"type": "content", "content": {"type": "text", "text": "running 9 tests\ntest result: ok. 9 passed"}}],
                "rawOutput": {"exit_code": 0}}),
            );
        } else {
            update(
                out,
                &session_id,
                json!({"sessionUpdate": "tool_call_update", "toolCallId": "run-1", "status": "failed",
                "content": [{"type": "content", "content": {"type": "text", "text": "The user declined."}}]}),
            );
            cancelled = answer["outcome"]["outcome"] == "cancelled";
        }
    }
    if !cancelled && lower.contains("edit") {
        update(
            out,
            &session_id,
            json!({"sessionUpdate": "tool_call", "toolCallId": "edit-1", "title": "Edit src/lib.rs", "kind": "edit",
            "status": "completed", "locations": [{"path": "src/lib.rs"}], "content": [{"type": "diff", "path": "src/lib.rs",
            "oldText": "//! A library.\npub fn add(a: i32, b: i32) -> i32 { a + b }",
            "newText": "//! A small library.\n\n/// Adds two numbers.\npub fn add(a: i32, b: i32) -> i32 { a + b }"}]}),
        );
    }
    let answer = if session.mode == "plan" {
        format!("**{who}** is in plan mode: here is the plan above. Switch the mode to act on it.")
    } else if lower.contains("write something long") {
        (1..=10)
            .map(|n| format!("{n}. {who} keeps writing so there is time to stop it.\n"))
            .collect()
    } else {
        format!(
            "**{who}** here. I looked at *{}*.\n\n```rust\nfn main() {{\n    println!(\"hello from {}\");\n}}\n```",
            subject, state.name
        )
    };
    let answer = if hierarchy && text.starts_with("[Atrium] New notices for you:") {
        format!("Fixture received:\n{text}")
    } else {
        answer
    };
    if !cancelled {
        cancelled = !stream(out, &session_id, "agent_message_chunk", &answer, &stop);
    }
    update(out, &session_id, {
        let turns = state
            .sessions
            .lock()
            .unwrap()
            .get(&session_id)
            .map_or(0, |s| s.history.len() as u64);
        let mut usage = json!({"sessionUpdate": "usage_update",
                "used": 18_400 + 6_200 * turns, "size": 200_000});
        if state.name == "claude" {
            usage["cost"] = json!({"amount": 0.042 * (turns + 1) as f64, "currency": "USD"});
        }
        if std::env::var_os("ATRIUM_MOCK_ACP_RATE_LIMITS").is_some() {
            usage["_meta"] = json!({"claude/rateLimit": {
                "five_hour": {"utilization": 42.0, "resets_at": "2026-11-24T12:00:00Z"},
                "seven_day": {"utilization": 17.0, "resets_at": "2026-11-30T00:00:00Z"}
            }});
        }
        usage
    });
    {
        let mut sessions = state.sessions.lock().unwrap();
        if let Some(s) = sessions.get_mut(&session_id) {
            if s.title.is_none() {
                s.title = Some(subject.clone());
                update(
                    out,
                    &session_id,
                    json!({"sessionUpdate": "session_info_update", "title": s.title}),
                );
            }
            s.history.push((text.clone(), answer.clone()));
        }
    }
    state.cancels.lock().unwrap().remove(&session_id);
    reply(
        out,
        id,
        json!({
            "stopReason": if cancelled || stop.load(Ordering::Relaxed) { "cancelled" } else { "end_turn" },
            // This turn's counts, as claude-agent-acp reports them. agy also
            // reports thoughts separately, which T3 folds into output.
            "usage": if state.name == "antigravity" {
                json!({"inputTokens": 1_200, "cachedReadTokens": 14_800, "cachedWriteTokens": 900,
                    "outputTokens": 640, "thoughtTokens": 300, "totalTokens": 17_840})
            } else {
                json!({"inputTokens": 1_200, "cachedReadTokens": 14_800, "cachedWriteTokens": 900,
                    "outputTokens": 640, "totalTokens": 17_540})
            },
        }),
    );
}
