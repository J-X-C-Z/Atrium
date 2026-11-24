//! Scenario driver for `tools/t3-diff`.
//!
//! Reads one JSON scenario from stdin, drives `atrium-engine` the way the T3
//! scenario drives T3, and writes the normalized event stream as JSON lines to
//! stdout. tools/t3-diff diffs the two streams.
//!
//! Two kinds of scenario use this binary:
//!
//! * `"kind": "engine"` (the default) runs turns through
//!   [`atrium_engine::runtime::CodexEngineRuntime`], the same
//!   `atrium_runtime::AgentRuntime` seam the app selects. Provider text and
//!   statuses therefore come from the ported adapter, not a bespoke driver.
//! * `"kind": "adapter"` drives the provider adapter directly for behaviour
//!   the T3 0.1.11 CLI cannot reach (mid-thread settings, `skills/list`,
//!   elicitation outside full access, `thread/read` versus `thread/resume`).
//!   These scenarios say `"t3": "skip: ..."`; the runner records the reason.
//!
//! Scenario (JSON on stdin), engine kind:
//!
//! ```json
//! {
//!   "name": "01-thread-lifecycle",
//!   "project": "scratch",
//!   "model": "mock-1",
//!   "turns": [{"text": "say hi"}, {"text": "now say bye"}],
//!   "expect": {"final_status": "completed"}
//! }
//! ```
//!
//! Adapter kind adds `actions` (one object each):
//!
//! ```json
//! {"op":"settings","model":"mock-1-pro","effort":"high","service_tier":"priority"}
//! {"op":"skills"} {"op":"config"} {"op":"read_snapshot"}
//! {"op":"hold_writer"} {"op":"read_vs_write"} {"op":"resume"}
//! {"op":"turn","text":"please elicit","answer":"accept","runtime_mode":"approval-required"}
//! ```
//!
//! Exit codes: 0 the scenario ran, 2 setup/environment error (no mock
//! binary, protocol failure, timeout).

use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use atrium_core::{AgentEvent, Item, TurnStatus};
use atrium_engine::contracts::{ModelSelection, ProviderThread};
use atrium_engine::diff::{NormalLine, Role, T3_SENDER_NOTE, normalize_status};
use atrium_engine::providers::adapter::{
    AdapterEvent, AdapterEventSink, AdapterTurnInput, EnsureThreadInput, OpenSessionInput,
    ProviderSessionRuntime, RuntimePolicy, RuntimeRequestResponse, TurnMessage,
};
use atrium_engine::providers::build_thread_settings_update;
use atrium_engine::providers::codex::{CodexAdapter, CodexAdapterConfig};
use atrium_engine::runtime::CodexEngineRuntime;
use atrium_runtime::{AgentRuntime, Delivery, EventSink, SessionSpec, TurnInput};
use codex_client::{CodexClient, CodexConfig};
use serde::Deserialize;
use serde_json::{Value, json};

fn main() {
    let args = match Args::parse(env::args().skip(1)) {
        Ok(Some(args)) => args,
        Ok(None) => {
            print_usage();
            return;
        }
        Err(error) => {
            eprintln!("atrium-engine-driver: {error}");
            print_usage();
            std::process::exit(2);
        }
    };
    if let Err(error) = run(&args) {
        eprintln!("atrium-engine-driver: {error}");
        std::process::exit(2);
    }
}

fn print_usage() {
    eprintln!(
        "usage: atrium-engine-driver [--scratch DIR] [--timeout-ms N] [--keep-scratch]\n\
         \x20                          [--real] [--codex-bin PATH] < scenario.json\n\
         env: MOCK_CODEX_BIN (default: target/{{debug,release}}/mock-codex or PATH)"
    );
}

struct Args {
    scratch: Option<PathBuf>,
    timeout: Duration,
    keep_scratch: bool,
    /// The Codex binary to drive. Defaults to `mock-codex`.
    codex_bin: Option<PathBuf>,
    /// Use the real Codex installation and its `CODEX_HOME` (for the
    /// real-machine script); otherwise a scratch `CODEX_HOME` keeps the mock
    /// away from the user's own threads.
    real: bool,
}

