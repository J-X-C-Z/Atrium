//! Scenario driver for `tools/t3-diff`.
//!
//! Reads one JSON scenario from stdin, drives `atrium-engine` the way the T3
//! scenario drives T3, and writes the normalized event stream as JSON lines to
//! stdout. tools/t3-diff diffs the two streams.
//!
//! The parallel ports of `orchestration` and `store` are not merged in this
//! worktree, so this driver runs a **deliberately small in-memory equivalent**
//! of the orchestration path: one thread, sequential turns, the same
//! append-only, one-turn-at-a-time semantics T3's CLI has. It speaks the real
//! Codex app-server protocol to the same `mock-codex` binary T3 uses, so the
//! normalized items and statuses come from the same provider output. Replace
//! [`run_scenario`] with calls into `atrium-engine::orchestration` when that
//! module lands; the normalizer in [`atrium_engine::diff`] does not change.
//!
//! Scenario (JSON on stdin):
//!
//! ```json
//! {
//!   "name": "01-thread-lifecycle",
//!   "project": "scratch",
//!   "provider": "mockcodex",
//!   "model": "mock-1",
//!   "turns": [{"text": "say hi"}, {"text": "now say bye"}],
//!   "expect": {"final_status": "completed"}
//! }
//! ```
//!
//! `project` is `"scratch"` or a directory. `turns` entries may also be plain
//! strings. Exit codes: 0 the scenario ran, 2 setup/environment error (no mock
//! binary, protocol failure, timeout).

use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use atrium_engine::diff::{NormalLine, Role, T3_SENDER_NOTE};
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
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    turns: Vec<TurnSpec>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    expect: Option<Expect>,
}

#[derive(Debug, Deserialize)]
struct Expect {
    #[serde(default)]
    final_status: Option<String>,
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

fn run(args: &Args) -> Result<(), String> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| format!("reading scenario: {error}"))?;
    let scenario: Scenario =
        serde_json::from_str(&input).map_err(|error| format!("parsing scenario: {error}"))?;

    let mock = find_mock_codex()?;
    let scratch = Scratch::new(scenario.project.as_str(), args)?;
    let timeout = scenario
        .timeout_ms
        .map(|millis| Duration::from_millis(millis.max(1000)))
        .unwrap_or(args.timeout);
    let model = scenario.model.as_deref().unwrap_or(DEFAULT_MODEL);

    let mut client = MockClient::spawn(&mock, scratch.path(), timeout)?;
    let mut lines = Vec::new();

    client.request(
        "initialize",
        json!({
            "clientInfo": {"name": "atrium-engine-driver", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {}
        }),
    )?;
    client.request("account/read", json!({}))?;
    let provider = scenario
        .provider
        .as_deref()
        .unwrap_or("mockcodex")
        .to_string();
    let launched = client.request(
        "thread/start",
        json!({
            "cwd": scratch.path().display().to_string(),
            "providerInstanceId": provider,
            "model": model,
            "modelProvider": "mock",
            "approvalPolicy": "never",
            "approvalsReviewer": "user",
            "sandbox": {"type": "dangerFullAccess"},
            "ephemeral": false
        }),
    )?;
    let thread_id = launched
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .ok_or_else(|| "thread/start returned no thread id".to_string())?
        .to_string();
    lines.push(NormalLine::thread_created());

    let mut final_status = "completed".to_string();
    for (ix, turn) in scenario.turns.iter().enumerate() {
        let ordinal = u32::try_from(ix + 1).unwrap_or(u32::MAX);
        match run_turn(
            &mut client,
            &thread_id,
            ordinal,
            turn.text(),
            model,
            &mut lines,
        )? {
            TurnEnd::Finished(status) => {
                let succeeded = status == "completed";
                final_status = status;
                if !succeeded {
                    break;
                }
            }
            TurnEnd::Waiting => {
                final_status = "waiting".to_string();
                break;
            }
        }
    }
    lines.push(NormalLine::thread_finished(&final_status));

    let rendered = atrium_engine::diff::render(&lines);
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(rendered.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|error| format!("writing stream: {error}"))?;

    // The expectation is enforced by the harness; the driver only reports the
    // stream. Keeping it here too makes `--no-t3` runs useful by themselves.
    if let Some(expect) = &scenario.expect
        && let Some(expected) = &expect.final_status
        && expected.to_ascii_lowercase() != final_status
    {
        eprintln!(
            "atrium-engine-driver: scenario {} expected final status {}, got {}",
            scenario.name, expected, final_status
        );
    }

    drop(client);
    Ok(())
}

