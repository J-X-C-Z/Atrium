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
use atrium_engine::acp_runtime::AcpEngineRuntime;
use atrium_engine::contracts::{ModelSelection, ProviderThread};
use atrium_engine::diff::{NormalLine, Role, T3_SENDER_NOTE, normalize_status};
use atrium_engine::providers::adapter::{
    AdapterEvent, AdapterEventSink, AdapterTurnInput, EnsureThreadInput, OpenSessionInput,
    ProviderSessionRuntime, RuntimePolicy, RuntimeRequestResponse, TurnMessage,
};
use atrium_engine::providers::build_thread_settings_update;
use atrium_engine::providers::codex::{CodexAdapter, CodexAdapterConfig};
use atrium_engine::providers::{
    AcpAdapter, AcpAdapterConfig, AcpFlavor, ProviderAdapter, ProviderConfig, SkillGroup,
};
use atrium_engine::runtime::CodexEngineRuntime;
use atrium_runtime::{AgentRuntime, Delivery, EventSink, SessionSpec, TurnInput};
use codex_client::{CodexClient, CodexConfig};
use serde::Deserialize;
use serde_json::{Value, json};

const DEFAULT_MODEL: &str = "mock-1";

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
        "usage: atrium-engine-driver [--scratch DIR] [--timeout-ms N] [--keep-scratch] < scenario.json\n\
         env: MOCK_CODEX_BIN (default: target/{{debug,release}}/mock-codex or PATH)"
    );
}

struct Args {
    scratch: Option<PathBuf>,
    timeout: Duration,
    keep_scratch: bool,
}

impl Args {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut parsed = Self {
            scratch: None,
            timeout: Duration::from_secs(90),
            keep_scratch: false,
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
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    expect: Option<Expect>,
    /// The mock ACP agent whose handshake to mimic (`claude`, `antigravity`).
    #[serde(default)]
    mock_name: Option<String>,
    /// `generic` (default) or `antigravity`.
    #[serde(default)]
    flavor: Option<String>,
    /// Ask the mock to report rate limits and trace what the engine returns.
    #[serde(default)]
    report_limits: bool,
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

    let scratch = Scratch::new(scenario.project.as_str(), args)?;
    let timeout = scenario
        .timeout_ms
        .map(|millis| Duration::from_millis(millis.max(1000)))
        .unwrap_or(args.timeout);
    let model = scenario
        .model
        .clone()
        .unwrap_or_else(|| DEFAULT_MODEL.into());