impl Args {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut parsed = Self {
            scratch: None,
            timeout: Duration::from_secs(90),
            keep_scratch: false,
            codex_bin: None,
            real: false,
        };
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--help" | "-h" => return Ok(None),
                "--scratch" => {
                    let value = args.next().ok_or("--scratch needs a directory")?;
                    parsed.scratch = Some(PathBuf::from(value));
                }
                "--timeout-ms" => {
                    let value = args.next().ok_or("--timeout-ms needs a number")?;
                    let millis: u64 = value
                        .parse()
                        .map_err(|_| format!("bad --timeout-ms {value}"))?;
                    parsed.timeout = Duration::from_millis(millis.max(1000));
                }
                "--keep-scratch" => parsed.keep_scratch = true,
                "--codex-bin" => {
                    let value = args.next().ok_or("--codex-bin needs a path")?;
                    parsed.codex_bin = Some(PathBuf::from(value));
                }
                "--real" => parsed.real = true,
                other => return Err(format!("unknown argument {other}")),
            }
        }
        Ok(Some(parsed))
    }
}

#[derive(Debug, Deserialize)]
struct Scenario {
    #[serde(default = "default_name")]
    name: String,
    #[serde(default = "default_project")]
    project: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    turns: Vec<TurnSpec>,
    #[serde(default)]
    actions: Vec<Action>,
    /// After the turns, read the thread with the runtime's read path
    /// (`thread/read`) and record what came back.
    #[serde(default)]
    read_snapshot: bool,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    expect: Option<Expect>,
}

#[derive(Debug, Deserialize)]
struct Expect {
    #[serde(default)]
    final_status: Option<String>,
    /// Protocol methods both sides must have called. Checked by run.py.
    #[serde(default)]
    #[allow(dead_code)]
    provider_methods: Vec<String>,
}

fn default_name() -> String {
    "scenario".to_string()
}

