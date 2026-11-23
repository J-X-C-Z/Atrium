use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
};

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
        Self::over(
            config.backend,
            BufReader::new(stdout),
            stdin,
            Some(Arc::new(ProcessGuard(Mutex::new(Some(child))))),
        )
        .await
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
            backend: backend.clone(),
            info: AgentInfo::default(),
            translator: Arc::new(Mutex::new(Translator::new(backend))),
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
            .await;
        let delivery = prompt_delivery(&result);
        let result = result.map_err(|e| self.explain(e).to_string());
        let usage = result.as_ref().ok().map(|r| r["usage"].clone());
        let result = result.map(|r| str_of(&r, "stopReason"));
        let events = self.translator.lock().expect("translator").finish_prompt(
            session_id,
            result,
            usage.as_ref(),
        );
        PromptOutcome { events, delivery }
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
                    backend: "test".into(),
                    info: AgentInfo::default(),
                    translator: Arc::new(Mutex::new(Translator::new("test"))),
                    permissions: Arc::default(),
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
