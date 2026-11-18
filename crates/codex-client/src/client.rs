use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
};

use anyhow::{Context as _, Result, anyhow};
use atrium_core::{
    Account, ApprovalDecision, Goal, GoalStatus, Item, RateLimits, RequestId, ThreadMeta,
    TurnStatus,
};
use futures::channel::mpsc;
use serde_json::{Value, json};

use crate::{
    process,
    rpc::{Connection, Incoming, RpcError},
    translate,
};

/// How to start Codex.
#[derive(Clone, Debug, Default)]
pub struct CodexConfig {
    /// The `codex` binary; found on `PATH` when `None`.
    pub binary: Option<PathBuf>,
    /// Extra arguments after `app-server`, such as `-c key=value` overrides.
    pub extra_args: Vec<String>,
    /// Extra environment for the server.
    pub env: Vec<(String, String)>,
}

/// Options for a new thread. `None` leaves the choice to Codex's config.
#[derive(Clone, Debug, Default)]
pub struct StartThread {
    pub cwd: Option<String>,
    pub model: Option<String>,
    /// `untrusted`, `on-request` or `never`.
    pub approval_policy: Option<String>,
    /// `read-only`, `workspace-write` or `danger-full-access`.
    pub sandbox: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub is_default: bool,
    /// Reasoning efforts the model accepts, such as `low`, `medium`, `high`.
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
    /// Faster service tiers the plan offers, as `(id, name, description)`,
    /// such as `("priority", "Fast", "2x speed, increased usage")`.
    pub service_tiers: Vec<(String, String, String)>,
}

/// How much Codex may do without asking, as Codex's own UI offers it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccessMode {
    /// Reads files; asks before any write or command outside the sandbox.
    ReadOnly,
    /// Writes in the workspace and runs commands there; asks for anything beyond.
    #[default]
    Auto,
    /// No sandbox and no questions.
    FullAccess,
}

impl AccessMode {
    pub const ALL: [AccessMode; 3] = [
        AccessMode::ReadOnly,
        AccessMode::Auto,
        AccessMode::FullAccess,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AccessMode::ReadOnly => "Read only",
            AccessMode::Auto => "Auto",
            AccessMode::FullAccess => "Full access",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            AccessMode::ReadOnly => "Reads files. Asks before editing or running commands.",
            AccessMode::Auto => "Edits and runs commands in the workspace. Asks for more.",
            AccessMode::FullAccess => "Edits anything and runs anything without asking.",
        }
    }

    pub fn approval_policy(self) -> &'static str {
        match self {
            AccessMode::ReadOnly | AccessMode::Auto => "on-request",
            AccessMode::FullAccess => "never",
        }
    }

    pub fn sandbox_mode(self) -> &'static str {
        match self {
            AccessMode::ReadOnly => "read-only",
            AccessMode::Auto => "workspace-write",
            AccessMode::FullAccess => "danger-full-access",
        }
    }

    /// The per-turn sandbox policy object `turn/start` takes.
    pub fn sandbox_policy(self) -> Value {
        match self {
            AccessMode::ReadOnly => json!({ "type": "readOnly", "networkAccess": false }),
            AccessMode::Auto => json!({
                "type": "workspaceWrite",
                "writableRoots": [],
                "networkAccess": false,
                "excludeTmpdirEnvVar": false,
                "excludeSlashTmp": false,
            }),
            AccessMode::FullAccess => json!({ "type": "dangerFullAccess" }),
        }
    }
}

/// One message from the user, with what it carries and how to run it.
#[derive(Clone, Debug, Default)]
pub struct TurnRequest {
    pub text: String,
    /// Local image files to show the model.
    pub images: Vec<PathBuf>,
    /// Files the message mentions with `@`.
    pub mentions: Vec<PathBuf>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub mode: Option<AccessMode>,
    /// A service tier id, such as `priority` for Fast.
    pub service_tier: Option<String>,
    /// Plan before acting: Codex proposes a plan and waits. Needs `model`.
    pub plan_mode: bool,
}