    let lines = match scenario.kind.as_deref() {
        Some("adapter") => {
            let mock = find_mock_codex()?;
            run_adapter(&scenario, &mock, scratch.path(), &model, timeout)?
        }
        Some("acp-engine") => {
            let mock = find_mock_acp()?;
            run_acp_engine(&scenario, &mock, scratch.path(), &model, timeout)?
        }
        Some("acp-adapter") => {
            let mock = find_mock_acp()?;
            run_acp_adapter(&scenario, &mock, scratch.path(), &model, timeout)?
        }
        _ => {
            let mock = find_mock_codex()?;
            run_engine(&scenario, &mock, scratch.path(), &model, timeout)?
        }
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

fn codex_config(mock: &Path, scratch: &Path) -> CodexConfig {
    let codex_home = scratch.join(".mock-codex-home");
    let _ = fs::create_dir_all(&codex_home);
    CodexConfig {
        binary: Some(mock.to_path_buf()),
        extra_args: Vec::new(),
        env: vec![("CODEX_HOME".into(), codex_home.display().to_string())],
    }
}

fn now_iso() -> String {
    jiff::Timestamp::now().to_string()
}

fn selection(model: &str, effort: Option<&str>, tier: Option<&str>) -> ModelSelection {
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
        model: model.into(),
        options: (!options.is_empty()).then_some(ProviderOptionSelections(options)),
    }
}

fn full_access_policy(scratch: &Path) -> RuntimePolicy {
    RuntimePolicy {
        runtime_mode: atrium_engine::contracts::RuntimeMode::FullAccess,
        interaction_mode: Default::default(),
        cwd: Some(scratch.display().to_string()),
        approval_policy: None,
        sandbox_policy: None,
        max_runtime_mode: None,
    }
}

fn policy_for(mode: Option<&str>, scratch: &Path) -> RuntimePolicy {
    use atrium_engine::contracts::RuntimeMode;
    let mut policy = full_access_policy(scratch);
    policy.runtime_mode = match mode.unwrap_or("full-access") {
        "approval-required" | "read-only" => RuntimeMode::ApprovalRequired,
        "auto-accept-edits" => RuntimeMode::AutoAcceptEdits,
        "auto" => RuntimeMode::Auto,
        _ => RuntimeMode::FullAccess,
    };
    policy
}

// --------------------------------------------------------------------------
// engine scenarios

struct EngineSession {
    runtime: CodexEngineRuntime,
    id: String,
    events: Receiver<AgentEvent>,
}

fn start_engine_session(mock: &Path, scratch: &Path, model: &str) -> Result<EngineSession, String> {
    let runtime = CodexEngineRuntime::new(CodexAdapterConfig {
        codex: codex_config(mock, scratch),
        display_name: Some("Mock Codex".into()),
        minimum_version: None,
    });
    let (tx, rx) = mpsc::channel();
    let sink: EventSink = Arc::new(move |event| {
        let _ = tx.send(event);
    });
    let spec = SessionSpec {
        agent: "codex".into(),
        model: Some(model.into()),
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
    mock: &Path,
    scratch: &Path,
    model: &str,
    timeout: Duration,
) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    let session = start_engine_session(mock, scratch, model)?;
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
    push(&mut lines, NormalLine::thread_finished(&final_status));
    Ok(lines)
}

enum TurnEnd {
    Finished(String),
    Waiting,
}

// --------------------------------------------------------------------------
// ACP scenarios

fn find_mock_acp() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("MOCK_ACP_BIN") {
        return Ok(PathBuf::from(path));
    }
    if let Ok(exe) = env::current_exe()
        && let Some(directory) = exe.parent()
    {
        let candidate = directory.join(if cfg!(windows) {
            "mock-acp.exe"
        } else {
            "mock-acp"
        });
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err("mock-acp not found; build it or set MOCK_ACP_BIN".into())
}

fn acp_config(mock: &Path, scenario: &Scenario) -> AcpAdapterConfig {
    let name = scenario
        .mock_name
        .clone()
        .unwrap_or_else(|| "claude".into());
    let flavor = match scenario.flavor.as_deref() {
        Some("antigravity") => AcpFlavor::Antigravity,
        _ if name == "antigravity" => AcpFlavor::Antigravity,
        _ => AcpFlavor::Generic,
    };
    let backend = if flavor == AcpFlavor::Antigravity {
        "antigravity"
    } else {
        "acp"
    };
    let mut config =
        AcpAdapterConfig::for_command(backend, mock.to_path_buf(), vec!["--name".into(), name]);
    config.flavor = flavor;
    if scenario.report_limits {
        config
            .acp
            .env
            .push(("ATRIUM_MOCK_ACP_RATE_LIMITS".into(), "1".into()));
    }
    config
}

struct AcpEngineSession {
    runtime: AcpEngineRuntime,
    id: String,
    events: Receiver<AgentEvent>,
}

fn run_acp_engine(
    scenario: &Scenario,
    mock: &Path,
    scratch: &Path,
    model: &str,
    timeout: Duration,
) -> Result<Vec<String>, String> {
    let runtime = AcpEngineRuntime::new(acp_config(mock, scenario));
    let (tx, rx) = mpsc::channel();
    let sink: EventSink = Arc::new(move |event| {
        let _ = tx.send(event);
    });
    let agent = scenario.flavor.clone().unwrap_or_else(|| "claude".into());
    let spec = SessionSpec {
        agent: agent.clone(),
        model: Some(model.into()),
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
        .map_err(|error| format!("acp engine start: {error}"))?;
    let session = AcpEngineSession {
        runtime,
        id,
        events: rx,
    };
    let mut lines = Vec::new();
    push(&mut lines, NormalLine::thread_created());
    let mut final_status = "completed".to_string();
    for (ix, turn) in scenario.turns.iter().enumerate() {
        let ordinal = u32::try_from(ix + 1).unwrap_or(u32::MAX);
        match acp_engine_turn_lines(&session, ordinal, turn.text(), timeout, &mut lines)? {
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
    if scenario.report_limits {
        let limits = session
            .runtime
            .limits(&agent)
            .map_err(|error| format!("acp engine limits: {error}"))?;
        push_trace(
            &mut lines,
            json!({
                "kind": "engine.limits",
                "present": limits.is_some(),
                "primary": limits.as_ref().and_then(|limits| limits.primary).map(|window| window.used_percent),
                "secondary": limits.as_ref().and_then(|limits| limits.secondary).map(|window| window.used_percent),
                "reached": limits.as_ref().is_some_and(|limits| limits.reached),
            }),
        );
        if let Some(scan) = session.runtime.usage_scan(0) {
            push_trace(
                &mut lines,
                json!({
                    "kind": "engine.usageScan",
                    "files": scan.files.len(),
                    "records": scan.files.iter().map(|file| file.records.len()).sum::<usize>(),
                    "errors": scan.errors.len(),
                }),
            );
        }
    }
    push(&mut lines, NormalLine::thread_finished(&final_status));
    Ok(lines)
}

fn acp_engine_turn_lines(
    session: &AcpEngineSession,
    ordinal: u32,
    text: &str,
    timeout: Duration,
    lines: &mut Vec<String>,
) -> Result<TurnEnd, String> {
    push(lines, NormalLine::run_started(ordinal));
    // ACP ignores the live user chunk (the app shows the sent message
    // itself), so the normalized stream records it here.
    push(
        lines,
        NormalLine::item(ordinal, Role::User, text.to_string()),
    );
    session
        .runtime
        .send(
            &session.id,
            TurnInput::text(with_sender_note(text)),
            Delivery::Start,
        )
        .map_err(|error| format!("acp engine send: {error}"))?;
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
                    "acp engine produced no turn event for {} ms",
                    timeout.as_millis()
                ));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err("acp engine event channel closed".into());
            }
        }
    }
}

