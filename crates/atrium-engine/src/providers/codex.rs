// Ported from apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts
// (T3 Code v0.1.11, MIT), on top of Atrium's existing `codex-client`
// transport rather than a second app-server client. T3's generated
// `effect-codex-app-server` request/notification names are the wire names
// used here.
//!
//! The Codex adapter: one app-server process per provider session, threads
//! started or resumed through it, turns started and steered, mid-thread
//! settings applied to a loaded thread, `thread/read` for views and sync so a
//! Codex desktop app keeps the writer lock, MCP elicitation mapped onto the
//! approval surface, and `skills/list` / `config/read` reporting.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use atrium_core::{AgentEvent, ApprovalKind, ApprovalRequest, Capabilities};
use codex_client::{
    CodexClient, CodexConfig, Incoming, ModelInfo, StartThread, ThreadSettingsUpdate,
};
use futures::StreamExt;
use serde_json::{Value, json};

use super::adapter::{
    AdapterEvent, AdapterEventSink, AdapterTurnInput, CODEX_DRIVER_KIND, EnsureThreadInput,
    OpenSessionInput, ProviderAdapter, ProviderAdapterError, ProviderConfig,
    ProviderSessionRuntime, RuntimePolicy, RuntimeRequestResponse, SelectionTransitionPlan,
    SkillGroup, SteerInput, ThreadSnapshot, provider_session,
};
use super::codex_elicitation::{describe_mcp_elicitation, to_mcp_elicitation_response};
use super::codex_turn::{
    TurnStartInput, build_thread_settings_update, build_turn_start_params, service_tier_value,
};
use crate::contracts::{
    ApprovalCapabilities, CheckpointCapabilities, ContextCapabilities, IdentityCapabilities,
    NativeRefStrength, NodeId, PlanningCapabilities, ProviderApprovalDecision,
    ProviderCapabilities, ProviderRef, ProviderRequestKind, ProviderSession, ProviderSessionId,
    ProviderThread, ProviderThreadId, ProviderThreadStatus, ProviderTurn, ProviderTurnId,
    ProviderTurnStatus, ResponseCapability, RuntimeMode, RuntimeRequest, RuntimeRequestId,
    RuntimeRequestKind, RuntimeRequestStatus, SessionCapabilities, StreamingCapabilities,
    SubagentCapabilities, TerminalStatusQuality, ThreadCapabilities, ThreadId, ToolCapabilities,
    TurnCapabilities,
};

/// Codex refuses `thread/resume` while another process holds the writer.
const THREAD_IN_USE_MARKER: &str = "in use";

/// Minimum Codex version T3 Code 0.1.11 accepts.
pub const MIN_CODEX_VERSION: [u64; 3] = [0, 159, 0];

/// How to construct a Codex adapter. The transport config is Atrium's.
#[derive(Clone, Debug, Default)]
pub struct CodexAdapterConfig {
    pub codex: CodexConfig,
    pub display_name: Option<String>,
    /// Refuse a server whose `userAgent` is older than this, as T3's
    /// compatibility gate does.
    pub minimum_version: Option<[u64; 3]>,
}

impl CodexAdapterConfig {
    pub fn for_binary(binary: impl Into<std::path::PathBuf>) -> Self {
        Self {
            codex: CodexConfig {
                binary: Some(binary.into()),
                ..CodexConfig::default()
            },
            display_name: Some("Codex".into()),
            minimum_version: Some(MIN_CODEX_VERSION),
        }
    }
}

/// T3's `CodexProviderCapabilitiesV2`.
pub fn codex_provider_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        sessions: SessionCapabilities {
            supports_multiple_provider_threads_per_session: true,
            supports_model_switch_in_session: true,
            supports_provider_switching_via_handoff: true,
            supports_runtime_mode_switch_in_session: true,
            pending_requests_survive_restart: false,
        },
        threads: ThreadCapabilities {
            can_create_empty_thread: true,
            can_read_thread_snapshot: true,
            can_rollback_thread: true,
            can_fork_thread: true,
            can_fork_from_turn: true,
            can_fork_while_source_running: Some(true),
            can_fork_from_subagent_thread: true,
            exposes_native_thread_id: true,
        },
        turns: TurnCapabilities {
            supports_native_goals: None,
            exposes_native_turn_id: true,
            emits_turn_started: true,
            emits_turn_completed: true,
            supports_interrupt: true,
            supports_native_resume: Some(true),
            supports_active_steering: true,
            active_steering_interrupts_tools: None,
            active_steering_accepts_attachments: None,
            active_steering_during_compaction: Some(true),
            supports_steering_by_interrupt_restart: true,
            supports_queued_messages: true,
            terminal_status_quality: TerminalStatusQuality::Strong,
        },
        streaming: StreamingCapabilities {
            streams_assistant_text: true,
            streams_reasoning: true,
            streams_tool_output: true,
            streams_plan_text: true,
            emits_message_completed: true,
        },
        tools: ToolCapabilities {
            exposes_tool_item_ids: true,
            emits_tool_started: true,
            emits_tool_completed: true,
            emits_tool_output: true,
            supports_mcp_tools: true,
            supports_dynamic_tool_callbacks: true,
        },
        approvals: ApprovalCapabilities {
            supports_command_approval: true,
            supports_file_read_approval: true,
            supports_file_change_approval: true,
            supports_apply_patch_approval: true,
            approvals_have_native_request_ids: true,
            approval_callbacks_are_live_only: true,
            approvals_can_originate_from_subagents: true,
        },
        planning: PlanningCapabilities {
            emits_plan_updated: true,
            emits_todo_list: true,
            emits_proposed_plan: true,
            supports_structured_questions: true,
            plan_deltas_have_item_ids: true,
        },
        subagents: SubagentCapabilities {
            supports_subagents: true,
            exposes_subagent_thread_ids: true,
            emits_subagent_lifecycle: true,
            can_wait_for_subagents: true,
            can_close_subagents: true,
            can_fork_subagent_thread: true,
        },
        context: ContextCapabilities {
            accepts_system_context: true,
            accepts_developer_context: true,
            accepts_synthetic_user_context: true,
            can_generate_summaries: true,
            can_consume_handoff_summaries: true,
            supports_delta_handoff: true,
            supports_full_thread_handoff: true,
            max_recommended_handoff_chars: None,
        },
        checkpointing: CheckpointCapabilities {
            app_can_checkpoint_filesystem: true,
            supports_nested_checkpoint_scopes: true,
            provider_can_rollback_conversation: true,
            provider_rollback_returns_snapshot: true,
            provider_can_read_conversation_snapshot: true,
        },
        identity: IdentityCapabilities {
            native_thread_ids: NativeRefStrength::Strong,
            native_turn_ids: NativeRefStrength::Strong,
            native_item_ids: NativeRefStrength::Strong,
            native_request_ids: NativeRefStrength::Strong,
        },
        runtime_policy: crate::contracts::RuntimePolicyCapabilities {
            enforcement: crate::contracts::RuntimePolicyEnforcement::Native,
        },
    }
}

/// Atrium's [`Capabilities`] derived from what `initialize`, `model/list` and
/// `skills/list` actually answered. T3's capability record is static; the
/// app's UI needs the narrower, catalog-derived one.
pub fn core_capabilities(models: &[ModelInfo], skills: Option<&[SkillGroup]>) -> Capabilities {
    let mut capabilities = Capabilities::codex();
    capabilities.model_choice = !models.is_empty();
    capabilities.effort_choice = models.iter().any(|model| !model.efforts.is_empty());
    capabilities.service_tiers = models.iter().any(|model| !model.service_tiers.is_empty());
    capabilities.skills = skills.is_some();
    capabilities.mcp_status = true;
    capabilities
}

fn parse_version(user_agent: &str) -> Option<[u64; 3]> {
    let version = user_agent.split_once('/')?.1.split_whitespace().next()?;
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts
        .next()
        .and_then(|patch| {
            patch
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .ok()
        })
        .unwrap_or(0);
    Some([major, minor, patch])
}

