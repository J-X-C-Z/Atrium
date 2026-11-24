//! The engine behind [`atrium_runtime::AgentRuntime`].
//!
//! The app's project layer, scheduler and workflow runner already talk to an
//! `AgentRuntime`. This implementation runs Codex sessions through the
//! provider adapter in [`crate::providers`], so those layers can select the
//! ported engine for a Codex backend without knowing anything about
//! app-server.
//!
//! A runtime session has its own id (a UUID) and remembers the provider's
//! native thread id. Events are retargeted to the runtime id before they
//! reach the app, so the app's bookkeeping stays unchanged, while resume and
//! handoff still have the native id to work with.
//!
//! The trait is synchronous: `start` and `send` do provider I/O, so the app
//! calls them through `RuntimeAdapters`' async helpers, which move the work
//! off GPUI's background executor. The engine is opt-in
//! (`ATRIUM_CODEX_ENGINE`) while the port is in review.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex, OnceLock, Weak,
        atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use atrium_core::{AgentEvent, ApprovalDecision, Capabilities, RateLimits, RequestId, Transcript};
use atrium_runtime::{
    AgentRuntime, Delivery, EventSink, RuntimeError, SessionId, SessionSpec, SessionSummary,
    TurnId, TurnInput, TurnOutcome,
};
use serde_json::Value;
use uuid::Uuid;

use crate::contracts::{
    MessageId, ModelSelection, NodeId, ProviderApprovalDecision, ProviderOptionSelection,
    ProviderOptionSelections, ProviderOptionValue, ProviderSessionId, ProviderThread, ProviderTurn,
    RunAttemptId, RunId, RuntimeMode, ThreadId,
};
use crate::providers::adapter::{
    AdapterEvent, AdapterEventSink, AdapterTurnInput, CODEX_DRIVER_KIND, EnsureThreadInput,
    OpenSessionInput, ProviderAdapter, ProviderSessionRuntime, RuntimePolicy,
    RuntimeRequestResponse, SteerInput, ThreadHistory, ThreadSnapshot, TurnMessage,
};
use crate::providers::codex::{CodexAdapter, CodexAdapterConfig};

/// A Codex engine the app can register for one backend name.
pub struct CodexEngineRuntime {
    inner: Arc<Inner>,
    adapter: Arc<dyn ProviderAdapter>,
    /// Thread-scoped settings the app registered for this backend.
    thread_env: Vec<(String, String)>,
    developer_instructions: Option<String>,
}

struct Inner {
    /// Sessions by the runtime's own id. Registration happens before the
    /// provider is opened, so events that arrive during a slow handshake are
    /// not dropped.
    sessions: Mutex<HashMap<SessionId, Arc<EngineSession>>>,
    next_session: AtomicU64,
}

/// The state a turn's start and completion race over, in one lock paired with
/// the session's [`Condvar`].
#[derive(Default)]
struct EngineState {
    active_turn: Option<ProviderTurn>,
    outcomes: HashMap<TurnId, TurnOutcome>,
    /// The newest turn the provider has named: running, or completed before
    /// `start_turn` returned. `wait(session, None)` uses it instead of an
    /// arbitrary map key.
    last_turn: Option<TurnId>,
    running: bool,
}

struct EngineSession {
    id: SessionId,
    agent: String,
    device: Option<String>,
    folder: PathBuf,
    model: Mutex<ModelSelection>,
    runtime_policy: RuntimePolicy,
    /// Set right after `open_session` returns. The session is already in
    /// `Inner.sessions` by then, so early events still land in its transcript.
    adapter: OnceLock<Arc<dyn ProviderSessionRuntime>>,
    provider_thread: Mutex<Option<ProviderThread>>,
    sink: EventSink,
    transcript: Mutex<Transcript>,
    state: Mutex<EngineState>,
    /// Paired with `state`; each session has its own, so a completion wakes
    /// only the waiters for that session.
    turn_ended: Condvar,
    /// Serializes `send(Start)` so two callers cannot both see idle.
    start_lock: Mutex<()>,
    updated_at_ms: AtomicI64,
    next_turn: AtomicU32,
    next_message: AtomicU64,
}

impl EngineSession {
    fn dispatch(&self, event: AgentEvent) {
        // The app knows this session by the runtime id; the provider uses its
        // native thread id. Retarget once, here.
        let event = event.with_thread_id(&self.id);
        self.transcript
            .lock()
            .expect("engine transcript")
            .apply(&event);
        self.updated_at_ms.store(now_ms(), Ordering::Relaxed);
        match &event {
            AgentEvent::TurnStarted { turn_id, .. } => {
                let mut state = self.state.lock().expect("engine state");
                state.running = true;
                state.last_turn = Some(turn_id.clone());
            }
            AgentEvent::TurnCompleted {
                turn_id,
                status,
                error,
                ..
            } => {
                let outcome = TurnOutcome {
                    turn_id: turn_id.clone(),
                    status: *status,
                    error: error.clone(),
                };
                {
                    let mut state = self.state.lock().expect("engine state");
                    state.outcomes.insert(turn_id.clone(), outcome);
                    state.last_turn = Some(turn_id.clone());
                    if state
                        .active_turn
                        .as_ref()
                        .is_some_and(|turn| turn.id.to_string() == *turn_id)
                    {
                        state.active_turn = None;
                    }
                    state.running = false;
                }
                self.turn_ended.notify_all();
            }
            _ => {}
        }
        (self.sink)(event);
    }

