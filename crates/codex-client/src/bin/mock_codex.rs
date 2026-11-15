//! A stand-in for `codex app-server` that speaks the same v2 protocol with a
//! scripted agent. It lets the UI and the client be developed and tested
//! without a Codex account:
//!
//! ```sh
//! CODEX_BIN=target/debug/mock-codex cargo run -p agentdesk-app
//! ```
//!
//! Every message gets a streamed, reasoned answer. A message containing
//! "run" asks to run a command first (and waits for approval); one
//! containing "edit" proposes a file change.

use std::{
    collections::HashMap,
    io::{self, BufRead, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

type Out = Arc<Mutex<io::Stdout>>;

fn send(out: &Out, message: Value) {
    let mut out = out.lock().unwrap();
    let _ = writeln!(out, "{message}");
    let _ = out.flush();
}

fn notify(out: &Out, method: &str, params: Value) {
    send(out, json!({ "method": method, "params": params }));
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

static SEQ: AtomicU64 = AtomicU64::new(1);
fn next_id(prefix: &str) -> String {
    format!("{prefix}-{:04}", SEQ.fetch_add(1, Ordering::Relaxed))
}

#[derive(Clone)]
struct Thread {
    id: String,
    preview: String,
    cwd: String,
    created: i64,
    updated: i64,
    turns: Vec<Value>,
}

fn thread_json(t: &Thread, with_turns: bool) -> Value {
    json!({
        "id": t.id, "sessionId": t.id, "preview": t.preview, "name": null, "cwd": t.cwd,
        "model": "mock-1", "modelProvider": "mock", "createdAt": t.created, "updatedAt": t.updated,
        "recencyAt": t.updated, "status": {"type": "idle"}, "ephemeral": false,
        "turns": if with_turns { Value::Array(t.turns.clone()) } else { json!([]) },
    })
}

struct State {
    threads: Mutex<Vec<Thread>>,
    /// Waiters for answers to our requests, by request id.
    answers: Mutex<HashMap<i64, mpsc::Sender<Value>>>,
    next_request: AtomicU64,
    interrupts: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

fn main() {
    let out: Out = Arc::new(Mutex::new(io::stdout()));
    let state = Arc::new(State {
        threads: Mutex::new(Vec::new()),
        answers: Mutex::new(HashMap::new()),
        next_request: AtomicU64::new(0),
        interrupts: Mutex::new(HashMap::new()),
    });
    // Two stored threads so the sidebar has history.
    {
        let mut threads = state.threads.lock().unwrap();
        for (ago, text, answer) in [
            (
                3600 * 26,
                "How is the workspace laid out?",
                "Three crates: **core**, **codex-client** and **app**.",
            ),
            (
                600,
                "Explain the transcript reducer",
                "It folds events into a flat list of rows.",
            ),
        ] {
            let id = next_id("thread");
            threads.push(Thread {
                id: id.clone(),
                preview: text.into(),
                cwd: std::env::current_dir().unwrap().display().to_string(),
                created: now() - ago,
                updated: now() - ago,
                turns: vec![json!({
                    "id": next_id("turn"), "status": "completed", "items": [
                        {"type": "userMessage", "id": next_id("item"), "content": [{"type": "text", "text": text, "text_elements": []}]},
                        {"type": "agentMessage", "id": next_id("item"), "text": answer},
                    ]
                })],
            });
        }
    }
    eprintln!("mock-codex: ready");
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = message.get("id").cloned();
        let method = message["method"].as_str().map(str::to_owned);
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match (method, id) {
            (Some(method), Some(id)) => handle(&method, id, params, &out, &state),
            (Some(_notification), None) => {}
            (None, Some(id)) => {
                // An answer to one of our requests.
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

fn reply(out: &Out, id: Value, result: Value) {
    send(out, json!({ "id": id, "result": result }));
}

fn handle(method: &str, id: Value, params: Value, out: &Out, state: &Arc<State>) {
    match method {
        "initialize" => reply(
            out,
            id,
            json!({"userAgent": "mock-codex/0.0.0", "codexHome": "/tmp/mock-codex", "platformFamily": "unix", "platformOs": std::env::consts::OS}),
        ),
        "account/read" => reply(
            out,
            id,
            json!({"account": {"type": "apiKey"}, "userId": null, "requiresOpenaiAuth": true}),
        ),
        "model/list" => reply(
            out,
            id,
            json!({"data": [
                {"id": "mock-1", "model": "mock-1", "displayName": "Mock 1", "description": "Fast scripted answers", "hidden": false, "isDefault": true},
                {"id": "mock-1-pro", "model": "mock-1-pro", "displayName": "Mock 1 Pro", "description": "Same script, more gravitas", "hidden": false, "isDefault": false}
            ], "nextCursor": null}),
        ),
        "thread/list" => {
            let mut threads = state.threads.lock().unwrap().clone();
            threads.sort_by_key(|t| std::cmp::Reverse(t.updated));
            let data: Vec<Value> = threads.iter().map(|t| thread_json(t, false)).collect();
            reply(
                out,
                id,
                json!({"data": data, "nextCursor": null, "backwardsCursor": null}),
            );
        }
        "thread/start" => {
            let thread = Thread {
                id: next_id("thread"),
                preview: String::new(),
                cwd: params["cwd"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| std::env::current_dir().unwrap().display().to_string()),
                created: now(),
                updated: now(),
                turns: Vec::new(),
            };
            let body = thread_json(&thread, false);
            state.threads.lock().unwrap().push(thread);
            reply(
                out,
                id,
                json!({"thread": body.clone(), "model": "mock-1", "modelProvider": "mock", "cwd": body["cwd"]}),
            );
            notify(out, "thread/started", json!({"thread": body}));
        }
        "thread/resume" | "thread/read" => {
            let wanted = params["threadId"].as_str().unwrap_or_default();
            let found = state
                .threads
                .lock()
                .unwrap()
                .iter()
                .find(|t| t.id == wanted)
                .cloned();
            match found {
                Some(thread) => reply(
                    out,
                    id,
                    json!({"thread": thread_json(&thread, true), "model": "mock-1"}),
                ),
                None => send(
                    out,
                    json!({"id": id, "error": {"code": -32602, "message": format!("no thread {wanted}")}}),
                ),
            }
        }
        "thread/archive" => {
            let wanted = params["threadId"].as_str().unwrap_or_default().to_string();
            state.threads.lock().unwrap().retain(|t| t.id != wanted);
            reply(out, id, json!({}));
        }
        "turn/start" => {
            let thread_id = params["threadId"].as_str().unwrap_or_default().to_string();
            let text = params["input"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let turn_id = next_id("turn");
            reply(
                out,
                id,
                json!({"turn": {"id": turn_id, "status": "inProgress", "items": []}}),
            );
            let stop = Arc::new(AtomicBool::new(false));
            state
                .interrupts
                .lock()
                .unwrap()
                .insert(turn_id.clone(), stop.clone());
            let (out, state) = (out.clone(), state.clone());
            thread::spawn(move || run_turn(&out, &state, &thread_id, &turn_id, &text, &stop));
        }
        "turn/interrupt" => {
            if let Some(flag) = state
                .interrupts
                .lock()
                .unwrap()
                .get(params["turnId"].as_str().unwrap_or_default())
            {
                flag.store(true, Ordering::Relaxed);
            }
            reply(out, id, json!({}));
        }
        other => send(
            out,
            json!({"id": id, "error": {"code": -32601, "message": format!("mock-codex does not implement {other}")}}),
        ),
    }
}

/// Sends a request to the client and waits for its answer.
fn ask(out: &Out, state: &State, method: &str, params: Value) -> Value {
    let key = state.next_request.fetch_add(1, Ordering::Relaxed) as i64;
    let (tx, rx) = mpsc::channel();
    state.answers.lock().unwrap().insert(key, tx);
    send(out, json!({"id": key, "method": method, "params": params}));
    let answer = rx.recv().unwrap_or(Value::Null);
    notify(
        out,
        "serverRequest/resolved",
        json!({"threadId": params["threadId"], "requestId": key}),
    );
    answer
}

fn pause(ms: u64) {
    thread::sleep(Duration::from_millis(ms));
}

fn stream(
    out: &Out,
    method: &str,
    ids: &Value,
    item_id: &str,
    text: &str,
    stop: &AtomicBool,
) -> bool {
    let words: Vec<&str> = text.split_inclusive(' ').collect();
    for chunk in words.chunks(2) {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let mut params = ids.clone();
        params["itemId"] = json!(item_id);
        params["delta"] = json!(chunk.concat());
        notify(out, method, params);
        pause(35);
    }
    true
}

fn run_turn(
    out: &Out,
    state: &State,
    thread_id: &str,
    turn_id: &str,
    text: &str,
    stop: &AtomicBool,
) {
    let ids = json!({"threadId": thread_id, "turnId": turn_id});
    let mut items: Vec<Value> = Vec::new();
    let with = |extra: Value| {
        let mut params = ids.clone();
        for (k, v) in extra.as_object().unwrap() {
            params[k] = v.clone();
        }
        params
    };
    notify(
        out,
        "turn/started",
        with(json!({"turn": {"id": turn_id, "status": "inProgress", "items": []}})),
    );
    let user = json!({"type": "userMessage", "id": next_id("item"), "clientId": null, "content": [{"type": "text", "text": text, "text_elements": []}]});
    notify(
        out,
        "item/started",
        with(json!({"item": user, "startedAtMs": 0})),
    );
    notify(
        out,
        "item/completed",
        with(json!({"item": user, "completedAtMs": 0})),
    );
    items.push(user);

    // Reasoning.
    let reasoning_id = next_id("item");
    notify(
        out,
        "item/started",
        with(
            json!({"item": {"type": "reasoning", "id": reasoning_id, "summary": [], "content": []}, "startedAtMs": 0}),
        ),
    );
    let thought = format!(
        "**Reading the request** The user wrote {} characters. I'll answer plainly and show what I did.",
        text.chars().count()
    );
    let mut interrupted = !stream(
        out,
        "item/reasoning/summaryTextDelta",
        &ids,
        &reasoning_id,
        &thought,
        stop,
    );
    let reasoning =
        json!({"type": "reasoning", "id": reasoning_id, "summary": [thought], "content": []});
    notify(
        out,
        "item/completed",
        with(json!({"item": reasoning, "completedAtMs": 0})),
    );
    items.push(reasoning);

    let lower = text.to_lowercase();
    if !interrupted && lower.contains("run") {
        let cmd_id = next_id("item");
        let command = "cargo test --workspace";
        let cwd = "/workspace";
        let mut item = json!({"type": "commandExecution", "id": cmd_id, "command": command, "cwd": cwd, "status": "inProgress",
            "aggregatedOutput": null, "exitCode": null, "durationMs": null, "commandActions": [], "source": "agent", "processId": null});
        notify(
            out,
            "item/started",
            with(json!({"item": item, "startedAtMs": 0})),
        );
        let answer = ask(
            out,
            state,
            "item/commandExecution/requestApproval",
            with(json!({
                "itemId": cmd_id, "kind": "command", "command": command, "cwd": cwd, "reason": "Run the test suite to check the change", "startedAtMs": 0, "environmentId": null
            })),
        );
        let decision = answer["decision"].as_str().unwrap_or("decline").to_string();
        if decision.starts_with("accept") {
            let mut output = String::new();
            for line in [
                "   Compiling agentdesk-core v0.1.0\n",
                "    Finished test profile in 2.31s\n",
                "running 6 tests\n",
                "test result: ok. 6 passed; 0 failed\n",
            ] {
                pause(250);
                output.push_str(line);
                notify(
                    out,
                    "item/commandExecution/outputDelta",
                    with(json!({"itemId": cmd_id, "delta": line})),
                );
            }
            item["status"] = json!("completed");
            item["exitCode"] = json!(0);
            item["durationMs"] = json!(1000);
            item["aggregatedOutput"] = json!(output);
        } else {
            item["status"] = json!("declined");
            interrupted = decision == "cancel";
        }
        notify(
            out,
            "item/completed",
            with(json!({"item": item, "completedAtMs": 0})),
        );
        items.push(item);
    }

    if !interrupted && lower.contains("edit") {
        let change = json!({"type": "fileChange", "id": next_id("item"), "status": "completed", "changes": [
            {"path": "src/lib.rs", "kind": {"type": "update", "move_path": null}, "diff": "@@ -1,3 +1,4 @@\n //! A library.\n+//! Now with docs.\n pub fn add(a: i32, b: i32) -> i32 {\n     a + b\n"}
        ]});
        notify(
            out,
            "item/started",
            with(json!({"item": change, "startedAtMs": 0})),
        );
        pause(300);
        notify(
            out,
            "item/completed",
            with(json!({"item": change, "completedAtMs": 0})),
        );
        items.push(change);
    }

    let answer_id = next_id("item");
    let answer = format!(
        "Here is what I found about **{}**.\n\n1. The request reached the mock agent.\n2. It streamed this answer word by word.\n\n```rust\nfn main() {{\n    println!(\"hello from mock-codex\");\n}}\n```\n\nAsk me to *run* or *edit* something to see approvals and file changes.",
        text.lines().next().unwrap_or("your message")
    );
    if !interrupted {
        notify(
            out,
            "item/started",
            with(
                json!({"item": {"type": "agentMessage", "id": answer_id, "text": ""}, "startedAtMs": 0}),
            ),
        );
        interrupted = !stream(
            out,
            "item/agentMessage/delta",
            &ids,
            &answer_id,
            &answer,
            stop,
        );
        let done = json!({"type": "agentMessage", "id": answer_id, "text": if interrupted { "" } else { answer.as_str() }});
        notify(
            out,
            "item/completed",
            with(json!({"item": done, "completedAtMs": 0})),
        );
        items.push(done);
    }

    notify(
        out,
        "thread/tokenUsage/updated",
        with(json!({"tokenUsage": {
        "total": {"totalTokens": 1234, "inputTokens": 1000, "cachedInputTokens": 0, "cacheWriteInputTokens": 0, "outputTokens": 234, "reasoningOutputTokens": 40},
        "last": {"totalTokens": 600, "inputTokens": 500, "cachedInputTokens": 0, "cacheWriteInputTokens": 0, "outputTokens": 100, "reasoningOutputTokens": 20},
        "modelContextWindow": 200000}})),
    );
    let status = if interrupted {
        "interrupted"
    } else {
        "completed"
    };
    let turn = json!({"id": turn_id, "status": status, "items": items, "error": null});
    notify(out, "turn/completed", with(json!({"turn": turn})));

    let mut threads = state.threads.lock().unwrap();
    if let Some(thread) = threads.iter_mut().find(|t| t.id == thread_id) {
        if thread.preview.is_empty() {
            thread.preview = text.to_string();
            notify(
                out,
                "thread/name/updated",
                json!({"threadId": thread_id, "threadName": text.lines().next().unwrap_or("")}),
            );
        }
        thread.updated = now();
        thread.turns.push(turn);
    }
}