fn version_is_new_enough(user_agent: &str, minimum: [u64; 3]) -> bool {
    parse_version(user_agent).is_none_or(|version| version >= minimum)
}

fn runtime_mode_policy(mode: RuntimeMode) -> (&'static str, &'static str) {
    match mode {
        RuntimeMode::ApprovalRequired => ("untrusted", "read-only"),
        RuntimeMode::AutoAcceptEdits | RuntimeMode::Auto => ("on-request", "workspace-write"),
        RuntimeMode::FullAccess => ("never", "danger-full-access"),
    }
}

/// One configured Codex provider instance.
pub struct CodexAdapter {
    instance_id: crate::contracts::ProviderInstanceId,
    config: CodexAdapterConfig,
    capabilities: ProviderCapabilities,
}

impl CodexAdapter {
    pub fn new(instance_id: impl Into<String>, config: CodexAdapterConfig) -> Self {
        Self {
            instance_id: crate::contracts::ProviderInstanceId::new(instance_id),
            config,
            capabilities: codex_provider_capabilities(),
        }
    }

    /// Open a session on a client the caller already owns (tests, a node, or
    /// a host that shares one app-server).
    pub fn open_with_client(
        &self,
        client: CodexClient,
        events: futures::channel::mpsc::UnboundedReceiver<Incoming>,
        input: OpenSessionInput,
        sink: AdapterEventSink,
    ) -> Result<Arc<dyn ProviderSessionRuntime>, ProviderAdapterError> {
        Ok(Arc::new(CodexSessionRuntime::new(
            self.instance_id.clone(),
            self.capabilities.clone(),
            client,
            events,
            input,
            sink,
        )))
    }
}

impl ProviderAdapter for CodexAdapter {
    fn instance_id(&self) -> &crate::contracts::ProviderInstanceId {
        &self.instance_id
    }

    fn driver(&self) -> &str {
        CODEX_DRIVER_KIND
    }

    fn checks_version_at_handshake(&self) -> bool {
        self.config.minimum_version.is_some()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }

    fn plan_selection_transition(
        &self,
        _current: &crate::contracts::ModelSelection,
        _target: &crate::contracts::ModelSelection,
    ) -> SelectionTransitionPlan {
        // Codex applies model and effort per turn.
        SelectionTransitionPlan::ApplyOnNextTurn
    }

    fn open_session(
        &self,
        input: OpenSessionInput,
        sink: AdapterEventSink,
    ) -> Result<Arc<dyn ProviderSessionRuntime>, ProviderAdapterError> {
        let driver = CODEX_DRIVER_KIND;
        let started = futures::executor::block_on(CodexClient::start(self.config.codex.clone()))
            .map_err(|error| {
                log::warn!("Codex app-server did not start: {error}");
                ProviderAdapterError::OpenSession {
                    driver: driver.into(),
                    provider_session_id: input.provider_session_id.to_string(),
                }
            })?;
        let (client, incoming) = started;
        if let Some(minimum) = self.config.minimum_version
            && !version_is_new_enough(&client.user_agent, minimum)
        {
            return Err(ProviderAdapterError::Protocol {
                driver: driver.into(),
                detail: format!(
                    "{} does not work with this T3 Code release. Update Codex to >= {}.{}.{}",
                    parse_version(&client.user_agent)
                        .map(|v| format!("{}.{}.{}", v[0], v[1], v[2]))
                        .unwrap_or_else(|| client.user_agent.clone()),
                    minimum[0],
                    minimum[1],
                    minimum[2]
                ),
            });
        }
        self.open_with_client(client, incoming, input, sink)
    }
}

#[derive(Clone)]
enum PendingKind {
    Elicitation { payload: Value },
    Approval,
    UserInput,
}

struct PendingRequest {
    id: Value,
    kind: PendingKind,
}

struct ActiveTurn {
    native_thread_id: String,
    provider_turn: ProviderTurn,
    run_ordinal: u32,
}

struct Inner {
    driver: String,
    instance_id: crate::contracts::ProviderInstanceId,
    provider_session_id: ProviderSessionId,
    provider_session: Mutex<ProviderSession>,
    client: CodexClient,
    sink: AdapterEventSink,
    pending: Mutex<HashMap<String, PendingRequest>>,
    runtime_policy: Mutex<RuntimePolicy>,
    loaded: Mutex<HashSet<String>>,
    threads: Mutex<HashMap<String, ProviderThread>>,
    active_turns: Mutex<HashMap<String, ActiveTurn>>,
    now: Mutex<String>,
    next_ordinal: AtomicU64,
}

impl Inner {
    fn emit(&self, event: AdapterEvent) {
        (self.sink)(event);
    }

    fn thread_for_native(&self, native: &str) -> Option<ProviderThread> {
        self.threads
            .lock()
            .expect("codex threads")
            .get(native)
            .cloned()
    }

    fn active_for_thread(&self, native: &str) -> Option<ProviderTurn> {
        self.active_turns
            .lock()
            .expect("codex turns")
            .values()
            .find(|turn| turn.native_thread_id == native)
            .map(|turn| turn.provider_turn.clone())
    }

    fn request_kind(&self, method: &str) -> Option<ProviderRequestKind> {
        Some(match method {
            "item/commandExecution/requestApproval" => ProviderRequestKind::Command,
            "item/fileChange/requestApproval" => ProviderRequestKind::FileChange,
            "item/permissions/requestApproval" => ProviderRequestKind::Permission,
            "item/tool/requestUserInput" => ProviderRequestKind::Permission,
            _ => return None,
        })
    }

    fn runtime_request(
        &self,
        id: &Value,
        params: &Value,
        kind: RuntimeRequestKind,
    ) -> RuntimeRequest {
        let id_text = id_text(id);
        let now = self.now.lock().expect("codex now").clone();
        let turn_id = params["turnId"]
            .as_str()
            .map(ProviderTurnId::new)
            .or_else(|| {
                params["threadId"]
                    .as_str()
                    .and_then(|native| self.active_for_thread(native))
                    .map(|turn| turn.id)
            });
        RuntimeRequest {
            id: RuntimeRequestId::new(id_text.clone()),
            node_id: NodeId::new(format!("runtime-request-{id_text}")),
            provider_turn_id: turn_id,
            native_request_ref: Some(ProviderRef {
                driver: crate::contracts::ProviderDriverKind::new(&self.driver),
                native_id: Some(id_text),
                strength: NativeRefStrength::Strong,
                fingerprint: None,
                ordinal: None,
            }),
            kind,
            status: RuntimeRequestStatus::Pending,
            response_capability: ResponseCapability::Live {
                provider_session_id: self.provider_session_id.clone(),
                supports_feedback: Some(false),
            },
            created_at: now,
            resolved_at: None,
            decision: None,
            feedback: None,
            answers: None,
        }
    }

    fn publish_runtime_request(&self, request: RuntimeRequest, thread_id: Option<ThreadId>) {
        self.emit(AdapterEvent::RuntimeRequestUpdated {
            driver: self.driver.clone(),
            thread_id,
            runtime_request: request,
        });
    }

