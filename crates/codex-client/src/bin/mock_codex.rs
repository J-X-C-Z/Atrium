//! A stand-in for `codex app-server` that speaks the same v2 protocol with a
//! scripted agent. It lets the UI and the client be developed and tested
//! without a Codex account:
//!
//! ```sh
//! CODEX_BIN=target/debug/mock-codex cargo run -p atrium-app
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
    // Unique across mock processes too, as real ids are: two devices'
    // mocks must not hand out the same thread id.
    format!(
        "{prefix}-{:x}-{:04}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

#[derive(Clone)]
struct Thread {
    id: String,
    preview: String,
    cwd: String,
    created: i64,
    updated: i64,
    turns: Vec<Value>,
    /// A throwaway fork, such as one that predicts the next message.
    ephemeral: bool,
}

fn thread_json(t: &Thread, with_turns: bool) -> Value {
    json!({
        "id": t.id, "sessionId": t.id, "preview": t.preview, "name": null, "cwd": t.cwd,
        "model": "mock-1", "modelProvider": "mock", "createdAt": t.created, "updatedAt": t.updated,
        "recencyAt": t.updated, "status": {"type": "idle"}, "ephemeral": t.ephemeral,
        "turns": if with_turns { Value::Array(t.turns.clone()) } else { json!([]) },
    })
}

struct State {
    threads: Mutex<Vec<Thread>>,
    /// Waiters for answers to our requests, by request id.
    answers: Mutex<HashMap<i64, mpsc::Sender<Value>>>,
    next_request: AtomicU64,
    interrupts: Mutex<HashMap<String, Arc<AtomicBool>>>,
    goals: Mutex<HashMap<String, Value>>,
    signed_in: AtomicBool,
    /// Share of the five-hour window used, in tenths of a percent.
    used: AtomicU64,
    /// The running turn of each thread, for steering.
    running: Mutex<HashMap<String, String>>,
    /// Messages steered into running turns, by turn.
    steered: Mutex<HashMap<String, Vec<String>>>,
}

fn main() {
    let out: Out = Arc::new(Mutex::new(io::stdout()));
    let state = Arc::new(State {
        threads: Mutex::new(Vec::new()),
        answers: Mutex::new(HashMap::new()),
        next_request: AtomicU64::new(0),
        interrupts: Mutex::new(HashMap::new()),
        goals: Mutex::new(HashMap::new()),
        signed_in: AtomicBool::new(true),
        used: AtomicU64::new(230),
        running: Mutex::new(HashMap::new()),
        steered: Mutex::new(HashMap::new()),
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
                ephemeral: false,
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
        "account/read" => {
            let account = if state.signed_in.load(Ordering::Relaxed) {
                json!({"type": "chatgpt", "email": "you@example.com", "planType": "pro", "workspaceName": null})
            } else {
                Value::Null
            };
            reply(
                out,
                id,
                json!({"account": account, "userId": null, "requiresOpenaiAuth": true}),
            );
        }
        "account/rateLimits/read" => {
            if !state.signed_in.load(Ordering::Relaxed) {
                send(
                    out,
                    json!({"id": id, "error": {"code": -32600, "message": "codex account authentication required to read rate limits"}}),
                );
            } else {
                let snapshot = rate_snapshot(state);
                reply(
                    out,
                    id,
                    json!({"ordinaryUsageAllowed": true, "rateLimits": snapshot, "rateLimitsByLimitId": {"codex": snapshot}, "rateLimitResetCredits": null, "accountId": null}),
                );
            }
        }
        "account/login/start" => {
            let login_id = next_id("login");
            reply(
                out,
                id,
                json!({"type": "chatgpt", "loginId": login_id, "authUrl": "https://auth.openai.com/oauth/authorize?mock=1"}),
            );
            let (out, state) = (out.clone(), state.clone());
            thread::spawn(move || {
                pause(1500);
                state.signed_in.store(true, Ordering::Relaxed);
                notify(
                    &out,
                    "account/login/completed",
                    json!({"loginId": login_id, "success": true, "error": null, "onboardingEntrypoint": null}),
                );
                notify(
                    &out,
                    "account/updated",
                    json!({"authMode": "chatgpt", "planType": "pro"}),
                );
            });
        }
        "account/login/cancel" => reply(out, id, json!({"status": "canceled"})),
        "account/logout" => {
            state.signed_in.store(false, Ordering::Relaxed);
            reply(out, id, json!({}));
            notify(
                out,
                "account/updated",
                json!({"authMode": null, "planType": null}),
            );
        }
        "memory/status" => reply(
            out,
            id,
            json!({"v2ConsolidatedThreads": 3, "v2Ready": true}),
        ),
        "thread/memoryMode/set" | "thread/unsubscribe" => reply(out, id, json!({})),
        "collaborationMode/list" => reply(
            out,
            id,
            json!({"data": [
            {"name": "Plan", "mode": "plan", "model": null, "reasoning_effort": "medium"},
            {"name": "Default", "mode": "default", "model": null, "reasoning_effort": null}]}),
        ),
        "thread/prediction/request" => send(
            out,
            // As Codex 0.169 answers: the client falls back to a prediction fork.
            json!({"id": id, "error": {"code": -32601, "message": "thread/prediction/request is not implemented yet"}}),
        ),
        "thread/fork" => {
            let wanted = params["threadId"].as_str().unwrap_or_default().to_string();
            let found = state
                .threads
                .lock()
                .unwrap()
                .iter()
                .find(|t| t.id == wanted)
                .cloned();
            let Some(parent) = found else {
                send(
                    out,
                    json!({"id": id, "error": {"code": -32600, "message": format!("no thread {wanted}")}}),
                );
                return;
            };
            let ephemeral = params["ephemeral"].as_bool().unwrap_or(false);
            let mut turns = parent.turns.clone();
            if let Some(last) = params["lastTurnId"].as_str()
                && let Some(ix) = turns.iter().position(|t| t["id"] == last)
            {
                turns.truncate(ix + 1);
            }
            let fork = Thread {
                id: next_id(if ephemeral { "fork" } else { "thread" }),
                preview: if ephemeral {
                    String::new()
                } else {
                    format!("{} (branch)", parent.preview)
                },
                cwd: parent.cwd.clone(),
                created: now(),
                updated: now(),
                turns,
                ephemeral,
            };
            let body = thread_json(&fork, false);
            state.threads.lock().unwrap().push(fork);
            reply(
                out,
                id,
                json!({"thread": body.clone(), "model": "mock-1", "modelProvider": "mock", "cwd": body["cwd"]}),
            );
            notify(out, "thread/started", json!({"thread": body}));
        }
        "thread/revert" => {
            let wanted = params["threadId"].as_str().unwrap_or_default().to_string();
            let before = params["beforeTurnId"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let mut threads = state.threads.lock().unwrap();
            let Some(thread) = threads.iter_mut().find(|t| t.id == wanted) else {
                send(
                    out,
                    json!({"id": id, "error": {"code": -32600, "message": "no such thread"}}),
                );
                return;
            };
            if let Some(ix) = thread.turns.iter().position(|t| t["id"] == before.as_str()) {
                thread.turns.truncate(ix);
            }
            let body = thread_json(thread, false);
            drop(threads);
            reply(
                out,
                id,
                json!({"thread": body, "turnsBackwardsCursor": null, "itemsBackwardsCursor": null}),
            );
            notify(out, "thread/reverted", json!({"threadId": wanted}));
        }
        "thread/search" => {
            let term = params["searchTerm"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase();
            let threads = state.threads.lock().unwrap().clone();
            let data: Vec<Value> = threads
                .iter()
                .filter(|t| !t.ephemeral)
                .filter_map(|t| {
                    let texts =
                        std::iter::once(t.preview.clone()).chain(t.turns.iter().flat_map(|turn| {
                            turn["items"]
                                .as_array()
                                .cloned()
                                .unwrap_or_default()
                                .into_iter()
                                .filter_map(|item| {
                                    item["text"].as_str().map(str::to_owned).or_else(|| {
                                        item["content"][0]["text"].as_str().map(str::to_owned)
                                    })
                                })
                        }));
                    let hit = texts
                        .into_iter()
                        .find(|text| text.to_lowercase().contains(&term))?;
                    Some(json!({"thread": thread_json(t, false), "snippet": hit}))
                })
                .collect();
            reply(
                out,
                id,
                json!({"data": data, "nextCursor": null, "backwardsCursor": null}),
            );
        }
        "thread/goal/get" => {
            let wanted = params["threadId"].as_str().unwrap_or_default();
            let goal = state
                .goals
                .lock()
                .unwrap()
                .get(wanted)
                .cloned()
                .unwrap_or(Value::Null);
            reply(out, id, json!({"goal": goal}));
        }
        "thread/goal/set" => {
            let thread_id = params["threadId"].as_str().unwrap_or_default().to_string();
            let mut goals = state.goals.lock().unwrap();
            let goal = goals.entry(thread_id.clone()).or_insert_with(|| {
                json!({
                "threadId": thread_id, "objective": "", "status": "active", "tokenBudget": null,
                "tokensUsed": 0, "timeUsedSeconds": 0, "createdAt": now(), "updatedAt": now()})
            });
            for key in ["objective", "status", "tokenBudget"] {
                if !params[key].is_null() {
                    goal[key] = params[key].clone();
                }
            }
            goal["updatedAt"] = json!(now());
            let goal = goal.clone();
            drop(goals);
            reply(out, id, json!({"goal": goal}));
            notify(
                out,
                "thread/goal/updated",
                json!({"threadId": thread_id, "turnId": null, "goal": goal}),
            );
        }
        "thread/goal/clear" => {
            let thread_id = params["threadId"].as_str().unwrap_or_default().to_string();
            state.goals.lock().unwrap().remove(&thread_id);
            reply(out, id, json!({}));
            notify(out, "thread/goal/cleared", json!({"threadId": thread_id}));
        }
        "turn/steer" => {
            let thread_id = params["threadId"].as_str().unwrap_or_default().to_string();
            let expected = params["expectedTurnId"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let running = state.running.lock().unwrap().get(&thread_id).cloned();
            match running {
                Some(turn) if turn == expected => {
                    let text = params["input"][0]["text"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    reply(out, id, json!({"turnId": turn}));
                    let user = json!({"type": "userMessage", "id": next_id("item"), "clientId": null, "content": params["input"]});
                    let ids = json!({"threadId": thread_id, "turnId": turn, "item": user, "startedAtMs": 0, "completedAtMs": 0});
                    notify(out, "item/started", ids.clone());
                    notify(out, "item/completed", ids);
                    state
                        .steered
                        .lock()
                        .unwrap()
                        .entry(turn)
                        .or_default()
                        .push(text);
                }
                _ => send(
                    out,
                    json!({"id": id, "error": {"code": -32600, "message": "no active turn to steer"}}),
                ),
            }
        }
        "model/list" => reply(
            out,
            id,
            json!({"data": [
                {"id": "mock-1", "model": "mock-1", "displayName": "Mock 1", "description": "Fast scripted answers", "hidden": false, "isDefault": true,
                 "supportedReasoningEfforts": [{"reasoningEffort": "low", "description": ""}, {"reasoningEffort": "medium", "description": ""}, {"reasoningEffort": "high", "description": ""}], "defaultReasoningEffort": "medium",
                 "serviceTiers": [{"id": "priority", "name": "Fast", "description": "2x speed, increased usage"}]},
                {"id": "mock-1-pro", "model": "mock-1-pro", "displayName": "Mock 1 Pro", "description": "Same script, more gravitas", "hidden": false, "isDefault": false,
                 "supportedReasoningEfforts": [{"reasoningEffort": "medium", "description": ""}, {"reasoningEffort": "high", "description": ""}, {"reasoningEffort": "xhigh", "description": ""}], "defaultReasoningEffort": "high",
                 "serviceTiers": [{"id": "priority", "name": "Fast", "description": "2x speed, increased usage"}]}
            ], "nextCursor": null}),
        ),
        "thread/list" => {
            let mut threads = state.threads.lock().unwrap().clone();
            threads.sort_by_key(|t| std::cmp::Reverse(t.updated));
            let data: Vec<Value> = threads
                .iter()
                .filter(|t| !t.ephemeral)
                .map(|t| thread_json(t, false))
                .collect();
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
                ephemeral: params["ephemeral"].as_bool().unwrap_or(false),
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
        "turn/start" | "review/start" => {
            let thread_id = params["threadId"].as_str().unwrap_or_default().to_string();
            let input = if method == "review/start" {
                let what = match params["target"]["type"].as_str() {
                    Some("custom") => params["target"]["instructions"]
                        .as_str()
                        .unwrap_or("")
                        .to_string(),
                    _ => "review the uncommitted changes".to_string(),
                };
                json!([{"type": "text", "text": format!("/review {what}"), "text_elements": []}])
            } else {
                params["input"].clone()
            };
            let turn_id = next_id("turn");
            let turn = json!({"id": turn_id, "status": "inProgress", "items": []});
            if method == "review/start" {
                reply(out, id, json!({"turn": turn, "reviewThreadId": thread_id}));
            } else {
                reply(out, id, json!({"turn": turn}));
            }
            let stop = Arc::new(AtomicBool::new(false));
            state
                .interrupts
                .lock()
                .unwrap()
                .insert(turn_id.clone(), stop.clone());
            let ephemeral = state
                .threads
                .lock()
                .unwrap()
                .iter()
                .any(|t| t.id == thread_id && t.ephemeral);
            let opts = Opts {
                plan: params["collaborationMode"]["mode"] == "plan",
                fast: params["serviceTier"] == "priority",
            };
            state
                .running
                .lock()
                .unwrap()
                .insert(thread_id.clone(), turn_id.clone());
            let (out, state) = (out.clone(), state.clone());
            thread::spawn(move || {
                if ephemeral {
                    run_prediction(&out, &state, &thread_id, &turn_id);
                } else {
                    run_turn(&out, &state, &thread_id, &turn_id, &input, &stop, opts);
                }
                state.running.lock().unwrap().remove(&thread_id);
            });
        }
        "thread/name/set" => {
            let wanted = params["threadId"].as_str().unwrap_or_default().to_string();
            let name = params["name"].as_str().unwrap_or_default().to_string();
            if let Some(thread) = state
                .threads
                .lock()
                .unwrap()
                .iter_mut()
                .find(|t| t.id == wanted)
            {
                thread.preview = name.clone();
            }
            reply(out, id, json!({}));
            notify(
                out,
                "thread/name/updated",
                json!({"threadId": wanted, "threadName": name}),
            );
        }
        "thread/compact/start" => {
            let thread_id = params["threadId"].as_str().unwrap_or_default().to_string();
            reply(out, id, json!({}));
            let turn_id = next_id("turn");
            let ids = json!({"threadId": thread_id, "turnId": turn_id});
            let mut started = ids.clone();
            started["turn"] = json!({"id": turn_id, "status": "inProgress", "items": []});
            notify(out, "turn/started", started);
            pause(400);
            let item = json!({"type": "contextCompaction", "id": next_id("item")});
            let mut done = ids.clone();
            done["item"] = item.clone();
            notify(out, "item/completed", done);
            let mut completed = ids.clone();
            completed["turn"] = json!({"id": turn_id, "status": "completed", "items": [item], "error": null, "durationMs": 400});
            notify(out, "turn/completed", completed);
        }
        "fuzzyFileSearch" => {
            let query = params["query"].as_str().unwrap_or_default().to_lowercase();
            let root = params["roots"][0].as_str().unwrap_or(".").to_string();
            let mut files = Vec::new();
            walk(
                std::path::Path::new(&root),
                std::path::Path::new(&root),
                0,
                &mut files,
            );
            let mut found: Vec<Value> = files
                .into_iter()
                .filter(|path| path.to_lowercase().contains(&query))
                .take(30)
                .map(|path| {
                    let name = path.rsplit('/').next().unwrap_or(&path).to_string();
                    json!({"root": root, "path": path, "match_type": "file", "file_name": name, "score": 1, "indices": null})
                })
                .collect();
            found.sort_by_key(|f| f["path"].as_str().map(str::len).unwrap_or(0));
            reply(out, id, json!({"files": found}));
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

/// Files under `dir`, relative to `root`, skipping hidden and build folders.
fn walk(root: &std::path::Path, dir: &std::path::Path, depth: usize, files: &mut Vec<String>) {
    if depth > 5 || files.len() > 2000 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, depth + 1, files);
        } else if let Ok(relative) = path.strip_prefix(root) {
            files.push(relative.display().to_string());
        }
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

/// How a turn was asked to run.
#[derive(Clone, Copy, Default)]
struct Opts {
    plan: bool,
    fast: bool,
}

thread_local! {
    /// Milliseconds between streamed chunks on this thread: Fast halves it.
    static PACE: std::cell::Cell<u64> = const { std::cell::Cell::new(35) };
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
        pause(PACE.with(|pace| pace.get()));
    }
    true
}

fn run_turn(
    out: &Out,
    state: &State,
    thread_id: &str,
    turn_id: &str,
    input: &Value,
    stop: &AtomicBool,
    opts: Opts,
) {
    PACE.with(|pace| pace.set(if opts.fast { 15 } else { 35 }));
    let started_at = std::time::Instant::now();
    let text = input
        .as_array()
        .and_then(|parts| parts.iter().find(|p| p["type"] == "text"))
        .and_then(|p| p["text"].as_str())
        .unwrap_or_default()
        .to_string();
    let text = text.as_str();
    let attached = input
        .as_array()
        .map(|parts| parts.iter().filter(|p| p["type"] != "text").count())
        .unwrap_or(0);
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
    let user =
        json!({"type": "userMessage", "id": next_id("item"), "clientId": null, "content": input});
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
        "**Reading the request** The user wrote {} characters and attached {attached} file(s). I'll answer plainly and show what I did.",
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

    let hierarchy = std::env::var_os("ATRIUM_HIERARCHY_MOCK").is_some();
    // Kickoff prose contains demo trigger words (for example "failed");
    // the hierarchy fixture exercises the protocol without triggering them.
    let lower = if hierarchy {
        String::new()
    } else {
        text.to_lowercase()
    };
    let mut chosen: Option<String> = None;
    if !interrupted && lower.contains("plan") {
        let steps = ["Read the code", "Make the change", "Run the tests"];
        for done in 0..=steps.len() {
            if stop.load(Ordering::Relaxed) {
                interrupted = true;
                break;
            }
            let plan: Vec<Value> = steps
                .iter()
                .enumerate()
                .map(|(ix, step)| {
                    let status = if ix < done {
                        "completed"
                    } else if ix == done {
                        "inProgress"
                    } else {
                        "pending"
                    };
                    json!({"step": step, "status": status})
                })
                .collect();
            notify(
                out,
                "turn/plan/updated",
                with(json!({"explanation": null, "plan": plan})),
            );
            pause(450);
        }
    }
    if !interrupted && lower.contains("fail") {
        let turn = json!({"id": turn_id, "status": "failed", "items": items, "durationMs": started_at.elapsed().as_millis() as u64,
            "error": {"message": "The model is overloaded. Try again in a moment.", "codexErrorInfo": null, "additionalDetails": null}});
        notify(out, "turn/completed", with(json!({"turn": turn})));
        return;
    }
    if !interrupted && lower.contains("run") {
        let cmd_id = next_id("item");
        // "script" runs a multi-line command, as heredocs in real transcripts do.
        let command = if lower.contains("script") {
            "/bin/zsh -lc \"python3 - <<'PY'\nfrom pathlib import Path\nimport json\nfor p in Path('.').glob('*.json'):\n    print(p, len(json.loads(p.read_text())))\nPY\""
        } else {
            "cargo test --workspace"
        };
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
                "   Compiling atrium-core v0.1.0\n",
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

    if !interrupted && lower.contains("ask") {
        let item_id = next_id("item");
        let answer = ask(
            out,
            state,
            "item/tool/requestUserInput",
            with(json!({
            "itemId": item_id, "isBlocking": true, "autoResolutionMs": null,
            "questions": [{"id": "storage", "header": "Storage", "question": "Where should the data live?",
                "isOther": true, "isSecret": false, "options": [
                    {"label": "SQLite", "description": "One file next to the app"},
                    {"label": "Postgres", "description": "A server you already run"}]}]})),
        );
        let picked = answer["answers"]["storage"]["answers"][0]
            .as_str()
            .unwrap_or("nothing")
            .to_string();
        chosen = Some(picked);
    }
    if !interrupted && lower.contains("search") {
        let item = json!({"type": "webSearch", "id": next_id("item"), "query": "gpui text input ime",
            "action": {"type": "open_page", "url": "https://www.gpui.rs/"}, "results": null});
        notify(
            out,
            "item/started",
            with(json!({"item": item, "startedAtMs": 0})),
        );
        pause(500);
        notify(
            out,
            "item/completed",
            with(json!({"item": item, "completedAtMs": 0})),
        );
        items.push(item);
    }
    if !interrupted && lower.contains("image") {
        let path = std::env::temp_dir().join(format!("mock-codex-{}.png", next_id("img")));
        write_png(&path, 320, 200);
        let mut item = json!({"type": "imageGeneration", "id": next_id("item"), "status": "inProgress",
            "revisedPrompt": "A soft gradient, from dusk blue to warm orange", "failure": null, "generationId": "g1"});
        notify(
            out,
            "item/started",
            with(json!({"item": item, "startedAtMs": 0})),
        );
        pause(900);
        item["status"] = json!("completed");
        item["savedPath"] = json!(path.display().to_string());
        notify(
            out,
            "item/completed",
            with(json!({"item": item, "completedAtMs": 0})),
        );
        items.push(item);
    }
    if !interrupted && lower.contains("agents") {
        let spawn = json!({"type": "collabAgentToolCall", "id": next_id("item"), "tool": "spawnAgent", "status": "inProgress",
            "senderThreadId": thread_id, "receiverThreadIds": [], "prompt": "Audit the error handling in codex-client",
            "model": "mock-1", "reasoningEffort": null, "agentsStates": {}});
        notify(
            out,
            "item/started",
            with(json!({"item": spawn, "startedAtMs": 0})),
        );
        pause(600);
        let mut done = spawn.clone();
        done["status"] = json!("completed");
        notify(
            out,
            "item/completed",
            with(json!({"item": done, "completedAtMs": 0})),
        );
        items.push(done);
    }

    let answer_id = next_id("item");
    let answer = format!(
        "Here is what I found about **{}**.\n\n1. The request reached the mock agent.\n2. It streamed this answer word by word.\n\n```rust\nfn main() {{\n    println!(\"hello from mock-codex\");\n}}\n```\n\nAsk me to *run* or *edit* something to see approvals and file changes.",
        text.lines().next().unwrap_or("your message")
    );
    let answer = if hierarchy && text.starts_with("[Atrium] New notices for you:") {
        format!("Fixture received:\n{text}")
    } else {
        answer
    };
    let answer = if opts.plan {
        "## Proposed plan\n\n1. Read `transcript.rs` and its tests.\n2. Add the new row kind.\n3. Cover it with a test.\n\nSay **go** and I'll do it.".to_string()
    } else {
        answer
    };
    let answer = if lower.contains("long") {
        let mut long = String::from("## A longer answer\n\n");
        for ix in 1..=12 {
            long.push_str(&format!("{ix}. Paragraph {ix} keeps going so there is time to press stop and watch the stream end cleanly.\n"));
        }
        long
    } else {
        answer
    };
    let answer = match &chosen {
        Some(picked) => format!(
            "You picked **{picked}**. I'll set up storage with it and keep the schema in `migrations/`."
        ),
        None => answer,
    };
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

    // Messages steered in while working get a reply at the next step.
    let steered = state
        .steered
        .lock()
        .unwrap()
        .remove(turn_id)
        .unwrap_or_default();
    if !interrupted && !steered.is_empty() {
        let reply = format!(
            "Noted while working: *{}*. I'll keep that in mind from here.",
            steered.join("; ")
        );
        let reply_id = next_id("item");
        notify(
            out,
            "item/started",
            with(
                json!({"item": {"type": "agentMessage", "id": reply_id, "text": ""}, "startedAtMs": 0}),
            ),
        );
        stream(
            out,
            "item/agentMessage/delta",
            &ids,
            &reply_id,
            &reply,
            stop,
        );
        let done = json!({"type": "agentMessage", "id": reply_id, "text": reply});
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
        with({
            // Grows turn by turn, like a real conversation.
            use std::sync::atomic::{AtomicU64, Ordering};
            static TURNS: AtomicU64 = AtomicU64::new(0);
            let n = TURNS.fetch_add(1, Ordering::Relaxed) + 1;
            // Each request re-sends the conversation so far.
            let input = (1..=n).map(|k| 9_000 + 3_100 * k).sum::<u64>();
            let cached = (1..n).map(|k| 9_000 + 3_100 * k).sum::<u64>() + 2_400;
            let (output, reasoning) = (280 * n, 60 * n);
            json!({"tokenUsage": {
                "total": {"totalTokens": input + output, "inputTokens": input, "cachedInputTokens": cached,
                    "cacheWriteInputTokens": 0, "outputTokens": output, "reasoningOutputTokens": reasoning},
                "last": {"totalTokens": 9_000 + 3_380 * n, "inputTokens": 9_000 + 3_100 * n,
                    "cachedInputTokens": 2_400 * n, "cacheWriteInputTokens": 0, "outputTokens": 280, "reasoningOutputTokens": 60},
                "modelContextWindow": 272000}})
        }),
    );
    let status = if interrupted {
        "interrupted"
    } else {
        "completed"
    };
    let turn = json!({"id": turn_id, "status": status, "items": items, "error": null,
        "durationMs": started_at.elapsed().as_millis() as u64});
    notify(out, "turn/completed", with(json!({"turn": turn})));
    state
        .used
        .fetch_add(if opts.fast { 40 } else { 20 }, Ordering::Relaxed);
    notify(
        out,
        "account/rateLimits/updated",
        json!({"rateLimits": rate_snapshot(state)}),
    );
    let goal = state.goals.lock().unwrap().get_mut(thread_id).map(|goal| {
        goal["tokensUsed"] = json!(goal["tokensUsed"].as_u64().unwrap_or(0) + 1234);
        goal["timeUsedSeconds"] = json!(goal["timeUsedSeconds"].as_u64().unwrap_or(0) + 3);
        goal.clone()
    });
    if let Some(goal) = goal {
        notify(
            out,
            "thread/goal/updated",
            json!({"threadId": thread_id, "turnId": turn_id, "goal": goal}),
        );
    }

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

fn rate_snapshot(state: &State) -> Value {
    let used = state.used.load(Ordering::Relaxed) as f64 / 10.;
    json!({"limitId": "codex", "limitName": null, "normalModelSlug": null,
        "primary": {"usedPercent": used.min(100.), "windowDurationMins": 300, "resetsAt": now() + 2 * 3600 + 17 * 60},
        "secondary": {"usedPercent": (used * 0.6 + 18.).min(100.), "windowDurationMins": 10080, "resetsAt": now() + 4 * 86400},
        "credits": {"hasCredits": true, "unlimited": false, "balance": "120"},
        "individualLimit": null, "spendControlReached": null, "planType": "pro", "rateLimitReachedType": null})
}

/// Answers a prediction fork: the user's likely next message, streamed.
fn run_prediction(out: &Out, state: &State, thread_id: &str, turn_id: &str) {
    let last_user = state
        .threads
        .lock()
        .unwrap()
        .iter()
        .find(|t| t.id == thread_id)
        .and_then(|t| t.turns.last().cloned())
        .and_then(|turn| {
            turn["items"][0]["content"][0]["text"]
                .as_str()
                .map(str::to_lowercase)
        })
        .unwrap_or_default();
    let guess = if last_user.contains("test") || last_user.contains("run") {
        "Great, now commit the change with a clear message"
    } else if last_user.contains("plan") {
        "Go ahead and implement step 1"
    } else if last_user.contains("edit") {
        "Show me the full diff before you continue"
    } else {
        "Can you add tests for this?"
    };
    let ids = json!({"threadId": thread_id, "turnId": turn_id});
    let mut started = ids.clone();
    started["turn"] = json!({"id": turn_id, "status": "inProgress", "items": []});
    notify(out, "turn/started", started);
    pause(600);
    let item_id = next_id("item");
    let item = json!({"type": "agentMessage", "id": item_id, "text": guess});
    let mut done = ids.clone();
    done["item"] = item.clone();
    notify(out, "item/completed", done);
    let mut completed = ids;
    completed["turn"] = json!({"id": turn_id, "status": "completed", "items": [item], "error": null, "durationMs": 600});
    notify(out, "turn/completed", completed);
}

/// Writes a small gradient PNG, standing in for a generated image.
fn write_png(path: &std::path::Path, width: u32, height: u32) {
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for &byte in data {
            crc ^= byte as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    fn adler32(data: &[u8]) -> u32 {
        let (mut a, mut b) = (1u32, 0u32);
        for &byte in data {
            a = (a + byte as u32) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend(data);
        out.extend(&body);
        out.extend(crc32(&body).to_be_bytes());
    }
    let mut raw = Vec::new();
    for y in 0..height {
        raw.push(0);
        for x in 0..width {
            let t = x as f32 / width as f32;
            let v = y as f32 / height as f32;
            raw.extend([
                (40. + 215. * t) as u8,
                (70. + 90. * (1. - v) * t) as u8,
                (180. * (1. - t) + 40. * v) as u8,
            ]);
        }
    }
    // zlib with stored (uncompressed) deflate blocks.
    let mut zlib = vec![0x78, 0x01];
    for (ix, block) in raw.chunks(65535).enumerate() {
        let last = (ix + 1) * 65535 >= raw.len();
        zlib.push(last as u8);
        zlib.extend((block.len() as u16).to_le_bytes());
        zlib.extend((!(block.len() as u16)).to_le_bytes());
        zlib.extend(block);
    }
    zlib.extend(adler32(&raw).to_be_bytes());
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut header = Vec::new();
    header.extend(width.to_be_bytes());
    header.extend(height.to_be_bytes());
    header.extend([8, 2, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &header);
    chunk(&mut png, b"IDAT", &zlib);
    chunk(&mut png, b"IEND", &[]);
    let _ = std::fs::write(path, png);
}