fn default_project() -> String {
    "scratch".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum TurnSpec {
    Text(String),
    Message { text: String },
}

impl TurnSpec {
    fn text(&self) -> &str {
        match self {
            Self::Text(text) | Self::Message { text } => text,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct Action {
    op: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    service_tier: Option<String>,
    #[serde(default)]
    answer: Option<String>,
    #[serde(default)]
    runtime_mode: Option<String>,
}

fn run(args: &Args) -> Result<(), String> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| format!("reading scenario: {error}"))?;
    let scenario: Scenario =
        serde_json::from_str(&input).map_err(|error| format!("parsing scenario: {error}"))?;

    let codex = match &args.codex_bin {
        Some(path) if path.is_file() => path.clone(),
        Some(path) => return Err(format!("--codex-bin {} is not a file", path.display())),
        None => find_mock_codex()?,
    };
    let scratch = Scratch::new(scenario.project.as_str(), args)?;
    let timeout = scenario
        .timeout_ms
        .map(|millis| Duration::from_millis(millis.max(1000)))
        .unwrap_or(args.timeout);
    let model = scenario.model.clone();

    let lines = if scenario.kind.as_deref() == Some("adapter") {
        run_adapter(
            &scenario,
            &codex,
            scratch.path(),
            model.as_deref(),
            args.real,
            timeout,
        )?
    } else {
        run_engine(
            &scenario,
            &codex,
            scratch.path(),
            model.as_deref(),
            args.real,
            timeout,
        )?
    };
    let final_status = last_status(&lines).unwrap_or_else(|| "completed".into());

    let mut stdout = io::stdout().lock();
    for line in &lines {
        writeln!(stdout, "{line}").map_err(|error| format!("writing stream: {error}"))?;
    }
    stdout
        .flush()
        .map_err(|error| format!("writing stream: {error}"))?;

    if let Some(expect) = &scenario.expect
        && let Some(expected) = &expect.final_status
        && expected.to_ascii_lowercase() != final_status
    {
        eprintln!(
            "atrium-engine-driver: scenario {} expected final status {}, got {}",
            scenario.name, expected, final_status
        );
    }
    Ok(())
}

fn push(lines: &mut Vec<String>, line: NormalLine) {
    lines.push(line.to_line());
}

fn push_trace(lines: &mut Vec<String>, value: Value) {
    lines.push(serde_json::to_string(&value).expect("trace lines always serialize"));
}

fn last_status(lines: &[String]) -> Option<String> {
    for line in lines.iter().rev() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value["kind"] == "thread.finished" {
            return value["status"].as_str().map(str::to_owned);
        }
    }
    None
}

fn codex_config(binary: &Path, scratch: &Path, real: bool) -> CodexConfig {
    if real {
        // The user's own Codex home and credentials; nothing is copied.
        return CodexConfig {
            binary: Some(binary.to_path_buf()),
            extra_args: Vec::new(),
            env: Vec::new(),
        };
    }
    let codex_home = scratch.join(".mock-codex-home");
    let _ = fs::create_dir_all(&codex_home);
    CodexConfig {
        binary: Some(binary.to_path_buf()),
        extra_args: Vec::new(),
        env: vec![("CODEX_HOME".into(), codex_home.display().to_string())],
    }
}

fn now_iso() -> String {
    jiff::Timestamp::now().to_string()
}

fn selection(model: Option<&str>, effort: Option<&str>, tier: Option<&str>) -> ModelSelection {
    use atrium_engine::contracts::{
        ProviderOptionSelection, ProviderOptionSelections, ProviderOptionValue,
    };
    let mut options = Vec::new();
    if let Some(effort) = effort {
        options.push(ProviderOptionSelection {
            id: "reasoningEffort".into(),
            value: ProviderOptionValue::String(effort.into()),
        });
    }
    if let Some(tier) = tier {
        options.push(ProviderOptionSelection {
            id: "serviceTier".into(),
            value: ProviderOptionValue::String(tier.into()),
        });
    }
    ModelSelection {
        instance_id: "mockcodex".into(),
        // An absent model stays empty: Codex's own default.
        model: model.unwrap_or_default().into(),
        options: (!options.is_empty()).then_some(ProviderOptionSelections(options)),
    }
}

fn full_access_policy(scratch: &Path) -> RuntimePolicy {
    policy_for(Some("full-access"), scratch)
}

fn policy_for(mode: Option<&str>, scratch: &Path) -> RuntimePolicy {
    use atrium_engine::contracts::RuntimeMode;
    let name = mode.unwrap_or("full-access");
    let access = codex_client::AccessMode::from_name(name);
    let runtime_mode = if name == "auto-accept-edits" {
        RuntimeMode::AutoAcceptEdits
    } else {
        match access {
            codex_client::AccessMode::ReadOnly => RuntimeMode::ApprovalRequired,
            codex_client::AccessMode::Auto => RuntimeMode::Auto,
            codex_client::AccessMode::FullAccess => RuntimeMode::FullAccess,
        }
    };
    RuntimePolicy {
        runtime_mode,
        interaction_mode: Default::default(),
        cwd: Some(scratch.display().to_string()),
        // The same explicit pair the app's access mode and the engine use.
        approval_policy: Some(serde_json::json!(access.approval_policy())),
        sandbox_policy: Some(access.sandbox_policy()),
        max_runtime_mode: None,
    }
}

// --------------------------------------------------------------------------
// engine scenarios

struct EngineSession {
    runtime: CodexEngineRuntime,
    id: String,
    events: Receiver<AgentEvent>,
}

fn start_engine_session(
    binary: &Path,
    scratch: &Path,
    model: Option<&str>,
    real: bool,
) -> Result<EngineSession, String> {
    let runtime = CodexEngineRuntime::new(CodexAdapterConfig {
        codex: codex_config(binary, scratch, real),
        display_name: Some("Codex".into()),
        minimum_version: None,
        thread_env: Vec::new(),
        developer_instructions: None,
    });
    let (tx, rx) = mpsc::channel();
    let sink: EventSink = Arc::new(move |event| {
        let _ = tx.send(event);
    });
    let spec = SessionSpec {
        agent: "codex".into(),
        model: model.map(str::to_owned),
        effort: None,
        service_tier: None,
        folder: scratch.to_path_buf(),
        worktree: None,
        device: None,
        mcp_servers: Vec::new(),
        first_message: None,
        access_mode: Some("full-access".into()),
    };
    let id = runtime
        .start(spec, sink)
        .map_err(|error| format!("engine start: {error}"))?;
    Ok(EngineSession {
        runtime,
        id,
        events: rx,
    })
}

fn run_engine(
    scenario: &Scenario,
    binary: &Path,
    scratch: &Path,
    model: Option<&str>,
    real: bool,
    timeout: Duration,
) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    let session = start_engine_session(binary, scratch, model, real)?;
    push(&mut lines, NormalLine::thread_created());
    let mut final_status = "completed".to_string();
    for (ix, turn) in scenario.turns.iter().enumerate() {
        let ordinal = u32::try_from(ix + 1).unwrap_or(u32::MAX);
        match engine_turn_lines(&session, ordinal, turn.text(), timeout, &mut lines)? {
            TurnEnd::Finished(status) => {
                final_status = status.clone();
                if status != "completed" {
                    break;
                }
            }
            TurnEnd::Waiting => {
                final_status = "waiting".into();
                break;
            }
        }
    }
    if scenario.read_snapshot {
        let read = session.runtime.read_snapshot(&session.id);
        let history = session.runtime.read_history(&session.id);
        push_trace(
            &mut lines,
            json!({
                "kind": "engine.readSnapshot",
                "read": if read.is_ok() { "ok" } else { "failed" },
                "providerTurns": read.as_ref().map(|snapshot| snapshot.provider_turns.len()).unwrap_or_default(),
                "messages": history.as_ref().map(|history| history.turns.len()).unwrap_or_default(),
            }),
        );
    }
    push(&mut lines, NormalLine::thread_finished(&final_status));
    Ok(lines)
}

