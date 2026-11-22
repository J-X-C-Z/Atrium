use super::*;
use agent_rpc::Connection;
use atrium_workflow::{Template, run::Run};
use futures::StreamExt;
use gpui::{AppContext, TestAppContext};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn mock(
    dir: PathBuf,
) -> (
    codex_client::CodexClient,
    futures::channel::mpsc::UnboundedReceiver<codex_client::Incoming>,
    thread::JoinHandle<Vec<Value>>,
) {
    let (client_socket, mut mock_socket) = UnixStream::pair().unwrap();
    let (connection, events) = Connection::new(
        BufReader::new(client_socket.try_clone().unwrap()),
        client_socket,
    );
    let client = codex_client::CodexClient::over(connection);
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    let handle = thread::spawn(move || {
        mock_socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut reader = BufReader::new(mock_socket.try_clone().unwrap());
        let mut dirs: HashMap<String, String> = HashMap::new();
        let mut next = 0u64;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(_) => break,
                Ok(_) => {}
            }
            let request: Value = serde_json::from_str(&line).unwrap();
            seen.lock().unwrap().push(request.clone());
            let method = request["method"].as_str().unwrap_or_default();
            let id = request["id"].clone();
            match method {
                "thread/start" => {
                    next += 1;
                    let thread = format!("wf-thread-{next}");
                    let cwd = request["params"]["cwd"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    dirs.insert(thread.clone(), cwd.clone());
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs();
                    writeln!(
                        mock_socket,
                        "{}",
                        json!({"id": id, "result": {"thread": {
                            "id": thread, "preview": "", "cwd": cwd,
                            "createdAt": now, "updatedAt": now, "turns": []
                        }}})
                    )
                    .unwrap();
                }
                "thread/name/set" => {
                    writeln!(mock_socket, "{}", json!({"id": id, "result": {}})).unwrap();
                }
                "turn/start" => {
                    next += 1;
                    let thread = request["params"]["threadId"].as_str().unwrap().to_string();
                    let turn = format!("wf-turn-{next}");
                    let text = request["params"]["input"][0]["text"]
                        .as_str()
                        .unwrap_or_default();
                    if let Some(dir) = dirs.get(&thread) {
                        let name = if text.contains("STEP1") {
                            "one.txt"
                        } else if text.contains("STEP2") {
                            "two.txt"
                        } else {
                            "three.txt"
                        };
                        fs::write(PathBuf::from(dir).join(name), "done").unwrap();
                    }
                    writeln!(
                        mock_socket,
                        "{}",
                        json!({"id": id, "result": {"turn": {"id": turn}}})
                    )
                    .unwrap();
                    for message in [
                        json!({"method": "turn/started", "params": {
                            "threadId": thread, "turn": {"id": turn, "status": "inProgress", "items": []}
                        }}),
                        json!({"method": "turn/completed", "params": {
                            "threadId": thread,
                            "turn": {"id": turn, "status": "completed", "items": [{
                                "type": "agentMessage", "id": format!("item-{turn}"),
                                "text": "done"
                            }], "error": null}
                        }}),
                    ] {
                        writeln!(mock_socket, "{message}").unwrap();
                    }
                }
                _ => {
                    writeln!(mock_socket, "{}", json!({"id": id, "result": {}})).unwrap();
                }
            }
        }
        let out = seen.lock().unwrap().clone();
        out
    });
    let _ = dir;
    (client, events, handle)
}

#[gpui::test(iterations = 1)]
fn the_runner_takes_three_steps_to_done(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let fixture = Fixture(std::env::temp_dir().join(uuid::Uuid::new_v4().to_string()));
    fs::create_dir_all(fixture.0.join(".atrium")).unwrap();
    let template = Template::parse(
        r#"
id = "runner"
name = "Runner"
[[steps]]
id = "one"
title = "One"
prompt = "STEP1 write one"
outputs = ["one.txt"]
[[steps]]
id = "two"
title = "Two"
prompt = "STEP2 write two"
outputs = ["two.txt"]
[[steps]]
id = "three"
title = "Three"
prompt = "STEP3 write three"
outputs = ["three.txt"]
"#,
    )
    .unwrap();
    let seen = drive(cx, &fixture.0, template);
    let saved = super::super::workflows::load_run(&fixture.0).unwrap();
    assert_eq!(saved.state, RunState::Done);
    for file in ["one.txt", "two.txt", "three.txt"] {
        assert!(fixture.0.join(file).exists(), "{file}");
    }
    let starts = seen
        .iter()
        .filter(|request| request["method"] == "thread/start")
        .count();
    assert_eq!(starts, 3, "each step gets a new conversation");
}