    fn on_notification(&self, method: &str, params: &Value) {
        if let Some(event) = codex_client::translate::event(method, params) {
            self.emit(AdapterEvent::Agent(event));
        }
        match method {
            "turn/started" => {
                let native_thread = params["threadId"].as_str().unwrap_or_default().to_string();
                let native_turn = params["turn"]["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let ordinal = self.next_ordinal.fetch_add(1, Ordering::Relaxed) as u32 + 1;
                let now = self.now.lock().expect("codex now").clone();
                let provider_thread_id = self
                    .thread_for_native(&native_thread)
                    .map(|thread| thread.id)
                    .unwrap_or_else(|| {
                        ProviderThreadId::new(format!("provider-thread-{native_thread}"))
                    });
                let turn = ProviderTurn {
                    id: ProviderTurnId::new(&native_turn),
                    provider_thread_id,
                    node_id: NodeId::new(format!("node-{native_turn}")),
                    run_attempt_id: None,
                    native_turn_ref: Some(ProviderRef {
                        driver: crate::contracts::ProviderDriverKind::new(&self.driver),
                        native_id: Some(native_turn.clone()),
                        strength: NativeRefStrength::Strong,
                        fingerprint: None,
                        ordinal: None,
                    }),
                    native_thread_ref: None,
                    ordinal,
                    status: ProviderTurnStatus::Running,
                    started_at: Some(now.clone()),
                    completed_at: None,
                    token_usage: None,
                    turn_token_usage: None,
                    model: self
                        .provider_session
                        .lock()
                        .expect("codex session")
                        .model
                        .clone(),
                };
                self.active_turns.lock().expect("codex turns").insert(
                    turn.id.to_string(),
                    ActiveTurn {
                        native_thread_id: native_thread,
                        provider_turn: turn.clone(),
                        run_ordinal: ordinal,
                    },
                );
                self.emit(AdapterEvent::ProviderTurnUpdated {
                    driver: self.driver.clone(),
                    thread_id: None,
                    provider_turn: turn,
                });
            }
            "turn/completed" => {
                let native_turn = params["turn"]["id"].as_str().unwrap_or_default();
                let status = match params["turn"]["status"].as_str() {
                    Some("completed") => ProviderTurnStatus::Completed,
                    Some("interrupted") => ProviderTurnStatus::Interrupted,
                    Some("failed") => ProviderTurnStatus::Failed,
                    _ => ProviderTurnStatus::Cancelled,
                };
                let status_wire = match status {
                    ProviderTurnStatus::Completed => "completed",
                    ProviderTurnStatus::Interrupted => "interrupted",
                    ProviderTurnStatus::Failed => "failed",
                    _ => "cancelled",
                };
                let now = self.now.lock().expect("codex now").clone();
                let active = self
                    .active_turns
                    .lock()
                    .expect("codex turns")
                    .remove(native_turn);
                if let Some(active) = active {
                    let mut turn = active.provider_turn;
                    turn.status = status;
                    turn.completed_at = Some(now);
                    let provider_thread_id = turn.provider_thread_id.to_string();
                    self.emit(AdapterEvent::ProviderTurnUpdated {
                        driver: self.driver.clone(),
                        thread_id: None,
                        provider_turn: turn,
                    });
                    self.emit(AdapterEvent::TurnTerminal {
                        driver: self.driver.clone(),
                        provider_thread_id,
                        provider_turn_id: native_turn.to_string(),
                        run_ordinal: active.run_ordinal,
                        status: status_wire.into(),
                    });
                }
            }
            "thread/settings/updated" => {
                let settings = &params["threadSettings"];
                let mut session = self.provider_session.lock().expect("codex session");
                if let Some(model) = settings["model"].as_str() {
                    session.model = Some(model.to_string());
                }
                session.updated_at = self.now.lock().expect("codex now").clone();
                let event = AdapterEvent::ProviderSessionUpdated {
                    driver: self.driver.clone(),
                    provider_session: session.clone(),
                };
                drop(session);
                self.emit(event);
            }
            _ => {}
        }
    }

    fn on_request(&self, id: Value, method: String, params: Value) {
        match method.as_str() {
            "mcpServer/elicitation/request" => {
                let acceptance =
                    to_mcp_elicitation_response(&params, ProviderApprovalDecision::Accept);
                if acceptance["action"].as_str() != Some("accept") {
                    let _ = self.client.respond_raw(id, json!({"action": "cancel"}));
                    return;
                }
                let full_access = self
                    .runtime_policy
                    .lock()
                    .expect("codex policy")
                    .runtime_mode
                    == RuntimeMode::FullAccess;
                if full_access {
                    let _ = self.client.respond_raw(id, acceptance);
                    return;
                }
                let described = describe_mcp_elicitation(&params);
                let thread_id = params["threadId"].as_str().map(ThreadId::new);
                let turn_id = params["turnId"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| {
                        params["threadId"]
                            .as_str()
                            .and_then(|native| self.active_for_thread(native))
                            .map(|turn| turn.id.to_string())
                    })
                    .unwrap_or_default();
                let request =
                    self.runtime_request(&id, &params, RuntimeRequestKind::McpElicitation);
                let approval = AgentEvent::ApprovalRequested(ApprovalRequest {
                    request_id: id.clone(),
                    thread_id: params["threadId"].as_str().unwrap_or_default().to_string(),
                    turn_id,
                    item_id: id_text(&id),
                    kind: ApprovalKind::Tool {
                        title: described.app_name.clone(),
                        detail: params["message"].as_str().map(str::to_owned),
                    },
                });
                self.pending.lock().expect("codex pending").insert(
                    id_text(&id),
                    PendingRequest {
                        id,
                        kind: PendingKind::Elicitation { payload: params },
                    },
                );
                self.emit(AdapterEvent::Agent(approval));
                self.publish_runtime_request(request, thread_id);
            }
            _ => {
                let Some(kind) = self.request_kind(&method) else {
                    let _ = self.client.respond_error_raw(
                        id,
                        -32601,
                        &format!("Atrium does not support {method} yet"),
                    );
                    return;
                };
                if let Some(event) = codex_client::translate::request(&id, &method, &params) {
                    self.emit(AdapterEvent::Agent(event));
                }
                let runtime_kind = match kind {
                    ProviderRequestKind::Command => RuntimeRequestKind::Command,
                    ProviderRequestKind::FileChange => RuntimeRequestKind::FileChange,
                    _ => RuntimeRequestKind::Permission,
                };
                let thread_id = params["threadId"].as_str().map(ThreadId::new);
                let request = self.runtime_request(&id, &params, runtime_kind);
                self.pending.lock().expect("codex pending").insert(
                    id_text(&id),
                    PendingRequest {
                        id,
                        kind: if method == "item/tool/requestUserInput" {
                            PendingKind::UserInput
                        } else {
                            PendingKind::Approval
                        },
                    },
                );
                self.publish_runtime_request(request, thread_id);
            }
        }
    }
}

fn id_text(id: &Value) -> String {
    match id {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// A live Codex provider session.
pub struct CodexSessionRuntime {
    inner: Arc<Inner>,
    core_capabilities: Mutex<Capabilities>,
    models: Vec<ModelInfo>,
}

impl CodexSessionRuntime {
    fn new(
        instance_id: crate::contracts::ProviderInstanceId,
        capabilities: ProviderCapabilities,
        client: CodexClient,
        events: futures::channel::mpsc::UnboundedReceiver<Incoming>,
        input: OpenSessionInput,
        sink: AdapterEventSink,
    ) -> Self {
        let cwd = input.runtime_policy.cwd.clone();
        let model = input.model_selection.model.clone();
        let cwds: Vec<String> = cwd.iter().cloned().collect();
        let models = futures::executor::block_on(client.model_list()).unwrap_or_default();
        let skills = futures::executor::block_on(client.skills_list(&cwds))
            .ok()
            .map(|value| parse_skill_groups(&value));
        // T3 reads the folder's config at session start (it names the stdio
        // MCP servers Codex starts per thread). Failures are not fatal.
        let _config = futures::executor::block_on(client.config_read(cwd.as_deref())).ok();
        let core = core_capabilities(&models, skills.as_deref());
        let session = provider_session(
            input.provider_session_id.clone(),
            instance_id.clone(),
            cwd.as_deref(),
            Some(&model),
            capabilities,
            &input.now,
        );
        let inner = Arc::new(Inner {
            driver: CODEX_DRIVER_KIND.into(),
            instance_id,
            provider_session_id: input.provider_session_id.clone(),
            provider_session: Mutex::new(session.clone()),
            client,
            sink: sink.clone(),
            pending: Mutex::new(HashMap::new()),
            runtime_policy: Mutex::new(input.runtime_policy),
            loaded: Mutex::new(HashSet::new()),
            threads: Mutex::new(HashMap::new()),
            active_turns: Mutex::new(HashMap::new()),
            now: Mutex::new(input.now),
            next_ordinal: AtomicU64::new(0),
        });
        (sink)(AdapterEvent::ProviderSessionUpdated {
            driver: CODEX_DRIVER_KIND.into(),
            provider_session: session,
        });
        let pump = inner.clone();
        thread::Builder::new()
            .name("codex-adapter-events".into())
            .spawn(move || {
                let mut events = events;
                futures::executor::block_on(async move {
                    while let Some(incoming) = events.next().await {
                        match incoming {
                            Incoming::Notification { method, params } => {
                                pump.on_notification(&method, &params)
                            }
                            Incoming::Request { id, method, params } => {
                                pump.on_request(id, method, params)
                            }
                            Incoming::Closed => break,
                        }
                    }
                });
            })
            .expect("spawn codex adapter event pump");
        Self {
            inner,
            core_capabilities: Mutex::new(core),
            models,
        }
    }

    pub fn core_capabilities(&self) -> Capabilities {
        self.core_capabilities
            .lock()
            .expect("codex capabilities")
            .clone()
    }

    pub fn models(&self) -> &[ModelInfo] {
        &self.models
    }

    /// The underlying client, for a host that shares the transport.
    pub fn client(&self) -> &CodexClient {
        &self.inner.client
    }

    fn native_thread_id(thread: &ProviderThread) -> Result<String, ProviderAdapterError> {
        thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.clone())
            .ok_or_else(|| {
                ProviderAdapterError::protocol(
                    CODEX_DRIVER_KIND,
                    format!(
                        "Provider thread {} is missing a native Codex thread id",
                        thread.id
                    ),
                )
            })
    }

    fn thread_in_use(driver: &str, native: &str, error: &str) -> ProviderAdapterError {
        ProviderAdapterError::ThreadInUse {
            driver: driver.into(),
            provider_thread_id: native.into(),
            detail: error.into(),
        }
    }

    fn update_thread(&self, thread: ProviderThread) -> ProviderThread {
        if let Some(native) = thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.clone())
        {
            self.inner
                .threads
                .lock()
                .expect("codex threads")
                .insert(native, thread.clone());
        }
        let event = AdapterEvent::ProviderThreadUpdated {
            driver: CODEX_DRIVER_KIND.into(),
            provider_thread: thread.clone(),
            pending_background_work: None,
        };
        self.inner.emit(event);
        thread
    }

    fn set_policy(&self, policy: &RuntimePolicy) {
        *self.inner.runtime_policy.lock().expect("codex policy") = policy.clone();
    }

    fn set_now(&self, now: &str) {
        *self.inner.now.lock().expect("codex now") = now.to_string();
    }

    fn mark_loaded(&self, native: &str) {
        self.inner
            .loaded
            .lock()
            .expect("codex loaded")
            .insert(native.to_string());
    }

    fn is_loaded(&self, native: &str) -> bool {
        self.inner
            .loaded
            .lock()
            .expect("codex loaded")
            .contains(native)
    }

    fn new_provider_thread(
        &self,
        app_thread: ThreadId,
        native: String,
        now: &str,
        status: ProviderThreadStatus,
    ) -> ProviderThread {
        ProviderThread {
            id: ProviderThreadId::new(format!("provider-thread-{native}")),
            driver: crate::contracts::ProviderDriverKind::new(CODEX_DRIVER_KIND),
            provider_instance_id: self.inner.instance_id.clone(),
            provider_session_id: Some(self.inner.provider_session_id.clone()),
            app_thread_id: Some(app_thread),
            owner_node_id: None,
            native_thread_ref: Some(ProviderRef {
                driver: crate::contracts::ProviderDriverKind::new(CODEX_DRIVER_KIND),
                native_id: Some(native),
                strength: NativeRefStrength::Strong,
                fingerprint: None,
                ordinal: None,
            }),
            native_conversation_head_ref: None,
            status,
            first_run_ordinal: None,
            last_run_ordinal: None,
            handoff_ids: Vec::new(),
            forked_from: None,
            pending_background_tasks: Vec::new(),
            context_usage: None,
            native_metadata: None,
            goal: None,
            created_at: now.to_string(),
            updated_at: now.to_string(),
        }
    }
}

impl ProviderSessionRuntime for CodexSessionRuntime {
    fn provider_session(&self) -> ProviderSession {
        self.inner
            .provider_session
            .lock()
            .expect("codex session")
            .clone()
    }