enum TurnEnd {
    Finished(String),
    Waiting,
}

fn engine_turn_lines(
    session: &EngineSession,
    ordinal: u32,
    text: &str,
    timeout: Duration,
    lines: &mut Vec<String>,
) -> Result<TurnEnd, String> {
    session
        .runtime
        .send(
            &session.id,
            TurnInput::text(with_sender_note(text)),
            Delivery::Start,
        )
        .map_err(|error| format!("engine send: {error}"))?;
    push(lines, NormalLine::run_started(ordinal));
    loop {
        match session.events.recv_timeout(timeout) {
            Ok(AgentEvent::ItemCompleted { item, .. }) => match item {
                Item::User { text, .. } => push(lines, NormalLine::item(ordinal, Role::User, text)),
                Item::Agent { text, .. } => {
                    push(lines, NormalLine::item(ordinal, Role::Assistant, text))
                }
                _ => {}
            },
            Ok(AgentEvent::ApprovalRequested(_)) | Ok(AgentEvent::UserInputRequested(_)) => {
                push(lines, NormalLine::approval_requested(ordinal));
                push(lines, NormalLine::run_finished(ordinal, "waiting"));
                return Ok(TurnEnd::Waiting);
            }
            Ok(AgentEvent::TurnCompleted { status, error, .. }) => {
                let status = match status {
                    TurnStatus::Completed => "completed",
                    TurnStatus::Interrupted => "interrupted",
                    TurnStatus::Failed => "failed",
                    TurnStatus::InProgress => "running",
                };
                if let Some(error) = error {
                    eprintln!("atrium-engine-driver: turn failed: {error}");
                }
                push(lines, NormalLine::run_finished(ordinal, status));
                return Ok(TurnEnd::Finished(normalize_status(status)));
            }
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {
                return Err(format!(
                    "engine produced no turn event for {} ms",
                    timeout.as_millis()
                ));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err("engine event channel closed".into());
            }
        }
    }
}

// --------------------------------------------------------------------------
// adapter scenarios

