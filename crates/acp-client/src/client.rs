use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
};

/// The line `agy acp` writes when it wants a browser sign-in (T3's
/// `ANTIGRAVITY_AUTH_STDOUT_PREFIX`), and the marker its browser helper
/// writes on stderr (`ANTIGRAVITY_AUTH_BROWSER_MARKER`).
const AUTH_STDOUT_PREFIX: &str = "Open the following link to authenticate the ACP server: ";
const AUTH_BROWSER_MARKER: &str = "__T3_ANTIGRAVITY_AUTH_URL__";

use agent_rpc::{Connection, Incoming, RpcError, process};
use anyhow::{Context as _, Result, anyhow};
use atrium_core::{AgentEvent, ApprovalDecision, ConfigOption, McpServer, RequestId, ThreadMeta};
use base64::Engine as _;
use futures::channel::mpsc;
use serde_json::{Value, json};

use crate::translate::{self, MODE_OPTION, Translator};

/// A local marker queued after a session's replayed history.
const REPLAY_DONE: &str = "_atrium/replay_done";

/// How to start an ACP agent.
#[derive(Clone, Debug)]
pub struct AcpConfig {
    /// Atrium's id for the agent, such as `claude`; tags its threads.
    pub backend: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// What the agent said about itself in the handshake.
#[derive(Clone, Debug, Default)]
pub struct AgentInfo {
    pub name: String,
    pub title: String,
    pub version: String,
    /// `session/load`: reopening a session replays its history.
    pub load_session: bool,
    pub list_sessions: bool,
    /// `session/resume`: reopening continues without replaying history.
    pub resume_session: bool,
    pub images: bool,
    /// The agent embeds text files as `resource` blocks.
    pub embedded_context: bool,
    /// The agent takes audio attachments as `audio` blocks.
    pub audio: bool,
    /// `logout`: the agent can clear its own stored credentials.
    pub logout: bool,
    /// Prompts may be sent while one runs (Claude Code queues or steers them).
    pub steering: bool,
    /// `(id, name, description)` of each way to sign in.
    pub auth_methods: Vec<(String, String, Option<String>)>,
}

/// A session the agent just made.
#[derive(Clone, Debug)]
pub struct SessionStart {
    pub session_id: String,
    pub options: Vec<ConfigOption>,
}

/// One host file to send with a prompt. The adapter resolves a contract
/// attachment id to this path; the client decides the ACP block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptAttachment {
    pub path: PathBuf,
    pub name: String,
    pub mime_type: String,
    /// The contract said `image`; the agent may still refuse non-image MIME.
    pub image: bool,
}

/// Reliable delivery evidence from a completed prompt RPC. Errors cannot prove
/// that the agent did not already act, even if a standard refusal code arrives.
#[derive(Debug, Clone, PartialEq)]
pub enum PromptDelivery {
    Accepted { stop_reason: String },
    Uncertain { error: RpcError },
}

/// The normal closing events plus delivery evidence, without changing the wire.
pub struct PromptOutcome {
    pub events: Vec<AgentEvent>,
    pub delivery: PromptDelivery,
    /// The prompt response's `usage` object, when the agent reported one. The
    /// events already carry the folded transcript view; an adapter that keeps
    /// per-turn usage (T3's `TurnTokenUsage`) reads it here.
    pub usage: Option<Value>,
}

/// A prompt's wire outcome, before the translator closes the turn.
pub struct PromptParts {
    pub result: Result<String, String>,
    pub usage: Option<Value>,
    pub delivery: PromptDelivery,
    pub limits: Option<atrium_core::RateLimits>,
}