    fn adapter(&self) -> Result<&Arc<dyn ProviderSessionRuntime>, RuntimeError> {
        self.adapter
            .get()
            .ok_or_else(|| RuntimeError::Failed("the session is still opening".into()))
    }

    fn provider_thread(&self) -> Result<ProviderThread, RuntimeError> {
        self.provider_thread
            .lock()
            .expect("engine provider thread")
            .clone()
            .ok_or_else(|| RuntimeError::Failed("the session has no provider thread".into()))
    }

    fn active_turn(&self) -> Option<ProviderTurn> {
        self.state.lock().expect("engine state").active_turn.clone()
    }
}

impl Default for CodexEngineRuntime {
    fn default() -> Self {
        Self::new(CodexAdapterConfig::default())
    }
}

impl CodexEngineRuntime {
    pub fn new(config: CodexAdapterConfig) -> Self {
        let thread_env = config.thread_env.clone();
        let developer_instructions = config.developer_instructions.clone();
        Self::with_adapter(
            Arc::new(CodexAdapter::new("codex", config)),
            thread_env,
            developer_instructions,
        )
    }

    fn with_adapter(
        adapter: Arc<dyn ProviderAdapter>,
        thread_env: Vec<(String, String)>,
        developer_instructions: Option<String>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                sessions: Mutex::new(HashMap::new()),
                next_session: AtomicU64::new(0),
            }),
            adapter,
            thread_env,
            developer_instructions,
        }
    }

    /// The provider adapter this runtime opens sessions with.
    pub fn adapter(&self) -> &Arc<dyn ProviderAdapter> {
        &self.adapter
    }

    fn session(&self, id: &str) -> Result<Arc<EngineSession>, RuntimeError> {
        self.inner
            .sessions
            .lock()
            .expect("engine sessions")
            .get(id)
            .cloned()
            .ok_or_else(|| RuntimeError::UnknownSession(id.to_string()))
    }

    fn remove_session(&self, id: &str) -> Option<Arc<EngineSession>> {
        self.inner
            .sessions
            .lock()
            .expect("engine sessions")
            .remove(id)
    }

    /// Close a session and drop its provider runtime. When it was the last
    /// session on a shared app-server, that process stops too.
    pub fn close_session(&self, id: &SessionId) -> Result<(), RuntimeError> {
        self.remove_session(id)
            .map(|_| ())
            .ok_or_else(|| RuntimeError::UnknownSession(id.clone()))
    }

    /// The provider's native thread id for a runtime session, which resume
    /// and handoff use.
    pub fn native_thread_id(&self, id: &SessionId) -> Result<Option<String>, RuntimeError> {
        let session = self.session(id)?;
        Ok(session
            .provider_thread
            .lock()
            .expect("engine provider thread")
            .as_ref()
            .and_then(|thread| thread.native_thread_ref.as_ref())
            .and_then(|native| native.native_id.clone()))
    }

    /// Read the thread through `thread/read` (the read path), for views and
    /// sync. This never takes the provider's writer lock.
    pub fn read_snapshot(&self, id: &SessionId) -> Result<ThreadSnapshot, RuntimeError> {
        let session = self.session(id)?;
        let thread = session.provider_thread()?;
        session
            .adapter()?
            .read_thread_snapshot(&thread)
            .map_err(RuntimeError::failed)
    }

    /// Read the thread's items through `thread/read`, for a view or a sync
    /// peer that needs the conversation rather than a live transcript.
    pub fn read_history(&self, id: &SessionId) -> Result<ThreadHistory, RuntimeError> {
        let session = self.session(id)?;
        let thread = session.provider_thread()?;
        session
            .adapter()?
            .read_thread_history(&thread)
            .map_err(RuntimeError::failed)
    }

    /// The skills read at session start.
    pub fn startup_skills(
        &self,
        id: &SessionId,
    ) -> Result<Option<Vec<crate::providers::adapter::SkillGroup>>, RuntimeError> {
        Ok(self.session(id)?.adapter()?.startup_skills())
    }

    /// The config read at session start.
    pub fn startup_config(
        &self,
        id: &SessionId,
    ) -> Result<Option<crate::providers::adapter::ProviderConfig>, RuntimeError> {
        Ok(self.session(id)?.adapter()?.startup_config())
    }
}

impl AgentRuntime for CodexEngineRuntime {
    fn capabilities(&self, agent: &str) -> Capabilities {
        if agent == CODEX_DRIVER_KIND || agent.starts_with("codex") {
            Capabilities::codex()
        } else {
            Capabilities::NONE
        }
    }

