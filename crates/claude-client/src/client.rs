// Ported from apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts (T3 Code v0.1.11, MIT).
// The spawn/control-pipe structure mirrors T3's `ClaudeAgentSdkQueryRunner`
// (`open`, `interrupt`, `setModel`, `setPermissionMode`, `listPermissionRules`)
// and the Agent SDK 0.3.321 `ProcessTransport` it drives.

//! Spawns `claude` in bidirectional stream-JSON mode and runs the control
//! channel.
//!
//! The client is deliberately runtime-free: a plain thread reads stdout and
//! pushes frames into a `futures` channel, and control replies are matched on
//! that same thread through a pending-request map. Callers can therefore use
//! it from GPUI's executor, Tokio, or a synchronous test.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc as std_mpsc,
    },
    thread,
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow, bail};
use futures::channel::mpsc;
use serde_json::{Value, json};

use crate::{
    args::{LaunchArgs, build_args, initialize_request},
    events::{ClaudeEvent, Effort, PermissionMode, SessionInit, apply_claude_prompt_effort_prefix},
    protocol::{
        ControlRequestEnvelope, ControlResponseBody, ControlResponseEnvelope, PermissionDecision,
        PermissionRequest, SessionInfo, UserContentBlock, UserTurnInput,
        session_info_from_response,
    },
};

/// How to start a `claude` process.
#[derive(Debug, Clone)]
pub struct ClaudeConfig {
    /// The `claude` executable, or a path to it.
    pub binary: PathBuf,
    /// Command-line settings.
    pub launch: LaunchArgs,
    pub cwd: Option<PathBuf>,
    /// Replaces, rather than extends, the inherited environment, as the SDK
    /// documents for its `env` option.
    pub env: Vec<(String, String)>,
    /// Sent in the `initialize` control request.
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    /// Custom subagent definitions, keyed by name.
    pub agents: Option<Value>,
    /// Skills to enable; `None` leaves the CLI's own list untouched.
    pub skills: Option<Vec<String>>,
    pub handshake_timeout: Duration,
    pub control_timeout: Duration,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("claude"),
            launch: LaunchArgs::default(),
            cwd: None,
            env: Vec::new(),
            system_prompt: None,
            append_system_prompt: None,
            agents: None,
            skills: None,
            handshake_timeout: Duration::from_secs(30),
            control_timeout: Duration::from_secs(30),
        }
    }
}

/// One frame delivered to the caller.
#[derive(Debug, Clone)]
pub enum Incoming {
    /// A translated stream-JSON frame.
    Event(ClaudeEvent),
    /// A `can_use_tool` prompt. Answer it with
    /// [`ClaudeClient::answer_permission`].
    Permission(PermissionRequest),
    /// Another control request the host may answer (hooks, elicitation,
    /// dialogs). Answer it with [`ClaudeClient::answer_control`] or
    /// [`ClaudeClient::reject_control`].
    Control(ControlRequestEnvelope),
    /// A frame that does not map to a typed event (kept for logging).
    Frame(Value),
    /// A line that was not valid JSON.
    Malformed { line: String, error: String },
    /// The process closed stdout.
    Closed,
}

/// Per-turn settings. Model and effort can change between turns.
#[derive(Debug, Clone, Default)]
pub struct TurnOptions {
    /// Switch the session to this model before sending (`set_model`).
    pub model: Option<String>,
    /// Effort for this prompt. `UltraThink` is prompt-injected exactly as T3
    /// does; other levels require a new session because the CLI only takes
    /// `--effort` at launch.
    pub effort: Option<Effort>,
    /// Client uuid the CLI echoes back, for binding replies to a send.
    pub uuid: Option<String>,
    /// Marks a host-composed prompt.
    pub client_composed: bool,
}

