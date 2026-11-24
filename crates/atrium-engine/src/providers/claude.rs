// Ported from apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts
// (T3 Code v0.1.11, MIT), on top of the `claude-client` crate which
// implements the Agent SDK's side of the CLI protocol. The capability record
// is `ClaudeProviderCapabilitiesV2`; the runtime-policy mapping is
// `permissionModeForClaudeRuntimePolicy` / `claudeRuntimeQueryPolicyForRuntimePolicy`;
// turn translation lives in [`super::claude_translate`].
//!
//! The Claude adapter: one `claude` process per native session, the session
//! started or resumed by its CLI session id, turns sent with text and images,
//! permissions answered on the control channel, model/effort applied per
//! turn, and the CLI's stream-JSON translated into the app's events.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, AtomicU64, Ordering},
    },
    thread,
};

use atrium_core::{AgentEvent, Capabilities};
use base64::Engine as _;
use claude_client::{
    ClaudeClient, ClaudeConfig, ControlRequestEnvelope, Incoming, PermissionDecision,
    PermissionRequest, SessionInfo, Tools, TurnOptions, UserContentBlock,
    events::{Effort, PermissionMode, SessionInit, TurnStatus as ClaudeTurnStatus},
};
use futures::StreamExt;
use serde_json::{Value, json};

use super::adapter::{
    AdapterEvent, AdapterEventSink, AdapterTurnInput, EnsureThreadInput, OpenSessionInput,
    ProviderAdapter, ProviderAdapterError, ProviderConfig, ProviderSessionRuntime, RuntimePolicy,
    RuntimeRequestResponse, SelectionTransitionPlan, SkillGroup, SteerInput, ThreadSnapshot,
    TurnMessage,
};
use super::claude_translate::{
    CLAUDE_DRIVER_KIND, ClaudeTranslator, core_turn_status, terminal_status_wire,
};
use crate::contracts::{
    ApprovalCapabilities, CheckpointCapabilities, ContextCapabilities, IdentityCapabilities,
    NativeRefStrength, NodeId, PlanningCapabilities, ProviderApprovalDecision,
    ProviderCapabilities, ProviderDriverKind, ProviderInteractionMode, ProviderRef,
    ProviderSession, ProviderSessionId, ProviderSessionStatus, ProviderThread, ProviderThreadId,
    ProviderThreadStatus, ProviderTurn, ProviderTurnId, ProviderTurnStatus, ResponseCapability,
    RuntimeMode, RuntimeRequest, RuntimeRequestId, RuntimeRequestKind, RuntimeRequestStatus,
    SessionCapabilities, StreamingCapabilities, SubagentCapabilities, TerminalStatusQuality,
    ThreadCapabilities, ThreadId, ToolCapabilities, TurnCapabilities,
};

/// T3's `ClaudeProviderCapabilitiesV2`.
pub fn claude_provider_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        sessions: SessionCapabilities {
            supports_multiple_provider_threads_per_session: false,
            supports_model_switch_in_session: true,
            supports_provider_switching_via_handoff: true,
            supports_runtime_mode_switch_in_session: false,
            pending_requests_survive_restart: false,
        },
        threads: ThreadCapabilities {
            can_create_empty_thread: true,
            can_read_thread_snapshot: false,
            can_rollback_thread: true,
            can_fork_thread: true,
            can_fork_from_turn: true,
            can_fork_while_source_running: Some(true),
            can_fork_from_subagent_thread: false,
            exposes_native_thread_id: true,
        },
        turns: TurnCapabilities {
            supports_native_goals: None,
            exposes_native_turn_id: false,
            emits_turn_started: true,
            emits_turn_completed: true,
            supports_interrupt: true,
            supports_native_resume: Some(true),
            supports_active_steering: true,
            active_steering_interrupts_tools: Some(false),
            active_steering_accepts_attachments: None,
            active_steering_during_compaction: None,
            supports_steering_by_interrupt_restart: false,
            supports_queued_messages: true,
            terminal_status_quality: TerminalStatusQuality::Strong,
        },
        streaming: StreamingCapabilities {
            streams_assistant_text: true,
            streams_reasoning: true,
            streams_tool_output: false,
            streams_plan_text: false,
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
            supports_apply_patch_approval: false,
            approvals_have_native_request_ids: true,
            approval_callbacks_are_live_only: true,
            approvals_can_originate_from_subagents: false,
        },
        planning: PlanningCapabilities {
            emits_plan_updated: true,
            emits_todo_list: true,
            emits_proposed_plan: true,
            supports_structured_questions: true,
            plan_deltas_have_item_ids: false,
        },
        subagents: SubagentCapabilities {
            supports_subagents: true,
            exposes_subagent_thread_ids: false,
            emits_subagent_lifecycle: true,
            can_wait_for_subagents: false,
            can_close_subagents: false,
            can_fork_subagent_thread: false,
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
            provider_can_read_conversation_snapshot: false,
        },
        identity: IdentityCapabilities {
            native_thread_ids: NativeRefStrength::Strong,
            native_turn_ids: NativeRefStrength::Weak,
            native_item_ids: NativeRefStrength::Strong,
            native_request_ids: NativeRefStrength::Strong,
        },
        runtime_policy: crate::contracts::RuntimePolicyCapabilities {
            enforcement: crate::contracts::RuntimePolicyEnforcement::Native,
        },
    }
}

/// The app's narrower [`Capabilities`], derived from what `initialize` and
/// `system/init` actually advertised. T3's capability record is static; the
/// UI needs to know which controls exist for this account.
pub fn core_capabilities(info: Option<&SessionInfo>, init: Option<&SessionInit>) -> Capabilities {
    let mut capabilities = Capabilities::claude();
    if let Some(info) = info {
        capabilities.model_choice = !info.models.is_empty();
        capabilities.effort_choice = info.models.iter().any(|model| {
            model.supports_effort == Some(true) || !model.supported_effort_levels.is_empty()
        });
        capabilities.slash_commands = !info.commands.is_empty() || capabilities.slash_commands;
    }
    if let Some(init) = init {
        capabilities.slash_commands =
            !init.slash_commands.is_empty() || capabilities.slash_commands;
        capabilities.skills = !init.skills.is_empty();
        capabilities.subagents = !init.agents.is_empty() || capabilities.subagents;
        // `system/init` carries the MCP status list, even when it is empty.
        capabilities.mcp_status = true;
    }
    capabilities
}