fn run_acp_adapter(
    scenario: &Scenario,
    mock: &Path,
    scratch: &Path,
    model: &str,
    timeout: Duration,
) -> Result<Vec<String>, String> {
    let adapter = AcpAdapter::new("acp-adapter", acp_config(mock, scenario));
    let (tx, events) = mpsc::channel();
    let sink: AdapterEventSink = Arc::new(move |event| {
        let _ = tx.send(event);
    });
    let policy = full_access_policy(scratch);
    let mut selection = selection(model, None, None);
    selection.instance_id = "acp-adapter".into();
    let session = adapter
        .open_session(
            OpenSessionInput {
                thread_id: "acp-adapter-app-thread".into(),
                provider_session_id: "acp-adapter-session".into(),
                model_selection: selection.clone(),
                runtime_policy: policy.clone(),
                resume_from_session: None,
                initial_native_thread_id: None,
                now: now_iso(),
            },
            sink,
        )
        .map_err(|error| format!("acp adapter open: {error}"))?;
    let mut provider_thread = session
        .ensure_thread(EnsureThreadInput {
            thread_id: "acp-adapter-app-thread".into(),
            title: None,
            model_selection: selection.clone(),
            runtime_policy: policy.clone(),
            provider_session_id: None,
            existing_provider_thread: None,
            force_new_native_thread: false,
            now: now_iso(),
        })
        .map_err(|error| format!("acp adapter ensure_thread: {error}"))?;
    let mut lines = Vec::new();
    push(&mut lines, NormalLine::thread_created());
    let mut final_status = "completed".to_string();
    let mut ordinal = 0u32;
    for action in &scenario.actions {
        match action.op.as_str() {
            "settings" => {
                if let Some(model) = &action.model {
                    selection.model = model.clone();
                }
                if let Some(effort) = &action.effort {
                    selection.options =
                        Some(atrium_engine::contracts::ProviderOptionSelections(vec![
                            atrium_engine::contracts::ProviderOptionSelection {
                                id: "reasoningEffort".into(),
                                value: atrium_engine::contracts::ProviderOptionValue::String(
                                    effort.clone(),
                                ),
                            },
                        ]));
                }
                session
                    .apply_model_selection(&provider_thread, &selection, Some(&policy))
                    .map_err(|error| format!("acp adapter settings: {error}"))?;
                let config = session.config(None).map_err(|error| error.to_string())?;
                push_trace(
                    &mut lines,
                    json!({
                        "kind": "adapter.settings",
                        "model": selection.model,
                        "options": config.config["configOptions"],
                    }),
                );
            }
            "skills" => {
                let groups: Vec<SkillGroup> = session
                    .skills(&[scratch.display().to_string()])
                    .map_err(|error| format!("acp adapter skills: {error}"))?;
                push_trace(
                    &mut lines,
                    json!({
                        "kind": "adapter.skills",
                        "groups": groups.len(),
                        "skills": groups.iter().map(|group| group.skills.len()).sum::<usize>(),
                        "errors": groups.iter().map(|group| group.errors.len()).sum::<usize>(),
                    }),
                );
            }
            "config" => {
                let config: ProviderConfig =
                    session
                        .config(Some(&scratch.display().to_string()))
                        .map_err(|error| format!("acp adapter config: {error}"))?;
                push_trace(
                    &mut lines,
                    json!({
                        "kind": "adapter.config",
                        "configIsObject": config.config.is_object(),
                        "originsIsObject": config.origins.is_object(),
                    }),
                );
            }
            "sign_in" => {
                // The mock accepts every method; the trace records which
                // method an Antigravity instance would use.
                push_trace(
                    &mut lines,
                    json!({
                        "kind": "adapter.signIn",
                        "method": action.text.clone().unwrap_or_else(|| "oauth-personal".into()),
                        "methods": adapter.config().flavor.auth_methods().len(),
                    }),
                );
            }
            "turn" => {
                ordinal += 1;
                let end = acp_adapter_turn(
                    session.as_ref(),
                    &mut provider_thread,
                    ordinal,
                    action.text.as_deref().unwrap_or_default(),
                    action.answer.as_deref(),
                    &selection,
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
            other => return Err(format!("unknown acp adapter action {other}")),
        }
    }
    push(&mut lines, NormalLine::thread_finished(&final_status));
    session.close();
    Ok(lines)
}

#[allow(clippy::too_many_arguments)]
fn acp_adapter_turn(
    session: &dyn ProviderSessionRuntime,
    provider_thread: &mut ProviderThread,
    ordinal: u32,
    text: &str,
    answer: Option<&str>,
    selection: &ModelSelection,
    policy: &RuntimePolicy,
    events: &Receiver<AdapterEvent>,
    timeout: Duration,
    lines: &mut Vec<String>,
) -> Result<TurnEnd, String> {
    push(lines, NormalLine::run_started(ordinal));
    let turn = session
        .start_turn(AdapterTurnInput {
            thread_id: provider_thread
                .app_thread_id
                .clone()
                .unwrap_or_else(|| "acp-adapter-app-thread".into()),
            run_id: format!("acp-adapter-run-{ordinal}").into(),
            run_ordinal: ordinal,
            provider_turn_ordinal: ordinal,
            attempt_id: format!("acp-adapter-attempt-{ordinal}").into(),
            root_node_id: format!("acp-adapter-node-{ordinal}").into(),
            provider_thread: provider_thread.clone(),
            message: TurnMessage::user(format!("acp-adapter-message-{ordinal}"), text),
            model_selection: selection.clone(),
            runtime_policy: policy.clone(),
            app_context: Vec::new(),
            attachment_paths: Vec::new(),
            now: now_iso(),
        })
        .map_err(|error| format!("acp adapter start_turn: {error}"))?;
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
                    if runtime_request.kind
                        == atrium_engine::contracts::RuntimeRequestKind::UserInput
                    {
                        let mut answers = atrium_engine::contracts::ProviderUserInputAnswers::new();
                        answers.insert(
                            runtime_request
                                .native_request_ref
                                .as_ref()
                                .and_then(|native| native.native_id.clone())
                                .unwrap_or_default(),
                            json!(answer),
                        );
                        session
                            .respond_to_runtime_request(RuntimeRequestResponse {
                                request_id: runtime_request.id.to_string(),
                                decision: None,
                                feedback: None,
                                answers: Some(answers),
                            })
                            .map_err(|error| format!("acp adapter answer: {error}"))?;
                    } else {
                        let decision = match answer {
                            "decline" => {
                                atrium_engine::contracts::ProviderApprovalDecision::Decline
                            }
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
                            .map_err(|error| format!("acp adapter respond: {error}"))?;
                    }
                    answered = true;
                    push_trace(
                        lines,
                        json!({"kind": "adapter.responded", "decision": answer}),
                    );
                }
            }
            Ok(AdapterEvent::ProviderTurnUpdated { provider_turn, .. })
                if provider_turn.id == turn.id
                    && provider_turn.status
                        != atrium_engine::contracts::ProviderTurnStatus::Running =>
            {
                if let Some(usage) = &provider_turn.turn_token_usage {
                    let mut trace = json!({"kind": "adapter.usage"});
                    if let (Some(trace), Some(usage)) = (trace.as_object_mut(), usage.as_object()) {
                        trace.extend(usage.clone());
                    }
                    push_trace(lines, trace);
                }
                // The agent's TurnCompleted follows and ends the scenario
                // turn; this record only carries the per-turn usage.
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
                    "acp adapter produced no turn event for {} ms",
                    timeout.as_millis()
                ));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err("acp adapter event channel closed".into());
            }
        }
    }
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
    mock: &Path,
    scratch: &Path,
    model: &str,
    timeout: Duration,
) -> Result<Vec<String>, String> {
    let config = CodexAdapterConfig {
        codex: codex_config(mock, scratch),
        display_name: Some("Mock Codex".into()),
        minimum_version: None,
    };
    let adapter = CodexAdapter::new("mockcodex", config);
    let (client, incoming) =
        futures::executor::block_on(CodexClient::start(codex_config(mock, scratch)))
            .map_err(|error| format!("starting mock-codex: {error}"))?;
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
    let mut current_model = model.to_string();

    for action in &scenario.actions {
        match action.op.as_str() {
            "settings" => {
                let model_name = action.model.as_deref().unwrap_or(&current_model);
                let selection = selection(
                    model_name,
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
                current_model = model_name.to_string();
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
                    Some(&selection(&current_model, None, None)),
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
                        Some(&selection(&current_model, None, None)),
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
            model_selection: selection("mock-1", None, None),
            runtime_policy: policy.clone(),
            app_context: Vec::new(),
            attachment_paths: Vec::new(),
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