struct Inner {
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Option<Child>>,
    pending: Mutex<HashMap<String, std_mpsc::Sender<ControlResponseBody>>>,
    permissions: Mutex<HashMap<String, PermissionRequest>>,
    session_id: Mutex<Option<String>>,
    session_info: Mutex<Option<SessionInfo>>,
    system_init: Mutex<Option<SessionInit>>,
    model: Mutex<Option<String>>,
    stderr_tail: Mutex<String>,
    closed: AtomicBool,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Ok(mut stdin) = self.stdin.lock()
            && let Some(mut stdin) = stdin.take()
        {
            let _ = stdin.flush();
        }
        if let Ok(mut child) = self.child.lock()
            && let Some(mut child) = child.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A running `claude` process. Cheap to clone.
#[derive(Clone)]
pub struct ClaudeClient {
    inner: Arc<Inner>,
    next_id: Arc<AtomicU64>,
    control_timeout: Duration,
}

impl ClaudeClient {
    /// Spawns the CLI, runs the `initialize` handshake, and returns the frame
    /// stream. The handshake blocks the calling thread until the control
    /// response arrives or `handshake_timeout` expires.
    pub async fn start(config: ClaudeConfig) -> Result<(Self, mpsc::UnboundedReceiver<Incoming>)> {
        let args = build_args(&config.launch);
        log::info!("starting {} {:?}", config.binary.display(), args);
        let mut command = Command::new(&config.binary);
        command
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = &config.cwd {
            command.current_dir(cwd);
        }
        if !config.env.is_empty() {
            command.env_clear();
            command.envs(config.env.iter().map(|(name, value)| (name, value)));
        }
        // The SDK stamps this so the CLI speaks the same protocol dialect.
        if !config
            .env
            .iter()
            .any(|(name, _)| name == "CLAUDE_CODE_ENTRYPOINT")
        {
            command.env("CLAUDE_CODE_ENTRYPOINT", "sdk-ts");
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("could not start {}", config.binary.display()))?;
        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take().context("no stderr")?;

        let (sender, receiver) = mpsc::unbounded();
        let inner = Arc::new(Inner {
            stdin: Mutex::new(Some(stdin)),
            child: Mutex::new(Some(child)),
            pending: Mutex::default(),
            permissions: Mutex::default(),
            session_id: Mutex::new(config.launch.session_id.clone()),
            session_info: Mutex::default(),
            system_init: Mutex::default(),
            model: Mutex::new(config.launch.model.clone()),
            stderr_tail: Mutex::default(),
            closed: AtomicBool::new(false),
        });

        let stderr_inner = Arc::clone(&inner);
        thread::Builder::new()
            .name("claude-stderr".into())
            .spawn(move || {
                for line in BufReader::new(stderr)
                    .lines()
                    .map_while(std::result::Result::ok)
                {
                    log::debug!("claude stderr: {line}");
                    if let Ok(mut tail) = stderr_inner.stderr_tail.lock() {
                        tail.push_str(&line);
                        tail.push('\n');
                        if tail.len() > 8 * 1024 {
                            let keep = tail.len() - 8 * 1024;
                            *tail = tail.split_off(keep);
                        }
                    }
                }
            })?;

        let reader_inner = Arc::clone(&inner);
        let reader_sender = sender.clone();
        thread::Builder::new()
            .name("claude-stdout".into())
            .spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    let line = match line {
                        Ok(line) => line,
                        Err(error) => {
                            let _ = reader_sender.unbounded_send(Incoming::Malformed {
                                line: String::new(),
                                error: error.to_string(),
                            });
                            break;
                        }
                    };
                    if line.trim().is_empty() {
                        continue;
                    }
                    let value: Value = match serde_json::from_str(&line) {
                        Ok(value) => value,
                        Err(error) => {
                            let _ = reader_sender.unbounded_send(Incoming::Malformed {
                                line,
                                error: error.to_string(),
                            });
                            continue;
                        }
                    };
                    dispatch(&reader_inner, &reader_sender, value);
                }
                reader_inner.closed.store(true, Ordering::SeqCst);
                let _ = reader_sender.unbounded_send(Incoming::Closed);
            })?;

        let client = Self {
            inner,
            next_id: Arc::new(AtomicU64::new(1)),
            control_timeout: config.control_timeout,
        };
        client
            .handshake(&config)
            .with_context(|| "claude initialize handshake failed")?;
        Ok((client, receiver))
    }

    fn handshake(&self, config: &ClaudeConfig) -> Result<()> {
        let request = initialize_request(
            config.system_prompt.as_deref(),
            config.append_system_prompt.as_deref(),
            config.agents.as_ref(),
            config.skills.as_deref(),
        );
        let response = self.request_control(request, config.handshake_timeout)?;
        let info = session_info_from_response(&response);
        *self.inner.session_info.lock().unwrap() = Some(info);
        Ok(())
    }

    /// The session id reported by the CLI, or the one forced at launch.
    pub fn session_id(&self) -> Option<String> {
        self.inner.session_id.lock().unwrap().clone()
    }

    /// What the `initialize` reply advertised.
    pub fn session_info(&self) -> Option<SessionInfo> {
        self.inner.session_info.lock().unwrap().clone()
    }

    /// The last `system/init` frame.
    pub fn system_init(&self) -> Option<SessionInit> {
        self.inner.system_init.lock().unwrap().clone()
    }

    /// Slash commands from the initialize reply, falling back to the
    /// `system/init` names when the CLI answered with no command list.
    pub fn command_names(&self) -> Vec<String> {
        if let Some(info) = self.session_info()
            && !info.commands.is_empty()
        {
            return info
                .commands
                .into_iter()
                .map(|command| command.name)
                .collect();
        }
        self.system_init()
            .map(|init| init.slash_commands)
            .unwrap_or_default()
    }

    /// Skills reported by `system/init`.
    pub fn skills(&self) -> Vec<String> {
        self.system_init()
            .map(|init| init.skills)
            .unwrap_or_default()
    }

    /// The CLI's recent stderr (bounded), for failure messages. It may hold
    /// private text; T3 keeps it out of turn failures for that reason.
    pub fn stderr_tail(&self) -> String {
        self.inner.stderr_tail.lock().unwrap().clone()
    }

    /// True once the process's stdout closed.
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    /// Sends one user turn with `content`.
    pub fn send_turn(&self, content: Vec<UserContentBlock>, options: &TurnOptions) -> Result<()> {
        if let Some(model) = &options.model {
            let current = self.inner.model.lock().unwrap().clone();
            if current.as_deref() != Some(model.as_str()) {
                self.set_model(Some(model))?;
                *self.inner.model.lock().unwrap() = Some(model.clone());
            }
        }
        let content = apply_effort(content, options.effort);
        let session_id = self.session_id().unwrap_or_default();
        let mut turn = UserTurnInput::new(session_id, content);
        turn.uuid = options.uuid.clone();
        if options.client_composed {
            turn.client_composed = Some(true);
        }
        self.write_value(&serde_json::to_value(turn)?)
    }

    /// Sends a text-only turn.
    pub fn send_text(&self, text: impl Into<String>) -> Result<()> {
        self.send_turn(vec![UserContentBlock::text(text)], &TurnOptions::default())
    }

    /// Answers a `can_use_tool` request.
    pub fn answer_permission(&self, request_id: &str, decision: &PermissionDecision) -> Result<()> {
        let tool_use_id = self
            .inner
            .permissions
            .lock()
            .unwrap()
            .remove(request_id)
            .and_then(|request| request.tool_use_id);
        let payload = decision.payload(tool_use_id.as_deref());
        let response = ControlResponseEnvelope::success(request_id, payload);
        self.write_value(&serde_json::to_value(response)?)
    }

    /// Answers any other control request with a success payload.
    pub fn answer_control(&self, request_id: &str, payload: Value) -> Result<()> {
        let response = ControlResponseEnvelope::success(request_id, payload);
        self.write_value(&serde_json::to_value(response)?)
    }

    /// Answers a control request with an error.
    pub fn reject_control(&self, request_id: &str, error: impl Into<String>) -> Result<()> {
        let response = ControlResponseEnvelope::error(request_id, error);
        self.write_value(&serde_json::to_value(response)?)
    }

    /// Interrupts the running turn and waits for the CLI's acknowledgement.
    pub fn interrupt(&self) -> Result<()> {
        self.request_control(json!({ "subtype": "interrupt" }), self.control_timeout)
            .map(|_| ())
    }

    /// Interrupts and also cancels queued commands (newer CLIs advertise
    /// `interrupt_cancel_queued_v1`).
    pub fn interrupt_cancelling_queued(&self) -> Result<()> {
        self.request_control(
            json!({ "subtype": "interrupt", "cancel_queued": true }),
            self.control_timeout,
        )
        .map(|_| ())
    }

    /// Switches the session model. `None` resets to the session default.
    pub fn set_model(&self, model: Option<&str>) -> Result<()> {
        let request = match model {
            Some(model) => json!({ "subtype": "set_model", "model": model }),
            None => json!({ "subtype": "set_model" }),
        };
        self.request_control(request, self.control_timeout)
            .map(|_| ())
    }

    /// Changes the permission mode mid-session.
    pub fn set_permission_mode(&self, mode: PermissionMode) -> Result<()> {
        self.request_control(
            json!({ "subtype": "set_permission_mode", "mode": mode.as_str() }),
            self.control_timeout,
        )
        .map(|_| ())
    }

    /// Sets the thinking-token budget. `None` resets to the session default.
    pub fn set_max_thinking_tokens(&self, tokens: Option<u64>) -> Result<()> {
        let request = match tokens {
            Some(tokens) => {
                json!({ "subtype": "set_max_thinking_tokens", "max_thinking_tokens": tokens })
            }
            None => json!({ "subtype": "set_max_thinking_tokens", "max_thinking_tokens": null }),
        };
        self.request_control(request, self.control_timeout)
            .map(|_| ())
    }

    /// Asks for the session's cost and plan rate limits (`get_usage`).
    pub fn request_usage(&self) -> Result<Value> {
        self.request_control(json!({ "subtype": "get_usage" }), self.control_timeout)
            .map(|response| response.response)
    }

    /// Asks for the context-window breakdown (`get_context_usage`).
    pub fn request_context_usage(&self, detail: Option<&str>) -> Result<Value> {
        let request = match detail {
            Some(detail) => json!({ "subtype": "get_context_usage", "detail": detail }),
            None => json!({ "subtype": "get_context_usage" }),
        };
        self.request_control(request, self.control_timeout)
            .map(|response| response.response)
    }

    /// Asks which models this account can use.
    pub fn list_models(&self) -> Result<Value> {
        self.request_control(json!({ "subtype": "list_models" }), self.control_timeout)
            .map(|response| response.response)
    }

    /// Stops the process and closes stdin.
    pub fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        if let Ok(mut stdin) = self.inner.stdin.lock()
            && let Some(mut stdin) = stdin.take()
        {
            let _ = stdin.flush();
        }
        if let Ok(mut child) = self.inner.child.lock()
            && let Some(mut child) = child.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn request_control(&self, request: Value, timeout: Duration) -> Result<ControlResponseBody> {
        let request_id = format!(
            "atrium-{}-{}",
            std::process::id(),
            self.next_id.fetch_add(1, Ordering::Relaxed)
        );
        let (sender, receiver) = std_mpsc::channel();
        self.inner
            .pending
            .lock()
            .unwrap()
            .insert(request_id.clone(), sender);
        let envelope = json!({
            "type": "control_request",
            "request_id": request_id,
            "request": request,
        });
        if let Err(error) = self.write_value(&envelope) {
            self.inner.pending.lock().unwrap().remove(&request_id);
            return Err(error);
        }
        let response = match receiver.recv_timeout(timeout) {
            Ok(response) => response,
            Err(_) => {
                self.inner.pending.lock().unwrap().remove(&request_id);
                bail!("claude did not answer control request {request_id} in time");
            }
        };
        if response.is_success() {
            Ok(response)
        } else {
            Err(anyhow!(
                "claude rejected control request: {}",
                response.error.unwrap_or_default()
            ))
        }
    }

    fn write_value(&self, value: &Value) -> Result<()> {
        let mut guard = self.inner.stdin.lock().unwrap();
        let stdin = guard.as_mut().context("claude stdin is closed")?;
        serde_json::to_writer(&mut *stdin, value)?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    }
}