    fn driver(&self) -> &str {
        CODEX_DRIVER_KIND
    }

    fn ensure_thread(
        &self,
        input: EnsureThreadInput,
    ) -> Result<ProviderThread, ProviderAdapterError> {
        self.set_policy(&input.runtime_policy);
        self.set_now(&input.now);
        if !input.force_new_native_thread
            && let Some(existing) = input.existing_provider_thread.clone()
            && existing.native_thread_ref.is_some()
        {
            return self.resume_thread(
                &existing,
                Some(&input.model_selection),
                Some(&input.runtime_policy),
                &input.now,
            );
        }
        let (approval, sandbox) = runtime_mode_policy(input.runtime_policy.runtime_mode);
        let start = StartThread {
            cwd: input.runtime_policy.cwd.clone(),
            model: Some(input.model_selection.model.clone()),
            approval_policy: Some(approval.into()),
            sandbox: Some(sandbox.into()),
            ..StartThread::default()
        };
        let meta = futures::executor::block_on(self.inner.client.thread_start(start)).map_err(
            |_error| ProviderAdapterError::EnsureThread {
                driver: CODEX_DRIVER_KIND.into(),
                thread_id: input.thread_id.to_string(),
            },
        )?;
        let thread = self.new_provider_thread(
            input.thread_id,
            meta.id.clone(),
            &input.now,
            ProviderThreadStatus::Active,
        );
        self.mark_loaded(&meta.id);
        if let Some(title) = input.title {
            let _ =
                futures::executor::block_on(self.inner.client.thread_set_name(&meta.id, &title));
        }
        Ok(self.update_thread(thread))
    }

    fn resume_thread(
        &self,
        provider_thread: &ProviderThread,
        model_selection: Option<&crate::contracts::ModelSelection>,
        runtime_policy: Option<&RuntimePolicy>,
        now: &str,
    ) -> Result<ProviderThread, ProviderAdapterError> {
        let native = Self::native_thread_id(provider_thread)?;
        if let Some(policy) = runtime_policy {
            self.set_policy(policy);
        }
        self.set_now(now);
        let mode = runtime_policy.map(|policy| match policy.runtime_mode {
            RuntimeMode::ApprovalRequired => codex_client::AccessMode::ReadOnly,
            RuntimeMode::AutoAcceptEdits | RuntimeMode::Auto => codex_client::AccessMode::Auto,
            RuntimeMode::FullAccess => codex_client::AccessMode::FullAccess,
        });
        let resumed = futures::executor::block_on(self.inner.client.thread_resume_in_mode(
            &native,
            &[],
            mode,
        ));
        match resumed {
            Ok(_) => {
                self.mark_loaded(&native);
                let mut thread = provider_thread.clone();
                thread.status = ProviderThreadStatus::Active;
                thread.provider_session_id = Some(self.inner.provider_session_id.clone());
                thread.updated_at = now.to_string();
                if let Some(selection) = model_selection {
                    let mut session = self.inner.provider_session.lock().expect("codex session");
                    session.model = Some(selection.model.clone());
                    session.updated_at = now.to_string();
                }
                Ok(self.update_thread(thread))
            }
            Err(error) => {
                let text = error.to_string();
                if text.to_ascii_lowercase().contains(THREAD_IN_USE_MARKER) {
                    Err(Self::thread_in_use(CODEX_DRIVER_KIND, &native, &text))
                } else {
                    Err(ProviderAdapterError::ResumeThread {
                        driver: CODEX_DRIVER_KIND.into(),
                        provider_thread_id: provider_thread.id.to_string(),
                    })
                }
            }
        }
    }