    fn start(&self, spec: SessionSpec, sink: EventSink) -> Result<SessionId, RuntimeError> {
        // Remote sessions need item 29 (the engine on a node); until then the
        // app's legacy path serves `codex@device`, never a local process.
        if spec.device.is_some() || spec.agent.contains('@') {
            return Err(RuntimeError::Unsupported(format!(
                "remote Codex sessions ({}) need the engine on a node (item 29); \
                 the engine runs Codex only on this machine",
                spec.device.as_deref().unwrap_or(&spec.agent)
            )));
        }
        let seq = self.inner.next_session.fetch_add(1, Ordering::Relaxed) + 1;
        let id = Uuid::new_v4().to_string();
        let model = model_selection(&spec);
        let runtime_policy = runtime_policy(&spec);
        let now = now_iso();
        let engine_session = Arc::new(EngineSession {
            id: id.clone(),
            agent: spec.agent.clone(),
            device: spec.device.clone(),
            folder: spec.folder.clone(),
            model: Mutex::new(model.clone()),
            runtime_policy: runtime_policy.clone(),
            adapter: OnceLock::new(),
            provider_thread: Mutex::new(None),
            sink,
            transcript: Mutex::new(Transcript::new()),
            state: Mutex::new(EngineState::default()),
            turn_ended: Condvar::new(),
            start_lock: Mutex::new(()),
            updated_at_ms: AtomicI64::new(now_ms()),
            next_turn: AtomicU32::new(0),
            next_message: AtomicU64::new(seq),
        });
        // Register BEFORE opening: the provider can emit events (a thread
        // notification, a resumed turn) while `initialize`/`thread/start`
        // block, and `dispatch` must find the session.
        self.inner
            .sessions
            .lock()
            .expect("engine sessions")
            .insert(id.clone(), engine_session.clone());
        let weak = Arc::downgrade(&engine_session);
        let events: AdapterEventSink = Arc::new(move |event| {
            if let AdapterEvent::Agent(event) = event
                && let Some(session) = Weak::upgrade(&weak)
            {
                session.dispatch(event);
            }
        });
        let provider_session = match self.adapter.open_session(
            OpenSessionInput {
                thread_id: ThreadId::new(id.clone()),
                provider_session_id: ProviderSessionId::new(format!("engine-session-{seq}")),
                model_selection: model.clone(),
                runtime_policy: runtime_policy.clone(),
                resume_from_session: None,
                initial_native_thread_id: None,
                now: now.clone(),
            },
            events,
        ) {
            Ok(session) => session,
            Err(error) => {
                self.remove_session(&id);
                return Err(RuntimeError::failed(error));
            }
        };
        let _ = engine_session.adapter.set(provider_session.clone());
        let provider_thread = match provider_session.ensure_thread(EnsureThreadInput {
            thread_id: ThreadId::new(id.clone()),
            title: spec
                .first_message
                .as_ref()
                .map(|input| input.text.chars().take(60).collect()),
            model_selection: model,
            runtime_policy,
            mcp_servers: spec.mcp_servers.clone(),
            env: self.thread_env.clone(),
            developer_instructions: self.developer_instructions.clone(),
            provider_session_id: None,
            existing_provider_thread: None,
            force_new_native_thread: false,
            now,
        }) {
            Ok(thread) => thread,
            Err(error) => {
                self.remove_session(&id);
                return Err(RuntimeError::failed(error));
            }
        };
        if provider_thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.clone())
            .is_none()
        {
            self.remove_session(&id);
            return Err(RuntimeError::Failed(
                "Codex returned no native thread id".into(),
            ));
        }
        *engine_session
            .provider_thread
            .lock()
            .expect("engine provider thread") = Some(provider_thread);
        if let Some(input) = spec.first_message
            && !input.is_empty()
            && let Err(error) = self.send(&id, input, Delivery::Start)
        {
            self.remove_session(&id);
            return Err(error);
        }
        Ok(id)
    }

    fn send(
        &self,
        session: &SessionId,
        input: TurnInput,
        delivery: Delivery,
    ) -> Result<TurnId, RuntimeError> {
        if input.is_empty() {
            return Err(RuntimeError::Failed("the message is empty".into()));
        }
        let session = self.session(session)?;
        match delivery {
            Delivery::Queue => Err(RuntimeError::Unsupported(
                "the engine does not hold queued messages".into(),
            )),
            Delivery::Steer => {
                let provider_turn = session.active_turn().ok_or_else(|| {
                    RuntimeError::Unsupported("nothing is running to steer into".into())
                })?;
                self.steer(&session.id, input)?;
                Ok(provider_turn.id.to_string())
            }
            Delivery::Start => {
                let _start = session.start_lock.lock().expect("engine start lock");
                {
                    let mut state = session.state.lock().expect("engine state");
                    if state.running || state.active_turn.is_some() {
                        return Err(RuntimeError::Unsupported(
                            "a turn is already running".into(),
                        ));
                    }
                    // Mark the start before it blocks: a fast TurnCompleted
                    // may arrive before `start_turn` returns, and it must see
                    // a running turn to reconcile, not a stale idle session.
                    state.running = true;
                }
                let provider_thread = session.provider_thread()?;
                let message_id = format!(
                    "engine-message-{}",
                    session.next_message.fetch_add(1, Ordering::Relaxed) + 1
                );
                let ordinal = session.next_turn.fetch_add(1, Ordering::Relaxed) + 1;
                let started = session.adapter()?.start_turn(AdapterTurnInput {
                    thread_id: ThreadId::new(
                        provider_thread
                            .app_thread_id
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_else(|| session.id.clone()),
                    ),
                    run_id: RunId::new(format!("engine-run-{ordinal}")),
                    run_ordinal: ordinal,
                    provider_turn_ordinal: ordinal,
                    attempt_id: RunAttemptId::new(format!("engine-attempt-{ordinal}")),
                    root_node_id: NodeId::new(format!("engine-node-{ordinal}")),
                    provider_thread,
                    message: message(input.text).with_id(MessageId::new(message_id)),
                    model_selection: session.model.lock().expect("engine model").clone(),
                    runtime_policy: session.runtime_policy.clone(),
                    app_context: Vec::new(),
                    now: now_iso(),
                });
                let provider_turn = match started {
                    Ok(turn) => turn,
                    Err(error) => {
                        session.state.lock().expect("engine state").running = false;
                        return Err(RuntimeError::failed(error));
                    }
                };
                let turn = provider_turn.id.to_string();
                {
                    let mut state = session.state.lock().expect("engine state");
                    if state.outcomes.contains_key(&turn) {
                        // The completion won the race: leave the session idle
                        // and keep the recorded outcome.
                        state.running = false;
                        if state.last_turn.is_none() {
                            state.last_turn = Some(turn.clone());
                        }
                    } else {
                        state.active_turn = Some(provider_turn);
                        state.last_turn = Some(turn.clone());
                        state.running = true;
                    }
                }
                session.updated_at_ms.store(now_ms(), Ordering::Relaxed);
                Ok(turn)
            }
        }
    }

    fn steer(&self, session: &SessionId, input: TurnInput) -> Result<(), RuntimeError> {
        if input.is_empty() {
            return Err(RuntimeError::Failed("the message is empty".into()));
        }
        let session = self.session(session)?;
        let turn = session
            .active_turn()
            .ok_or_else(|| RuntimeError::Unsupported("nothing is running to steer into".into()))?;
        let provider_thread = session.provider_thread()?;
        let message_id = format!(
            "engine-message-{}",
            session.next_message.fetch_add(1, Ordering::Relaxed) + 1
        );
        session
            .adapter()?
            .steer_turn(SteerInput {
                thread_id: ThreadId::new(
                    provider_thread
                        .app_thread_id
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| session.id.clone()),
                ),
                run_id: RunId::new("engine-steer"),
                provider_thread,
                provider_turn_id: turn.id.clone(),
                message: message(input.text).with_id(MessageId::new(message_id)),
                model_selection: Some(session.model.lock().expect("engine model").clone()),
                now: now_iso(),
            })
            .map_err(RuntimeError::failed)
    }

    fn interrupt(&self, session: &SessionId) -> Result<(), RuntimeError> {
        let session = self.session(session)?;
        let Some(turn) = session.active_turn() else {
            return Ok(());
        };
        let provider_thread = session.provider_thread()?;
        session
            .adapter()?
            .interrupt_turn(&provider_thread, &turn.id)
            .map_err(RuntimeError::failed)
    }

    fn answer_approval(
        &self,
        session: &SessionId,
        request: RequestId,
        decision: ApprovalDecision,
    ) -> Result<(), RuntimeError> {
        let session = self.session(session)?;
        let decision = match decision {
            ApprovalDecision::Accept => ProviderApprovalDecision::Accept,
            ApprovalDecision::AcceptForSession => ProviderApprovalDecision::AcceptForSession,
            ApprovalDecision::Decline => ProviderApprovalDecision::Decline,
            ApprovalDecision::Cancel => ProviderApprovalDecision::Cancel,
        };
        session
            .adapter()?
            .respond_to_runtime_request(RuntimeRequestResponse {
                request_id: request_id_text(&request),
                decision: Some(decision),
                feedback: None,
                answers: None,
            })
            .map_err(RuntimeError::failed)
    }

    fn wait(
        &self,
        session: &SessionId,
        turn: Option<&TurnId>,
        timeout_ms: u64,
    ) -> Result<TurnOutcome, RuntimeError> {
        let session = self.session(session)?;
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let mut state = session.state.lock().expect("engine state");
        loop {
            // The newest turn: the explicit one, the running one, or the last
            // one the provider named. Never an arbitrary map key.
            let wanted = turn
                .cloned()
                .or_else(|| state.active_turn.as_ref().map(|turn| turn.id.to_string()))
                .or_else(|| state.last_turn.clone());
            if let Some(turn) = wanted
                && let Some(outcome) = state.outcomes.get(&turn)
            {
                return Ok(outcome.clone());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(RuntimeError::Timeout(timeout_ms));
            }
            let (guard, _) = session
                .turn_ended
                .wait_timeout(state, deadline - now)
                .expect("engine wait");
            state = guard;
        }
    }

    fn transcript(&self, session: &SessionId) -> Result<Transcript, RuntimeError> {
        Ok(self
            .session(session)?
            .transcript
            .lock()
            .expect("engine transcript")
            .clone())
    }

    fn sessions(&self) -> Result<Vec<SessionSummary>, RuntimeError> {
        let sessions = self.inner.sessions.lock().expect("engine sessions");
        Ok(sessions
            .iter()
            .map(|(id, session)| SessionSummary {
                session_id: id.clone(),
                agent: session.agent.clone(),
                folder: session.folder.clone(),
                device: session.device.clone(),
                title: None,
                running: session.state.lock().expect("engine state").running,
                updated_at_ms: session.updated_at_ms.load(Ordering::Relaxed),
            })
            .collect())
    }

    fn limits(&self, _agent: &str) -> Result<Option<RateLimits>, RuntimeError> {
        // The usage and limits reader is item 26.
        Ok(None)
    }
}

