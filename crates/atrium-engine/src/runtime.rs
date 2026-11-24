//! The engine behind [`atrium_runtime::AgentRuntime`].
//!
//! The app's project layer, scheduler and workflow runner already talk to an
//! `AgentRuntime`. This implementation runs Codex sessions through the
//! provider adapter in [`crate::providers`], so those layers can select the
//! ported engine for a Codex backend without knowing anything about
//! app-server.
//!
//! A session's id is the native Codex thread id, which is also the thread id
//! on every translated event; that keeps the app's transcript bookkeeping
//! unchanged.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use atrium_core::{AgentEvent, ApprovalDecision, Capabilities, RateLimits, RequestId, Transcript};
use atrium_runtime::{
    AgentRuntime, Delivery, EventSink, RuntimeError, SessionId, SessionSpec, SessionSummary,
    TurnId, TurnInput, TurnOutcome,
};

use crate::contracts::{
    MessageId, ModelSelection, NodeId, ProviderApprovalDecision, ProviderOptionSelection,
    ProviderOptionSelections, ProviderOptionValue, ProviderSessionId, ProviderThread, ProviderTurn,
    RunAttemptId, RunId, RuntimeMode, ThreadId,
};
use crate::providers::adapter::{
    AdapterEvent, AdapterEventSink, AdapterTurnInput, CODEX_DRIVER_KIND, EnsureThreadInput,
    OpenSessionInput, ProviderAdapter, ProviderSessionRuntime, RuntimePolicy,
    RuntimeRequestResponse, SteerInput, TurnMessage,
};
use crate::providers::codex::{CodexAdapter, CodexAdapterConfig};

/// A Codex engine the app can register for one backend name.
pub struct CodexEngineRuntime {
    inner: Arc<Inner>,
    adapter: CodexAdapter,
}

struct Inner {
    sessions: Mutex<HashMap<SessionId, Arc<EngineSession>>>,
    turn_ended: Condvar,
    next_session: AtomicU64,
}

struct EngineSession {
    agent: String,
    device: Option<String>,
    folder: PathBuf,
    model: Mutex<ModelSelection>,
    runtime_policy: RuntimePolicy,
    adapter: Arc<dyn ProviderSessionRuntime>,
    provider_thread: Mutex<ProviderThread>,
    sink: EventSink,
    transcript: Mutex<Transcript>,
    active_turn: Mutex<Option<ProviderTurn>>,
    outcomes: Mutex<HashMap<TurnId, TurnOutcome>>,
    running: AtomicBool,
    updated_at_ms: AtomicI64,
    next_turn: AtomicU32,
    next_message: AtomicU64,
}

impl Inner {
    fn dispatch(&self, event: AgentEvent) {
        let Some(thread) = event.thread_id() else {
            return;
        };
        let session = self
            .sessions
            .lock()
            .expect("engine sessions")
            .get(thread)
            .cloned();
        let Some(session) = session else { return };
        session
            .transcript
            .lock()
            .expect("engine transcript")
            .apply(&event);
        session.updated_at_ms.store(now_ms(), Ordering::Relaxed);
        match &event {
            AgentEvent::TurnStarted { .. } => session.running.store(true, Ordering::Relaxed),
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
                session
                    .outcomes
                    .lock()
                    .expect("engine outcomes")
                    .insert(turn_id.clone(), outcome);
                *session.active_turn.lock().expect("engine active turn") = None;
                session.running.store(false, Ordering::Relaxed);
                self.turn_ended.notify_all();
            }
            _ => {}
        }
        (session.sink)(event);
    }
}

impl Default for CodexEngineRuntime {
    fn default() -> Self {
        Self::new(CodexAdapterConfig::default())
    }
}