fn run_adapter(
    scenario: &Scenario,
    binary: &Path,
    scratch: &Path,
    model: Option<&str>,
    real: bool,
    timeout: Duration,
) -> Result<Vec<String>, String> {
    let config = CodexAdapterConfig {
        codex: codex_config(binary, scratch, real),
        display_name: Some("Codex".into()),
        minimum_version: None,
        thread_env: Vec::new(),
        developer_instructions: None,
    };
    let adapter = CodexAdapter::new("mockcodex", config);
    let (client, incoming) =
        futures::executor::block_on(CodexClient::start(codex_config(binary, scratch, real)))
            .map_err(|error| format!("starting Codex: {error}"))?;
    let (tx, events) = mpsc::channel();
    let sink: AdapterEventSink = Arc::new(move |event| {
        let _ = tx.send(event);
    });
    let mut policy = full_access_policy(scratch);
    let session = adapter
        .open_with_client(
            client.clone(),
            incoming,
            OpenSessionInput {
                thread_id: "adapter-app-thread".into(),
                provider_session_id: "adapter-session".into(),
                model_selection: selection(model, None, None),
                runtime_policy: policy.clone(),
                resume_from_session: None,
                initial_native_thread_id: None,
                now: now_iso(),
            },
            sink,
        )
        .map_err(|error| format!("adapter open: {error}"))?;
    let mut lines = Vec::new();
    let mut provider_thread = session
        .ensure_thread(EnsureThreadInput {
            thread_id: "adapter-app-thread".into(),
            title: None,
            model_selection: selection(model, None, None),
            runtime_policy: policy.clone(),
            mcp_servers: Vec::new(),
            env: Vec::new(),
            developer_instructions: None,
            provider_session_id: None,
            existing_provider_thread: None,
            force_new_native_thread: false,
            now: now_iso(),
        })
        .map_err(|error| format!("adapter ensure_thread: {error}"))?;
    let native = provider_thread
        .native_thread_ref
        .as_ref()
        .and_then(|native| native.native_id.clone())
        .unwrap_or_default();
    push(&mut lines, NormalLine::thread_created());
    let mut final_status = "completed".to_string();
    let mut ordinal = 0u32;
    let mut current_model = model.map(str::to_owned);
    // When the scenario does not name a model, use the catalog's default for
    // a mid-thread change; the first turn still omits it (Codex's default).
    let default_model = if current_model.is_none() {
        futures::executor::block_on(client.model_list())
            .ok()
            .and_then(|models| {
                models
                    .iter()
                    .find(|model| model.is_default)
                    .or_else(|| models.first())
                    .map(|model| model.id.clone())
            })
    } else {
        None
    };

    for action in &scenario.actions {
        match action.op.as_str() {
            "settings" => {
                let model_name = action
                    .model
                    .clone()
                    .or_else(|| current_model.clone())
                    .or_else(|| default_model.clone());
                let selection = selection(
                    model_name.as_deref(),
                    action.effort.as_deref(),
                    action.service_tier.as_deref(),
                );
                if let Some(runtime_mode) = action.runtime_mode.as_deref() {
                    policy.runtime_mode = policy_for(Some(runtime_mode), scratch).runtime_mode;
                }
                let update =
                    build_thread_settings_update(&native, &selection, policy.runtime_mode, None);
                session
                    .apply_model_selection(&provider_thread, &selection, Some(&policy))
                    .map_err(|error| format!("adapter settings: {error}"))?;
                let applied = await_settings_update(&events, timeout)?;
                push_trace(
                    &mut lines,
                    json!({
                        "kind": "adapter.settings",
                        "model": update.model,
                        "effort": update.effort,
                        "serviceTier": update.service_tier,
                        "approvalPolicy": update.approval_policy,
                        "applied": applied,
                    }),
                );
                current_model = model_name;
            }
            "skills" => {
                let groups = session
                    .skills(&[scratch.display().to_string()])
                    .map_err(|error| format!("adapter skills: {error}"))?;
                push_trace(
                    &mut lines,
                    json!({
                        "kind": "adapter.skills",
                        "groups": groups.len(),
                        "cwds": groups.iter().map(|group| group.cwd.clone()).collect::<Vec<_>>(),
                    }),
                );
            }
            "config" => {
                let config = session
                    .config(Some(&scratch.display().to_string()))
                    .map_err(|error| format!("adapter config: {error}"))?;
                push_trace(
                    &mut lines,
                    json!({
                        "kind": "adapter.config",
                        "configIsObject": config.config.is_object(),
                        "originsIsObject": config.origins.is_object(),
                    }),
                );
            }
            "read_snapshot" => {
                let snapshot = session
                    .read_thread_snapshot(&provider_thread)
                    .map_err(|error| format!("adapter read snapshot: {error}"))?;
                push_trace(
                    &mut lines,
                    json!({
                        "kind": "adapter.readSnapshot",
                        "messages": snapshot.messages.len(),
                        "turns": snapshot.provider_turns.len(),
                    }),
                );
            }
            "hold_writer" => {
                futures::executor::block_on(
                    client.request_raw("debug/holdWriter", json!({"threadId": native})),
                )
                .map_err(|error| format!("adapter hold_writer: {error}"))?;
                push_trace(&mut lines, json!({"kind": "adapter.writerHeld"}));
            }
            "read_vs_write" => {
                let read = session.read_thread_snapshot(&provider_thread);
                let resume = session.resume_thread(
                    &provider_thread,
                    Some(&selection(current_model.as_deref(), None, None)),
                    Some(&policy),
                    &now_iso(),
                );
                push_trace(
                    &mut lines,
                    json!({
                        "kind": "adapter.readVsWrite",
                        "read": if read.is_ok() { "ok" } else { "failed" },
                        "resume": match &resume {
                            Ok(_) => "ok".to_string(),
                            Err(error) => error.to_string(),
                        },
                    }),
                );
                if let Ok(thread) = resume {
                    provider_thread = thread;
                }
            }
            "resume" => {
                provider_thread = session
                    .resume_thread(
                        &provider_thread,
                        Some(&selection(current_model.as_deref(), None, None)),
                        Some(&policy),
                        &now_iso(),
                    )
                    .map_err(|error| format!("adapter resume: {error}"))?;
                push_trace(
                    &mut lines,
                    json!({"kind": "adapter.resume", "status": "active"}),
                );
            }
            "turn" => {
                ordinal += 1;
                if let Some(mode) = action.runtime_mode.as_deref() {
                    policy.runtime_mode = policy_for(Some(mode), scratch).runtime_mode;
                }
                let end = adapter_turn(
                    session.as_ref(),
                    &provider_thread,
                    ordinal,
                    action.text.as_deref().unwrap_or_default(),
                    current_model.as_deref(),
                    action.answer.as_deref(),
                    &policy,
                    &events,
                    timeout,
                    &mut lines,
                )?;
                match end {
                    TurnEnd::Finished(status) => {
                        final_status = status.clone();
                        if status != "completed" {
                            break;
                        }
                    }
                    TurnEnd::Waiting => {
                        final_status = "waiting".into();
                        break;
                    }
                }
            }
            other => return Err(format!("unknown adapter action {other}")),
        }
    }
    push(&mut lines, NormalLine::thread_finished(&final_status));
    session.close();
    Ok(lines)
}