/// A small helper so callers can build a one-line message without the rest
/// of the adapter's fields.
trait TurnMessageBuilder {
    fn with_id(self, id: MessageId) -> TurnMessage;
}

fn message(text: String) -> TurnMessage {
    TurnMessage {
        message_id: MessageId::new("engine-message"),
        text,
        attachments: Vec::new(),
        created_by: crate::contracts::Actor::User,
        creation_source: crate::contracts::CreationSource::Server,
        scheduled_task_id: None,
        sender_thread_id: None,
    }
}

impl TurnMessageBuilder for TurnMessage {
    fn with_id(mut self, id: MessageId) -> TurnMessage {
        self.message_id = id;
        self
    }
}

/// The model the user chose. An empty model means "not chosen": the adapter
/// leaves it out so Codex applies its own default, as the app's chat path
/// does. There is no hard-coded fallback.
fn model_selection(spec: &SessionSpec) -> ModelSelection {
    let mut options = Vec::new();
    if let Some(effort) = &spec.effort {
        options.push(ProviderOptionSelection {
            id: "reasoningEffort".into(),
            value: ProviderOptionValue::String(effort.clone()),
        });
    }
    if let Some(tier) = &spec.service_tier {
        options.push(ProviderOptionSelection {
            id: "serviceTier".into(),
            value: ProviderOptionValue::String(tier.clone()),
        });
    }
    ModelSelection {
        instance_id: spec.agent.clone().into(),
        model: spec.model.clone().unwrap_or_default(),
        options: (!options.is_empty()).then_some(ProviderOptionSelections(options)),
    }
}