/// How to answer a question the agent asked: `(question id, answers)`.
pub type Answers = Vec<(String, Vec<String>)>;

/// What `review/start` looks at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewTarget {
    UncommittedChanges,
    BaseBranch(String),
    Custom(String),
}

/// What a prediction is asked to say, when Codex has no built-in predictor.
pub const PREDICTION_PROMPT: &str = "Predict the next message I am most likely to send in this conversation. \
Reply with only that message, written as I would write it, in my language, at most 20 words. \
Do not run tools. If there is no good guess, reply with nothing.";

/// Where a requested prediction will come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prediction {
    /// Codex sends `thread/prediction/updated` for the thread.
    Notified,
    /// The answer of this ephemeral fork's turn is the prediction.
    Fork(String),
}

/// Kills the server when the last client handle goes.
struct ProcessGuard(Mutex<Option<Child>>);

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.lock().ok().and_then(|mut c| c.take()) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A running `codex app-server`. Cheap to clone.
#[derive(Clone)]
pub struct CodexClient {
    connection: Connection,
    _process: Option<Arc<dyn std::any::Any + Send + Sync>>,
    pub user_agent: String,
    pub codex_home: String,
    pub binary: PathBuf,
}

impl CodexClient {
    /// Spawns the server and runs the `initialize` handshake.
    /// Events (notifications and server requests) arrive on the returned channel.
    pub async fn start(config: CodexConfig) -> Result<(Self, mpsc::UnboundedReceiver<Incoming>)> {
        let binary = process::find_codex(config.binary.as_deref()).map_err(|e| anyhow!(e))?;
        log::info!("starting {} app-server", binary.display());
        let mut child = Command::new(&binary)
            .arg("app-server")
            .args(&config.extra_args)
            .env("PATH", process::effective_path())
            .envs(config.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("could not start {}", binary.display()))?;
        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take().context("no stderr")?;
        thread::Builder::new()
            .name("codex-stderr".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    log::debug!("codex stderr: {}", strip_ansi(&line));
                }
            })?;
        let (connection, events) = Connection::new(BufReader::new(stdout), stdin);
        let guard = Arc::new(ProcessGuard(Mutex::new(Some(child))));
        let mut client = Self {
            connection,
            _process: Some(guard),
            user_agent: String::new(),
            codex_home: String::new(),
            binary,
        };
        client.initialize().await?;
        Ok((client, events))
    }

    /// A server reached through other pipes, such as `codex app-server` run
    /// by an `atrium-node` on another device; `keep` lives as long as the client.
    pub async fn remote(
        reader: impl std::io::BufRead + Send + 'static,
        writer: impl std::io::Write + Send + 'static,
        keep: Arc<dyn std::any::Any + Send + Sync>,
    ) -> Result<(Self, mpsc::UnboundedReceiver<Incoming>)> {
        let (connection, events) = Connection::new(reader, writer);
        let mut client = Self {
            connection,
            _process: Some(keep),
            user_agent: String::new(),
            codex_home: String::new(),
            binary: PathBuf::new(),
        };
        client.initialize().await?;
        Ok((client, events))
    }

    /// A client over an existing connection, such as an in-process test server.
    pub fn over(connection: Connection) -> Self {
        Self {
            connection,
            _process: None,
            user_agent: String::new(),
            codex_home: String::new(),
            binary: PathBuf::new(),
        }
    }

    pub async fn initialize(&mut self) -> Result<()> {
        let result = self
            .connection
            .request(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "atrium",
                        "title": "Atrium",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "capabilities": {
                        // Prediction, goals, memory, plan mode and steering
                        // live in the experimental API.
                        "experimentalApi": true,
                        "requestAttestation": false,
                    },
                }),
            )
            .await
            .context("Codex did not accept the handshake")?;
        self.user_agent = str_of(&result, "userAgent");
        self.codex_home = str_of(&result, "codexHome");
        self.connection.notify("initialized", None)?;
        Ok(())
    }

    pub async fn thread_start(&self, options: StartThread) -> Result<ThreadMeta> {
        let mut params = serde_json::Map::new();
        put(&mut params, "cwd", options.cwd);
        put(&mut params, "model", options.model);
        put(&mut params, "approvalPolicy", options.approval_policy);
        put(&mut params, "sandbox", options.sandbox);
        let result = self
            .connection
            .request("thread/start", Value::Object(params))
            .await?;
        translate::thread_meta(&result["thread"])
            .ok_or_else(|| anyhow!("thread/start returned no thread"))
    }

    /// Loads a stored thread so it can continue, with its history.
    pub async fn thread_resume(
        &self,
        thread_id: &str,
    ) -> Result<(ThreadMeta, Vec<(String, TurnStatus, Vec<Item>)>)> {
        let result = self
            .connection
            .request("thread/resume", json!({ "threadId": thread_id }))
            .await?;
        let thread = &result["thread"];
        let meta = translate::thread_meta(thread)
            .ok_or_else(|| anyhow!("thread/resume returned no thread"))?;
        let turns = thread["turns"]
            .as_array()
            .map(|turns| turns.iter().filter_map(translate::turn).collect())
            .unwrap_or_default();
        Ok((meta, turns))
    }

    /// Stored threads, newest first.
    pub async fn thread_list(&self, limit: u32) -> Result<Vec<ThreadMeta>> {
        let result = self
            .connection
            .request(
                "thread/list",
                json!({ "limit": limit, "sortKey": "updated_at", "sortDirection": "desc" }),
            )
            .await;
        // Older servers do not know every sort option; fall back to defaults.
        let result = match result {
            Err(RpcError::Server { .. }) => {
                self.connection
                    .request("thread/list", json!({ "limit": limit }))
                    .await?
            }
            other => other?,
        };
        Ok(result["data"]
            .as_array()
            .map(|threads| threads.iter().filter_map(translate::thread_meta).collect())
            .unwrap_or_default())
    }

    pub async fn thread_archive(&self, thread_id: &str) -> Result<()> {
        self.connection
            .request("thread/archive", json!({ "threadId": thread_id }))
            .await?;
        Ok(())
    }

    fn input(request: &TurnRequest) -> Vec<Value> {
        let mut input = vec![json!({ "type": "text", "text": request.text, "text_elements": [] })];
        for image in &request.images {
            input.push(json!({ "type": "localImage", "path": image.display().to_string() }));
        }
        for path in &request.mentions {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());
            input.push(
                json!({ "type": "mention", "name": name, "path": path.display().to_string() }),
            );
        }
        input
    }

    /// Sends the user's message, starting a turn. Returns the turn's id.
    pub async fn turn_start(&self, thread_id: &str, request: TurnRequest) -> Result<String> {
        let mut params = json!({ "threadId": thread_id, "input": Self::input(&request) });
        if let Some(model) = &request.model {
            params["model"] = json!(model);
            // Plan mode rides on the collaboration mode, which names its model.
            params["collaborationMode"] = json!({
                "mode": if request.plan_mode { "plan" } else { "default" },
                "settings": {
                    "model": model,
                    "reasoning_effort": request.effort,
                    "developer_instructions": null,
                },
            });
        }
        if let Some(effort) = &request.effort {
            params["effort"] = json!(effort);
        }
        if let Some(mode) = request.mode {
            params["approvalPolicy"] = json!(mode.approval_policy());
            params["sandboxPolicy"] = mode.sandbox_policy();
        }
        if let Some(tier) = &request.service_tier {
            params["serviceTier"] = json!(tier);
        }
        let result = self.connection.request("turn/start", params).await?;
        Ok(str_of(&result["turn"], "id"))
    }

    /// Adds the user's message to the running turn, which reads it at its next step.
    pub async fn turn_steer(
        &self,
        thread_id: &str,
        turn_id: &str,
        request: TurnRequest,
    ) -> Result<String> {
        let result = self
            .connection
            .request(
                "turn/steer",
                json!({ "threadId": thread_id, "expectedTurnId": turn_id, "input": Self::input(&request) }),
            )
            .await?;
        Ok(str_of(&result, "turnId"))
    }

    /// Asks Codex to predict the user's next message after `source_turn_id`.
    /// Codex answers with `thread/prediction/updated`. Where it does not
    /// implement the request yet, a throwaway fork in prediction mode answers
    /// instead: then the returned id is the fork's, and its reply is the guess.
    pub async fn request_prediction(
        &self,
        thread_id: &str,
        source_turn_id: &str,
    ) -> Result<Prediction> {
        let asked = self
            .connection
            .request(
                "thread/prediction/request",
                json!({ "threadId": thread_id, "sourceTurnId": source_turn_id }),
            )
            .await;
        match asked {
            Ok(_) => return Ok(Prediction::Notified),
            Err(RpcError::Server { code: -32601, .. }) => {}
            Err(error) => return Err(error.into()),
        }
        let forked = self
            .connection
            .request(
                "thread/fork",
                json!({
                    "threadId": thread_id,
                    "lastTurnId": source_turn_id,
                    "ephemeral": true,
                    "experimentalPredictionMode": true,
                    "experimentalDisablePredictionMcp": true,
                }),
            )
            .await?;
        let fork_id = str_of(&forked["thread"], "id");
        self.connection
            .request(
                "turn/start",
                json!({
                    "threadId": fork_id,
                    "input": [{ "type": "text", "text": PREDICTION_PROMPT, "text_elements": [] }],
                }),
            )
            .await?;
        Ok(Prediction::Fork(fork_id))
    }

    /// Stops receiving a thread's events and lets the server unload it.
    pub async fn thread_unsubscribe(&self, thread_id: &str) -> Result<()> {
        self.connection
            .request("thread/unsubscribe", json!({ "threadId": thread_id }))
            .await?;
        Ok(())
    }

    /// Drops `before_turn_id` and every later turn from the thread's history
    /// (files on disk are not touched), so the conversation can go another way.
    pub async fn thread_revert(&self, thread_id: &str, before_turn_id: &str) -> Result<()> {
        self.connection
            .request(
                "thread/revert",
                json!({ "threadId": thread_id, "beforeTurnId": before_turn_id }),
            )
            .await?;
        Ok(())
    }

    /// Copies the thread through `last_turn_id` into a new chat.
    pub async fn thread_fork(
        &self,
        thread_id: &str,
        last_turn_id: Option<&str>,
    ) -> Result<ThreadMeta> {
        let mut params = json!({ "threadId": thread_id });
        if let Some(turn) = last_turn_id {
            params["lastTurnId"] = json!(turn);
        }
        let result = self.connection.request("thread/fork", params).await?;
        translate::thread_meta(&result["thread"])
            .ok_or_else(|| anyhow!("thread/fork returned no thread"))
    }

    /// Chats whose content mentions `term`, with a matching snippet each.
    pub async fn thread_search(&self, term: &str, limit: u32) -> Result<Vec<(ThreadMeta, String)>> {
        let result = self
            .connection
            .request(
                "thread/search",
                json!({ "searchTerm": term, "limit": limit, "sortKey": "recency_at" }),
            )
            .await?;
        Ok(result["data"]
            .as_array()
            .map(|hits| {
                hits.iter()
                    .filter_map(|hit| {
                        Some((
                            translate::thread_meta(&hit["thread"])?,
                            str_of(hit, "snippet"),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn goal_get(&self, thread_id: &str) -> Result<Option<Goal>> {
        let result = self
            .connection
            .request("thread/goal/get", json!({ "threadId": thread_id }))
            .await?;
        Ok(translate::goal(&result["goal"]))
    }

    /// Sets or updates the thread's goal. `None` fields keep their value.
    pub async fn goal_set(
        &self,
        thread_id: &str,
        objective: Option<&str>,
        status: Option<GoalStatus>,
        token_budget: Option<u64>,
    ) -> Result<Option<Goal>> {
        let mut params = json!({ "threadId": thread_id, "origin": "user" });
        if let Some(objective) = objective {
            params["objective"] = json!(objective);
        }
        if let Some(status) = status {
            params["status"] = json!(status.wire());
        }
        if let Some(budget) = token_budget {
            params["tokenBudget"] = json!(budget);
        }
        let result = self.connection.request("thread/goal/set", params).await?;
        Ok(translate::goal(&result["goal"]))
    }

    pub async fn goal_clear(&self, thread_id: &str) -> Result<()> {
        self.connection
            .request("thread/goal/clear", json!({ "threadId": thread_id }))
            .await?;
        Ok(())
    }

    /// Whether the thread may use and add to Codex's memories.
    pub async fn memory_mode_set(&self, thread_id: &str, enabled: bool) -> Result<()> {
        self.connection
            .request(
                "thread/memoryMode/set",
                json!({ "threadId": thread_id, "mode": if enabled { "enabled" } else { "disabled" } }),
            )
            .await?;
        Ok(())
    }

    /// How many chats Codex has folded into memory, and whether memory is ready.
    pub async fn memory_status(&self) -> Result<(u64, bool)> {
        let result = self.connection.request("memory/status", json!({})).await?;
        Ok((
            result["v2ConsolidatedThreads"].as_u64().unwrap_or_default(),
            result["v2Ready"].as_bool().unwrap_or(false),
        ))
    }

    /// The subscription's usage windows and credits.
    pub async fn rate_limits(&self) -> Result<RateLimits> {
        let result = self
            .connection
            .request("account/rateLimits/read", json!({}))
            .await?;
        let codex = &result["rateLimitsByLimitId"]["codex"];
        let snapshot = if codex.is_object() {
            codex
        } else {
            &result["rateLimits"]
        };
        Ok(translate::rate_limits(snapshot))
    }

    /// The signed-in account, if any, and whether Codex needs one.
    pub async fn account(&self) -> Result<(Option<Account>, bool)> {
        let result = self.connection.request("account/read", json!({})).await?;
        Ok((
            translate::account(&result["account"]),
            result["requiresOpenaiAuth"].as_bool().unwrap_or(false),
        ))
    }

    /// Starts signing in with ChatGPT in the browser: `(login id, URL to open)`.
    /// Completion arrives as `account/login/completed`.
    pub async fn login_chatgpt(&self) -> Result<(String, String)> {
        let result = self
            .connection
            .request("account/login/start", json!({ "type": "chatgpt" }))
            .await?;
        Ok((str_of(&result, "loginId"), str_of(&result, "authUrl")))
    }

    pub async fn login_cancel(&self, login_id: &str) -> Result<()> {
        self.connection
            .request("account/login/cancel", json!({ "loginId": login_id }))
            .await?;
        Ok(())
    }

    pub async fn logout(&self) -> Result<()> {
        self.connection
            .request("account/logout", Value::Null)
            .await?;
        Ok(())
    }

    /// Answers the agent's questions.
    pub fn answer_user_input(&self, request_id: RequestId, answers: Answers) -> Result<()> {
        let answers: serde_json::Map<String, Value> = answers
            .into_iter()
            .map(|(id, given)| (id, json!({ "answers": given })))
            .collect();
        self.connection
            .respond(request_id, json!({ "answers": answers }))?;
        Ok(())
    }

    pub async fn thread_set_name(&self, thread_id: &str, name: &str) -> Result<()> {
        self.connection
            .request(
                "thread/name/set",
                json!({ "threadId": thread_id, "name": name }),
            )
            .await?;
        Ok(())
    }

    /// Summarizes the thread's history to free context.
    pub async fn thread_compact(&self, thread_id: &str) -> Result<()> {
        self.connection
            .request("thread/compact/start", json!({ "threadId": thread_id }))
            .await?;
        Ok(())
    }

    /// Starts a code review turn inline in the thread.
    pub async fn review_start(&self, thread_id: &str, target: ReviewTarget) -> Result<()> {
        let target = match target {
            ReviewTarget::UncommittedChanges => json!({ "type": "uncommittedChanges" }),
            ReviewTarget::BaseBranch(branch) => json!({ "type": "baseBranch", "branch": branch }),
            ReviewTarget::Custom(instructions) => {
                json!({ "type": "custom", "instructions": instructions })
            }
        };
        self.connection
            .request(
                "review/start",
                json!({ "threadId": thread_id, "target": target }),
            )
            .await?;
        Ok(())
    }

    /// Files under `root` matching `query`, best first, as paths relative to `root`.
    pub async fn fuzzy_file_search(&self, query: &str, root: &str) -> Result<Vec<String>> {
        let result = self
            .connection
            .request(
                "fuzzyFileSearch",
                json!({ "query": query, "roots": [root], "cancellationToken": null }),
            )
            .await?;
        Ok(result["files"]
            .as_array()
            .map(|files| {
                files
                    .iter()
                    .filter(|file| file["match_type"] != "directory")
                    .filter_map(|file| file["path"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn turn_interrupt(&self, thread_id: &str, turn_id: &str) -> Result<()> {
        self.connection
            .request(
                "turn/interrupt",
                json!({ "threadId": thread_id, "turnId": turn_id }),
            )
            .await?;
        Ok(())
    }

    pub async fn model_list(&self) -> Result<Vec<ModelInfo>> {
        let result = self.connection.request("model/list", json!({})).await?;
        Ok(result["data"]
            .as_array()
            .map(|models| {
                models
                    .iter()
                    .filter(|m| !m["hidden"].as_bool().unwrap_or(false))
                    .map(|m| ModelInfo {
                        id: str_of(m, "model"),
                        display_name: m["displayName"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| str_of(m, "model")),
                        description: str_of(m, "description"),
                        is_default: m["isDefault"].as_bool().unwrap_or(false),
                        efforts: m["supportedReasoningEfforts"]
                            .as_array()
                            .map(|efforts| {
                                efforts
                                    .iter()
                                    .filter_map(|e| {
                                        e["reasoningEffort"].as_str().map(str::to_owned)
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        default_effort: m["defaultReasoningEffort"].as_str().map(str::to_owned),
                        service_tiers: m["serviceTiers"]
                            .as_array()
                            .map(|tiers| {
                                tiers
                                    .iter()
                                    .map(|t| {
                                        (
                                            str_of(t, "id"),
                                            str_of(t, "name"),
                                            str_of(t, "description"),
                                        )
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Whether Codex has credentials. `None` when the server cannot tell.
    pub async fn signed_in(&self) -> Option<bool> {
        let result = self
            .connection
            .request("account/read", json!({}))
            .await
            .ok()?;
        let has_account = !result["account"].is_null();
        let needs = result["requiresOpenaiAuth"].as_bool().unwrap_or(false);
        Some(has_account || !needs)
    }

    /// Answers an approval request.
    pub fn answer_approval(&self, request_id: RequestId, decision: ApprovalDecision) -> Result<()> {
        let decision = match decision {
            ApprovalDecision::Accept => "accept",
            ApprovalDecision::AcceptForSession => "acceptForSession",
            ApprovalDecision::Decline => "decline",
            ApprovalDecision::Cancel => "cancel",
        };
        self.connection
            .respond(request_id, json!({ "decision": decision }))?;
        Ok(())
    }

    /// Declines a server request this client does not handle, so the server does not wait forever.
    pub fn reject_request(&self, request_id: RequestId, method: &str) -> Result<()> {
        self.connection.respond_error(
            request_id,
            -32601,
            &format!("Atrium does not support {method} yet"),
        )?;
        Ok(())
    }
}

fn put(params: &mut serde_json::Map<String, Value>, key: &str, value: Option<String>) {
    if let Some(value) = value {
        params.insert(key.to_string(), Value::String(value));
    }
}

fn str_of(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
}

/// Codex colors its logs; our log file should not hold escape codes.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn ansi_codes_are_removed() {
        assert_eq!(
            super::strip_ansi("\u{1b}[2mtime\u{1b}[0m ERROR x"),
            "time ERROR x"
        );
    }
}