#[gpui::test(iterations = 1)]
fn a_repeating_step_runs_once_per_listed_item(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let fixture = Fixture(std::env::temp_dir().join(uuid::Uuid::new_v4().to_string()));
    fs::create_dir_all(fixture.0.join(".atrium")).unwrap();
    fs::write(
        fixture.0.join("items.json"),
        r#"[{"id": "a", "title": "First"}, {"id": "b", "title": "Second"}]"#,
    )
    .unwrap();
    let template = Template::parse(
        r#"
id = "each"
name = "Each"
[[steps]]
id = "one"
title = "One"
prompt = "STEP1 list"
outputs = ["one.txt", "items.json"]
[[steps]]
id = "two"
title = "Two"
needs = ["one"]
each = { from = "items.json" }
prompt = "STEP2 {{item.title}}"
outputs = ["two.txt"]
"#,
    )
    .unwrap();
    let seen = drive(cx, &fixture.0, template);
    let saved = super::super::workflows::load_run(&fixture.0).unwrap();
    assert_eq!(saved.state, RunState::Done);
    let items: Vec<&str> = saved
        .steps
        .iter()
        .filter(|step| step.is_item())
        .map(|step| step.id.as_str())
        .collect();
    assert_eq!(items, ["two:a", "two:b"]);
    let prompts: Vec<&str> = seen
        .iter()
        .filter(|request| request["method"] == "turn/start")
        .filter_map(|request| request["params"]["input"][0]["text"].as_str())
        .collect();
    assert!(prompts.iter().any(|p| p.contains("STEP2 First")));
    assert!(prompts.iter().any(|p| p.contains("STEP2 Second")));
}

/// Runs `template` in `dir` against the mock until it is done; returns the
/// requests the mock saw.
fn drive(cx: &mut TestAppContext, dir: &Path, template: Template) -> Vec<Value> {
    let run = Run::new(template.clone(), HashMap::new(), &[], 1).unwrap();
    super::super::workflows::save_run(dir, &run).unwrap();
    fs::write(
        dir.to_path_buf().join(".atrium").join("workflow.toml"),
        template.to_toml(),
    )
    .unwrap();
    let (client, mut events, mock) = mock(dir.to_path_buf().clone());
    let ws = cx.new(|cx| {
        let mut ws = Workspace::with_settings(crate::settings::Settings {
            workflow_runs: vec![dir.to_path_buf().clone()],
            ..Default::default()
        });
        ws.client = Some(client);
        ws._pump = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                if this
                    .update(cx, |ws: &mut Workspace, cx| ws.handle(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
        ws
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        ws.update(cx, |ws, cx| ws.workflow_tick(cx));
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(250));
        let done = ws.read_with(cx, |ws, _| {
            ws.workflow_runs
                .first()
                .is_some_and(|entry| entry.run.state == RunState::Done)
        });
        if done {
            break;
        }
        thread::sleep(Duration::from_millis(5));
        if Instant::now() >= deadline {
            let saved = super::super::workflows::load_run(dir).unwrap();
            eprintln!(
                "workflow state {:?}, steps {:?}",
                saved.state,
                saved
                    .steps
                    .iter()
                    .map(|step| (&step.id, step.status, step.attempts.len()))
                    .collect::<Vec<_>>()
            );
            eprintln!(
                "threads {:?}",
                ws.read_with(cx, |ws, _| {
                    ws.threads
                        .iter()
                        .map(|thread| (thread.id.clone(), thread.cwd.clone()))
                        .collect::<Vec<_>>()
                })
            );
            eprintln!(
                "transcript {:?}",
                saved
                    .steps
                    .first()
                    .and_then(|step| step.thread())
                    .and_then(|thread| ws.read_with(cx, |ws, _| {
                        ws.transcripts.get(thread).map(|transcript| {
                            (
                                transcript.is_running(),
                                transcript
                                    .entries()
                                    .iter()
                                    .map(|entry| format!("{:?}", entry.kind))
                                    .collect::<Vec<_>>(),
                            )
                        })
                    }))
            );
        }
        assert!(Instant::now() < deadline, "workflow did not finish");
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        thread::sleep(Duration::from_millis(5));
    }
    drop(ws);
    cx.run_until_parked();
    mock.join().unwrap()
}