    fn start_turn(&self, input: AdapterTurnInput) -> Result<ProviderTurn, ProviderAdapterError> {
        let native = Self::native_thread_id(&input.provider_thread)?;
        self.set_policy(&input.runtime_policy);
        self.set_now(&input.now);
        // A turn needs the thread loaded. Resuming is the write path; a
        // snapshot read alone does not load it.
        if !self.is_loaded(&native) {
            self.resume_thread(
                &input.provider_thread,
                Some(&input.model_selection),
                Some(&input.runtime_policy),
                &input.now,
            )?;
        }
        let params = build_turn_start_params(TurnStartInput {
            native_thread_id: &native,
            text: &input.message.text,
            model_selection: &input.model_selection,
            runtime_mode: input.runtime_policy.runtime_mode,
            interaction_mode: input.runtime_policy.interaction_mode,
            cwd: input.runtime_policy.cwd.as_deref(),
            approval_policy_override: input.runtime_policy.approval_policy.as_ref(),
            sandbox_policy_override: input.runtime_policy.sandbox_policy.as_ref(),
            configured_reasoning_summary: None,
            omit_service_tier: false,
        });
        let native_turn =
            futures::executor::block_on(self.inner.client.turn_start_with_params(params))
                .map_err(|_error| ProviderAdapterError::TurnStart {
                    driver: CODEX_DRIVER_KIND.into(),
                    thread_id: input.thread_id.to_string(),
                    provider_thread_id: input.provider_thread.id.to_string(),
                    run_id: input.run_id.to_string(),
                })
                .map_err(|error| {
                    log::warn!("Codex turn/start failed: {error:?}");
                    error
                })?;
        let turn = ProviderTurn {
            id: ProviderTurnId::new(&native_turn),
            provider_thread_id: input.provider_thread.id.clone(),
            node_id: input.root_node_id.clone(),
            run_attempt_id: Some(input.attempt_id.clone()),
            native_turn_ref: Some(ProviderRef {
                driver: crate::contracts::ProviderDriverKind::new(CODEX_DRIVER_KIND),
                native_id: Some(native_turn.clone()),
                strength: NativeRefStrength::Strong,
                fingerprint: None,
                ordinal: None,
            }),
            native_thread_ref: None,
            ordinal: input.provider_turn_ordinal,
            status: ProviderTurnStatus::Running,
            started_at: Some(input.now.clone()),
            completed_at: None,
            token_usage: None,
            turn_token_usage: None,
            model: Some(input.model_selection.model.clone()),
        };
        self.inner.active_turns.lock().expect("codex turns").insert(
            native_turn.clone(),
            ActiveTurn {
                native_thread_id: native,
                provider_turn: turn.clone(),
                run_ordinal: input.run_ordinal,
            },
        );
        self.inner.emit(AdapterEvent::ProviderTurnUpdated {
            driver: CODEX_DRIVER_KIND.into(),
            thread_id: Some(input.thread_id),
            provider_turn: turn.clone(),
        });
        Ok(turn)
    }

    fn steer_turn(&self, input: SteerInput) -> Result<(), ProviderAdapterError> {
        let native = Self::native_thread_id(&input.provider_thread)?;
        self.set_now(&input.now);
        // T3 applies a changed service tier before steering. A failure here
        // must not lose the message.
        if let Some(selection) = &input.model_selection
            && let Some(tier) = service_tier_value(selection)
        {
            let update = ThreadSettingsUpdate {
                thread_id: native.clone(),
                service_tier: Some(tier),
                ..ThreadSettingsUpdate::default()
            };
            if let Err(error) =
                futures::executor::block_on(self.inner.client.thread_settings_update(update))
            {
                log::warn!("orchestration-v2.codex-service-tier-update-failed: {error}");
            }
        }
        let params = json!({
            "threadId": native,
            "expectedTurnId": input.provider_turn_id.to_string(),
            "input": [{"type": "text", "text": input.message.text, "text_elements": []}],
        });
        futures::executor::block_on(self.inner.client.turn_steer_with_params(params))
            .map(|_| ())
            .map_err(|error| {
                let text = error.to_string();
                if text.contains("no active turn") {
                    ProviderAdapterError::SteerRunUnsupported {
                        driver: CODEX_DRIVER_KIND.into(),
                        provider_thread_id: input.provider_thread.id.to_string(),
                    }
                } else {
                    ProviderAdapterError::SteerRun {
                        driver: CODEX_DRIVER_KIND.into(),
                        provider_thread_id: input.provider_thread.id.to_string(),
                        provider_turn_id: input.provider_turn_id.to_string(),
                    }
                }
            })
    }

    fn interrupt_turn(
        &self,
        provider_thread: &ProviderThread,
        provider_turn_id: &ProviderTurnId,
    ) -> Result<(), ProviderAdapterError> {
        let native = Self::native_thread_id(provider_thread)?;
        futures::executor::block_on(
            self.inner
                .client
                .turn_interrupt(&native, provider_turn_id.as_str()),
        )
        .map_err(|_| ProviderAdapterError::Interrupt {
            driver: CODEX_DRIVER_KIND.into(),
            provider_thread_id: provider_thread.id.to_string(),
            provider_turn_id: provider_turn_id.to_string(),
        })
    }

    fn apply_model_selection(
        &self,
        provider_thread: &ProviderThread,
        model_selection: &crate::contracts::ModelSelection,
        runtime_policy: Option<&RuntimePolicy>,
    ) -> Result<(), ProviderAdapterError> {
        let Some(native) = provider_thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.clone())
        else {
            return Ok(());
        };
        let runtime_mode = runtime_policy
            .map(|policy| policy.runtime_mode)
            .unwrap_or_else(|| {
                self.inner
                    .runtime_policy
                    .lock()
                    .expect("codex policy")
                    .runtime_mode
            });
        if let Some(policy) = runtime_policy {
            self.set_policy(policy);
        }
        // T3 applies thread-scoped selection to a loaded thread; an unloaded
        // one picks the new selection up on its next turn.
        if !self.is_loaded(&native) {
            return Ok(());
        }
        let update = build_thread_settings_update(
            &native,
            model_selection,
            runtime_mode,
            runtime_policy.and_then(|policy| policy.approval_policy.as_ref()),
        );
        futures::executor::block_on(self.inner.client.thread_settings_update(update)).map_err(
            |error| {
                log::warn!("thread/settings/update failed: {error}");
                ProviderAdapterError::Protocol {
                    driver: CODEX_DRIVER_KIND.into(),
                    detail: "thread/settings/update failed".into(),
                }
            },
        )?;
        let mut session = self.inner.provider_session.lock().expect("codex session");
        session.model = Some(model_selection.model.clone());
        session.updated_at = self.inner.now.lock().expect("codex now").clone();
        Ok(())
    }

    fn apply_thread_title(
        &self,
        provider_thread: &ProviderThread,
        title: &str,
    ) -> Result<(), ProviderAdapterError> {
        let native = Self::native_thread_id(provider_thread)?;
        futures::executor::block_on(self.inner.client.thread_set_name(&native, title))
            .map_err(|error| ProviderAdapterError::protocol(CODEX_DRIVER_KIND, error.to_string()))
    }

