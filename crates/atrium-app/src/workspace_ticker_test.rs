//! A real Workspace entity and ticker, with no window, subprocess, or network.
use super::{Settings, Transcript, Workspace};
use crate::project::{Project, Role, Workstream};
use agent_rpc::Connection;
use codex_client::CodexClient;
use gpui::{AppContext, TestAppContext};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[gpui::test(iterations = 1)]
fn scheduler_tick_sends_notice_and_commits_rpc_acceptance(cx: &mut TestAppContext) {
    // This fixture deliberately combines a virtual GPUI timer with real
    // socket I/O: the RPC reader thread wakes the foreground future.
    cx.executor().allow_parking();
    let fixture = Fixture(
        std::env::temp_dir().join(format!("atrium-workspace-ticker-{}", uuid::Uuid::new_v4())),
    );
    fs::create_dir(&fixture.0).unwrap();
    let mut project = Project::open(&fixture.0).unwrap();
    project.file.streams.push(Workstream {
        id: "controller".into(),
        name: "Controller".into(),
        backend: "codex".into(),
        thread_id: Some("fixture-thread".into()),
        role: Role::Controller,
        ..Default::default()
    });
    project.save().unwrap();

    let (client_socket, mut mock_socket) = UnixStream::pair().unwrap();
    mock_socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let (connection, _incoming) = Connection::new(
        BufReader::new(client_socket.try_clone().unwrap()),
        client_socket,
    );
    let (sent, received) = mpsc::channel();
    let mock = thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(mock_socket.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["method"], "turn/start");
        assert_eq!(request["params"]["threadId"], "fixture-thread");
        assert!(
            request["params"]["input"][0]["text"]
                .as_str()
                .unwrap()
                .contains("fixture milestone for real Workspace ticker")
        );
        writeln!(
            mock_socket,
            "{}",
            json!({
                "id": request["id"], "result": {"turn": {"id": "fixture-turn"}}
            })
        )
        .unwrap();
        sent.send(()).unwrap();
    });

    let settings = Settings {
        projects: vec![fixture.0.clone()],
        ..Default::default()
    };
    let workspace = cx.new(|cx| {
        let mut ws = Workspace::with_settings(settings);
        ws.client = Some(CodexClient::over(connection));
        ws.transcripts
            .insert("fixture-thread".into(), Transcript::new());
        ws.init_projects(cx); // Arms the real two-second Workspace scheduler.
        ws
    });
    cx.run_until_parked();
    let before_post = project.latest_change().unwrap();
    project
        .post(
            "lead",
            vec!["controller".into()],
            "fixture milestone for real Workspace ticker",
        )
        .unwrap();
    fs::File::options()
        .write(true)
        .open(fixture.0.join(".atrium/notices.md"))
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(before_post + Duration::from_secs(2)))
        .unwrap();
    assert!(received.try_recv().is_err(), "the timer did not fire yet");
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();

    // The GPUI clock is virtual; the std UnixStream reader is an OS thread.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Project::open(&fixture.0)
        .unwrap()
        .stream("controller")
        .unwrap()
        .notices_seen
        != 1
    {
        assert!(
            Instant::now() < deadline,
            "real Workspace timer failed to settle the notice"
        );
        thread::sleep(Duration::from_millis(5));
        cx.run_until_parked();
    }
    received.recv_timeout(Duration::from_secs(1)).unwrap();
    mock.join().unwrap();
    assert!(
        !Project::open(&fixture.0)
            .unwrap()
            .notice_delivery_blocked("controller")
    );
    drop(workspace);
}