fn apply_effort(
    mut content: Vec<UserContentBlock>,
    effort: Option<Effort>,
) -> Vec<UserContentBlock> {
    if effort != Some(Effort::UltraThink) {
        return content;
    }
    for block in &mut content {
        if let UserContentBlock::Text { text } = block {
            *text = apply_claude_prompt_effort_prefix(text, effort);
            break;
        }
    }
    content
}

fn dispatch(inner: &Arc<Inner>, sender: &mpsc::UnboundedSender<Incoming>, value: Value) {
    if let Some(session_id) = value.get("session_id").and_then(Value::as_str)
        && !session_id.is_empty()
    {
        *inner.session_id.lock().unwrap() = Some(session_id.to_owned());
    }
    match value.get("type").and_then(Value::as_str) {
        Some("control_response") => {
            let Ok(envelope) =
                serde_json::from_value::<crate::protocol::ControlResponseEnvelope>(value)
            else {
                return;
            };
            for pending in &envelope.response.pending_permission_requests {
                if let Some(request) = PermissionRequest::from_envelope(pending) {
                    inner
                        .permissions
                        .lock()
                        .unwrap()
                        .insert(request.request_id.clone(), request.clone());
                    let _ = sender.unbounded_send(Incoming::Permission(request));
                }
            }
            if let Some(pending) = inner
                .pending
                .lock()
                .unwrap()
                .remove(&envelope.response.request_id)
            {
                let _ = pending.send(envelope.response);
            }
        }
        Some("control_request") => {
            let Ok(envelope) = serde_json::from_value::<ControlRequestEnvelope>(value) else {
                return;
            };
            if let Some(request) = PermissionRequest::from_envelope(&envelope) {
                inner
                    .permissions
                    .lock()
                    .unwrap()
                    .insert(request.request_id.clone(), request.clone());
                let _ = sender.unbounded_send(Incoming::Permission(request));
            } else {
                let _ = sender.unbounded_send(Incoming::Control(envelope));
            }
        }
        _ => {
            if let Some(event) = crate::events::event(&value) {
                if let ClaudeEvent::SessionInit(init) = &event {
                    *inner.system_init.lock().unwrap() = Some(init.clone());
                }
                let _ = sender.unbounded_send(Incoming::Event(event));
            } else {
                let _ = sender.unbounded_send(Incoming::Frame(value));
            }
        }
    }
}
