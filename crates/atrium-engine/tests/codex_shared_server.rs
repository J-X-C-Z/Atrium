//! One app-server per Codex installation, and its lifetime.
//!
//! The app's legacy path keeps one `CodexClient` and shares it. These tests
//! drive the adapter against `mock-codex` and check that two sessions share
//! one process and that closing the last session stops it.

use std::{
    env,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use atrium_core::AgentEvent;
use atrium_engine::{
    contracts::{ModelSelection, NodeId, RunAttemptId, RunId, RuntimeMode, ThreadId},
    providers::{
        CodexAdapterConfig,
        adapter::{
            AdapterEvent, AdapterEventSink, AdapterTurnInput, EnsureThreadInput, OpenSessionInput,
            ProviderAdapter, ProviderSessionRuntime, RuntimePolicy, TurnMessage,
        },
        codex::CodexAdapter,
    },
};
use codex_client::CodexConfig;

fn mock_codex() -> Option<PathBuf> {
    if let Some(path) = env::var_os("MOCK_CODEX_BIN").map(PathBuf::from) {
        return path.is_file().then_some(path);
    }
    let mut dirs = Vec::new();
    if let Some(dir) = env::var_os("CARGO_TARGET_DIR") {
        dirs.push(PathBuf::from(dir));
    }
    if let Ok(exe) = env::current_exe()
        && let Some(debug) = exe.parent().and_then(|deps| deps.parent())
    {
        dirs.push(debug.to_path_buf());
    }
    if let Some(root) = PathBuf::from(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2) {
        dirs.push(root.join("target").join("debug"));
    }
    let name = format!("mock-codex{}", env::consts::EXE_SUFFIX);
    dirs.into_iter()
        .map(|dir| dir.join(&name))
        .find(|path| path.is_file())
        .or_else(|| {
            env::var_os("PATH").and_then(|path| {
                env::split_paths(&path)
                    .map(|dir| dir.join(&name))
                    .find(|path| path.is_file())
            })
        })
}

fn policy(cwd: &str) -> RuntimePolicy {
    RuntimePolicy {
        runtime_mode: RuntimeMode::FullAccess,
        interaction_mode: Default::default(),
        cwd: Some(cwd.to_string()),
        approval_policy: None,
        sandbox_policy: None,
        max_runtime_mode: None,
    }
}

fn wait_until(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if check() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("condition did not hold in time");
}

#[test]
fn two_sessions_share_one_server_and_the_last_close_stops_it() {
    let Some(binary) = mock_codex() else {
        eprintln!("mock-codex was not built; skipping the shared-server test");
        return;
    };
    let home = tempfile::tempdir().expect("temp CODEX_HOME");
    let adapter = CodexAdapter::new(
        "codex",
        CodexAdapterConfig {
            codex: CodexConfig {
                binary: Some(binary),
                extra_args: Vec::new(),
                env: vec![("CODEX_HOME".into(), home.path().display().to_string())],
            },
            display_name: Some("Codex".into()),
            minimum_version: None,
            thread_env: Vec::new(),
            developer_instructions: None,
        },
    );
    let (tx, events) = std::sync::mpsc::channel();
    let tx = Mutex::new(tx);
    let sink: AdapterEventSink = Arc::new(move |event| {
        if let AdapterEvent::Agent(event) = event {
            let _ = tx.lock().expect("events").send(event);
        }
    });
    let cwd = home.path().display().to_string();
    let open = |name: &str| -> Arc<dyn ProviderSessionRuntime> {
        adapter
            .open_session(
                OpenSessionInput {
                    thread_id: ThreadId::new(name),
                    provider_session_id: name.into(),
                    model_selection: ModelSelection::new("codex", "mock-1"),
                    runtime_policy: policy(&cwd),
                    resume_from_session: None,
                    initial_native_thread_id: None,
                    now: "2026-11-24T00:00:00.000Z".into(),
                },
                sink.clone(),
            )
            .expect("open session")
    };
    // One turn on a session; returns the native thread id it ran on.
    let run_one = |session: &Arc<dyn ProviderSessionRuntime>, name: &str| -> String {
        let thread = session
            .ensure_thread(EnsureThreadInput {
                thread_id: ThreadId::new(name),
                title: None,
                model_selection: ModelSelection::new("codex", "mock-1"),
                runtime_policy: policy(&cwd),
                mcp_servers: Vec::new(),
                env: Vec::new(),
                developer_instructions: None,
                provider_session_id: None,
                existing_provider_thread: None,
                force_new_native_thread: false,
                now: "2026-11-24T00:00:00.000Z".into(),
            })
            .expect("ensure thread");
        let native = thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.clone())
            .expect("native thread id");
        session
            .start_turn(AdapterTurnInput {
                thread_id: ThreadId::new(name),
                run_id: RunId::new(format!("run-{name}")),
                run_ordinal: 1,
                provider_turn_ordinal: 1,
                attempt_id: RunAttemptId::new(format!("attempt-{name}")),
                root_node_id: NodeId::new(format!("node-{name}")),
                provider_thread: thread,
                message: TurnMessage::user(format!("message-{name}"), "say hi"),
                model_selection: ModelSelection::new("codex", "mock-1"),
                runtime_policy: policy(&cwd),
                app_context: Vec::new(),
                now: "2026-11-24T00:00:00.000Z".into(),
            })
            .expect("start turn");
        native
    };

    let first = open("first");
    let second = open("second");
    let pid = adapter.shared_server_pid().expect("one server process");
    assert_eq!(adapter.shared_server_sessions(), 2);
    assert!(adapter.shared_server_running());
    // Both sessions were served by the same process.
    assert_eq!(adapter.shared_server_pid(), Some(pid));

    // Both turns complete, routed to the right session by native thread id.
    let first_native = run_one(&first, "first");
    let second_native = run_one(&second, "second");
    let mut completed = Vec::new();
    while completed.len() < 2 {
        match events.recv_timeout(Duration::from_secs(5)) {
            Ok(AgentEvent::TurnCompleted {
                thread_id, status, ..
            }) => {
                assert_eq!(status, atrium_core::TurnStatus::Completed);
                completed.push(thread_id);
            }
            Ok(_) => {}
            Err(error) => panic!("turns did not complete: {error}"),
        }
    }
    completed.sort();
    let mut expected = vec![first_native, second_native];
    expected.sort();
    assert_eq!(completed, expected);

    first.close();
    assert_eq!(adapter.shared_server_sessions(), 1);
    assert_eq!(adapter.shared_server_pid(), Some(pid));
    assert!(adapter.shared_server_running());

    second.close();
    wait_until(|| !adapter.shared_server_running());
    assert_eq!(adapter.shared_server_sessions(), 0);
    assert_eq!(adapter.shared_server_pid(), None);
}
