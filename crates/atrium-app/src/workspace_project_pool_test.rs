//! Exercise the real ticker and local clients with scriptable stdio RPC peers.
use super::*;
use crate::project::{agent::NewTask, pool::TaskLock};
use crate::workspace::{AcpAgent, AgentStatus, Settings};
use acp_client::AcpClient;
use agent_rpc::Connection;
use codex_client::CodexClient;
use futures::StreamExt;
use gpui::{AppContext, TestAppContext};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::Path,
    thread,
};

#[gpui::test(iterations = 1)]
fn local_codex_pool_drains_without_mcp(cx: &mut TestAppContext) {
    exercise(cx, "codex");
}

#[gpui::test(iterations = 1)]
fn local_acp_pool_drains_without_mcp(cx: &mut TestAppContext) {
    exercise(cx, "claude");
}

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn exercise(cx: &mut TestAppContext, backend: &str) {
    cx.executor().allow_parking();
    let fixture =
        Fixture(std::env::temp_dir().join(format!("atrium-local-pool-{}", uuid::Uuid::new_v4())));
    let mut project = Project::open(&fixture.0).unwrap();
    project.file.max_workers = 2;
    project.file.autopilot.tasks_per_session = 0;
    project.file.streams.push(Workstream {
        id: "web".into(),
        name: "Web".into(),
        role: Role::Lead,
        backend: backend.into(),
        thread_id: Some("lead-session".into()),
        ..Default::default()
    });
    project.save().unwrap();
    let tasks = (0..6)
        .map(|i| NewTask {
            title: format!("Task {i}"),
            goal: format!("Check item {i}"),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    project.create_tasks("web", &tasks).unwrap();
    let (client, peer) = UnixStream::pair().unwrap();
    let stop = peer.try_clone().unwrap();
    let root = fixture.0.clone();
    let peer = thread::spawn(move || serve(peer, &root));
    let reader = BufReader::new(client.try_clone().unwrap());
    let workspace = cx.new(|cx| {
        let mut ws = Workspace::with_settings(Settings {
            projects: vec![fixture.0.clone()],
            quiet_notifications: true,
            no_predictions: true,
            ..Default::default()
        });
        if backend == "codex" {
            let (connection, mut events) = Connection::new(reader, client);
            ws.client = Some(CodexClient::over(connection));
            ws._pump = Some(cx.spawn(async move |this, cx| {
                while let Some(event) = events.next().await {
                    if this.update(cx, |ws, cx| ws.handle(event, cx)).is_err() {
                        break;
                    }
                }
            }));
        } else {
            let (client, _events) =
                futures::executor::block_on(AcpClient::over(backend.into(), reader, client, None))
                    .unwrap();
            let profile = agents::AgentProfile {
                id: backend.into(),
                name: "Mock".into(),
                short: "M".into(),
                command: None,
                install: String::new(),
            };
            ws.agents.push(AcpAgent {
                profile,
                status: AgentStatus::Ready,
                client: Some(client),
                info: None,
                remote: None,
                link: None,
                _pump: None,
            });
        }
        ws.transcripts
            .insert("lead-session".into(), Transcript::new());
        ws.init_projects(cx);
        ws
    });
    // The executor controls the ticker, while the production debounce uses std::Instant.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        workspace.update(cx, |ws, _| {
            for last in ws.project_schedule.last_wake.values_mut() {
                *last = Instant::now() - RETRY_DELAY;
            }
        });
        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        let project = Project::load(&fixture.0).unwrap();
        if project.file.reported_tasks.len() == 6 {
            assert_eq!(project.pool("web").done, 6);
            assert!(project.pool("web").drained());
            assert_eq!(project.pool_workers("web").count(), 2);
            assert!(project.unread("web").is_empty());
            break;
        }
        assert!(
            Instant::now() < deadline && !peer.is_finished(),
            "pool stalled: {:?}",
            project.pool("web")
        );
        thread::sleep(Duration::from_millis(10));
    }
    stop.shutdown(Shutdown::Both).unwrap();
    let (starts, finished, digests) = peer.join().unwrap();
    assert_eq!((starts, finished.len()), (2, 6));
    assert!(digests > 0);
}

fn serve(mut socket: UnixStream, root: &Path) -> (usize, HashSet<String>, usize) {
    let reader = BufReader::new(socket.try_clone().unwrap());
    let mut starts = 0;
    let mut finished = HashSet::new();
    let mut digests = 0;
    for line in reader.lines() {
        let line = line.unwrap();
        let rpc: Value = serde_json::from_str(&line).unwrap();
        let method = rpc["method"].as_str().unwrap();
        let params = &rpc["params"];
        let mut terminal = None;
        let result = match method {
            "initialize" => json!({"protocolVersion":1}),
            "thread/start" | "session/new" => {
                let args = if method == "thread/start" {
                    &params["config"]["mcp_servers.atrium"]["args"]
                } else {
                    &params["mcpServers"][0]["args"]
                };
                let worker = args.as_array().unwrap().last().unwrap().as_str().unwrap();
                let project = Project::load(root).unwrap();
                assert!(
                    project.task_of(project.stream(worker).unwrap()).is_some(),
                    "claim must precede backend start"
                );
                starts += 1;
                json!({"sessionId":worker,"thread":{"id":worker}})
            }
            "thread/name/set" | "thread/goal/set" => json!({}),
            "turn/start" | "session/prompt" => {
                let codex = method == "turn/start";
                let session = params[if codex { "threadId" } else { "sessionId" }]
                    .as_str()
                    .unwrap();
                let text = params[if codex { "input" } else { "prompt" }][0]["text"]
                    .as_str()
                    .unwrap();
                if session == "lead-session" {
                    assert!(text.contains("checked by local worker"));
                    digests += 1;
                } else {
                    let project = Project::load(root).unwrap();
                    let task = project.task_of(project.stream(session).unwrap()).unwrap();
                    assert!(text.contains(&format!("`{}`", task.key())), "{text}");
                    assert!(
                        finished.insert(task.key()),
                        "second dispatch of the same task"
                    );
                    // A tool-less agent edits only its assigned file and ends the turn.
                    let _lock = TaskLock::take(&project).unwrap();
                    let path = root.join(task.rel_path());
                    let body = fs::read_to_string(&path)
                        .unwrap()
                        .replace("status: running", "status: done");
                    fs::write(
                        path,
                        format!("{body}\n## Result\nchecked by local worker\n"),
                    )
                    .unwrap();
                }
                let turn = format!("turn-{}", rpc["id"]);
                if codex {
                    terminal = Some((session.to_owned(), turn.clone()));
                }
                json!({"turn":{"id":turn},"stopReason":"end_turn"})
            }
            other => panic!("unexpected pool RPC {other}"),
        };
        writeln!(
            socket,
            "{}",
            json!({"jsonrpc":"2.0","id":rpc["id"],"result":result})
        )
        .unwrap();
        if let Some((session, turn)) = terminal {
            for method in ["turn/started", "turn/completed"] {
                writeln!(socket,"{}",json!({"method":method,"params":{"threadId":session,"turn":{"id":turn,"status":"completed"}}})).unwrap();
            }
        }
    }
    (starts, finished, digests)
}