fn await_settings_update(
    events: &Receiver<AdapterEvent>,
    timeout: Duration,
) -> Result<bool, String> {
    loop {
        match events.recv_timeout(timeout) {
            Ok(AdapterEvent::ProviderSessionUpdated { .. }) => return Ok(true),
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => return Ok(false),
            Err(RecvTimeoutError::Disconnected) => return Ok(false),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn adapter_turn(
    session: &dyn ProviderSessionRuntime,
    provider_thread: &ProviderThread,
    ordinal: u32,
    text: &str,
    model: Option<&str>,
    answer: Option<&str>,
    policy: &RuntimePolicy,
    events: &Receiver<AdapterEvent>,
    timeout: Duration,
    lines: &mut Vec<String>,
) -> Result<TurnEnd, String> {
    push(lines, NormalLine::run_started(ordinal));
    session
        .start_turn(AdapterTurnInput {
            thread_id: provider_thread
                .app_thread_id
                .clone()
                .unwrap_or_else(|| "adapter-app-thread".into()),
            run_id: format!("adapter-run-{ordinal}").into(),
            run_ordinal: ordinal,
            provider_turn_ordinal: ordinal,
            attempt_id: format!("adapter-attempt-{ordinal}").into(),
            root_node_id: format!("adapter-node-{ordinal}").into(),
            provider_thread: provider_thread.clone(),
            message: TurnMessage::user(
                format!("adapter-message-{ordinal}"),
                with_sender_note(text),
            ),
            model_selection: selection(model, None, None),
            runtime_policy: policy.clone(),
            app_context: Vec::new(),
            now: now_iso(),
        })
        .map_err(|error| format!("adapter start_turn: {error}"))?;
    let mut answered = false;
    loop {
        match events.recv_timeout(timeout) {
            Ok(AdapterEvent::Agent(AgentEvent::ItemCompleted { item, .. })) => match item {
                Item::User { text, .. } => push(lines, NormalLine::item(ordinal, Role::User, text)),
                Item::Agent { text, .. } => {
                    push(lines, NormalLine::item(ordinal, Role::Assistant, text))
                }
                _ => {}
            },
            Ok(AdapterEvent::RuntimeRequestUpdated {
                runtime_request, ..
            }) => {
                push(lines, NormalLine::approval_requested(ordinal));
                push_trace(
                    lines,
                    json!({
                        "kind": "adapter.runtimeRequest",
                        "requestKind": runtime_request.kind.as_wire(),
                        "status": "pending",
                    }),
                );
                let Some(answer) = answer else {
                    push(lines, NormalLine::run_finished(ordinal, "waiting"));
                    return Ok(TurnEnd::Waiting);
                };
                if !answered {
                    let decision = match answer {
                        "decline" => atrium_engine::contracts::ProviderApprovalDecision::Decline,
                        "cancel" => atrium_engine::contracts::ProviderApprovalDecision::Cancel,
                        _ => atrium_engine::contracts::ProviderApprovalDecision::Accept,
                    };
                    session
                        .respond_to_runtime_request(RuntimeRequestResponse {
                            request_id: runtime_request.id.to_string(),
                            decision: Some(decision),
                            feedback: None,
                            answers: None,
                        })
                        .map_err(|error| format!("adapter respond: {error}"))?;
                    answered = true;
                    push_trace(
                        lines,
                        json!({"kind": "adapter.responded", "decision": answer}),
                    );
                }
            }
            Ok(AdapterEvent::Agent(AgentEvent::TurnCompleted { status, .. })) => {
                let status = match status {
                    TurnStatus::Completed => "completed",
                    TurnStatus::Interrupted => "interrupted",
                    TurnStatus::Failed => "failed",
                    TurnStatus::InProgress => "running",
                };
                push(lines, NormalLine::run_finished(ordinal, status));
                return Ok(TurnEnd::Finished(normalize_status(status)));
            }
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {
                return Err(format!(
                    "adapter produced no turn event for {} ms",
                    timeout.as_millis()
                ));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err("adapter event channel closed".into());
            }
        }
    }
}

// --------------------------------------------------------------------------
// scratch space and binaries

fn with_sender_note(text: &str) -> String {
    if text.trim_start().starts_with('/') {
        text.to_string()
    } else {
        format!("{T3_SENDER_NOTE}\n\n{text}")
    }
}

struct Scratch {
    path: PathBuf,
    owned: bool,
    keep: bool,
}

impl Scratch {
    fn new(project: &str, args: &Args) -> Result<Self, String> {
        if let Some(path) = &args.scratch {
            fs::create_dir_all(path)
                .map_err(|error| format!("creating scratch {}: {error}", path.display()))?;
            return Ok(Self {
                path: path.clone(),
                owned: false,
                keep: true,
            });
        }
        if project != "scratch" {
            let path = PathBuf::from(project);
            fs::create_dir_all(&path)
                .map_err(|error| format!("creating project {}: {error}", path.display()))?;
            return Ok(Self {
                path,
                owned: false,
                keep: true,
            });
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = env::temp_dir().join(format!(
            "atrium-engine-driver-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&path)
            .map_err(|error| format!("creating scratch {}: {error}", path.display()))?;
        Ok(Self {
            path,
            owned: true,
            keep: args.keep_scratch,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if self.owned && !self.keep {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn find_mock_codex() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("MOCK_CODEX_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "MOCK_CODEX_BIN points at {}, which is not a file",
            path.display()
        ));
    }

    let mut target_dirs = Vec::new();
    if let Some(dir) = env::var_os("CARGO_TARGET_DIR") {
        target_dirs.push(PathBuf::from(dir));
    }
    if let Some(root) = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2) {
        target_dirs.push(root.join("target"));
    }
    if let Ok(cwd) = env::current_dir() {
        target_dirs.push(cwd.join("target"));
    }
    let name = format!("mock-codex{}", env::consts::EXE_SUFFIX);
    for dir in target_dirs {
        for profile in ["debug", "release"] {
            let candidate = dir.join(profile).join(&name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    if let Some(path) = find_in_path("mock-codex") {
        return Ok(path);
    }
    Err(
        "mock-codex not found; build it (cargo build -p codex-client --bin mock-codex) \
         or set MOCK_CODEX_BIN"
            .to_string(),
    )
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let exe = format!("{name}{}", env::consts::EXE_SUFFIX);
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|dir| dir.join(&exe))
            .find(|candidate| candidate.is_file())
    })
}
