//! A real Workspace entity and ticker, with no window, subprocess, or network.
use super::{Settings, Transcript, Workspace};
use crate::project::{Project, Role, Workstream};
use agent_rpc::Connection;
use codex_client::CodexClient;
use gpui::{AppContext, TestAppContext};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, ErrorKind, Write},
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
    exercise_notice_delivery(cx, false, false);
}

#[gpui::test(iterations = 1)]
fn scheduler_tick_holds_notice_on_rpc_error_and_does_not_retry(cx: &mut TestAppContext) {
    exercise_notice_delivery(cx, true, false);
}

#[gpui::test(iterations = 1)]
fn unattended_notice_wake_uses_scoped_turn_and_durable_budget(cx: &mut TestAppContext) {
    exercise_notice_delivery(cx, false, true);
}

fn exercise_notice_delivery(cx: &mut TestAppContext, rpc_error: bool, unattended: bool) {
    // Real socket I/O wakes the GPUI foreground task externally.
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
    project.file.autopilot.on = unattended;
    project.file.autopilot.daily_turns = 1;
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
    let (check_no_retry, await_no_retry_check) = mpsc::channel();
    let mock = thread::spawn(move || {
        let mut line = String::new();
        let mut reader = BufReader::new(mock_socket.try_clone().unwrap());
        reader.read_line(&mut line).unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["method"], "turn/start");
        if unattended {
            assert_eq!(request["params"]["sandboxPolicy"]["type"], "workspaceWrite");
            assert_eq!(request["params"]["approvalPolicy"], "on-request");
            assert!(
                request["params"]["input"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("Check-in:")
            );
        }
        assert_eq!(request["params"]["threadId"], "fixture-thread");
        assert!(
            request["params"]["input"][0]["text"]
                .as_str()
                .unwrap()
                .contains("fixture milestone for real Workspace ticker")
        );
        let response = if rpc_error {
            json!({
                "id": request["id"],
                "error": {"code": -32602, "message": "fixture turn refused"}
            })
        } else {
            json!({"id": request["id"], "result": {"turn": {"id": "fixture-turn"}}})
        };
        writeln!(mock_socket, "{response}").unwrap();
        sent.send(()).unwrap();
        if rpc_error {
            // Keep transport live until the Workspace has handled the explicit
            // response and driven more ticks than its retry backoff.
            await_no_retry_check
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            reader
                .get_ref()
                .set_read_timeout(Some(Duration::from_millis(250)))
                .unwrap();
            line.clear();
            match reader.read_line(&mut line) {
                Err(error)
                    if line.is_empty()
                        && matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                result => {
                    panic!("expected live mock transport with no retry: {result:?}, {line:?}")
                }
            }
        }
    });

    let settings = Settings {
        mode: codex_client::AccessMode::FullAccess,
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
    loop {
        let persisted = Project::open(&fixture.0).unwrap();
        let cursor = persisted.stream("controller").unwrap().notices_seen;
        if (rpc_error && persisted.notice_delivery_uncertain("controller"))
            || (!rpc_error && cursor == 1)
        {
            assert_eq!(cursor, if rpc_error { 0 } else { 1 });
            assert_eq!(persisted.notice_delivery_blocked("controller"), rpc_error);
            if rpc_error {
                assert_eq!(persisted.unread("controller").len(), 1);
            }
            break;
        }
        assert!(
            Instant::now() < deadline,
            "real Workspace timer failed to settle the notice"
        );
        thread::sleep(Duration::from_millis(5));
        cx.run_until_parked();
    }
    received.recv_timeout(Duration::from_secs(1)).unwrap();
    if rpc_error {
        // A successful submission removed the old pending wake. Simulate a
        // fresh wake trigger and age the real wall-clock cooldown separately:
        // the GPUI clock below only controls its asynchronous timer.
        workspace.update(cx, |ws, _| {
            assert!(!ws.transcripts["fixture-thread"].is_running());
            ws.project_schedule
                .test_requeue_after_backoff(fixture.0.clone(), "controller".into());
        });
        // Drive real ticker iterations explicitly: one large jump could
        // wake only one iteration of its repeating sleep.
        for _ in 0..17 {
            cx.executor().advance_clock(Duration::from_secs(2));
            cx.run_until_parked();
        }
        workspace.update(cx, |ws, _| {
            ws.project_schedule
                .assert_test_wake_attempt_still_pending(fixture.0.clone(), "controller".into());
        });
        check_no_retry.send(()).unwrap();
    }
    mock.join().unwrap();
    let persisted = Project::open(&fixture.0).unwrap();
    assert_eq!(persisted.notice_delivery_uncertain("controller"), rpc_error);
    assert_eq!(
        persisted.stream("controller").unwrap().notices_seen,
        if rpc_error { 0 } else { 1 }
    );
    if unattended {
        let day: Value = serde_json::from_slice(
            &fs::read(fixture.0.join(".atrium/autopilot-turns.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(day["turns"], 1);
    }
    drop(persisted);
    drop(workspace);
    cx.run_until_parked();
    if rpc_error {
        assert_reopened_workspace_holds_notice(cx, &fixture.0);
    }
}

fn assert_reopened_workspace_holds_notice(cx: &mut TestAppContext, root: &PathBuf) {
    // A new entity and transport read the same temporary disk state via real
    // startup initialization. This is not a process crash or remote replay.
    let (client, mock) = UnixStream::pair().unwrap();
    let (connection, _incoming) =
        Connection::new(BufReader::new(client.try_clone().unwrap()), client);
    mock.set_read_timeout(Some(Duration::from_millis(250)))
        .unwrap();
    let reopened = cx.new(|cx| {
        let mut ws = Workspace::with_settings(Settings {
            projects: vec![root.clone()],
            ..Default::default()
        });
        ws.client = Some(CodexClient::over(connection));
        ws.transcripts
            .insert("fixture-thread".into(), Transcript::new());
        ws.init_projects(cx);
        // Simulate a later wake trigger; a new scheduler has no old cooldown.
        ws.project_schedule
            .test_queue_first_wake(root.clone(), "controller".into());
        ws
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    reopened.update(cx, |ws, _| {
        ws.project_schedule
            .assert_test_wake_attempt_still_pending(root.clone(), "controller".into());
    });
    let mut unexpected = String::new();
    match BufReader::new(mock).read_line(&mut unexpected) {
        Err(error)
            if unexpected.is_empty()
                && matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
        result => {
            panic!("expected live reopened transport with no resend: {result:?}, {unexpected:?}")
        }
    }
    let persisted = Project::open(root).unwrap();
    assert!(persisted.notice_delivery_uncertain("controller"));
    assert_eq!(persisted.stream("controller").unwrap().notices_seen, 0);
    assert_eq!(persisted.unread("controller").len(), 1);
}