/// How to construct a Claude adapter. The transport config is Atrium's.
#[derive(Clone, Debug, Default)]
pub struct ClaudeAdapterConfig {
    pub claude: ClaudeConfig,
    pub display_name: Option<String>,
}

impl ClaudeAdapterConfig {
    pub fn for_binary(binary: impl Into<PathBuf>) -> Self {
        Self {
            claude: ClaudeConfig {
                binary: binary.into(),
                ..ClaudeConfig::default()
            },
            display_name: Some("Claude Code".into()),
        }
    }
}

/// One configured Claude provider instance.
pub struct ClaudeAdapter {
    instance_id: crate::contracts::ProviderInstanceId,
    config: ClaudeAdapterConfig,
    capabilities: ProviderCapabilities,
}

impl ClaudeAdapter {
    pub fn new(instance_id: impl Into<String>, config: ClaudeAdapterConfig) -> Self {
        Self {
            instance_id: crate::contracts::ProviderInstanceId::new(instance_id),
            config,
            capabilities: claude_provider_capabilities(),
        }
    }

    /// Open a session on a client the caller already owns.
    pub fn open_with_client(
        &self,
        client: ClaudeClient,
        events: futures::channel::mpsc::UnboundedReceiver<Incoming>,
        input: OpenSessionInput,
        sink: AdapterEventSink,
    ) -> Result<Arc<dyn ProviderSessionRuntime>, ProviderAdapterError> {
        Ok(Arc::new(ClaudeSessionRuntime::new(
            self.instance_id.clone(),
            self.capabilities.clone(),
            Some(client),
            Some(events),
            input,
            sink,
            self.config.clone(),
        )))
    }
}

impl ProviderAdapter for ClaudeAdapter {
    fn instance_id(&self) -> &crate::contracts::ProviderInstanceId {
        &self.instance_id
    }

    fn driver(&self) -> &str {
        CLAUDE_DRIVER_KIND
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }

    fn plan_selection_transition(
        &self,
        _current: &crate::contracts::ModelSelection,
        _target: &crate::contracts::ModelSelection,
    ) -> SelectionTransitionPlan {
        // Claude applies model and effort per turn (`set_model` plus the
        // prompt's effort prefix), as T3 does.
        SelectionTransitionPlan::ApplyOnNextTurn
    }

    fn open_session(
        &self,
        input: OpenSessionInput,
        sink: AdapterEventSink,
    ) -> Result<Arc<dyn ProviderSessionRuntime>, ProviderAdapterError> {
        let session = ClaudeSessionRuntime::new(
            self.instance_id.clone(),
            self.capabilities.clone(),
            None,
            None,
            input,
            sink,
            self.config.clone(),
        );
        Ok(Arc::new(session))
    }
}

#[derive(Clone)]
enum Pending {
    Permission(Box<PermissionRequest>),
    Control(ControlRequestEnvelope),
}

struct ActiveTurn {
    provider_turn: ProviderTurn,
}

struct Inner {
    driver: String,
    instance_id: crate::contracts::ProviderInstanceId,
    provider_session_id: ProviderSessionId,
    provider_session: Mutex<ProviderSession>,
    sink: AdapterEventSink,
    config: Mutex<ClaudeAdapterConfig>,
    runtime_policy: Mutex<RuntimePolicy>,
    model: Mutex<Option<String>>,
    effort: Mutex<Option<Effort>>,
    client: Mutex<Option<ClaudeClient>>,
    translator: Mutex<ClaudeTranslator>,
    pending: Mutex<HashMap<String, Pending>>,
    active_turn: Mutex<Option<ActiveTurn>>,
    threads: Mutex<HashMap<String, ProviderThread>>,
    core_capabilities: Mutex<Capabilities>,
    now: Mutex<String>,
    now_secs: AtomicI64,
    next_ordinal: AtomicU64,
}

impl Inner {
    fn emit(&self, event: AdapterEvent) {
        (self.sink)(event);
    }

    fn current_now(&self) -> String {
        self.now.lock().expect("claude now").clone()
    }

    fn set_now(&self, now: &str) {
        *self.now.lock().expect("claude now") = now.to_owned();
        self.now_secs.store(epoch_seconds(now), Ordering::Relaxed);
    }