    fn respond_to_runtime_request(
        &self,
        input: RuntimeRequestResponse,
    ) -> Result<(), ProviderAdapterError> {
        let pending = self
            .inner
            .pending
            .lock()
            .expect("codex pending")
            .remove(&input.request_id)
            .ok_or_else(|| ProviderAdapterError::RuntimeRequestResponse {
                driver: CODEX_DRIVER_KIND.into(),
                request_id: input.request_id.clone(),
            })?;
        let result = match pending.kind {
            PendingKind::Elicitation { payload } => to_mcp_elicitation_response(
                &payload,
                input.decision.unwrap_or(ProviderApprovalDecision::Cancel),
            ),
            PendingKind::Approval => {
                let decision = match input.decision.unwrap_or(ProviderApprovalDecision::Cancel) {
                    ProviderApprovalDecision::Accept => "accept",
                    ProviderApprovalDecision::AcceptForSession
                    | ProviderApprovalDecision::AcceptAlways => "acceptForSession",
                    ProviderApprovalDecision::Decline => "decline",
                    ProviderApprovalDecision::Cancel => "cancel",
                };
                json!({"decision": decision})
            }
            PendingKind::UserInput => {
                let answers: serde_json::Map<String, Value> = input
                    .answers
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(id, value)| (id, json!({"answers": value})))
                    .collect();
                json!({"answers": answers})
            }
        };
        self.inner
            .client
            .respond_raw(pending.id, result)
            .map_err(|_| ProviderAdapterError::RuntimeRequestResponse {
                driver: CODEX_DRIVER_KIND.into(),
                request_id: input.request_id,
            })
    }

    fn read_thread_snapshot(
        &self,
        provider_thread: &ProviderThread,
    ) -> Result<ThreadSnapshot, ProviderAdapterError> {
        let native = Self::native_thread_id(provider_thread)?;
        let (meta, _turns) = futures::executor::block_on(self.inner.client.thread_read(&native))
            .map_err(|_| ProviderAdapterError::ReadThreadSnapshot {
                driver: CODEX_DRIVER_KIND.into(),
                provider_thread_id: provider_thread.id.to_string(),
            })?;
        let mut thread = provider_thread.clone();
        thread.native_thread_ref = Some(ProviderRef {
            driver: crate::contracts::ProviderDriverKind::new(CODEX_DRIVER_KIND),
            native_id: Some(meta.id),
            strength: NativeRefStrength::Strong,
            fingerprint: None,
            ordinal: None,
        });
        // T3's snapshot puts the raw provider payload on the record and
        // leaves messages/turns for the projection layer.
        Ok(ThreadSnapshot {
            provider_thread: thread,
            provider_turns: Vec::new(),
            messages: Vec::new(),
            runtime_requests: Vec::new(),
            provider_payload: None,
        })
    }

    fn skills(&self, cwds: &[String]) -> Result<Vec<SkillGroup>, ProviderAdapterError> {
        let value =
            futures::executor::block_on(self.inner.client.skills_list(cwds)).map_err(|error| {
                ProviderAdapterError::protocol(CODEX_DRIVER_KIND, error.to_string())
            })?;
        Ok(parse_skill_groups(&value))
    }

    fn config(&self, cwd: Option<&str>) -> Result<ProviderConfig, ProviderAdapterError> {
        let value =
            futures::executor::block_on(self.inner.client.config_read(cwd)).map_err(|error| {
                ProviderAdapterError::protocol(CODEX_DRIVER_KIND, error.to_string())
            })?;
        Ok(ProviderConfig {
            config: value.get("config").cloned().unwrap_or(Value::Null),
            origins: value.get("origins").cloned().unwrap_or(Value::Null),
        })
    }

    fn unload_thread(&self, provider_thread: &ProviderThread) -> Result<(), ProviderAdapterError> {
        let native = Self::native_thread_id(provider_thread)?;
        futures::executor::block_on(self.inner.client.thread_unsubscribe(&native)).map_err(
            |error| ProviderAdapterError::protocol(CODEX_DRIVER_KIND, error.to_string()),
        )?;
        self.inner
            .loaded
            .lock()
            .expect("codex loaded")
            .remove(&native);
        Ok(())
    }

    fn close(&self) {
        // Dropping the process handle kills the app-server.
    }
}