/// The runtime policy for an app access mode. This is the one mapping shared
/// with `codex-client::AccessMode`, so the engine sends the same approval and
/// sandbox pair the app's legacy `StartThread` path sends.
fn runtime_policy(spec: &SessionSpec) -> RuntimePolicy {
    let name = spec.access_mode.as_deref().unwrap_or("auto");
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
        cwd: Some(spec.folder.display().to_string()),
        approval_policy: Some(Value::String(access.approval_policy().into())),
        sandbox_policy: Some(access.sandbox_policy()),
        max_runtime_mode: None,
    }
}

fn request_id_text(request: &RequestId) -> String {
    match request {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

fn now_iso() -> String {
    jiff::Timestamp::now().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;

    use crate::contracts::{
        ProviderInstanceId, ProviderSession, ProviderThreadId, ProviderThreadStatus,
        ProviderTurnId, ProviderTurnStatus,
    };
    use crate::providers::adapter::{ProviderConfig, SkillGroup, ThreadHistory, ThreadSnapshot};
    use crate::providers::codex::codex_provider_capabilities;
    use atrium_core::{Item, TurnStatus};

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Script {
        /// Complete the turn synchronously inside `start_turn`.
        FastCompletion,
        /// Emit an item while `ensure_thread` is still running.
        ItemDuringEnsure,
        /// Wait for the test to drive completion.
        Manual,
    }

    struct ScriptedAdapter {
        script: Script,
        last: Mutex<Option<Arc<ScriptedSession>>>,
    }

    impl ScriptedAdapter {
        fn new(script: Script) -> Arc<Self> {
            Arc::new(Self {
                script,
                last: Mutex::new(None),
            })
        }

        fn session(&self) -> Arc<ScriptedSession> {
            self.last
                .lock()
                .unwrap()
                .clone()
                .expect("a session was opened")
        }

        fn ensure_input(&self) -> EnsureThreadInput {
            self.session()
                .ensure
                .lock()
                .unwrap()
                .clone()
                .expect("ensure_thread ran")
        }
    }

    struct ScriptedSession {
        script: Script,
        sink: AdapterEventSink,
        thread: Mutex<Option<ProviderThread>>,
        ensure: Mutex<Option<EnsureThreadInput>>,
    }

    impl ScriptedSession {
        fn emit_agent(&self, text: &str, turn: &str) {
            (self.sink)(AdapterEvent::Agent(AgentEvent::ItemCompleted {
                thread_id: "native-thread".into(),
                turn_id: turn.into(),
                item: Item::Agent {
                    id: "early-item".into(),
                    text: text.into(),
                },
            }));
        }

        fn emit_completed(&self, turn: &str) {
            (self.sink)(AdapterEvent::Agent(AgentEvent::TurnCompleted {
                thread_id: "native-thread".into(),
                turn_id: turn.into(),
                status: TurnStatus::Completed,
                error: None,
                duration_ms: None,
            }));
        }

        fn emit_completed_after(self: &Arc<Self>, turn: String, delay: Duration) {
            let session = self.clone();
            thread::spawn(move || {
                thread::sleep(delay);
                session.emit_completed(&turn);
            });
        }
    }

    fn scripted_thread(native: &str) -> ProviderThread {
        use crate::contracts::{NativeRefStrength, ProviderRef};
        ProviderThread {
            id: ProviderThreadId::new(format!("provider-thread-{native}")),
            driver: crate::contracts::ProviderDriverKind::new(CODEX_DRIVER_KIND),
            provider_instance_id: ProviderInstanceId::new("codex"),
            provider_session_id: Some(ProviderSessionId::new("scripted-session")),
            app_thread_id: Some(ThreadId::new("app-thread")),
            owner_node_id: None,
            native_thread_ref: Some(ProviderRef {
                driver: crate::contracts::ProviderDriverKind::new(CODEX_DRIVER_KIND),
                native_id: Some(native.into()),
                strength: NativeRefStrength::Strong,
                fingerprint: None,
                ordinal: None,
            }),
            native_conversation_head_ref: None,
            status: ProviderThreadStatus::Active,
            first_run_ordinal: None,
            last_run_ordinal: None,
            handoff_ids: Vec::new(),
            forked_from: None,
            pending_background_tasks: Vec::new(),
            context_usage: None,
            native_metadata: None,
            goal: None,
            created_at: "2026-11-24T00:00:00.000Z".into(),
            updated_at: "2026-11-24T00:00:00.000Z".into(),
        }
    }

    fn scripted_turn(input: &AdapterTurnInput) -> ProviderTurn {
        use crate::contracts::{NativeRefStrength, ProviderRef};
        ProviderTurn {
            id: ProviderTurnId::new(format!("native-turn-{}", input.run_ordinal)),
            provider_thread_id: input.provider_thread.id.clone(),
            node_id: input.root_node_id.clone(),
            run_attempt_id: Some(input.attempt_id.clone()),
            native_turn_ref: Some(ProviderRef {
                driver: crate::contracts::ProviderDriverKind::new(CODEX_DRIVER_KIND),
                native_id: Some(format!("native-turn-{}", input.run_ordinal)),
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
            model: None,
        }
    }

    impl ProviderAdapter for ScriptedAdapter {
        fn instance_id(&self) -> &ProviderInstanceId {
            static ID: OnceLock<ProviderInstanceId> = OnceLock::new();
            ID.get_or_init(|| ProviderInstanceId::new("codex"))
        }

        fn driver(&self) -> &str {
            CODEX_DRIVER_KIND
        }

        fn capabilities(&self) -> crate::contracts::ProviderCapabilities {
            codex_provider_capabilities()
        }

        fn plan_selection_transition(
            &self,
            _current: &ModelSelection,
            _target: &ModelSelection,
        ) -> crate::providers::adapter::SelectionTransitionPlan {
            crate::providers::adapter::SelectionTransitionPlan::ApplyOnNextTurn
        }

        fn open_session(
            &self,
            _input: OpenSessionInput,
            events: AdapterEventSink,
        ) -> Result<Arc<dyn ProviderSessionRuntime>, crate::providers::adapter::ProviderAdapterError>
        {
            let session = Arc::new(ScriptedSession {
                script: self.script,
                sink: events,
                thread: Mutex::new(None),
                ensure: Mutex::new(None),
            });
            *self.last.lock().unwrap() = Some(session.clone());
            Ok(session)
        }
    }

    impl ProviderSessionRuntime for ScriptedSession {
        fn provider_session(&self) -> ProviderSession {
            crate::providers::adapter::provider_session(
                ProviderSessionId::new("scripted-session"),
                ProviderInstanceId::new("codex"),
                Some("/tmp"),
                None,
                codex_provider_capabilities(),
                "2026-11-24T00:00:00.000Z",
            )
        }

        fn driver(&self) -> &str {
            CODEX_DRIVER_KIND
        }

        fn ensure_thread(
            &self,
            input: EnsureThreadInput,
        ) -> Result<ProviderThread, crate::providers::adapter::ProviderAdapterError> {
            *self.ensure.lock().unwrap() = Some(input);
            let thread = scripted_thread("native-thread");
            *self.thread.lock().unwrap() = Some(thread.clone());
            if self.script == Script::ItemDuringEnsure {
                // Emitted before `start` returns, as a provider notification
                // can arrive during the handshake.
                self.emit_agent("early", "early-turn");
            }
            Ok(thread)
        }

        fn resume_thread(
            &self,
            provider_thread: &ProviderThread,
            _model_selection: Option<&ModelSelection>,
            _runtime_policy: Option<&RuntimePolicy>,
            _now: &str,
        ) -> Result<ProviderThread, crate::providers::adapter::ProviderAdapterError> {
            Ok(provider_thread.clone())
        }

        fn start_turn(
            &self,
            input: AdapterTurnInput,
        ) -> Result<ProviderTurn, crate::providers::adapter::ProviderAdapterError> {
            let turn = scripted_turn(&input);
            if self.script == Script::FastCompletion {
                let turn_id = turn.id.to_string();
                (self.sink)(AdapterEvent::Agent(AgentEvent::TurnStarted {
                    thread_id: "native-thread".into(),
                    turn_id: turn_id.clone(),
                }));
                (self.sink)(AdapterEvent::Agent(AgentEvent::TurnCompleted {
                    thread_id: "native-thread".into(),
                    turn_id,
                    status: TurnStatus::Completed,
                    error: None,
                    duration_ms: None,
                }));
            }
            Ok(turn)
        }

        fn steer_turn(
            &self,
            _input: SteerInput,
        ) -> Result<(), crate::providers::adapter::ProviderAdapterError> {
            Ok(())
        }

        fn interrupt_turn(
            &self,
            _provider_thread: &ProviderThread,
            _provider_turn_id: &ProviderTurnId,
        ) -> Result<(), crate::providers::adapter::ProviderAdapterError> {
            Ok(())
        }

        fn apply_model_selection(
            &self,
            _provider_thread: &ProviderThread,
            _model_selection: &ModelSelection,
            _runtime_policy: Option<&RuntimePolicy>,
        ) -> Result<(), crate::providers::adapter::ProviderAdapterError> {
            Ok(())
        }

        fn apply_thread_title(
            &self,
            _provider_thread: &ProviderThread,
            _title: &str,
        ) -> Result<(), crate::providers::adapter::ProviderAdapterError> {
            Ok(())
        }

        fn respond_to_runtime_request(
            &self,
            _input: RuntimeRequestResponse,
        ) -> Result<(), crate::providers::adapter::ProviderAdapterError> {
            Ok(())
        }

        fn read_thread_snapshot(
            &self,
            provider_thread: &ProviderThread,
        ) -> Result<ThreadSnapshot, crate::providers::adapter::ProviderAdapterError> {
            Ok(ThreadSnapshot {
                provider_thread: provider_thread.clone(),
                provider_turns: Vec::new(),
                messages: Vec::new(),
                runtime_requests: Vec::new(),
                provider_payload: None,
            })
        }

        fn read_thread_history(
            &self,
            _provider_thread: &ProviderThread,
        ) -> Result<ThreadHistory, crate::providers::adapter::ProviderAdapterError> {
            Ok(ThreadHistory {
                meta: atrium_core::ThreadMeta {
                    id: "native-thread".into(),
                    backend: "codex".into(),
                    name: None,
                    preview: String::new(),
                    cwd: "/tmp".into(),
                    model: None,
                    created_at: 0,
                    updated_at: 0,
                },
                turns: Vec::new(),
            })
        }

        fn skills(
            &self,
            _cwds: &[String],
        ) -> Result<Vec<SkillGroup>, crate::providers::adapter::ProviderAdapterError> {
            Ok(Vec::new())
        }

        fn config(
            &self,
            _cwd: Option<&str>,
        ) -> Result<ProviderConfig, crate::providers::adapter::ProviderAdapterError> {
            Ok(ProviderConfig::default())
        }

        fn startup_skills(&self) -> Option<Vec<SkillGroup>> {
            Some(Vec::new())
        }

        fn startup_config(&self) -> Option<ProviderConfig> {
            Some(ProviderConfig::default())
        }

        fn unload_thread(
            &self,
            _provider_thread: &ProviderThread,
        ) -> Result<(), crate::providers::adapter::ProviderAdapterError> {
            Ok(())
        }

        fn close(&self) {}
    }

    fn spec(device: Option<&str>) -> SessionSpec {
        SessionSpec {
            agent: device.map_or_else(|| "codex".into(), |device| format!("codex@{device}")),
            model: Some("mock-1".into()),
            effort: Some("high".into()),
            service_tier: Some("priority".into()),
            folder: PathBuf::from("/tmp/work"),
            worktree: None,
            device: device.map(str::to_owned),
            mcp_servers: Vec::new(),
            first_message: None,
            access_mode: Some("read-only".into()),
        }
    }

    fn engine(script: Script) -> (Arc<ScriptedAdapter>, CodexEngineRuntime) {
        let adapter = ScriptedAdapter::new(script);
        let runtime = CodexEngineRuntime::with_adapter(adapter.clone(), Vec::new(), None);
        (adapter, runtime)
    }

    #[test]
    fn access_modes_use_one_mapping_and_never_default_the_model() {
        let mut spec = spec(None);
        let policy = runtime_policy(&spec);
        assert_eq!(policy.runtime_mode, RuntimeMode::ApprovalRequired);
        // The app's AccessMode::ReadOnly pair, not T3's untrusted alias.
        assert_eq!(
            policy.approval_policy,
            Some(Value::String("on-request".into()))
        );
        assert_eq!(policy.sandbox_policy.as_ref().unwrap()["type"], "readOnly");
        spec.access_mode = Some("full-access".into());
        assert_eq!(runtime_policy(&spec).runtime_mode, RuntimeMode::FullAccess);
        spec.access_mode = Some("wat".into());
        assert_eq!(runtime_policy(&spec).runtime_mode, RuntimeMode::Auto);

        spec.model = Some("mock-1".into());
        let selection = model_selection(&spec);
        assert_eq!(selection.model, "mock-1");
        let options = selection.options.unwrap();
        assert_eq!(options.0[0].id, "reasoningEffort");
        assert_eq!(options.0[1].value.as_str(), Some("priority"));

        // No chosen model: empty, so the adapter lets Codex decide.
        spec.model = None;
        assert!(model_selection(&spec).model.is_empty());
    }

    #[test]
    fn remote_specs_are_rejected_not_run_locally() {
        let (adapter, runtime) = engine(Script::Manual);
        let error = runtime
            .start(spec(Some("nas")), Arc::new(|_| {}))
            .expect_err("remote sessions need item 29");
        assert!(matches!(error, RuntimeError::Unsupported(_)), "{error}");
        // Nothing was opened, and no session was left behind.
        assert!(adapter.last.lock().unwrap().is_none());
        assert!(runtime.sessions().unwrap().is_empty());
    }

    #[test]
    fn start_registers_first_and_accepts_events_from_a_slow_open() {
        let (adapter, runtime) = engine(Script::ItemDuringEnsure);
        let (tx, events) = mpsc::channel();
        let sink: EventSink = Arc::new(move |event| {
            let _ = tx.send(event);
        });
        let id = runtime.start(spec(None), sink).expect("start");
        // The item was emitted from `ensure_thread`, before `start` returned.
        let received = events.try_recv().expect("the early item reached the sink");
        assert_eq!(received.thread_id(), Some(id.as_str()));
        assert!(
            runtime
                .transcript(&id)
                .unwrap()
                .entries()
                .iter()
                .any(|entry| matches!(
                    &entry.kind,
                    atrium_core::EntryKind::Item(Item::Agent { text, .. }) if text == "early"
                ))
        );
        let _ = adapter;
    }

    #[test]
    fn a_fast_completion_does_not_leave_the_session_running() {
        let (adapter, runtime) = engine(Script::FastCompletion);
        let id = runtime.start(spec(None), Arc::new(|_| {})).expect("start");
        let session = adapter.session();
        let turn = runtime
            .send(&id, TurnInput::text("hi"), Delivery::Start)
            .expect("send");
        // `start_turn` completed the turn synchronously, before returning.
        let outcome = runtime
            .wait(&id, Some(&turn), 1_000)
            .expect("the completion was not lost");
        assert_eq!(outcome.status, TurnStatus::Completed);
        assert!(!runtime.sessions().unwrap()[0].running);
        // The session is idle again.
        runtime
            .send(&id, TurnInput::text("again"), Delivery::Start)
            .expect("a fast completion must not wedge the session");
        let _ = session;
    }

    #[test]
    fn wait_without_a_turn_uses_the_newest_turn_and_its_own_condvar() {
        let (adapter, runtime) = engine(Script::Manual);
        let id = runtime.start(spec(None), Arc::new(|_| {})).expect("start");
        let first = runtime
            .send(&id, TurnInput::text("one"), Delivery::Start)
            .unwrap();
        let session = adapter.session();
        session.emit_completed(&first);
        let second = runtime
            .send(&id, TurnInput::text("two"), Delivery::Start)
            .unwrap();
        // Completion for the second turn arrives on another thread; `wait`
        // must sleep on this session's condvar and wake for it.
        session.emit_completed_after(second.clone(), Duration::from_millis(30));
        let outcome = runtime.wait(&id, None, 2_000).expect("wait newest");
        assert_eq!(outcome.turn_id, second);
    }

    #[test]
    fn runtime_sessions_have_their_own_ids_mapped_to_the_native_thread() {
        let (_adapter, runtime) = engine(Script::Manual);
        let (tx, events) = mpsc::channel::<AgentEvent>();
        let sink: EventSink = Arc::new(move |event| {
            let _ = tx.send(event);
        });
        let id = runtime.start(spec(None), sink).expect("start");
        assert_ne!(id, "native-thread");
        assert_eq!(
            runtime.native_thread_id(&id).unwrap().as_deref(),
            Some("native-thread")
        );
        let round = runtime
            .send(&id, TurnInput::text("hi"), Delivery::Start)
            .unwrap();
        assert_eq!(round, "native-turn-1");
        let _ = events;
    }

    #[test]
    fn start_passes_the_registered_registration_into_the_thread() {
        let adapter = ScriptedAdapter::new(Script::Manual);
        let runtime = CodexEngineRuntime::with_adapter(
            adapter.clone(),
            vec![("ATRIUM_PROJECT".into(), "/tmp/work".into())],
            Some("be concise".into()),
        );
        let mut spec = spec(None);
        spec.mcp_servers = vec![atrium_core::McpServer {
            name: "atrium".into(),
            command: "atrium".into(),
            args: vec!["mcp".into()],
            ..atrium_core::McpServer::default()
        }];
        runtime.start(spec, Arc::new(|_| {})).expect("start");
        let ensure = adapter.ensure_input();
        assert_eq!(ensure.mcp_servers.len(), 1);
        assert_eq!(
            ensure.env,
            vec![("ATRIUM_PROJECT".into(), "/tmp/work".into())]
        );
        assert_eq!(ensure.developer_instructions.as_deref(), Some("be concise"));
    }

    #[test]
    fn reads_use_the_provider_read_path_and_startup_reads_are_exposed() {
        let (_adapter, runtime) = engine(Script::Manual);
        let id = runtime.start(spec(None), Arc::new(|_| {})).expect("start");
        let snapshot = runtime.read_snapshot(&id).expect("read snapshot");
        assert_eq!(
            snapshot.provider_thread.id.to_string(),
            "provider-thread-native-thread"
        );
        let history = runtime.read_history(&id).expect("read history");
        assert_eq!(history.meta.id, "native-thread");
        assert!(runtime.startup_skills(&id).unwrap().is_some());
        assert!(runtime.startup_config(&id).unwrap().is_some());
    }

    #[test]
    fn closing_a_session_removes_it() {
        let (adapter, runtime) = engine(Script::Manual);
        let id = runtime.start(spec(None), Arc::new(|_| {})).expect("start");
        runtime.close_session(&id).expect("close");
        assert!(runtime.sessions().unwrap().is_empty());
        assert!(matches!(
            runtime.transcript(&id),
            Err(RuntimeError::UnknownSession(_))
        ));
        let _ = adapter;
    }
}