fn prompt_delivery(result: &std::result::Result<Value, RpcError>) -> PromptDelivery {
    match result {
        Ok(value) => match value["stopReason"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
        {
            Some(reason) => PromptDelivery::Accepted {
                stop_reason: reason.to_owned(),
            },
            None => PromptDelivery::Uncertain {
                error: RpcError::Decode("prompt reply lacks a nonempty stopReason".into()),
            },
        },
        Err(error) => PromptDelivery::Uncertain {
            error: error.clone(),
        },
    }
}

/// A permission option: `(option id, kind)`, kind such as `allow_once`.
type PermissionOption = (String, String);

/// A request id as the map keys it: a JSON string by its text, anything else
/// by its JSON text, so `"100"` and `100` name the same pending request.
fn permission_key(id: &RequestId) -> String {
    match id {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// A sign-in URL on an agent's stderr: either the plain agy line or the
/// browser helper's JSON-string marker.
fn auth_url_in_line(line: &str) -> Option<String> {
    let line = line.trim_end();
    if let Some(rest) = line.strip_prefix(AUTH_STDOUT_PREFIX) {
        return Some(rest.trim().to_string()).filter(|url| !url.is_empty());
    }
    if let Some(rest) = line.strip_prefix(AUTH_BROWSER_MARKER) {
        return serde_json::from_str::<String>(rest.trim())
            .ok()
            .filter(|url| !url.is_empty());
    }
    None
}

/// What a pending `session/request_permission` carried, kept until the
/// approval UI answers it. The raw params let a flavor recognize native
/// questions (which share the method) without re-parsing the notification.
#[derive(Clone, Default)]
struct PendingPermission {
    /// The id exactly as the agent sent it. A host that carries the id as
    /// text (the engine's runtime request id) still answers the wire id.
    id: Value,
    options: Vec<PermissionOption>,
    params: Value,
}

struct ProcessGuard(Mutex<Option<Child>>);

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.lock().ok().and_then(|mut c| c.take()) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A running ACP agent. Cheap to clone.
#[derive(Clone)]
pub struct AcpClient {
    connection: Connection,
    /// Keeps the agent alive while a clone remains: the child process, or
    /// a process on another device.
    _process: Option<Arc<dyn std::any::Any + Send + Sync>>,
    /// The local child, when this client spawned one, so a host can stop it
    /// instead of waiting for the last clone to drop.
    process: Option<Arc<ProcessGuard>>,
    pub backend: String,
    pub info: AgentInfo,
    translator: Arc<Mutex<Translator>>,
    /// The options and raw params each pending permission request offered, by
    /// request id.
    permissions: Arc<Mutex<HashMap<String, PendingPermission>>>,
    /// Sign-in URLs the agent announced on stderr, oldest first. The native
    /// process performs the exchange; Atrium only forwards the link.
    auth_urls: Arc<Mutex<Vec<String>>>,
}

impl AcpClient {
    /// Spawns the agent and runs the `initialize` handshake.
    pub async fn start(config: AcpConfig) -> Result<(Self, mpsc::UnboundedReceiver<Incoming>)> {
        log::info!("starting {} {:?}", config.program.display(), config.args);
        let mut child = Command::new(&config.program)
            .args(&config.args)
            .env("PATH", process::effective_path())
            .envs(config.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("could not start {}", config.program.display()))?;
        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take().context("no stderr")?;
        let backend = config.backend.clone();
        let auth_urls = Arc::new(Mutex::new(Vec::new()));
        let stderr_urls = auth_urls.clone();
        thread::Builder::new()
            .name(format!("{backend}-stderr"))
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    log::debug!("{backend} stderr: {line}");
                    if let Some(url) = auth_url_in_line(&line)
                        && let Ok(mut urls) = stderr_urls.lock()
                    {
                        urls.push(url);
                    }
                }
            })?;
        let guard = Arc::new(ProcessGuard(Mutex::new(Some(child))));
        let (mut client, incoming) = Self::over(
            config.backend,
            BufReader::new(stdout),
            stdin,
            Some(guard.clone() as Arc<dyn std::any::Any + Send + Sync>),
        )
        .await?;
        client.process = Some(guard);
        client.auth_urls = auth_urls;
        Ok((client, incoming))
    }

    /// Stops a locally spawned agent. A process reached over other pipes is
    /// owned elsewhere and is left alone.
    pub fn stop(&self) {
        if let Some(guard) = &self.process {
            let child = guard.0.lock().ok().and_then(|mut child| child.take());
            if let Some(mut child) = child {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    /// An agent reached through other pipes, such as a process an
    /// `atrium-node` runs on another device; `keep` lives as long as the client.
    pub async fn over(
        backend: String,
        reader: impl std::io::BufRead + Send + 'static,
        writer: impl std::io::Write + Send + 'static,
        keep: Option<Arc<dyn std::any::Any + Send + Sync>>,
    ) -> Result<(Self, mpsc::UnboundedReceiver<Incoming>)> {
        let peer: &'static str = Box::leak(backend.clone().into_boxed_str());
        let (connection, events) = Connection::jsonrpc2(reader, writer, peer);
        let mut client = Self {
            connection,
            _process: keep,
            process: None,
            backend: backend.clone(),
            info: AgentInfo::default(),
            translator: Arc::new(Mutex::new(Translator::new(backend))),
            permissions: Arc::default(),
            auth_urls: Arc::default(),
        };
        client.initialize().await?;
        Ok((client, events))
    }

    async fn initialize(&mut self) -> Result<()> {
        let result = self
            .connection
            .request(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    // The agent reads and writes files and runs commands itself.
                    "clientCapabilities": {
                        "fs": { "readTextFile": false, "writeTextFile": false },
                        "terminal": false,
                    },
                    "clientInfo": { "name": "atrium", "title": "Atrium", "version": env!("CARGO_PKG_VERSION") },
                }),
            )
            .await
            .context("the agent did not accept the ACP handshake")?;
        let caps = &result["agentCapabilities"];
        let sessions = &caps["sessionCapabilities"];
        self.info = AgentInfo {
            name: str_of(&result["agentInfo"], "name"),
            title: str_of(&result["agentInfo"], "title"),
            version: str_of(&result["agentInfo"], "version"),
            load_session: caps["loadSession"].as_bool().unwrap_or(false),
            list_sessions: !sessions["list"].is_null(),
            resume_session: !sessions["resume"].is_null(),
            images: caps["promptCapabilities"]["image"]
                .as_bool()
                .unwrap_or(false),
            embedded_context: caps["promptCapabilities"]["embeddedContext"]
                .as_bool()
                .unwrap_or(false),
            audio: caps["promptCapabilities"]["audio"]
                .as_bool()
                .unwrap_or(false),
            logout: !caps["auth"]["logout"].is_null(),
            steering: result["_meta"]["steering"]["supported"]
                .as_bool()
                .unwrap_or(false)
                || caps["_meta"]["claudeCode"]["promptQueueing"]
                    .as_bool()
                    .unwrap_or(false),
            auth_methods: result["authMethods"]
                .as_array()
                .map(|methods| {
                    methods
                        .iter()
                        .map(|m| {
                            (
                                str_of(m, "id"),
                                str_of(m, "name"),
                                m["description"].as_str().map(str::to_owned),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        Ok(())
    }

    /// Translates what the agent sent. Permission requests become approvals
    /// (answered with [`Self::answer_permission`]); other requests are declined.
    pub fn translate(&self, incoming: Incoming) -> Vec<AgentEvent> {
        match incoming {
            Incoming::Notification { method, params } if method == "session/update" => {
                self.translator.lock().expect("translator").update(&params)
            }
            Incoming::Notification { method, params } if method == REPLAY_DONE => self
                .translator
                .lock()
                .expect("translator")
                .end_replay(params["sessionId"].as_str().unwrap_or_default()),
            Incoming::Notification { .. } => Vec::new(),
            Incoming::Request { id, method, params } if method == "session/request_permission" => {
                let options: Vec<(String, String)> = params["options"]
                    .as_array()
                    .map(|options| {
                        options
                            .iter()
                            .map(|o| (str_of(o, "optionId"), str_of(o, "kind")))
                            .collect()
                    })
                    .unwrap_or_default();
                self.permissions.lock().expect("permissions").insert(
                    permission_key(&id),
                    PendingPermission {
                        id: id.clone(),
                        options,
                        params: params.clone(),
                    },
                );
                vec![
                    self.translator
                        .lock()
                        .expect("translator")
                        .permission(&id, &params),
                ]
            }
            Incoming::Request { id, method, .. } => {
                log::warn!("{}: declining {method}", self.backend);
                let _ = self.connection.respond_error(
                    id,
                    -32601,
                    &format!("Atrium does not offer {method}"),
                );
                Vec::new()
            }
            Incoming::Closed => vec![AgentEvent::Disconnected {
                reason: format!("{} exited", self.info.title_or(&self.backend)),
            }],
        }
    }

    /// Answers a permission request with the option matching `decision`.
    pub fn answer_permission(
        &self,
        request_id: RequestId,
        decision: ApprovalDecision,
    ) -> Result<()> {
        let pending = self
            .permissions
            .lock()
            .expect("permissions")
            .remove(&permission_key(&request_id))
            .unwrap_or_default();
        let wanted: &[&str] = match decision {
            ApprovalDecision::Accept => &["allow_once", "allow_always"],
            ApprovalDecision::AcceptForSession => &["allow_always", "allow_once"],
            ApprovalDecision::Decline => &["reject_once", "reject_always"],
            ApprovalDecision::Cancel => &[],
        };
        let picked = wanted
            .iter()
            .find_map(|kind| pending.options.iter().find(|(_, k)| k == kind))
            .map(|(id, _)| id.clone());
        let outcome = match picked {
            Some(option) => json!({ "outcome": "selected", "optionId": option }),
            None => json!({ "outcome": "cancelled" }),
        };
        let id = if pending.id.is_null() {
            request_id
        } else {
            pending.id
        };
        self.connection.respond(id, json!({ "outcome": outcome }))?;
        Ok(())
    }

    /// Answers a permission request with an exact native option id. Native
    /// questions (Antigravity's `interaction_` prompt) are not approvals, so
    /// their selected option is the answer; an unknown id cancels.
    pub fn answer_permission_option(&self, request_id: RequestId, option_id: &str) -> Result<()> {
        let pending = self
            .permissions
            .lock()
            .expect("permissions")
            .remove(&permission_key(&request_id))
            .unwrap_or_default();
        let known = pending
            .options
            .iter()
            .any(|(id, _)| id == option_id)
            .then(|| option_id.to_string());
        let outcome = match known {
            Some(option) => json!({ "outcome": "selected", "optionId": option }),
            None => json!({ "outcome": "cancelled" }),
        };
        let id = if pending.id.is_null() {
            request_id
        } else {
            pending.id
        };
        self.connection.respond(id, json!({ "outcome": outcome }))?;
        Ok(())
    }

    /// The raw params of a pending permission request, for a flavor that
    /// recognizes native questions sharing the method. `None` once answered.
    pub fn permission_params(&self, request_id: &RequestId) -> Option<Value> {
        self.permissions
            .lock()
            .expect("permissions")
            .get(&permission_key(request_id))
            .map(|pending| pending.params.clone())
    }

    /// The option ids and kinds a pending permission offered.
    pub fn permission_options(&self, request_id: &RequestId) -> Vec<(String, String)> {
        self.permissions
            .lock()
            .expect("permissions")
            .get(&permission_key(request_id))
            .map(|pending| pending.options.clone())
            .unwrap_or_default()
    }

    /// How many permission requests are awaiting an answer. A prompt is not
    /// idle while one is open, so the inactivity watchdog must not fire.
    pub fn pending_permission_count(&self) -> usize {
        self.permissions.lock().expect("permissions").len()
    }

    /// Sign-in URLs the agent announced, draining them. The native process
    /// owns the token exchange and storage; nothing here reads a credential.
    pub fn take_auth_urls(&self) -> Vec<String> {
        std::mem::take(&mut *self.auth_urls.lock().expect("auth urls"))
    }

    pub async fn new_session(&self, cwd: &Path) -> Result<SessionStart> {
        self.new_session_with(cwd, &[]).await
    }

    /// [`Self::new_session`] with `mcp_servers` for the agent to use.
    pub async fn new_session_with(
        &self,
        cwd: &Path,
        mcp_servers: &[McpServer],
    ) -> Result<SessionStart> {
        let result = self
            .connection
            .request(
                "session/new",
                json!({ "cwd": cwd.display().to_string(), "mcpServers": mcp_json(mcp_servers) }),
            )
            .await
            .map_err(|e| self.explain(e))?;
        let session_id = str_of(&result, "sessionId");
        let options = translate::config_options(&result);
        self.translator
            .lock()
            .expect("translator")
            .set_options(&session_id, options.clone());
        Ok(SessionStart {
            session_id,
            options,
        })
    }

    /// Reopens a stored session. With `session/load` its history is replayed
    /// as updates (translate them as they come); with `session/resume` it
    /// continues without history. Returns the session's settings.
    pub async fn open_session(&self, session_id: &str, cwd: &str) -> Result<Vec<ConfigOption>> {
        self.open_session_with(session_id, cwd, &[]).await
    }

    /// [`Self::open_session`] with `mcp_servers` for the agent to use.
    pub async fn open_session_with(
        &self,
        session_id: &str,
        cwd: &str,
        mcp_servers: &[McpServer],
    ) -> Result<Vec<ConfigOption>> {
        let params =
            json!({ "sessionId": session_id, "cwd": cwd, "mcpServers": mcp_json(mcp_servers) });
        let result = if self.info.load_session {
            self.translator
                .lock()
                .expect("translator")
                .begin_replay(session_id);
            let loaded = self.connection.request("session/load", params).await;
            // The replayed updates are queued ahead of this marker, which ends the replay.
            self.connection.inject(Incoming::Notification {
                method: REPLAY_DONE.into(),
                params: json!({ "sessionId": session_id }),
            });
            loaded.map_err(|e| self.explain(e))?
        } else if self.info.resume_session {
            self.connection
                .request("session/resume", params)
                .await
                .map_err(|e| self.explain(e))?
        } else {
            return Err(anyhow!(
                "{} cannot reopen past sessions",
                self.info.title_or(&self.backend)
            ));
        };
        let options = translate::config_options(&result);
        self.translator
            .lock()
            .expect("translator")
            .set_options(session_id, options.clone());
        Ok(options)
    }

    /// Stored sessions, newest first, when the agent can list them.
    pub async fn list_sessions(&self) -> Result<Vec<ThreadMeta>> {
        if !self.info.list_sessions {
            return Ok(Vec::new());
        }
        let result = self.connection.request("session/list", json!({})).await?;
        let mut sessions: Vec<ThreadMeta> = result["sessions"]
            .as_array()
            .map(|sessions| {
                sessions
                    .iter()
                    .filter_map(|info| translate::session_meta(&self.backend, info))
                    .collect()
            })
            .unwrap_or_default();
        sessions.sort_by_key(|t| std::cmp::Reverse(t.updated_at));
        Ok(sessions)
    }

    /// Starts a turn: the events to show now, and the turn's id.
    pub fn begin_turn(&self, session_id: &str) -> (String, Vec<AgentEvent>) {
        self.translator
            .lock()
            .expect("translator")
            .begin_turn(session_id)
    }

    pub fn active_turn(&self, session_id: &str) -> Option<String> {
        self.translator
            .lock()
            .expect("translator")
            .active_turn(session_id)
            .map(str::to_owned)
    }

    /// The session's latest settings (as last set or patched).
    pub fn session_options(&self, session_id: &str) -> Vec<ConfigOption> {
        self.translator
            .lock()
            .expect("translator")
            .options(session_id)
    }

    /// Sends a prompt and waits for the turn to end; updates stream meanwhile.
    /// Returns the events that close the turn.
    pub async fn prompt(
        &self,
        session_id: &str,
        text: &str,
        images: &[PathBuf],
        mentions: &[PathBuf],
    ) -> Vec<AgentEvent> {
        self.prompt_with_outcome(session_id, text, images, mentions)
            .await
            .events
    }

    /// Like `prompt`, but preserves reliable acceptance versus uncertainty.
    /// All errors are uncertain: streaming updates can precede an RPC error.
    pub async fn prompt_with_outcome(
        &self,
        session_id: &str,
        text: &str,
        images: &[PathBuf],
        mentions: &[PathBuf],
    ) -> PromptOutcome {
        let parts = self.prompt_parts(session_id, text, images, mentions).await;
        let mut events = self.finish_prompt(session_id, parts.result, parts.usage.as_ref());
        events.extend(parts.limits.map(AgentEvent::RateLimitsUpdated));
        PromptOutcome {
            events,
            delivery: parts.delivery,
            usage: parts.usage,
        }
    }

    /// Sends a prompt and returns its outcome without closing the turn.
    ///
    /// A host that owns the `session/update` stream closes the turn itself
    /// with [`Self::finish_prompt`] after the stream is quiet, so trailing
    /// updates cannot arrive after `TurnCompleted`.
    pub async fn prompt_parts(
        &self,
        session_id: &str,
        text: &str,
        images: &[PathBuf],
        mentions: &[PathBuf],
    ) -> PromptParts {
        let attachments: Vec<PromptAttachment> = images
            .iter()
            .map(|path| prompt_attachment(path, true))
            .chain(mentions.iter().map(|path| prompt_attachment(path, false)))
            .collect();
        let blocks = match self.prompt_blocks_for(text, &attachments) {
            Ok(blocks) => blocks,
            Err(error) => {
                return PromptParts {
                    result: Err(error.to_string()),
                    usage: None,
                    delivery: PromptDelivery::Uncertain {
                        error: RpcError::Decode(error.to_string()),
                    },
                    limits: None,
                };
            }
        };
        self.prompt_with_blocks(session_id, blocks).await
    }

    /// The blocks T3's ACP adapter sends: the text with a saved-path note for
    /// every attachment, then image blocks for supported images. Files ride
    /// on their path note, as T3's `providerMessageTextWithAttachmentPaths`
    /// does; a flavor that embeds them builds its own blocks.
    pub fn prompt_blocks_for(
        &self,
        text: &str,
        attachments: &[PromptAttachment],
    ) -> Result<Vec<Value>> {
        let mut text = text.to_string();
        for attachment in attachments {
            let kind = if attachment.image { "image" } else { "file" };
            let note = format!(
                "[Attached {kind} \"{}\" is saved at: {}]",
                attachment.name,
                attachment.path.display()
            );
            if text.is_empty() {
                text = note;
            } else {
                text.push_str("\n\n");
                text.push_str(&note);
            }
        }
        let mut blocks = Vec::new();
        if !text.is_empty() {
            blocks.push(json!({ "type": "text", "text": text }));
        }
        for attachment in attachments.iter().filter(|attachment| attachment.image) {
            let mime = attachment.mime_type.to_ascii_lowercase();
            // T3's `isProviderNativeImageAttachment`.
            if !matches!(
                mime.as_str(),
                "image/gif" | "image/jpeg" | "image/png" | "image/webp"
            ) {
                continue;
            }
            if !self.info.images {
                return Err(anyhow!("ACP driver did not negotiate image prompt support"));
            }
            let bytes = std::fs::read(&attachment.path)
                .with_context(|| format!("Failed to read attachment '{}'", attachment.name))?;
            blocks.push(json!({
                "type": "image",
                "mimeType": attachment.mime_type,
                "data": base64::engine::general_purpose::STANDARD.encode(bytes),
                "uri": format!("file://{}", attachment.path.display()),
            }));
        }
        if blocks.is_empty() {
            return Err(anyhow!("A turn requires text or supported attachments"));
        }
        Ok(blocks)
    }

    /// Sends already-built ACP prompt blocks and returns the wire outcome.
    pub async fn prompt_with_blocks(&self, session_id: &str, blocks: Vec<Value>) -> PromptParts {
        let result = self
            .connection
            .request(
                "session/prompt",
                json!({ "sessionId": session_id, "prompt": blocks }),
            )
            .await;
        let delivery = prompt_delivery(&result);
        let result = result.map_err(|e| self.explain(e).to_string());
        let usage = result
            .as_ref()
            .ok()
            .map(|r| r["usage"].clone())
            .filter(Value::is_object);
        let limits = result
            .as_ref()
            .ok()
            .and_then(atrium_core::parse_meta_limits);
        let result = result.map(|r| str_of(&r, "stopReason"));
        PromptParts {
            result,
            usage,
            delivery,
            limits,
        }
    }

    /// Closes the session's running turn: `result` is the prompt's
    /// `stopReason` or error.
    pub fn finish_prompt(
        &self,
        session_id: &str,
        result: Result<String, String>,
        usage: Option<&Value>,
    ) -> Vec<AgentEvent> {
        self.translator
            .lock()
            .expect("translator")
            .finish_prompt(session_id, result, usage)
    }

    /// Asks the running turn to stop; its prompt then ends as cancelled.
    pub fn cancel(&self, session_id: &str) -> Result<()> {
        self.connection
            .notify("session/cancel", Some(json!({ "sessionId": session_id })))?;
        Ok(())
    }

    /// Changes one of the session's settings. Returns the updated settings
    /// when the agent sends them back.
    pub async fn set_option(
        &self,
        session_id: &str,
        option: &str,
        value: &str,
    ) -> Result<Option<Vec<ConfigOption>>> {
        if option == MODE_OPTION {
            self.connection
                .request(
                    "session/set_mode",
                    json!({ "sessionId": session_id, "modeId": value }),
                )
                .await?;
            return Ok(None);
        }
        let result = self
            .connection
            .request(
                "session/set_config_option",
                json!({ "sessionId": session_id, "configId": option, "value": value }),
            )
            .await?;
        let options = translate::config_options(&result);
        if options.is_empty() {
            return Ok(None);
        }
        self.translator
            .lock()
            .expect("translator")
            .set_options(session_id, options.clone());
        Ok(Some(options))
    }

    /// Signs in with one of the agent's methods.
    pub async fn authenticate(&self, method_id: &str) -> Result<()> {
        self.connection
            .request("authenticate", json!({ "methodId": method_id }))
            .await?;
        Ok(())
    }

    /// Clears the agent's stored credentials through ACP `logout`. T3 only
    /// offers this when the handshake advertised `auth.logout`.
    pub async fn logout(&self) -> Result<()> {
        if !self.info.logout {
            return Err(anyhow!(
                "{} does not support sign-out",
                self.info.title_or(&self.backend)
            ));
        }
        self.connection.request("logout", json!({})).await?;
        Ok(())
    }

    pub async fn close_session(&self, session_id: &str) -> Result<()> {
        self.connection
            .request("session/close", json!({ "sessionId": session_id }))
            .await?;
        Ok(())
    }

    /// Turns "authentication required" into something a person can act on.
    fn explain(&self, error: RpcError) -> anyhow::Error {
        let name = self.info.title_or(&self.backend);
        match &error {
            RpcError::Server { code: -32000, .. } => {
                let how = self
                    .info
                    .auth_methods
                    .first()
                    .map(|(_, name, description)| match description {
                        Some(text) => format!("{name}: {text}"),
                        None => name.clone(),
                    })
                    .unwrap_or_else(|| "sign in with its command-line tool".into());
                anyhow!("{name} needs you to sign in ({how}).")
            }
            _ => anyhow!("{name}: {error}"),
        }
    }
}

impl AgentInfo {
    fn title_or(&self, fallback: &str) -> String {
        if !self.title.is_empty() {
            self.title.clone()
        } else if !self.name.is_empty() {
            self.name.clone()
        } else {
            fallback.to_string()
        }
    }
}

fn str_of(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
}

/// A path-only prompt attachment, with the MIME its extension implies.
fn prompt_attachment(path: &Path, image: bool) -> PromptAttachment {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    PromptAttachment {
        path: path.to_path_buf(),
        name,
        mime_type: mime_for_path(path).to_string(),
        image,
    }
}

fn mime_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("txt" | "md" | "log") => "text/plain",
        Some("json") => "application/json",
        Some("pdf") => "application/pdf",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("ogg") => "audio/ogg",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod prompt_outcome_tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::{Shutdown, TcpListener, TcpStream},
        time::Duration,
    };

    #[test]
    fn typed_prompt_preserves_events_and_never_retries_uncertain_delivery() {
        let (done, finished) = std::sync::mpsc::channel();
        thread::spawn(move || {
            for case in 0..5 {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                peer.set_write_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let server = thread::spawn(move || {
                    let mut reader = BufReader::new(peer.try_clone().unwrap());
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let request: Value = serde_json::from_str(&line).unwrap();
                    assert_eq!(request["method"], "session/prompt");
                    if case == 2 {
                        writeln!(peer, "{}", json!({"jsonrpc":"2.0", "method":"session/update", "params":{"sessionId":"session", "update":{"sessionUpdate":"agent_message_chunk", "content":{"type":"text", "text":"already acted"}}}})).unwrap();
                    }
                    if case != 3 {
                        let response = if case == 1 || case == 2 {
                            json!({"jsonrpc":"2.0", "id":request["id"], "error":{"code":-32602,"message":"refused"}})
                        } else {
                            json!({"jsonrpc":"2.0", "id":request["id"], "result":if case == 0 {json!({"stopReason":"end_turn"})} else {json!({})}})
                        };
                        writeln!(peer, "{response}").unwrap();
                    }
                    peer.shutdown(Shutdown::Write).unwrap();
                    let mut remainder = String::new();
                    match reader.read_to_string(&mut remainder) {
                        Ok(_) => {}
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        Err(e) => panic!("unexpected read: {e}"),
                    }
                    assert!(remainder.is_empty(), "prompt retried or cancelled on wire");
                });
                let (connection, mut incoming) = Connection::jsonrpc2(
                    BufReader::new(stream.try_clone().unwrap()),
                    stream,
                    "test",
                );
                let client = AcpClient {
                    connection,
                    _process: None,
                    process: None,
                    backend: "test".into(),
                    info: AgentInfo::default(),
                    translator: Arc::new(Mutex::new(Translator::new("test"))),
                    permissions: Arc::default(),
                    auth_urls: Arc::default(),
                };
                client.begin_turn("session");
                let outcome = futures::executor::block_on(client.prompt_with_outcome(
                    "session",
                    "hello",
                    &[],
                    &[],
                ));
                assert!(!outcome.events.is_empty());
                match (case, outcome.delivery) {
                    (0, PromptDelivery::Accepted { stop_reason }) => {
                        assert_eq!(stop_reason, "end_turn")
                    }
                    (
                        1 | 2,
                        PromptDelivery::Uncertain {
                            error: RpcError::Server { code: -32602, .. },
                        },
                    ) => {}
                    (
                        3,
                        PromptDelivery::Uncertain {
                            error: RpcError::Closed,
                        },
                    ) => {}
                    (
                        4,
                        PromptDelivery::Uncertain {
                            error: RpcError::Decode(_),
                        },
                    ) => {}
                    other => panic!("unexpected outcome: {other:?}"),
                }
                if case == 2 {
                    assert!(
                        matches!(incoming.try_recv(), Ok(Incoming::Notification { method, .. }) if method == "session/update")
                    );
                }
                drop(client);
                server.join().unwrap();
            }
            done.send(()).unwrap();
        });
        finished
            .recv_timeout(Duration::from_secs(10))
            .expect("prompt cases completed before deadline");
    }
}

/// `servers` as ACP `McpServer` entries: stdio, or `http` for a server with
/// a URL.
fn mcp_json(servers: &[McpServer]) -> Value {
    let pairs = |list: &[(String, String)]| -> Vec<Value> {
        list.iter()
            .map(|(k, v)| json!({ "name": k, "value": v }))
            .collect()
    };
    servers
        .iter()
        .map(|s| match &s.url {
            Some(url) => json!({
                "type": "http",
                "name": s.name,
                "url": url,
                "headers": pairs(&s.headers),
            }),
            None => json!({
                "name": s.name,
                "command": s.command,
                "args": s.args,
                "env": pairs(&s.env),
            }),
        })
        .collect()
}

#[cfg(test)]
mod mcp_json_tests {
    use super::*;

    #[test]
    fn http_servers_are_passed_as_http_entries() {
        let json = mcp_json(&[
            McpServer {
                name: "local".into(),
                command: "atrium".into(),
                ..Default::default()
            },
            McpServer::http(
                "atrium",
                "http://100.64.0.1:7381/v1/mcp",
                vec![("Authorization".into(), "Bearer t".into())],
            ),
        ]);
        assert_eq!(json[0]["command"], "atrium");
        assert!(json[0]["type"].is_null());
        assert_eq!(json[1]["type"], "http");
        assert_eq!(json[1]["url"], "http://100.64.0.1:7381/v1/mcp");
        assert_eq!(
            json[1]["headers"],
            json!([{ "name": "Authorization", "value": "Bearer t" }])
        );
    }
}