    fn update_thread(&self, thread: ProviderThread) -> ProviderThread {
        if let Some(native) = thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.clone())
        {
            self.threads
                .lock()
                .expect("claude threads")
                .insert(native, thread.clone());
        }
        self.emit(AdapterEvent::ProviderThreadUpdated {
            driver: self.driver.clone(),
            provider_thread: thread.clone(),
            pending_background_work: None,
        });
        thread
    }

    fn set_model(&self, model: Option<String>) {
        *self.model.lock().expect("claude model") = model.filter(|model| !model.is_empty());
    }

    fn model(&self) -> Option<String> {
        self.model.lock().expect("claude model").clone()
    }

    fn spawn(
        self: &Arc<Self>,
        native: &str,
        resume: bool,
    ) -> Result<ClaudeClient, ProviderAdapterError> {
        // One native session per process: retiring the old process before a
        // resume keeps orphaned CLIs from lingering.
        if let Some(previous) = self.client.lock().expect("claude client").take() {
            previous.close();
        }
        let mut config = self.config.lock().expect("claude config").clone();
        let policy = self.runtime_policy.lock().expect("claude policy").clone();
        let query = QueryPolicy::for_runtime_policy(&policy);
        config.claude.cwd = policy.cwd.clone().map(PathBuf::from);
        config.claude.launch.model = self.model();
        config.claude.launch.effort = *self.effort.lock().expect("claude effort");
        config.claude.launch.permission_mode = Some(query.permission_mode);
        config.claude.launch.allow_dangerously_skip_permissions =
            query.permission_mode == PermissionMode::BypassPermissions;
        config.claude.launch.tools = Some(query.tools);
        config.claude.launch.allowed_tools = query.allowed_tools;
        config.claude.launch.permission_prompt_tool_stdio = query.install_permission_callback;
        config.claude.launch.include_partial_messages = true;
        if resume {
            config.claude.launch.resume = Some(native.to_owned());
            config.claude.launch.session_id = None;
        } else {
            config.claude.launch.session_id = Some(native.to_owned());
            config.claude.launch.resume = None;
        }

        let (client, incoming) = futures::executor::block_on(ClaudeClient::start(config.claude))
            .map_err(|error| {
                let text = error.to_string();
                log::warn!("Claude CLI did not start: {text}");
                if resume && text.contains("No conversation found") {
                    ProviderAdapterError::ResumeThread {
                        driver: CLAUDE_DRIVER_KIND.into(),
                        provider_thread_id: native.to_owned(),
                    }
                } else {
                    ProviderAdapterError::OpenSession {
                        driver: CLAUDE_DRIVER_KIND.into(),
                        provider_session_id: native.to_owned(),
                    }
                }
            })?;
        *self.client.lock().expect("claude client") = Some(client.clone());

        let capabilities = core_capabilities(
            client.session_info().as_ref(),
            client.system_init().as_ref(),
        );
        *self.core_capabilities.lock().expect("claude capabilities") = capabilities.clone();
        {
            let mut session = self.provider_session.lock().expect("claude session");
            session.status = ProviderSessionStatus::Ready;
            session.model = self.model();
            session.updated_at = self.current_now();
            let event = AdapterEvent::ProviderSessionUpdated {
                driver: self.driver.clone(),
                provider_session: session.clone(),
            };
            drop(session);
            self.emit(event);
        }
        if let Some(info) = client.session_info() {
            let translator = self.translator.lock().expect("claude translator");
            let event = translator.commands(Some(&info), &SessionInit::default());
            drop(translator);
            self.emit(AdapterEvent::Agent(event));
        }

        let inner = Arc::clone(self);
        let pump_client = client.clone();
        thread::Builder::new()
            .name("claude-adapter-events".into())
            .spawn(move || {
                futures::executor::block_on(async move {
                    let mut incoming = incoming;
                    while let Some(frame) = incoming.next().await {
                        match frame {
                            Incoming::Event(event) => inner.on_event(event),
                            Incoming::Permission(request) => inner.on_permission(request),
                            Incoming::Control(envelope) => inner.on_control(envelope),
                            Incoming::Frame(value) => log::debug!("claude frame: {value}"),
                            Incoming::Malformed { line, error } => {
                                log::warn!("claude sent a malformed line ({error}): {line}")
                            }
                            Incoming::Closed => {
                                let _ = pump_client.is_closed();
                                inner.emit(AdapterEvent::Agent(AgentEvent::Disconnected {
                                    reason: "the Claude process exited".into(),
                                }));
                                break;
                            }
                        }
                    }
                });
            })
            .expect("spawn claude adapter event pump");
        Ok(client)
    }

    fn on_event(&self, event: claude_client::ClaudeEvent) {
        let now = self.now_secs.load(Ordering::Relaxed);
        let events = {
            let mut translator = self.translator.lock().expect("claude translator");
            translator.on_event(&event, now)
        };
        if let claude_client::ClaudeEvent::SessionInit(init) = &event {
            let info = self
                .client
                .lock()
                .expect("claude client")
                .as_ref()
                .and_then(|client| client.session_info());
            let capabilities = core_capabilities(info.as_ref(), Some(init));
            *self.core_capabilities.lock().expect("claude capabilities") = capabilities;
        }
        for event in events {
            self.emit(AdapterEvent::Agent(event));
        }
        if let claude_client::ClaudeEvent::Result(result) = &event {
            self.finish_turn(result.terminal_status());
        }
    }

    fn finish_turn(&self, status: ClaudeTurnStatus) {
        let now = self.current_now();
        let Some(active) = self.active_turn.lock().expect("claude turns").take() else {
            return;
        };
        let mut turn = active.provider_turn;
        turn.status = match core_turn_status(status) {
            atrium_core::TurnStatus::Completed => ProviderTurnStatus::Completed,
            atrium_core::TurnStatus::Interrupted => ProviderTurnStatus::Interrupted,
            atrium_core::TurnStatus::Failed => ProviderTurnStatus::Failed,
            atrium_core::TurnStatus::InProgress => ProviderTurnStatus::Running,
        };
        turn.completed_at = Some(now);
        let provider_thread_id = turn.provider_thread_id.to_string();
        let provider_turn_id = turn.id.to_string();
        let run_ordinal = turn.ordinal;
        self.emit(AdapterEvent::ProviderTurnUpdated {
            driver: self.driver.clone(),
            thread_id: None,
            provider_turn: turn,
        });
        self.emit(AdapterEvent::TurnTerminal {
            driver: self.driver.clone(),
            provider_thread_id,
            provider_turn_id,
            run_ordinal,
            status: terminal_status_wire(status).into(),
        });
    }

    fn on_permission(&self, request: PermissionRequest) {
        let (approval, request_kind, turn_id) = {
            let mut translator = self.translator.lock().expect("claude translator");
            let (approval, kind) = translator.permission(&request);
            let turn_id = translator.current_turn().map(ProviderTurnId::new);
            (approval, kind, turn_id)
        };
        let id = request.request_id.clone();
        self.pending
            .lock()
            .expect("claude pending")
            .insert(id.clone(), Pending::Permission(Box::new(request)));
        self.emit(AdapterEvent::Agent(approval));
        self.publish_runtime_request(&id, runtime_request_kind(request_kind), turn_id);
    }

    fn on_control(&self, envelope: ControlRequestEnvelope) {
        let id = envelope.request_id.clone();
        let subtype = envelope.subtype().to_owned();
        let detail = serde_json::to_string(&envelope.request.fields).unwrap_or_default();
        let turn_id = self
            .translator
            .lock()
            .expect("claude translator")
            .current_turn()
            .map(ProviderTurnId::new);
        self.pending
            .lock()
            .expect("claude pending")
            .insert(id.clone(), Pending::Control(envelope));
        let approval = AgentEvent::ApprovalRequested(atrium_core::ApprovalRequest {
            request_id: Value::String(id.clone()),
            thread_id: self
                .translator
                .lock()
                .expect("claude translator")
                .thread_id()
                .to_owned(),
            turn_id: turn_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            item_id: id.clone(),
            kind: atrium_core::ApprovalKind::Tool {
                title: subtype,
                detail: Some(detail).filter(|detail| !detail.is_empty()),
            },
        });
        self.emit(AdapterEvent::Agent(approval));
        self.publish_runtime_request(&id, RuntimeRequestKind::Permission, turn_id);
    }

    fn publish_runtime_request(
        &self,
        id: &str,
        kind: RuntimeRequestKind,
        turn_id: Option<ProviderTurnId>,
    ) {
        let now = self.current_now();
        let request = RuntimeRequest {
            id: RuntimeRequestId::new(id.to_owned()),
            node_id: NodeId::new(format!("claude-runtime-request-{id}")),
            provider_turn_id: turn_id,
            native_request_ref: Some(ProviderRef {
                driver: ProviderDriverKind::new(CLAUDE_DRIVER_KIND),
                native_id: Some(id.to_owned()),
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
        };
        self.emit(AdapterEvent::RuntimeRequestUpdated {
            driver: self.driver.clone(),
            thread_id: None,
            runtime_request: request,
        });
    }

    fn answer_pending(&self, input: RuntimeRequestResponse) -> Result<(), ProviderAdapterError> {
        let pending = self
            .pending
            .lock()
            .expect("claude pending")
            .remove(&input.request_id)
            .ok_or_else(|| ProviderAdapterError::RuntimeRequestResponse {
                driver: CLAUDE_DRIVER_KIND.into(),
                request_id: input.request_id.clone(),
            })?;
        let decision = input.decision.unwrap_or(ProviderApprovalDecision::Cancel);
        let client = self
            .client
            .lock()
            .expect("claude client")
            .clone()
            .ok_or_else(|| ProviderAdapterError::SessionStopped {
                driver: CLAUDE_DRIVER_KIND.into(),
            })?;
        match pending {
            Pending::Permission(request) => {
                let answer = match decision {
                    ProviderApprovalDecision::Accept
                    | ProviderApprovalDecision::AcceptForSession
                    | ProviderApprovalDecision::AcceptAlways => PermissionDecision::Allow {
                        updated_input: None,
                        updated_permissions: match decision {
                            ProviderApprovalDecision::AcceptForSession
                            | ProviderApprovalDecision::AcceptAlways => {
                                request.permission_suggestions.clone()
                            }
                            _ => Vec::new(),
                        },
                    },
                    ProviderApprovalDecision::Decline => PermissionDecision::Deny {
                        message: "The user declined this tool.".into(),
                        interrupt: false,
                    },
                    ProviderApprovalDecision::Cancel => PermissionDecision::Deny {
                        message: "The user cancelled the turn.".into(),
                        interrupt: true,
                    },
                };
                client
                    .answer_permission(&request.request_id, &answer)
                    .map_err(|_| ProviderAdapterError::RuntimeRequestResponse {
                        driver: CLAUDE_DRIVER_KIND.into(),
                        request_id: input.request_id.clone(),
                    })?;
            }
            Pending::Control(envelope) => {
                let request_id = envelope.request_id.clone();
                let result = match decision {
                    ProviderApprovalDecision::Accept
                    | ProviderApprovalDecision::AcceptForSession
                    | ProviderApprovalDecision::AcceptAlways => {
                        client.answer_control(&request_id, json!({}))
                    }
                    ProviderApprovalDecision::Decline | ProviderApprovalDecision::Cancel => {
                        client.reject_control(&request_id, "The user declined this request.")
                    }
                };
                result.map_err(|_| ProviderAdapterError::RuntimeRequestResponse {
                    driver: CLAUDE_DRIVER_KIND.into(),
                    request_id: input.request_id.clone(),
                })?;
            }
        }
        let thread_id = self
            .translator
            .lock()
            .expect("claude translator")
            .thread_id()
            .to_owned();
        self.emit(AdapterEvent::Agent(AgentEvent::ApprovalResolved {
            thread_id,
            request_id: Value::String(input.request_id),
        }));
        Ok(())
    }

    fn native_thread_id(thread: &ProviderThread) -> Result<String, ProviderAdapterError> {
        thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.clone())
            .ok_or_else(|| {
                ProviderAdapterError::protocol(
                    CLAUDE_DRIVER_KIND,
                    format!(
                        "Provider thread {} is missing a native Claude session id",
                        thread.id
                    ),
                )
            })
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
            driver: ProviderDriverKind::new(CLAUDE_DRIVER_KIND),
            provider_instance_id: self.instance_id.clone(),
            provider_session_id: Some(self.provider_session_id.clone()),
            app_thread_id: Some(app_thread),
            owner_node_id: None,
            native_thread_ref: Some(ProviderRef {
                driver: ProviderDriverKind::new(CLAUDE_DRIVER_KIND),
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
            created_at: now.to_owned(),
            updated_at: now.to_owned(),
        }
    }
}

/// A live Claude provider session.
pub struct ClaudeSessionRuntime {
    inner: Arc<Inner>,
}

impl ClaudeSessionRuntime {
    #[allow(clippy::too_many_arguments)]
    fn new(
        instance_id: crate::contracts::ProviderInstanceId,
        capabilities: ProviderCapabilities,
        client: Option<ClaudeClient>,
        events: Option<futures::channel::mpsc::UnboundedReceiver<Incoming>>,
        input: OpenSessionInput,
        sink: AdapterEventSink,
        config: ClaudeAdapterConfig,
    ) -> Self {
        let cwd = input
            .runtime_policy
            .cwd
            .clone()
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|path| path.display().to_string())
            })
            .unwrap_or_default();
        let model = Some(input.model_selection.model.clone()).filter(|model| !model.is_empty());
        let translator = ClaudeTranslator::new(CLAUDE_DRIVER_KIND, String::new());
        let session = ProviderSession {
            id: input.provider_session_id.clone(),
            driver: ProviderDriverKind::new(CLAUDE_DRIVER_KIND),
            provider_instance_id: instance_id.clone(),
            status: ProviderSessionStatus::Ready,
            cwd,
            model: model.clone(),
            capabilities,
            extension_statuses: Vec::new(),
            created_at: input.now.clone(),
            updated_at: input.now.clone(),
            last_error: None,
        };
        let inner = Arc::new(Inner {
            driver: CLAUDE_DRIVER_KIND.into(),
            instance_id,
            provider_session_id: input.provider_session_id.clone(),
            provider_session: Mutex::new(session.clone()),
            sink: sink.clone(),
            config: Mutex::new(config),
            runtime_policy: Mutex::new(input.runtime_policy),
            model: Mutex::new(model),
            effort: Mutex::new(effort_of(&input.model_selection)),
            client: Mutex::new(client.clone()),
            translator: Mutex::new(translator),
            pending: Mutex::new(HashMap::new()),
            active_turn: Mutex::new(None),
            threads: Mutex::new(HashMap::new()),
            core_capabilities: Mutex::new(Capabilities::claude()),
            now: Mutex::new(input.now),
            now_secs: AtomicI64::new(0),
            next_ordinal: AtomicU64::new(0),
        });
        if let (Some(client), Some(mut incoming)) = (client, events) {
            if let Some(id) = client.session_id() {
                *inner.translator.lock().expect("claude translator") =
                    ClaudeTranslator::new(CLAUDE_DRIVER_KIND, id);
            }
            let capabilities = core_capabilities(
                client.session_info().as_ref(),
                client.system_init().as_ref(),
            );
            *inner.core_capabilities.lock().expect("claude capabilities") = capabilities;
            let inner_for_pump = Arc::clone(&inner);
            thread::Builder::new()
                .name("claude-adapter-events".into())
                .spawn(move || {
                    futures::executor::block_on(async move {
                        while let Some(frame) = incoming.next().await {
                            match frame {
                                Incoming::Event(event) => inner_for_pump.on_event(event),
                                Incoming::Permission(request) => {
                                    inner_for_pump.on_permission(request)
                                }
                                Incoming::Control(envelope) => inner_for_pump.on_control(envelope),
                                Incoming::Frame(value) => log::debug!("claude frame: {value}"),
                                Incoming::Malformed { line, error } => {
                                    log::warn!("claude sent a malformed line ({error}): {line}")
                                }
                                Incoming::Closed => {
                                    inner_for_pump.emit(AdapterEvent::Agent(
                                        AgentEvent::Disconnected {
                                            reason: "the Claude process exited".into(),
                                        },
                                    ));
                                    break;
                                }
                            }
                        }
                    });
                })
                .expect("spawn claude adapter event pump");
        }
        (sink)(AdapterEvent::ProviderSessionUpdated {
            driver: CLAUDE_DRIVER_KIND.into(),
            provider_session: session,
        });
        Self { inner }
    }

    pub fn core_capabilities(&self) -> Capabilities {
        self.inner
            .core_capabilities
            .lock()
            .expect("claude capabilities")
            .clone()
    }

    fn ensure_client(
        &self,
        provider_thread: &ProviderThread,
    ) -> Result<String, ProviderAdapterError> {
        let native = Inner::native_thread_id(provider_thread)?;
        let alive = self
            .inner
            .client
            .lock()
            .expect("claude client")
            .as_ref()
            .is_some_and(|client| {
                !client.is_closed()
                    && client
                        .session_id()
                        .as_deref()
                        .is_none_or(|id| id == native.as_str())
            });
        if !alive {
            self.inner.spawn(&native, true)?;
            *self.inner.translator.lock().expect("claude translator") =
                ClaudeTranslator::new(CLAUDE_DRIVER_KIND, native.clone());
        }
        Ok(native)
    }

    fn send(
        &self,
        message: &TurnMessage,
        options: &TurnOptions,
    ) -> Result<(), ProviderAdapterError> {
        let client = self
            .inner
            .client
            .lock()
            .expect("claude client")
            .clone()
            .ok_or_else(|| ProviderAdapterError::SessionStopped {
                driver: CLAUDE_DRIVER_KIND.into(),
            })?;
        let content = user_content(&message.text, &message.image_paths)?;
        client
            .send_turn(content, options)
            .map_err(ProviderAdapterError::failed)
    }

    fn turn_id(ordinal: u32) -> ProviderTurnId {
        ProviderTurnId::new(format!("claude-turn-{ordinal}"))
    }
}