impl CodexEngineRuntime {
    pub fn new(config: CodexAdapterConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                sessions: Mutex::new(HashMap::new()),
                turn_ended: Condvar::new(),
                next_session: AtomicU64::new(0),
            }),
            adapter: CodexAdapter::new("codex", config),
        }
    }

    /// The provider adapter this runtime opens sessions with.
    pub fn adapter(&self) -> &CodexAdapter {
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
        let seq = self.inner.next_session.fetch_add(1, Ordering::Relaxed) + 1;
        let model = model_selection(&spec);
        let runtime_policy = runtime_policy(&spec);
        let now = now_iso();
        let provider_session = ProviderSessionId::new(format!("engine-session-{seq}"));
        let app_thread = ThreadId::new(format!("engine-app-thread-{seq}"));
        let inner = self.inner.clone();
        let events: AdapterEventSink = Arc::new(move |event| {
            if let AdapterEvent::Agent(event) = event {
                inner.dispatch(event);
            }
        });
        let session = self
            .adapter
            .open_session(
                OpenSessionInput {
                    thread_id: app_thread.clone(),
                    provider_session_id: provider_session,
                    model_selection: model.clone(),
                    runtime_policy: runtime_policy.clone(),
                    resume_from_session: None,
                    initial_native_thread_id: None,
                    now: now.clone(),
                },
                events,
            )
            .map_err(RuntimeError::failed)?;
        let provider_thread = session
            .ensure_thread(EnsureThreadInput {
                thread_id: app_thread,
                title: spec
                    .first_message
                    .as_ref()
                    .map(|input| input.text.chars().take(60).collect()),
                model_selection: model.clone(),
                runtime_policy: runtime_policy.clone(),
                provider_session_id: None,
                existing_provider_thread: None,
                force_new_native_thread: false,
                now,
            })
            .map_err(RuntimeError::failed)?;
        let native = provider_thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.clone())
            .ok_or_else(|| RuntimeError::Failed("Codex returned no native thread id".into()))?;
        let engine_session = Arc::new(EngineSession {
            agent: spec.agent.clone(),
            device: spec.device.clone(),
            folder: spec.folder.clone(),
            model: Mutex::new(model),
            runtime_policy,
            adapter: session,
            provider_thread: Mutex::new(provider_thread),
            sink,
            transcript: Mutex::new(Transcript::new()),
            active_turn: Mutex::new(None),
            outcomes: Mutex::new(HashMap::new()),
            running: AtomicBool::new(false),
            updated_at_ms: AtomicI64::new(now_ms()),
            next_turn: AtomicU32::new(0),
            next_message: AtomicU64::new(0),
        });
        self.inner
            .sessions
            .lock()
            .expect("engine sessions")
            .insert(native.clone(), engine_session);
        if let Some(input) = spec.first_message
            && !input.is_empty()
            && let Err(error) = self.send(&native, input, Delivery::Start)
        {
            self.inner
                .sessions
                .lock()
                .expect("engine sessions")
                .remove(&native);
            return Err(error);
        }
        Ok(native)
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
        let session_key = session.clone();
        let session = self.session(session)?;
        match delivery {
            Delivery::Queue => Err(RuntimeError::Unsupported(
                "the engine does not hold queued messages".into(),
            )),
            Delivery::Steer => {
                self.steer(&session_key, input)?;
                session
                    .active_turn
                    .lock()
                    .expect("engine active turn")
                    .as_ref()
                    .map(|turn| turn.id.to_string())
                    .ok_or_else(|| {
                        RuntimeError::Unsupported("nothing is running to steer into".into())
                    })
            }
            Delivery::Start => {
                if session.running.load(Ordering::Relaxed) {
                    return Err(RuntimeError::Unsupported(
                        "a turn is already running".into(),
                    ));
                }
                let provider_thread = session
                    .provider_thread
                    .lock()
                    .expect("engine provider thread")
                    .clone();
                let message_id = format!(
                    "engine-message-{}",
                    session.next_message.fetch_add(1, Ordering::Relaxed) + 1
                );
                let ordinal = session.next_turn.fetch_add(1, Ordering::Relaxed) + 1;
                let provider_turn = session
                    .adapter
                    .start_turn(AdapterTurnInput {
                        thread_id: ThreadId::new(
                            provider_thread
                                .app_thread_id
                                .as_ref()
                                .map(ToString::to_string)
                                .unwrap_or_else(|| "engine-app-thread".into()),
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
                    })
                    .map_err(RuntimeError::failed)?;
                let turn = provider_turn.id.to_string();
                *session.active_turn.lock().expect("engine active turn") = Some(provider_turn);
                session.running.store(true, Ordering::Relaxed);
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
            .active_turn
            .lock()
            .expect("engine active turn")
            .clone()
            .ok_or_else(|| RuntimeError::Unsupported("nothing is running to steer into".into()))?;
        let provider_thread = session
            .provider_thread
            .lock()
            .expect("engine provider thread")
            .clone();
        let message_id = format!(
            "engine-message-{}",
            session.next_message.fetch_add(1, Ordering::Relaxed) + 1
        );
        session
            .adapter
            .steer_turn(SteerInput {
                thread_id: ThreadId::new(
                    provider_thread
                        .app_thread_id
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "engine-app-thread".into()),
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
        let Some(turn) = session
            .active_turn
            .lock()
            .expect("engine active turn")
            .clone()
        else {
            return Ok(());
        };
        let provider_thread = session
            .provider_thread
            .lock()
            .expect("engine provider thread")
            .clone();
        session
            .adapter
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
            .adapter
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
        let mut outcomes = session.outcomes.lock().expect("engine outcomes");
        loop {
            let wanted = turn
                .cloned()
                .or_else(|| {
                    session
                        .active_turn
                        .lock()
                        .expect("engine active turn")
                        .as_ref()
                        .map(|turn| turn.id.to_string())
                })
                .or_else(|| outcomes.keys().next().cloned());
            if let Some(turn) = wanted
                && let Some(outcome) = outcomes.get(&turn)
            {
                return Ok(outcome.clone());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(RuntimeError::Timeout(timeout_ms));
            }
            let (guard, _) = self
                .inner
                .turn_ended
                .wait_timeout(outcomes, deadline - now)
                .expect("engine wait");
            outcomes = guard;
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
                running: session.running.load(Ordering::Relaxed),
                updated_at_ms: session.updated_at_ms.load(Ordering::Relaxed),
            })
            .collect())
    }

    fn limits(&self, _agent: &str) -> Result<Option<RateLimits>, RuntimeError> {
        // The typed seam is `ProviderSessionRuntime::usage_limits` (item 26).
        // Neither the Codex adapter (item 22) nor the Claude adapter (item 23)
        // implements it yet, so there is no report to project to `RateLimits`.
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

fn model_selection(spec: &SessionSpec) -> ModelSelection {
    let model = spec.model.clone().unwrap_or_else(|| "gpt-5".to_string());
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
        model,
        options: (!options.is_empty()).then_some(ProviderOptionSelections(options)),
    }
}

fn runtime_policy(spec: &SessionSpec) -> RuntimePolicy {
    let runtime_mode = match spec.access_mode.as_deref().unwrap_or("auto") {
        "read-only" | "readOnly" | "untrusted" | "approval-required" => {
            RuntimeMode::ApprovalRequired
        }
        "auto-accept-edits" => RuntimeMode::AutoAcceptEdits,
        "full-access" | "fullAccess" | "never" => RuntimeMode::FullAccess,
        _ => RuntimeMode::Auto,
    };
    RuntimePolicy {
        runtime_mode,
        interaction_mode: Default::default(),
        cwd: Some(spec.folder.display().to_string()),
        approval_policy: None,
        sandbox_policy: None,
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

    #[test]
    fn access_modes_map_to_runtime_modes() {
        let mut spec = SessionSpec {
            agent: "codex".into(),
            model: Some("mock-1".into()),
            effort: Some("high".into()),
            service_tier: Some("priority".into()),
            folder: PathBuf::from("/tmp/work"),
            worktree: None,
            device: None,
            mcp_servers: Vec::new(),
            first_message: None,
            access_mode: Some("read-only".into()),
        };
        assert_eq!(
            runtime_policy(&spec).runtime_mode,
            RuntimeMode::ApprovalRequired
        );
        spec.access_mode = Some("full-access".into());
        assert_eq!(runtime_policy(&spec).runtime_mode, RuntimeMode::FullAccess);
        spec.access_mode = Some("wat".into());
        assert_eq!(runtime_policy(&spec).runtime_mode, RuntimeMode::Auto);

        let selection = model_selection(&spec);
        assert_eq!(selection.model, "mock-1");
        let options = selection.options.unwrap();
        assert_eq!(options.0[0].id, "reasoningEffort");
        assert_eq!(options.0[1].value.as_str(), Some("priority"));
    }
}