fn parse_skill_groups(value: &Value) -> Vec<SkillGroup> {
    value["data"]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .map(|group| SkillGroup {
                    cwd: group["cwd"].as_str().unwrap_or_default().to_string(),
                    skills: group["skills"].as_array().cloned().unwrap_or_default(),
                    errors: group["errors"].as_array().cloned().unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_client::ModelInfo;

    // Ported from apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts,
    // `CodexProviderCapabilitiesV2` (T3 Code v0.1.11, MIT).
    #[test]
    fn codex_capabilities_match_t3s_record() {
        let capabilities = codex_provider_capabilities();
        assert!(capabilities.sessions.supports_model_switch_in_session);
        assert!(capabilities.threads.can_create_empty_thread);
        assert!(capabilities.threads.can_read_thread_snapshot);
        assert!(capabilities.turns.supports_active_steering);
        assert_eq!(
            capabilities.turns.terminal_status_quality,
            TerminalStatusQuality::Strong
        );
        assert!(capabilities.approvals.approvals_have_native_request_ids);
        assert_eq!(
            capabilities.runtime_policy.enforcement,
            crate::contracts::RuntimePolicyEnforcement::Native
        );
        // This is the JSON the provider session carries.
        let value = serde_json::to_value(&capabilities).unwrap();
        assert_eq!(
            value["sessions"]["supportsMultipleProviderThreadsPerSession"],
            true
        );
        assert_eq!(value["turns"]["supportsNativeResume"], true);
        assert_eq!(value["runtimePolicy"]["enforcement"], "native");
    }

    // Derived where T3 derives a provider snapshot: the catalog decides which
    // controls exist (T3's provider probe does the same through model/list and
    // skills/list; see docs/t3-spike.md).
    #[test]
    fn core_capabilities_follow_the_catalog_and_skills_list() {
        let empty = core_capabilities(&[], None);
        assert!(!empty.model_choice);
        assert!(!empty.effort_choice);
        assert!(!empty.skills);

        let models = vec![ModelInfo {
            id: "mock-1".into(),
            display_name: "Mock 1".into(),
            description: String::new(),
            is_default: true,
            efforts: vec!["low".into(), "high".into()],
            default_effort: Some("low".into()),
            service_tiers: vec![("priority".into(), "Fast".into(), String::new())],
        }];
        let skills = vec![SkillGroup {
            cwd: "/workspace".into(),
            skills: Vec::new(),
            errors: Vec::new(),
        }];
        let capabilities = core_capabilities(&models, Some(&skills));
        assert!(capabilities.model_choice);
        assert!(capabilities.effort_choice);
        assert!(capabilities.service_tiers);
        // An implemented `skills/list` is healthy even with no skills.
        assert!(capabilities.skills);
    }

    #[test]
    fn version_gate_reads_the_user_agent_like_t3() {
        assert!(version_is_new_enough(
            "mock-codex/0.160.0",
            MIN_CODEX_VERSION
        ));
        assert!(version_is_new_enough(
            "codex_cli_rs/0.159.0",
            MIN_CODEX_VERSION
        ));
        assert!(!version_is_new_enough(
            "codex_cli_rs/0.100.0",
            MIN_CODEX_VERSION
        ));
        assert!(version_is_new_enough(
            "something-without-a-slash",
            MIN_CODEX_VERSION
        ));
        assert_eq!(parse_version("codex_cli_rs/0.160.0 (x)"), Some([0, 160, 0]));
    }

    // ------------------------------------------------------------------
    // In-process app-server peer. These exercise the adapter against the
    // same wire methods T3's effect-codex-app-server client tests drive,
    // without needing the mock binary or a Codex account.

    #[cfg(unix)]
    mod peer {
        use std::{
            io::{BufRead, BufReader, Write},
            os::unix::net::UnixStream,
            sync::{
                Arc, Mutex,
                atomic::{AtomicBool, Ordering},
                mpsc::{Receiver, RecvTimeoutError, channel},
            },
            thread,
            time::{Duration, Instant},
        };

        use atrium_core::{AgentEvent, TurnStatus};
        use codex_client::{CodexClient, rpc::Connection};
        use serde_json::{Value, json};

        use super::super::*;
        use crate::contracts::{
            ModelSelection, ProviderOptionSelection, ProviderOptionSelections, ProviderOptionValue,
            ProviderThread, RuntimeMode, ThreadId,
        };
        use crate::providers::adapter::{
            AdapterEvent, AdapterEventSink, AdapterTurnInput, EnsureThreadInput,
            ProviderSessionRuntime, RuntimePolicy, RuntimeRequestResponse, TurnMessage,
        };

        #[derive(Default)]
        struct PeerState {
            requests: Mutex<Vec<Value>>,
            elicitation_response: Mutex<Option<Value>>,
            resume_in_use: AtomicBool,
            elicit: AtomicBool,
        }

        fn send(writer: &mut UnixStream, message: Value) {
            let _ = writeln!(writer, "{message}");
            let _ = writer.flush();
        }

        fn reply(writer: &mut UnixStream, id: &Value, result: Value) {
            send(writer, json!({"id": id, "result": result}));
        }

        fn notify(writer: &mut UnixStream, method: &str, params: Value) {
            send(writer, json!({"method": method, "params": params}));
        }

        fn thread_payload(_with_turns: bool) -> Value {
            json!({
                "thread": {
                    "id": "native-thread",
                    "sessionId": "native-thread",
                    "preview": "hi",
                    "cwd": "/workspace",
                    "model": "mock-1",
                    "modelProvider": "mock",
                    "createdAt": 1,
                    "updatedAt": 2,
                    "recencyAt": 2,
                    "status": {"type": "idle"},
                    "ephemeral": false,
                    "turns": [],
                    "cliVersion": "0.160.0",
                    "projectId": null,
                    "source": "appServer",
                },
                "model": "mock-1",
                "modelProvider": "mock",
                "cwd": "/workspace",
                "approvalPolicy": "on-request",
                "approvalsReviewer": "user",
                "sandbox": {"type": "workspaceWrite"},
            })
        }

        fn finish_turn(writer: &mut UnixStream) {
            let ids = json!({"threadId": "native-thread", "turnId": "native-turn"});
            let mut item = ids.clone();
            item["item"] = json!({
                "type": "agentMessage",
                "id": "native-item",
                "text": "done",
            });
            notify(writer, "item/completed", item);
            let mut turn = ids.clone();
            turn["turn"] = json!({"id": "native-turn", "status": "completed", "items": []});
            notify(writer, "turn/completed", turn);
        }

        fn peer_loop(mut stream: UnixStream, state: Arc<PeerState>) {
            let reader = match stream.try_clone() {
                Ok(reader) => BufReader::new(reader),
                Err(_) => return,
            };
            let mut awaiting_elicitation: Option<Value> = None;
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let id = message.get("id").cloned();
                let method = message["method"].as_str().map(str::to_owned);
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                let Some(id) = id else { continue };
                let Some(method) = method else {
                    // A response to the peer's elicitation request.
                    if awaiting_elicitation.take() == Some(id.clone()) {
                        *state.elicitation_response.lock().unwrap() = Some(message);
                        finish_turn(&mut stream);
                    }
                    continue;
                };
                state.requests.lock().unwrap().push(message.clone());
                match method.as_str() {
                    "initialize" => reply(
                        &mut stream,
                        &id,
                        json!({
                            "userAgent": "codex_cli_rs/0.160.0",
                            "codexHome": "/tmp/codex-home",
                            "platformFamily": "unix",
                            "platformOs": "linux"
                        }),
                    ),
                    "account/read" => reply(
                        &mut stream,
                        &id,
                        json!({"account": null, "requiresOpenaiAuth": false}),
                    ),
                    "skills/list" => reply(
                        &mut stream,
                        &id,
                        json!({"data": [{"cwd": "/workspace", "skills": [], "errors": []}]}),
                    ),
                    "config/read" => reply(&mut stream, &id, json!({"config": {}, "origins": {}})),
                    "model/list" => reply(
                        &mut stream,
                        &id,
                        json!({"data": [{
                            "id": "mock-1", "model": "mock-1", "displayName": "Mock 1",
                            "description": "", "hidden": false, "isDefault": true,
                            "supportedReasoningEfforts": [{"reasoningEffort": "low"}],
                            "defaultReasoningEffort": "low",
                            "serviceTiers": [{"id": "priority", "name": "Fast", "description": ""}]
                        }]}),
                    ),
                    "thread/start" => {
                        reply(&mut stream, &id, thread_payload(false));
                        notify(
                            &mut stream,
                            "thread/started",
                            json!({"thread": thread_payload(false)["thread"]}),
                        );
                    }
                    "thread/read" => reply(&mut stream, &id, thread_payload(true)),
                    "thread/resume" => {
                        if state.resume_in_use.load(Ordering::Relaxed) {
                            send(
                                &mut stream,
                                json!({"id": id, "error": {"code": -32600, "message": "thread native-thread is in use by another process"}}),
                            );
                        } else {
                            reply(&mut stream, &id, thread_payload(true));
                        }
                    }
                    "thread/settings/update" => {
                        reply(&mut stream, &id, json!({}));
                        notify(
                            &mut stream,
                            "thread/settings/updated",
                            json!({
                                "threadId": params["threadId"],
                                "threadSettings": {
                                    "model": params["model"],
                                    "effort": params["effort"],
                                    "serviceTier": params["serviceTier"],
                                    "approvalPolicy": params["approvalPolicy"],
                                }
                            }),
                        );
                    }
                    "thread/name/set" | "thread/unsubscribe" => reply(&mut stream, &id, json!({})),
                    "turn/start" => {
                        reply(&mut stream, &id, json!({"turn": {"id": "native-turn"}}));
                        notify(
                            &mut stream,
                            "turn/started",
                            json!({"threadId": "native-thread", "turn": {"id": "native-turn"}}),
                        );
                        if state.elicit.load(Ordering::Relaxed) {
                            awaiting_elicitation = Some(json!(900));
                            send(
                                &mut stream,
                                json!({
                                    "id": 900,
                                    "method": "mcpServer/elicitation/request",
                                    "params": {
                                        "threadId": "native-thread",
                                        "turnId": "native-turn",
                                        "serverName": "browser",
                                        "mode": "form",
                                        "message": "Allow ChatGPT to use Google Chrome?",
                                        "requestedSchema": {
                                            "type": "object",
                                            "properties": {"approval": {"type": "string", "enum": ["once", "always"]}},
                                            "required": ["approval"]
                                        }
                                    }
                                }),
                            );
                        } else {
                            finish_turn(&mut stream);
                        }
                    }
                    "turn/steer" | "turn/interrupt" => reply(&mut stream, &id, json!({})),
                    _ => reply(&mut stream, &id, json!({})),
                }
            }
        }

        fn start_peer(
            state: Arc<PeerState>,
        ) -> (
            CodexClient,
            futures::channel::mpsc::UnboundedReceiver<Incoming>,
        ) {
            let (local, peer) = UnixStream::pair().expect("socket pair");
            let reader = local.try_clone().expect("clone");
            let (connection, events) = Connection::new(BufReader::new(reader), local);
            thread::spawn(move || peer_loop(peer, state));
            let mut client = CodexClient::over(connection);
            futures::executor::block_on(client.initialize()).expect("initialize");
            (client, events)
        }

        fn policy(mode: RuntimeMode) -> RuntimePolicy {
            RuntimePolicy {
                runtime_mode: mode,
                interaction_mode: Default::default(),
                cwd: Some("/workspace".into()),
                approval_policy: None,
                sandbox_policy: None,
                max_runtime_mode: None,
            }
        }

        fn model(model: &str, effort: Option<&str>, tier: Option<&str>) -> ModelSelection {
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

        fn open(
            state: Arc<PeerState>,
        ) -> (
            Arc<dyn ProviderSessionRuntime>,
            Receiver<AdapterEvent>,
            ProviderThread,
        ) {
            let (client, incoming) = start_peer(state);
            let adapter = CodexAdapter::new(
                "mockcodex",
                CodexAdapterConfig {
                    codex: Default::default(),
                    display_name: None,
                    minimum_version: None,
                },
            );
            let (tx, events) = channel();
            let sink: AdapterEventSink = Arc::new(move |event| {
                let _ = tx.send(event);
            });
            let session = adapter
                .open_with_client(
                    client,
                    incoming,
                    OpenSessionInput {
                        thread_id: ThreadId::new("app-thread"),
                        provider_session_id: "session-1".into(),
                        model_selection: model("mock-1", None, None),
                        runtime_policy: policy(RuntimeMode::Auto),
                        resume_from_session: None,
                        initial_native_thread_id: None,
                        now: "2026-11-24T00:00:00.000Z".into(),
                    },
                    sink,
                )
                .expect("open session");
            let thread = session
                .ensure_thread(EnsureThreadInput {
                    thread_id: ThreadId::new("app-thread"),
                    title: None,
                    model_selection: model("mock-1", None, None),
                    runtime_policy: policy(RuntimeMode::Auto),
                    provider_session_id: None,
                    existing_provider_thread: None,
                    force_new_native_thread: false,
                    now: "2026-11-24T00:00:00.000Z".into(),
                })
                .expect("ensure thread");
            (session, events, thread)
        }

        fn recv_until(
            events: &Receiver<AdapterEvent>,
            deadline: Instant,
            mut predicate: impl FnMut(&AdapterEvent) -> bool,
        ) -> AdapterEvent {
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match events.recv_timeout(left) {
                    Ok(event) if predicate(&event) => return event,
                    Ok(_) => {}
                    Err(RecvTimeoutError::Timeout) => panic!("event did not arrive"),
                    Err(RecvTimeoutError::Disconnected) => panic!("event channel closed"),
                }
            }
        }

        fn wait_for(state: &PeerState, method: &str) -> Value {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let Some(request) = state
                    .requests
                    .lock()
                    .unwrap()
                    .iter()
                    .rev()
                    .find(|request| request["method"] == method)
                {
                    return request.clone();
                }
                assert!(Instant::now() < deadline, "{method} was never sent");
                thread::sleep(Duration::from_millis(10));
            }
        }

        // Equivalent of effect-codex-app-server client.test.ts,
        // "initializes, handles typed server requests, and reads account and
        // skills data" (T3 Code v0.1.11, MIT), exercised through the adapter.
        #[test]
        fn initializes_reads_skills_and_config_and_reports_capabilities() {
            let (session, _events, _thread) = open(Arc::new(PeerState::default()));
            assert_eq!(session.skills(&["/workspace".into()]).unwrap().len(), 1);
            let config = session.config(Some("/workspace")).unwrap();
            assert!(config.config.is_object());
            assert!(config.origins.is_object());
            assert!(
                session
                    .provider_session()
                    .capabilities
                    .threads
                    .can_read_thread_snapshot
            );
        }

        // Ported from CodexAdapterV2.test.ts's service-tier cases (T3 Code
        // v0.1.11, MIT), extended to the model, effort and approval policy
        // the Atrium adapter also applies to a loaded thread.
        #[test]
        fn applies_mid_thread_settings_to_a_loaded_thread() {
            let state = Arc::new(PeerState::default());
            let (session, events, thread) = open(state.clone());
            let selection = model("mock-1-pro", Some("high"), Some("priority"));
            session
                .apply_model_selection(&thread, &selection, Some(&policy(RuntimeMode::Auto)))
                .expect("apply settings");
            let request = wait_for(&state, "thread/settings/update");
            assert_eq!(request["params"]["model"], "mock-1-pro");
            assert_eq!(request["params"]["effort"], "high");
            assert_eq!(request["params"]["serviceTier"], "priority");
            assert_eq!(request["params"]["approvalPolicy"], "on-request");
            recv_until(&events, Instant::now() + Duration::from_secs(2), |event| {
                matches!(event, AdapterEvent::ProviderSessionUpdated { provider_session, .. }
                    if provider_session.model.as_deref() == Some("mock-1-pro"))
            });
        }

        // read-vs-write: `thread/read` succeeds while another writer holds
        // the lock; `thread/resume` reports the thread in use.
        #[test]
        fn read_snapshot_does_not_take_the_writer_lock_but_resume_reports_in_use() {
            let state = Arc::new(PeerState::default());
            let (session, _events, thread) = open(state.clone());
            state.resume_in_use.store(true, Ordering::Relaxed);
            let snapshot = session
                .read_thread_snapshot(&thread)
                .expect("thread/read does not need the writer");
            assert!(snapshot.messages.is_empty());
            let error = session
                .resume_thread(
                    &thread,
                    Some(&model("mock-1", None, None)),
                    Some(&policy(RuntimeMode::Auto)),
                    "2026-11-24T00:00:00.000Z",
                )
                .expect_err("thread/resume is the write path");
            assert!(
                matches!(error, ProviderAdapterError::ThreadInUse { .. }),
                "{error}"
            );
        }

        // Ported from CodexAdapterV2.test.ts's MCP approval cases (T3 Code
        // v0.1.11, MIT): outside full access the request becomes a runtime
        // request, and the answer goes back to Codex.
        #[test]
        fn maps_mcp_elicitation_to_a_runtime_request_and_answers_it() {
            let state = Arc::new(PeerState::default());
            state.elicit.store(true, Ordering::Relaxed);
            let (session, events, thread) = open(state.clone());
            session
                .start_turn(AdapterTurnInput {
                    thread_id: ThreadId::new("app-thread"),
                    run_id: "run-1".into(),
                    run_ordinal: 1,
                    provider_turn_ordinal: 1,
                    attempt_id: "attempt-1".into(),
                    root_node_id: "node-1".into(),
                    provider_thread: thread.clone(),
                    message: TurnMessage::user("message-1", "please"),
                    model_selection: model("mock-1", None, None),
                    runtime_policy: policy(RuntimeMode::ApprovalRequired),
                    app_context: Vec::new(),
                    now: "2026-11-24T00:00:00.000Z".into(),
                })
                .expect("start turn");
            let request = recv_until(&events, Instant::now() + Duration::from_secs(2), |event| {
                matches!(event, AdapterEvent::RuntimeRequestUpdated { runtime_request, .. }
                    if runtime_request.kind == crate::contracts::RuntimeRequestKind::McpElicitation)
            });
            let AdapterEvent::RuntimeRequestUpdated {
                runtime_request, ..
            } = request
            else {
                unreachable!()
            };
            session
                .respond_to_runtime_request(RuntimeRequestResponse {
                    request_id: runtime_request.id.to_string(),
                    decision: Some(ProviderApprovalDecision::Accept),
                    feedback: None,
                    answers: None,
                })
                .expect("respond");
            let deadline = Instant::now() + Duration::from_secs(2);
            let answered = loop {
                if let Some(answer) = state.elicitation_response.lock().unwrap().clone() {
                    break answer;
                }
                assert!(
                    Instant::now() < deadline,
                    "the peer saw no answer to the elicitation"
                );
                thread::sleep(Duration::from_millis(10));
            };
            assert_eq!(
                answered["result"],
                json!({"action": "accept", "content": {"approval": "once"}})
            );
            recv_until(&events, Instant::now() + Duration::from_secs(2), |event| {
                matches!(event, AdapterEvent::Agent(AgentEvent::TurnCompleted { status, .. })
                    if *status == TurnStatus::Completed)
            });
        }

        #[test]
        fn closes_cleanly() {
            let (session, _events, _thread) = open(Arc::new(PeerState::default()));
            session.close();
        }
    }
}