impl ProviderSessionRuntime for ClaudeSessionRuntime {
    fn provider_session(&self) -> ProviderSession {
        self.inner
            .provider_session
            .lock()
            .expect("claude session")
            .clone()
    }

    fn driver(&self) -> &str {
        CLAUDE_DRIVER_KIND
    }

    fn ensure_thread(
        &self,
        input: EnsureThreadInput,
    ) -> Result<ProviderThread, ProviderAdapterError> {
        *self.inner.runtime_policy.lock().expect("claude policy") = input.runtime_policy.clone();
        self.inner.set_now(&input.now);
        self.inner
            .set_model(Some(input.model_selection.model.clone()));
        *self.inner.effort.lock().expect("claude effort") = effort_of(&input.model_selection);
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
        let native = input
            .existing_provider_thread
            .as_ref()
            .and_then(|thread| thread.native_thread_ref.as_ref())
            .and_then(|native| native.native_id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        self.inner.spawn(&native, false)?;
        {
            let mut translator = self.inner.translator.lock().expect("claude translator");
            let mut fresh = ClaudeTranslator::new(CLAUDE_DRIVER_KIND, native.clone());
            fresh.set_title(input.title.clone());
            *translator = fresh;
        }
        let thread = self.inner.new_provider_thread(
            input.thread_id,
            native,
            &input.now,
            ProviderThreadStatus::Active,
        );
        Ok(self.inner.update_thread(thread))
    }

    fn resume_thread(
        &self,
        provider_thread: &ProviderThread,
        model_selection: Option<&crate::contracts::ModelSelection>,
        runtime_policy: Option<&RuntimePolicy>,
        now: &str,
    ) -> Result<ProviderThread, ProviderAdapterError> {
        let native = Inner::native_thread_id(provider_thread)?;
        if let Some(policy) = runtime_policy {
            *self.inner.runtime_policy.lock().expect("claude policy") = policy.clone();
        }
        self.inner.set_now(now);
        if let Some(selection) = model_selection {
            self.inner.set_model(Some(selection.model.clone()));
            *self.inner.effort.lock().expect("claude effort") = effort_of(selection);
        }
        let alive = self
            .inner
            .client
            .lock()
            .expect("claude client")
            .as_ref()
            .is_some_and(|client| {
                !client.is_closed() && client.session_id().as_deref() == Some(native.as_str())
            });
        if !alive {
            self.inner.spawn(&native, true)?;
            *self.inner.translator.lock().expect("claude translator") =
                ClaudeTranslator::new(CLAUDE_DRIVER_KIND, native.clone());
        }
        let mut thread = provider_thread.clone();
        thread.status = ProviderThreadStatus::Active;
        thread.provider_session_id = Some(self.inner.provider_session_id.clone());
        thread.updated_at = now.to_owned();
        Ok(self.inner.update_thread(thread))
    }

    fn start_turn(&self, input: AdapterTurnInput) -> Result<ProviderTurn, ProviderAdapterError> {
        let native = self.ensure_client(&input.provider_thread)?;
        self.inner.set_now(&input.now);
        *self.inner.runtime_policy.lock().expect("claude policy") = input.runtime_policy.clone();
        self.inner
            .set_model(Some(input.model_selection.model.clone()));
        *self.inner.effort.lock().expect("claude effort") = effort_of(&input.model_selection);

        let ordinal = if input.provider_turn_ordinal > 0 {
            input.provider_turn_ordinal
        } else {
            self.inner.next_ordinal.fetch_add(1, Ordering::Relaxed) as u32 + 1
        };
        let turn_id = Self::turn_id(ordinal);
        let model = self.inner.model();
        let turn_started = {
            let mut translator = self.inner.translator.lock().expect("claude translator");
            let event = translator.begin_turn(turn_id.to_string(), model.clone());
            let user = translator.user_item(
                turn_id.as_str(),
                input.message.message_id.as_str(),
                &input.message.text,
                input
                    .message
                    .attachments
                    .iter()
                    .map(|attachment| format!("{attachment:?}"))
                    .collect(),
            );
            (event, user)
        };
        self.inner.emit(AdapterEvent::Agent(turn_started.0));
        self.inner.emit(AdapterEvent::Agent(turn_started.1));

        let options = TurnOptions {
            model,
            effort: *self.inner.effort.lock().expect("claude effort"),
            uuid: Some(input.message.message_id.to_string()),
            client_composed: false,
        };
        let provider_turn = ProviderTurn {
            id: turn_id,
            provider_thread_id: input.provider_thread.id.clone(),
            node_id: input.root_node_id.clone(),
            run_attempt_id: Some(input.attempt_id.clone()),
            native_turn_ref: None,
            native_thread_ref: None,
            ordinal,
            status: ProviderTurnStatus::Running,
            started_at: Some(input.now.clone()),
            completed_at: None,
            token_usage: None,
            turn_token_usage: None,
            model: Some(input.model_selection.model.clone()).filter(|model| !model.is_empty()),
        };
        self.inner
            .active_turn
            .lock()
            .expect("claude turns")
            .replace(ActiveTurn {
                provider_turn: provider_turn.clone(),
            });
        self.inner.emit(AdapterEvent::ProviderTurnUpdated {
            driver: CLAUDE_DRIVER_KIND.into(),
            thread_id: Some(input.thread_id.clone()),
            provider_turn: provider_turn.clone(),
        });
        if let Err(error) = self.send(&input.message, &options) {
            self.inner.finish_turn(ClaudeTurnStatus::Failed);
            self.inner
                .emit(AdapterEvent::Agent(AgentEvent::TurnCompleted {
                    thread_id: native,
                    turn_id: provider_turn.id.to_string(),
                    status: atrium_core::TurnStatus::Failed,
                    error: Some(error.to_string()),
                    duration_ms: None,
                }));
            return Err(error);
        }
        Ok(provider_turn)
    }

    fn steer_turn(&self, input: SteerInput) -> Result<(), ProviderAdapterError> {
        self.inner.set_now(&input.now);
        if let Some(selection) = &input.model_selection {
            self.inner.set_model(Some(selection.model.clone()));
            *self.inner.effort.lock().expect("claude effort") = effort_of(selection);
        }
        {
            let translator = self.inner.translator.lock().expect("claude translator");
            let item = translator.user_item(
                input.provider_turn_id.as_str(),
                input.message.message_id.as_str(),
                &input.message.text,
                Vec::new(),
            );
            drop(translator);
            self.inner.emit(AdapterEvent::Agent(item));
        }
        let options = TurnOptions {
            model: self.inner.model(),
            effort: *self.inner.effort.lock().expect("claude effort"),
            uuid: Some(input.message.message_id.to_string()),
            client_composed: false,
        };
        self.send(&input.message, &options)?;
        Ok(())
    }

    fn interrupt_turn(
        &self,
        _provider_thread: &ProviderThread,
        _provider_turn_id: &ProviderTurnId,
    ) -> Result<(), ProviderAdapterError> {
        let client = self
            .inner
            .client
            .lock()
            .expect("claude client")
            .clone()
            .ok_or_else(|| ProviderAdapterError::SessionStopped {
                driver: CLAUDE_DRIVER_KIND.into(),
            })?;
        client.interrupt().map_err(|error| {
            ProviderAdapterError::failed(format!("Claude interrupt failed: {error}"))
        })
    }

    fn apply_model_selection(
        &self,
        _provider_thread: &ProviderThread,
        model_selection: &crate::contracts::ModelSelection,
        runtime_policy: Option<&RuntimePolicy>,
    ) -> Result<(), ProviderAdapterError> {
        if let Some(policy) = runtime_policy {
            *self.inner.runtime_policy.lock().expect("claude policy") = policy.clone();
        }
        self.inner.set_model(Some(model_selection.model.clone()));
        *self.inner.effort.lock().expect("claude effort") = effort_of(model_selection);
        let policy = self
            .inner
            .runtime_policy
            .lock()
            .expect("claude policy")
            .clone();
        let query = QueryPolicy::for_runtime_policy(&policy);
        let client = self.inner.client.lock().expect("claude client").clone();
        if let Some(client) = client {
            client
                .set_model(Some(model_selection.model.as_str()))
                .map_err(ProviderAdapterError::failed)?;
            client
                .set_permission_mode(query.permission_mode)
                .map_err(ProviderAdapterError::failed)?;
        }
        let mut session = self.inner.provider_session.lock().expect("claude session");
        session.model = Some(model_selection.model.clone());
        session.updated_at = self.inner.current_now();
        let event = AdapterEvent::ProviderSessionUpdated {
            driver: CLAUDE_DRIVER_KIND.into(),
            provider_session: session.clone(),
        };
        drop(session);
        self.inner.emit(event);
        Ok(())
    }

    fn apply_thread_title(
        &self,
        _provider_thread: &ProviderThread,
        title: &str,
    ) -> Result<(), ProviderAdapterError> {
        // Claude's CLI has no control request for a thread name; the app
        // keeps the title on its own record.
        self.inner
            .translator
            .lock()
            .expect("claude translator")
            .set_title(Some(title.to_owned()));
        Ok(())
    }

    fn respond_to_runtime_request(
        &self,
        input: RuntimeRequestResponse,
    ) -> Result<(), ProviderAdapterError> {
        self.inner.answer_pending(input)
    }

    fn read_thread_snapshot(
        &self,
        provider_thread: &ProviderThread,
    ) -> Result<ThreadSnapshot, ProviderAdapterError> {
        // T3's capability record says Claude cannot read a conversation
        // snapshot; return the thread record and leave messages/turns to the
        // projection layer.
        Ok(ThreadSnapshot {
            provider_thread: provider_thread.clone(),
            provider_turns: Vec::new(),
            messages: Vec::new(),
            runtime_requests: Vec::new(),
            provider_payload: None,
        })
    }

    fn skills(&self, cwds: &[String]) -> Result<Vec<SkillGroup>, ProviderAdapterError> {
        let client = self.inner.client.lock().expect("claude client").clone();
        let names = client
            .as_ref()
            .map(|client| client.skills())
            .unwrap_or_default();
        let commands = client
            .as_ref()
            .and_then(|client| client.session_info())
            .map(|info| {
                info.commands
                    .into_iter()
                    .filter(|command| !command.builtin)
                    .map(|command| {
                        json!({
                            "name": command.name,
                            "description": command.description,
                            "argumentHint": command.argument_hint,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Ok(cwds
            .iter()
            .map(|cwd| SkillGroup {
                cwd: cwd.clone(),
                skills: names
                    .iter()
                    .map(|name| json!({"name": name, "description": ""}))
                    .chain(commands.iter().cloned())
                    .collect(),
                errors: Vec::new(),
            })
            .collect())
    }

    fn config(&self, _cwd: Option<&str>) -> Result<ProviderConfig, ProviderAdapterError> {
        let client = self.inner.client.lock().expect("claude client").clone();
        let commands = client
            .as_ref()
            .map(|client| client.command_names())
            .unwrap_or_default();
        let skills = client
            .as_ref()
            .map(|client| client.skills())
            .unwrap_or_default();
        let session = self.provider_session();
        Ok(ProviderConfig {
            config: json!({
                "model": session.model,
                "permissionMode": QueryPolicy::for_runtime_policy(
                    &self.inner.runtime_policy.lock().expect("claude policy")
                ).permission_mode.as_str(),
                "commands": commands,
                "skills": skills,
            }),
            origins: json!({}),
        })
    }

    fn unload_thread(&self, _provider_thread: &ProviderThread) -> Result<(), ProviderAdapterError> {
        // One Claude process owns one native session; it stays loaded until
        // the provider session closes.
        Ok(())
    }

    fn close(&self) {
        if let Some(client) = self.inner.client.lock().expect("claude client").take() {
            client.close();
        }
    }
}

/// The permission mode and tool set T3 derives from a runtime policy
/// (`claudeRuntimeQueryPolicyForRuntimePolicy`).
#[derive(Clone, Debug)]
struct QueryPolicy {
    permission_mode: PermissionMode,
    tools: Tools,
    allowed_tools: Vec<String>,
    install_permission_callback: bool,
}

impl QueryPolicy {
    fn for_runtime_policy(policy: &RuntimePolicy) -> Self {
        let sandbox = policy
            .sandbox_policy
            .as_ref()
            .and_then(sandbox_kind)
            .map(str::to_owned);
        let permission_mode = permission_mode_for(policy, sandbox.as_deref());
        let read_only = sandbox.as_deref() == Some("read-only");
        let tools = if read_only {
            Tools::List(
                ["Read", "Glob", "Grep"]
                    .iter()
                    .map(|tool| (*tool).to_owned())
                    .collect(),
            )
        } else {
            Tools::ClaudeCode
        };
        let allowed_tools = if read_only {
            ["Read", "Glob", "Grep"]
                .iter()
                .map(|tool| (*tool).to_owned())
                .collect()
        } else {
            Vec::new()
        };
        let install_permission_callback = policy.interaction_mode == ProviderInteractionMode::Plan
            || match policy.approval_policy.as_ref() {
                Some(value) => !is_never(value),
                None => matches!(
                    policy.runtime_mode,
                    RuntimeMode::ApprovalRequired | RuntimeMode::AutoAcceptEdits
                ),
            };
        Self {
            permission_mode,
            tools,
            allowed_tools,
            install_permission_callback,
        }
    }
}

fn is_never(value: &Value) -> bool {
    value.as_str() == Some("never") || value.as_bool() == Some(false)
}

fn sandbox_kind(value: &Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.get("type").and_then(Value::as_str))
        .map(|kind| match kind {
            "dangerFullAccess" | "danger-full-access" => "danger-full-access",
            "workspaceWrite" | "workspace-write" => "workspace-write",
            "readOnly" | "read-only" => "read-only",
            other => other,
        })
}

fn permission_mode_for(policy: &RuntimePolicy, sandbox: Option<&str>) -> PermissionMode {
    if policy.interaction_mode == ProviderInteractionMode::Plan {
        return PermissionMode::Plan;
    }
    if let Some(approval) = policy.approval_policy.as_ref() {
        if !is_never(approval) {
            return PermissionMode::Default;
        }
        return match sandbox {
            Some("read-only") => PermissionMode::DontAsk,
            Some("danger-full-access") => PermissionMode::BypassPermissions,
            _ => match policy.runtime_mode {
                RuntimeMode::ApprovalRequired => PermissionMode::DontAsk,
                RuntimeMode::AutoAcceptEdits => PermissionMode::AcceptEdits,
                RuntimeMode::Auto => PermissionMode::Auto,
                RuntimeMode::FullAccess => PermissionMode::BypassPermissions,
            },
        };
    }
    match sandbox {
        Some("read-only") => PermissionMode::DontAsk,
        Some("danger-full-access") => {
            if policy.runtime_mode == RuntimeMode::ApprovalRequired {
                PermissionMode::Default
            } else {
                PermissionMode::BypassPermissions
            }
        }
        _ => match policy.runtime_mode {
            RuntimeMode::ApprovalRequired => PermissionMode::Default,
            RuntimeMode::AutoAcceptEdits => PermissionMode::AcceptEdits,
            RuntimeMode::Auto => PermissionMode::Auto,
            RuntimeMode::FullAccess => PermissionMode::BypassPermissions,
        },
    }
}

fn effort_of(selection: &crate::contracts::ModelSelection) -> Option<Effort> {
    let options = selection.options.as_ref()?;
    let value = options.as_slice().iter().find_map(|option| {
        matches!(option.id.as_str(), "reasoningEffort" | "effort")
            .then(|| option.value.as_str())
            .flatten()
    })?;
    Some(match value {
        "low" => Effort::Low,
        "medium" => Effort::Medium,
        "high" => Effort::High,
        "xhigh" | "xHigh" => Effort::XHigh,
        "max" => Effort::Max,
        "ultrathink" | "ultraThink" => Effort::UltraThink,
        _ => return None,
    })
}

fn runtime_request_kind(kind: &str) -> RuntimeRequestKind {
    match kind {
        "command" => RuntimeRequestKind::Command,
        "file-read" => RuntimeRequestKind::FileRead,
        "file-change" => RuntimeRequestKind::FileChange,
        _ => RuntimeRequestKind::Permission,
    }
}

fn user_content(
    text: &str,
    image_paths: &[String],
) -> Result<Vec<UserContentBlock>, ProviderAdapterError> {
    let mut content = Vec::new();
    if !text.is_empty() {
        content.push(UserContentBlock::text(text));
    }
    for path in image_paths {
        let bytes = std::fs::read(path).map_err(|error| {
            ProviderAdapterError::failed(format!("reading image {path}: {error}"))
        })?;
        content.push(UserContentBlock::image(
            mime_for(path),
            encode_base64(&bytes),
        ));
    }
    if content.is_empty() {
        return Err(ProviderAdapterError::Failed("the message is empty".into()));
    }
    Ok(content)
}

fn mime_for(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "image/png",
    }
}

fn encode_base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn epoch_seconds(now: &str) -> i64 {
    now.parse::<jiff::Timestamp>()
        .map(|timestamp| timestamp.as_second())
        .unwrap_or_else(|_| jiff::Timestamp::now().as_second())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::ModelSelection;
    use claude_client::events::Effort;

    // Ported from ClaudeAdapterV2.test.ts's capability cases (T3 Code
    // v0.1.11, MIT).
    #[test]
    fn claude_capabilities_match_t3s_record() {
        let capabilities = claude_provider_capabilities();
        assert!(
            !capabilities
                .sessions
                .supports_multiple_provider_threads_per_session
        );
        assert!(capabilities.sessions.supports_model_switch_in_session);
        assert!(!capabilities.threads.can_read_thread_snapshot);
        assert!(capabilities.turns.supports_active_steering);
        assert!(!capabilities.turns.exposes_native_turn_id);
        assert!(capabilities.subagents.emits_subagent_lifecycle);
        assert_eq!(
            capabilities.turns.terminal_status_quality,
            TerminalStatusQuality::Strong
        );
        assert_eq!(
            capabilities.identity.native_turn_ids,
            NativeRefStrength::Weak
        );
        let value = serde_json::to_value(&capabilities).unwrap();
        assert_eq!(
            value["sessions"]["supportsMultipleProviderThreadsPerSession"],
            false
        );
        assert_eq!(value["turns"]["supportsActiveSteering"], true);
        assert_eq!(value["runtimePolicy"]["enforcement"], "native");
    }

    #[test]
    fn core_capabilities_narrow_to_what_the_cli_advertised() {
        let empty = core_capabilities(None, None);
        assert!(empty.model_choice, "the conservative record allows models");
        assert!(empty.skills, "the conservative record allows skills");

        let info = SessionInfo {
            commands: vec![claude_client::SlashCommand {
                name: "review".into(),
                ..Default::default()
            }],
            models: vec![claude_client::protocol::ModelInfo {
                value: "claude-sonnet-4-6".into(),
                supports_effort: Some(true),
                ..Default::default()
            }],
            ..Default::default()
        };
        let init = SessionInit {
            slash_commands: vec!["review".into()],
            skills: vec!["deploy".into()],
            agents: vec!["Explore".into()],
            ..Default::default()
        };
        let capabilities = core_capabilities(Some(&info), Some(&init));
        assert!(capabilities.model_choice);
        assert!(capabilities.effort_choice);
        assert!(capabilities.skills);
        assert!(capabilities.subagents);
        assert!(capabilities.slash_commands);
        assert!(capabilities.mcp_status);
    }

    // Ported from ClaudeAdapterV2.test.ts's `permissionModeForClaudeRuntimePolicy`
    // cases (T3 Code v0.1.11, MIT).
    #[test]
    fn runtime_modes_map_to_claude_permission_modes() {
        let policy = |mode| RuntimePolicy {
            runtime_mode: mode,
            ..Default::default()
        };
        assert_eq!(
            permission_mode_for(&policy(RuntimeMode::ApprovalRequired), None),
            PermissionMode::Default
        );
        assert_eq!(
            permission_mode_for(&policy(RuntimeMode::AutoAcceptEdits), None),
            PermissionMode::AcceptEdits
        );
        assert_eq!(
            permission_mode_for(&policy(RuntimeMode::Auto), None),
            PermissionMode::Auto
        );
        assert_eq!(
            permission_mode_for(&policy(RuntimeMode::FullAccess), None),
            PermissionMode::BypassPermissions
        );
        let mut plan = policy(RuntimeMode::FullAccess);
        plan.interaction_mode = ProviderInteractionMode::Plan;
        assert_eq!(permission_mode_for(&plan, None), PermissionMode::Plan);
        let mut read_only = policy(RuntimeMode::FullAccess);
        read_only.sandbox_policy = Some(json!("read-only"));
        assert_eq!(
            permission_mode_for(&read_only, Some("read-only")),
            PermissionMode::DontAsk
        );
    }

    #[test]
    fn a_read_only_policy_limits_the_tools() {
        let mut policy = RuntimePolicy {
            runtime_mode: RuntimeMode::ApprovalRequired,
            ..Default::default()
        };
        policy.sandbox_policy = Some(json!({"type": "readOnly"}));
        let query = QueryPolicy::for_runtime_policy(&policy);
        assert!(matches!(query.tools, Tools::List(_)));
        assert_eq!(query.allowed_tools, ["Read", "Glob", "Grep"]);
        assert!(query.install_permission_callback);
    }

    #[test]
    fn effort_is_read_from_the_reasoning_effort_option() {
        use crate::contracts::{
            ProviderOptionSelection, ProviderOptionSelections, ProviderOptionValue,
        };
        let selection = ModelSelection {
            instance_id: "claude".into(),
            model: "claude-sonnet-4-6".into(),
            options: Some(ProviderOptionSelections(vec![ProviderOptionSelection {
                id: "reasoningEffort".into(),
                value: ProviderOptionValue::String("xhigh".into()),
            }])),
        };
        assert_eq!(effort_of(&selection), Some(Effort::XHigh));
        assert_eq!(
            effort_of(&ModelSelection::new("claude", "claude-sonnet-4-6")),
            None
        );
    }

    #[test]
    fn images_become_base64_content_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        std::fs::write(&path, [0x89, b'P', b'N', b'G']).unwrap();
        let content = user_content("look", &[path.display().to_string()]).unwrap();
        assert_eq!(content.len(), 2);
        assert!(matches!(content[0], UserContentBlock::Text { .. }));
        match &content[1] {
            UserContentBlock::Image { source } => {
                assert_eq!(source.media_type, "image/png");
                assert_eq!(source.data, "iVBORw==");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn empty_messages_are_refused() {
        assert!(user_content("", &[]).is_err());
    }
}