enum TurnEnd {
    Finished(String),
    Waiting,
}

fn run_turn(
    client: &mut MockClient,
    thread_id: &str,
    ordinal: u32,
    text: &str,
    model: &str,
    lines: &mut Vec<NormalLine>,
) -> Result<TurnEnd, String> {
    lines.push(NormalLine::run_started(ordinal));
    let id = client.alloc_id();
    client.send(&json!({
        "id": id,
        "method": "turn/start",
        "params": {
            "threadId": thread_id,
            "input": [{"type": "text", "text": with_sender_note(text), "text_elements": []}],
            "model": model,
            "approvalPolicy": "never",
            "sandboxPolicy": {"type": "dangerFullAccess"}
        }
    }))?;

    loop {
        let message = client.recv()?;
        if message.get("id").and_then(Value::as_i64) == Some(id as i64) {
            if let Some(error) = message.get("error") {
                let detail = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("turn/start failed");
                lines.push(NormalLine::run_finished(ordinal, "failed"));
                return Err(format!("turn/start failed: {detail}"));
            }
            continue;
        }

        let Some(method) = message.get("method").and_then(Value::as_str) else {
            continue;
        };
        match method {
            "item/completed" => {
                if let Some(item) = message.pointer("/params/item")
                    && let Some((role, text)) = item_message(item)
                {
                    lines.push(NormalLine::item(ordinal, role, text));
                }
            }
            "turn/completed" => {
                let status = message
                    .pointer("/params/turn/status")
                    .and_then(Value::as_str)
                    .unwrap_or("failed");
                lines.push(NormalLine::run_finished(ordinal, status));
                return Ok(TurnEnd::Finished(atrium_engine::diff::normalize_status(
                    status,
                )));
            }
            // A person must answer before the turn can go on. The engine has
            // no user here, exactly like the T3 CLI with `--wait`.
            "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/permissions/requestApproval"
            | "item/tool/requestUserInput" => {
                lines.push(NormalLine::approval_requested(ordinal));
                lines.push(NormalLine::run_finished(ordinal, "waiting"));
                return Ok(TurnEnd::Waiting);
            }
            _ => {}
        }
    }
}

fn item_message(item: &Value) -> Option<(Role, String)> {
    match item.get("type")?.as_str()? {
        "userMessage" => {
            let content = item.get("content")?.as_array()?;
            let text = content
                .iter()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("");
            Some((Role::User, text))
        }
        "agentMessage" => Some((
            Role::Assistant,
            item.get("text").and_then(Value::as_str)?.to_string(),
        )),
        _ => None,
    }
}

/// T3 adds a sender note to messages an agent sends through its tools. The
/// driver sends it too so the scripted mock produces the same answer on both
/// sides; [`atrium_engine::diff::normalize_text`] removes it again.
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

struct MockClient {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
    timeout: Duration,
}

impl MockClient {
    fn spawn(binary: &Path, cwd: &Path, timeout: Duration) -> Result<Self, String> {
        let codex_home = cwd.join(".mock-codex-home");
        fs::create_dir_all(&codex_home)
            .map_err(|error| format!("creating CODEX_HOME {}: {error}", codex_home.display()))?;
        let mut child = Command::new(binary)
            .arg("app-server")
            .current_dir(cwd)
            .env("CODEX_HOME", &codex_home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("starting {}: {error}", binary.display()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "mock-codex has no stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "mock-codex has no stdout".to_string())?;
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            lines,
            next_id: 0,
            timeout,
        })
    }

    fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn send(&mut self, message: &Value) -> Result<(), String> {
        writeln!(self.stdin, "{message}")
            .map_err(|error| format!("writing to mock-codex: {error}"))?;
        self.stdin
            .flush()
            .map_err(|error| format!("flushing mock-codex: {error}"))
    }

    fn recv(&self) -> Result<Value, String> {
        loop {
            match self.lines.recv_timeout(self.timeout) {
                Ok(line) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<Value>(line) {
                        Ok(value) => return Ok(value),
                        Err(_) => continue,
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!(
                        "mock-codex sent nothing for {} ms",
                        self.timeout.as_millis()
                    ));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("mock-codex closed its output".to_string());
                }
            }
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.alloc_id();
        self.send(&json!({"id": id, "method": method, "params": params}))?;
        loop {
            let message = self.recv()?;
            if message.get("id").and_then(Value::as_i64) != Some(id as i64) {
                continue;
            }
            if let Some(error) = message.get("error") {
                let detail = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error");
                return Err(format!("{method} failed: {detail}"));
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

impl Drop for MockClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
