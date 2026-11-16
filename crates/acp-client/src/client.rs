use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
};

use agent_rpc::{Connection, Incoming, RpcError, process};
use agentdesk_core::{AgentEvent, ApprovalDecision, ConfigOption, RequestId, ThreadMeta};
use anyhow::{Context as _, Result, anyhow};
use base64::Engine as _;
use futures::channel::mpsc;
use serde_json::{Value, json};

use crate::translate::{self, MODE_OPTION, Translator};

/// A local marker queued after a session's replayed history.
const REPLAY_DONE: &str = "_agentdesk/replay_done";

/// How to start an ACP agent.
#[derive(Clone, Debug)]
pub struct AcpConfig {
    /// AgentDesk's id for the agent, such as `claude`; tags its threads.
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

/// A permission option: `(option id, kind)`, kind such as `allow_once`.
type PermissionOption = (String, String);

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
    _process: Option<Arc<ProcessGuard>>,
    pub backend: String,
    pub info: AgentInfo,
    translator: Arc<Mutex<Translator>>,
    /// The options each pending permission request offered, by request id.
    permissions: Arc<Mutex<HashMap<String, Vec<PermissionOption>>>>,
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
        thread::Builder::new()
            .name(format!("{backend}-stderr"))
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    log::debug!("{backend} stderr: {line}");
                }
            })?;
        let peer: &'static str = Box::leak(config.backend.clone().into_boxed_str());
        let (connection, events) = Connection::jsonrpc2(BufReader::new(stdout), stdin, peer);
        let mut client = Self {
            connection,
            _process: Some(Arc::new(ProcessGuard(Mutex::new(Some(child))))),
            backend: config.backend.clone(),
            info: AgentInfo::default(),
            translator: Arc::new(Mutex::new(Translator::new(config.backend))),
            permissions: Arc::default(),
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
                    "clientInfo": { "name": "agentdesk", "title": "AgentDesk", "version": env!("CARGO_PKG_VERSION") },
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
                self.permissions
                    .lock()
                    .expect("permissions")
                    .insert(id.to_string(), options);
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
                    &format!("AgentDesk does not offer {method}"),
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
        let options = self
            .permissions
            .lock()
            .expect("permissions")
            .remove(&request_id.to_string())
            .unwrap_or_default();
        let wanted: &[&str] = match decision {
            ApprovalDecision::Accept => &["allow_once", "allow_always"],
            ApprovalDecision::AcceptForSession => &["allow_always", "allow_once"],
            ApprovalDecision::Decline => &["reject_once", "reject_always"],
            ApprovalDecision::Cancel => &[],
        };
        let picked = wanted
            .iter()
            .find_map(|kind| options.iter().find(|(_, k)| k == kind))
            .map(|(id, _)| id.clone());
        let outcome = match picked {
            Some(option) => json!({ "outcome": "selected", "optionId": option }),
            None => json!({ "outcome": "cancelled" }),
        };
        self.connection
            .respond(request_id, json!({ "outcome": outcome }))?;
        Ok(())
    }

    pub async fn new_session(&self, cwd: &Path) -> Result<SessionStart> {
        let result = self
            .connection
            .request(
                "session/new",
                json!({ "cwd": cwd.display().to_string(), "mcpServers": [] }),
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
        let params = json!({ "sessionId": session_id, "cwd": cwd, "mcpServers": [] });
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

    /// Sends a prompt and waits for the turn to end; updates stream meanwhile.
    /// Returns the events that close the turn.
    pub async fn prompt(
        &self,
        session_id: &str,
        text: &str,
        images: &[PathBuf],
        mentions: &[PathBuf],
    ) -> Vec<AgentEvent> {
        let mut blocks = vec![json!({ "type": "text", "text": text })];
        for path in images {
            match image_block(path) {
                Ok(block) if self.info.images => blocks.push(block),
                Ok(_) => blocks.push(link_block(path)),
                Err(error) => log::warn!("could not attach {}: {error:#}", path.display()),
            }
        }
        for path in mentions {
            blocks.push(link_block(path));
        }
        let result = self
            .connection
            .request(
                "session/prompt",
                json!({ "sessionId": session_id, "prompt": blocks }),
            )
            .await
            .map(|r| str_of(&r, "stopReason"))
            .map_err(|e| self.explain(e).to_string());
        self.translator
            .lock()
            .expect("translator")
            .finish_prompt(session_id, result)
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

fn link_block(path: &Path) -> Value {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    json!({ "type": "resource_link", "name": name, "uri": format!("file://{}", path.display()) })
}

fn image_block(path: &Path) -> Result<Value> {
    let bytes = std::fs::read(path)?;
    let mime = match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "image/png",
    };
    Ok(json!({
        "type": "image",
        "mimeType": mime,
        "data": base64::engine::general_purpose::STANDARD.encode(bytes),
        "uri": format!("file://{}", path.display()),
    }))
}
