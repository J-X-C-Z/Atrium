//! Today's agent clients behind [`atrium_runtime::AgentRuntime`].
//!
//! The project layer (scheduler, pools, workflow runner, team) speaks to this
//! seam instead of holding a `CodexClient` or `AcpClient`. The clients stay in
//! `workspace.rs` / `workspace_acp.rs`, which register them here; this file
//! only translates the trait calls into the clients' own calls and keeps the
//! session bookkeeping the trait needs (`wait`, `transcript`, `limits`).
//!
//! Events already travel through the app's pumps so the existing
//! `AgentEvent` -> `Transcript` folding stays where it was. Each pump tells
//! this adapter what it saw with [`RuntimeAdapters::observe`], which lets
//! `wait` block on `TurnCompleted`. Events the adapter itself opens or closes
//! (an ACP prompt's `TurnStarted` / `TurnCompleted`) go to the session's
//! `sink` and into the app through the same handler.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

use acp_client::{AcpClient, PromptDelivery};
use atrium_core::{
    AgentEvent, ApprovalDecision, Capabilities, ConfigOption, RateLimits, RequestId, Transcript,
};
use atrium_runtime::{
    AgentRuntime, Delivery, EventSink, RuntimeError, SessionId, SessionSpec, SessionSummary,
    TurnId, TurnInput, TurnOutcome,
};
use codex_client::{AccessMode, CodexClient, StartThread, TurnRequest};

use crate::agents;

/// The clients the app has connected, and the sessions started through them.
///
/// Cheap to clone: all state is behind one `Arc`.
#[derive(Clone)]
pub struct RuntimeAdapters {
    inner: Arc<Inner>,
}

struct Inner {
    codex: Mutex<HashMap<String, RegisteredCodex>>,
    acp: Mutex<HashMap<String, RegisteredAcp>>,
    sessions: Mutex<HashMap<SessionId, Session>>,
    /// Paired with `sessions`: `wait` sleeps here until a turn completes.
    turn_ended: Condvar,
    caps: Mutex<HashMap<String, Capabilities>>,
}

#[derive(Clone)]
struct RegisteredCodex {
    client: CodexClient,
    /// Environment variables the project's turns run with.
    env: Vec<(String, String)>,
}

#[derive(Clone)]
struct RegisteredAcp {
    client: AcpClient,
}

struct Session {
    /// Backend id exactly as the app names it (`codex`, `codex@nas`, ...).
    agent: String,
    device: Option<String>,
    folder: PathBuf,
    title: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    service_tier: Option<String>,
    access: AccessMode,
    transcript: Transcript,
    sink: EventSink,
    active_turn: Option<TurnId>,
    outcomes: HashMap<TurnId, TurnOutcome>,
    last_outcome: Option<TurnOutcome>,
    running: bool,
    updated_at_ms: i64,
    /// ACP's per-session settings, for the app to show.
    acp_options: Vec<ConfigOption>,
}

impl Inner {
    fn observe(&self, event: &AgentEvent) {
        let Some(thread) = event.thread_id() else {
            return;
        };
        let mut sessions = self.sessions.lock().expect("runtime sessions");
        let Some(session) = sessions.get_mut(thread) else {
            return;
        };
        session.transcript.apply(event);
        session.updated_at_ms = now_ms();
        match event {
            AgentEvent::TurnStarted { turn_id, .. } => {
                session.active_turn = Some(turn_id.clone());
                session.running = true;
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
                session.outcomes.insert(turn_id.clone(), outcome.clone());
                session.last_outcome = Some(outcome);
                if session.active_turn.as_deref() == Some(turn_id.as_str()) {
                    session.active_turn = None;
                }
                session.running = false;
                self.turn_ended.notify_all();
            }
            AgentEvent::ThreadNameUpdated { name, .. } => {
                session.title = name.clone();
            }
            _ => {}
        }
    }

    /// Record what happened and hand the event to the session's sink.
    fn dispatch(&self, event: AgentEvent) {
        self.observe(&event);
        let sink = event.thread_id().and_then(|thread| {
            self.sessions
                .lock()
                .expect("runtime sessions")
                .get(thread)
                .map(|session| session.sink.clone())
        });
        if let Some(sink) = sink {
            sink(event);
        }
    }
}

impl RuntimeAdapters {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                codex: Mutex::new(HashMap::new()),
                acp: Mutex::new(HashMap::new()),
                sessions: Mutex::new(HashMap::new()),
                turn_ended: Condvar::new(),
                caps: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Registers (or replaces) this machine's or a device's Codex.
    pub fn register_codex(&self, backend: &str, client: CodexClient, env: Vec<(String, String)>) {
        self.inner
            .codex
            .lock()
            .expect("runtime codex")
            .insert(backend.to_string(), RegisteredCodex { client, env });
        self.inner
            .caps
            .lock()
            .expect("runtime caps")
            .insert(backend.to_string(), Capabilities::codex());
    }

    /// Registers an ACP agent, local or reached through a node.
    pub fn register_acp(&self, backend: &str, client: AcpClient) {
        let mut caps = Capabilities::acp();
        caps.images = client.info.images;
        caps.steer = client.info.steering;
        caps.interrupt = true;
        caps.slash_commands = true;
        self.inner
            .acp
            .lock()
            .expect("runtime acp")
            .insert(backend.to_string(), RegisteredAcp { client });
        self.inner
            .caps
            .lock()
            .expect("runtime caps")
            .insert(backend.to_string(), caps);
    }

    /// Forgets a backend that disconnected; its sessions stay for the app to
    /// see, but calls on them now fail like a stopped client.
    pub fn forget_backend(&self, backend: &str) {
        self.inner
            .codex
            .lock()
            .expect("runtime codex")
            .remove(backend);
        self.inner.acp.lock().expect("runtime acp").remove(backend);
        self.inner
            .caps
            .lock()
            .expect("runtime caps")
            .remove(backend);
    }

    /// Whether this runtime started `session` (so the project layer may use
    /// the trait for it rather than the chat path).
    pub fn has_session(&self, session: &str) -> bool {
        self.inner
            .sessions
            .lock()
            .expect("runtime sessions")
            .contains_key(session)
    }

    /// The settings an ACP session offered when it started.
    pub fn session_options(&self, session: &str) -> Option<Vec<ConfigOption>> {
        self.inner
            .sessions
            .lock()
            .expect("runtime sessions")
            .get(session)
            .map(|session| session.acp_options.clone())
    }

    /// Records what an app pump translated. Keeps `wait` and `transcript`
    /// honest for every backend without moving the folding out of the app.
    pub fn observe(&self, _backend: &str, event: &AgentEvent) {
        self.inner.observe(event);
    }

    fn codex_client(&self, backend: &str) -> Result<RegisteredCodex, RuntimeError> {
        self.inner
            .codex
            .lock()
            .expect("runtime codex")
            .get(backend)
            .cloned()
            .ok_or_else(|| RuntimeError::Unsupported(format!("{backend} is not connected")))
    }

    fn acp_client(&self, backend: &str) -> Result<AcpClient, RuntimeError> {
        self.inner
            .acp
            .lock()
            .expect("runtime acp")
            .get(backend)
            .map(|registered| registered.client.clone())
            .ok_or_else(|| RuntimeError::Unsupported(format!("{backend} is not connected")))
    }

    fn start_codex(&self, spec: SessionSpec, sink: EventSink) -> Result<SessionId, RuntimeError> {
        let registered = self.codex_client(&spec.agent)?;
        let access = access_mode(spec.access_mode.as_deref());
        let options = StartThread {
            cwd: Some(spec.folder.display().to_string()),
            model: spec.model.clone(),
            approval_policy: Some(access.approval_policy().into()),
            sandbox: Some(access.sandbox_mode().into()),
            mcp_servers: spec.mcp_servers.clone(),
            env: registered.env.clone(),
            developer_instructions: None,
        };
        let meta = futures::executor::block_on(registered.client.thread_start(options))
            .map_err(RuntimeError::failed)?;
        let session = session_from_spec(&spec, sink, meta.name.clone(), Vec::new());
        let id = meta.id;
        self.insert_session(id.clone(), session);
        if let Some(input) = spec.first_message
            && !input.is_empty()
            && let Err(error) = self.send(&id, input, Delivery::Start)
        {
            self.inner
                .sessions
                .lock()
                .expect("runtime sessions")
                .remove(&id);
            return Err(error);
        }
        Ok(id)
    }

    fn start_acp(&self, spec: SessionSpec, sink: EventSink) -> Result<SessionId, RuntimeError> {
        let client = self.acp_client(&spec.agent)?;
        let start =
            futures::executor::block_on(client.new_session_with(&spec.folder, &spec.mcp_servers))
                .map_err(RuntimeError::failed)?;
        let session = session_from_spec(&spec, sink, None, start.options);
        let id = start.session_id;
        self.insert_session(id.clone(), session);
        if let Some(input) = spec.first_message
            && !input.is_empty()
            && let Err(error) = self.send(&id, input, Delivery::Start)
        {
            self.inner
                .sessions
                .lock()
                .expect("runtime sessions")
                .remove(&id);
            return Err(error);
        }
        Ok(id)
    }

    fn insert_session(&self, id: SessionId, session: Session) {
        self.inner
            .sessions
            .lock()
            .expect("runtime sessions")
            .insert(id, session);
    }

    fn session_snapshot(
        &self,
        session: &SessionId,
    ) -> Result<(String, Option<TurnId>, bool), RuntimeError> {
        let sessions = self.inner.sessions.lock().expect("runtime sessions");
        let session = sessions
            .get(session)
            .ok_or_else(|| RuntimeError::UnknownSession(session.clone()))?;
        Ok((
            session.agent.clone(),
            session.active_turn.clone(),
            session.running,
        ))
    }

    fn send_codex(
        &self,
        session: &SessionId,
        input: TurnInput,
        delivery: Delivery,
    ) -> Result<TurnId, RuntimeError> {
        let (agent, active, _) = self.session_snapshot(session)?;
        let registered = self.codex_client(&agent)?;
        let request = {
            let sessions = self.inner.sessions.lock().expect("runtime sessions");
            let state = sessions
                .get(session)
                .ok_or_else(|| RuntimeError::UnknownSession(session.clone()))?;
            turn_request(state, &input)
        };
        let turn = match delivery {
            Delivery::Steer => {
                let turn = active.ok_or_else(|| {
                    RuntimeError::Unsupported("nothing is running to steer into".into())
                })?;
                futures::executor::block_on(registered.client.turn_steer(session, &turn, request))
                    .map_err(RuntimeError::failed)?;
                turn
            }
            Delivery::Start => {
                futures::executor::block_on(registered.client.turn_start(session, request))
                    .map_err(RuntimeError::failed)?
            }
            Delivery::Queue => {
                return Err(RuntimeError::Unsupported(
                    "this runtime does not hold queued messages".into(),
                ));
            }
        };
        self.record_started_turn(session, &turn);
        Ok(turn)
    }

    fn send_acp(
        &self,
        session: &SessionId,
        input: TurnInput,
        delivery: Delivery,
    ) -> Result<TurnId, RuntimeError> {
        let (agent, active, _) = self.session_snapshot(session)?;
        let client = self.acp_client(&agent)?;
        if delivery == Delivery::Queue {
            return Err(RuntimeError::Unsupported(
                "this runtime does not hold queued messages".into(),
            ));
        }
        if delivery == Delivery::Steer {
            if !client.info.steering {
                return Err(RuntimeError::Unsupported(format!(
                    "{agent} cannot steer a running turn"
                )));
            }
            if let Some(turn) = active {
                self.run_acp_prompt(agent, client, session.clone(), input);
                return Ok(turn);
            }
        }
        if active.is_some() {
            return Err(RuntimeError::Unsupported(format!(
                "{agent} is still working"
            )));
        }
        let (turn, events) = client.begin_turn(session);
        self.record_started_turn(session, &turn);
        for event in events {
            self.inner.dispatch(event);
        }
        self.run_acp_prompt(agent, client, session.clone(), input);
        Ok(turn)
    }

    /// Sends an ACP prompt on its own thread; `prompt` ends the turn.
    fn run_acp_prompt(
        &self,
        backend: String,
        client: AcpClient,
        session: SessionId,
        input: TurnInput,
    ) {
        let inner = self.inner.clone();
        let result = std::thread::Builder::new()
            .name(format!("{backend}-prompt"))
            .spawn(move || {
                let outcome = futures::executor::block_on(client.prompt_with_outcome(
                    &session,
                    &input.text,
                    &input.images,
                    &input.attachments,
                ));
                for event in outcome.events {
                    inner.dispatch(event);
                }
                if let PromptDelivery::Uncertain { error } = outcome.delivery {
                    inner.dispatch(AgentEvent::Error {
                        thread_id: Some(session),
                        message: error.to_string(),
                        will_retry: false,
                    });
                }
            });
        if let Err(error) = result {
            log::warn!("could not send an ACP prompt: {error}");
        }
    }

    fn record_started_turn(&self, session: &SessionId, turn: &TurnId) {
        let mut sessions = self.inner.sessions.lock().expect("runtime sessions");
        if let Some(state) = sessions.get_mut(session) {
            // A fast provider may already have completed this turn.
            if !state.outcomes.contains_key(turn) {
                state.active_turn = Some(turn.clone());
                state.running = true;
            }
            state.updated_at_ms = now_ms();
        }
    }
}

impl Default for RuntimeAdapters {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentRuntime for RuntimeAdapters {
    fn capabilities(&self, agent: &str) -> Capabilities {
        if let Some(caps) = self
            .inner
            .caps
            .lock()
            .expect("runtime caps")
            .get(agent)
            .cloned()
        {
            return caps;
        }
        crate::capabilities::for_backend(agent)
    }

    fn start(&self, spec: SessionSpec, sink: EventSink) -> Result<SessionId, RuntimeError> {
        if agents::is_codex(&spec.agent) {
            self.start_codex(spec, sink)
        } else {
            self.start_acp(spec, sink)
        }
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
        let (agent, _, _) = self.session_snapshot(session)?;
        if agents::is_codex(&agent) {
            self.send_codex(session, input, delivery)
        } else {
            self.send_acp(session, input, delivery)
        }
    }

    fn steer(&self, session: &SessionId, input: TurnInput) -> Result<(), RuntimeError> {
        if input.is_empty() {
            return Err(RuntimeError::Failed("the message is empty".into()));
        }
        let (agent, active, _) = self.session_snapshot(session)?;
        if agents::is_codex(&agent) {
            let registered = self.codex_client(&agent)?;
            let turn = active.ok_or_else(|| {
                RuntimeError::Unsupported("nothing is running to steer into".into())
            })?;
            let request = {
                let sessions = self.inner.sessions.lock().expect("runtime sessions");
                let state = sessions
                    .get(session)
                    .ok_or_else(|| RuntimeError::UnknownSession(session.clone()))?;
                turn_request(state, &input)
            };
            futures::executor::block_on(registered.client.turn_steer(session, &turn, request))
                .map_err(RuntimeError::failed)?;
            Ok(())
        } else {
            let client = self.acp_client(&agent)?;
            if !client.info.steering {
                return Err(RuntimeError::Unsupported(format!(
                    "{agent} cannot steer a running turn"
                )));
            }
            if active.is_none() {
                return Err(RuntimeError::Unsupported(
                    "nothing is running to steer into".into(),
                ));
            }
            self.run_acp_prompt(agent, client, session.clone(), input);
            Ok(())
        }
    }

    fn interrupt(&self, session: &SessionId) -> Result<(), RuntimeError> {
        let (agent, active, _) = self.session_snapshot(session)?;
        if agents::is_codex(&agent) {
            let Some(turn) = active else {
                return Ok(());
            };
            let registered = self.codex_client(&agent)?;
            futures::executor::block_on(registered.client.turn_interrupt(session, &turn))
                .map_err(RuntimeError::failed)
        } else {
            let client = self.acp_client(&agent)?;
            client.cancel(session).map_err(RuntimeError::failed)
        }
    }

    fn answer_approval(
        &self,
        session: &SessionId,
        request: RequestId,
        decision: ApprovalDecision,
    ) -> Result<(), RuntimeError> {
        let (agent, _, _) = self.session_snapshot(session)?;
        if agents::is_codex(&agent) {
            let registered = self.codex_client(&agent)?;
            registered
                .client
                .answer_approval(request, decision)
                .map_err(RuntimeError::failed)
        } else {
            let client = self.acp_client(&agent)?;
            client
                .answer_permission(request, decision)
                .map_err(RuntimeError::failed)
        }
    }

    fn wait(
        &self,
        session: &SessionId,
        turn: Option<&TurnId>,
        timeout_ms: u64,
    ) -> Result<TurnOutcome, RuntimeError> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let mut sessions = self.inner.sessions.lock().expect("runtime sessions");
        loop {
            let state = sessions
                .get(session)
                .ok_or_else(|| RuntimeError::UnknownSession(session.clone()))?;
            let wanted = turn
                .cloned()
                .or_else(|| state.active_turn.clone())
                .or_else(|| state.last_outcome.as_ref().map(|out| out.turn_id.clone()));
            if let Some(turn) = wanted
                && let Some(outcome) = state.outcomes.get(&turn)
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
                .wait_timeout(sessions, deadline - now)
                .expect("runtime wait");
            sessions = guard;
        }
    }

    fn transcript(&self, session: &SessionId) -> Result<Transcript, RuntimeError> {
        self.inner
            .sessions
            .lock()
            .expect("runtime sessions")
            .get(session)
            .map(|state| state.transcript.clone())
            .ok_or_else(|| RuntimeError::UnknownSession(session.clone()))
    }

    fn sessions(&self) -> Result<Vec<SessionSummary>, RuntimeError> {
        let sessions = self.inner.sessions.lock().expect("runtime sessions");
        Ok(sessions
            .iter()
            .map(|(id, state)| SessionSummary {
                session_id: id.clone(),
                agent: state.agent.clone(),
                folder: state.folder.clone(),
                device: state.device.clone(),
                title: state.title.clone(),
                running: state.running,
                updated_at_ms: state.updated_at_ms,
            })
            .collect())
    }

    fn limits(&self, agent: &str) -> Result<Option<RateLimits>, RuntimeError> {
        if !agents::is_codex(agent) {
            return Ok(None);
        }
        let registered = self.codex_client(agent)?;
        futures::executor::block_on(registered.client.rate_limits())
            .map(Some)
            .map_err(RuntimeError::failed)
    }
}

fn session_from_spec(
    spec: &SessionSpec,
    sink: EventSink,
    title: Option<String>,
    acp_options: Vec<ConfigOption>,
) -> Session {
    Session {
        agent: spec.agent.clone(),
        device: spec.device.clone(),
        folder: spec.folder.clone(),
        title: title.or_else(|| {
            spec.first_message
                .as_ref()
                .map(|input| input.text.chars().take(60).collect())
        }),
        model: spec.model.clone(),
        effort: spec.effort.clone(),
        service_tier: spec.service_tier.clone(),
        access: access_mode(spec.access_mode.as_deref()),
        transcript: Transcript::new(),
        sink,
        active_turn: None,
        outcomes: HashMap::new(),
        last_outcome: None,
        running: false,
        updated_at_ms: now_ms(),
        acp_options,
    }
}

fn turn_request(state: &Session, input: &TurnInput) -> TurnRequest {
    TurnRequest {
        text: input.text.clone(),
        images: input.images.clone(),
        mentions: input.attachments.clone(),
        model: state.model.clone(),
        effort: state.effort.clone(),
        mode: Some(state.access),
        service_tier: state.service_tier.clone(),
        plan_mode: false,
    }
}

fn access_mode(name: Option<&str>) -> AccessMode {
    match name.unwrap_or_default() {
        "read-only" | "readOnly" | "untrusted" => AccessMode::ReadOnly,
        "full-access" | "fullAccess" | "never" => AccessMode::FullAccess,
        _ => AccessMode::Auto,
    }
}

/// The name [`SessionSpec::access_mode`] takes for an app access mode.
pub fn access_mode_name(mode: AccessMode) -> &'static str {
    match mode {
        AccessMode::ReadOnly => "read-only",
        AccessMode::Auto => "auto",
        AccessMode::FullAccess => "full-access",
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

// The scheduler and send paths go through these small functions, so their
// tests can drive them with `atrium_runtime::MockRuntime` alone.

pub(crate) fn start_session(
    runtime: &dyn AgentRuntime,
    spec: SessionSpec,
    sink: EventSink,
) -> Result<SessionId, RuntimeError> {
    runtime.start(spec, sink)
}

pub(crate) fn send_turn(
    runtime: &dyn AgentRuntime,
    session: &SessionId,
    input: TurnInput,
    delivery: Delivery,
) -> Result<TurnId, RuntimeError> {
    runtime.send(session, input, delivery)
}

pub(crate) fn steer_turn(
    runtime: &dyn AgentRuntime,
    session: &SessionId,
    input: TurnInput,
) -> Result<(), RuntimeError> {
    runtime.steer(session, input)
}

pub(crate) fn interrupt_turn(
    runtime: &dyn AgentRuntime,
    session: &SessionId,
) -> Result<(), RuntimeError> {
    runtime.interrupt(session)
}

#[cfg(test)]
pub(crate) fn wait_turn(
    runtime: &dyn AgentRuntime,
    session: &SessionId,
    turn: Option<&TurnId>,
    timeout_ms: u64,
) -> Result<TurnOutcome, RuntimeError> {
    runtime.wait(session, turn, timeout_ms)
}

#[cfg(test)]
pub(crate) fn limits_of(
    runtime: &dyn AgentRuntime,
    agent: &str,
) -> Result<Option<RateLimits>, RuntimeError> {
    runtime.limits(agent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use atrium_core::TurnStatus;
    use atrium_runtime::MockRuntime;

    fn spec() -> SessionSpec {
        SessionSpec {
            agent: "codex".into(),
            model: Some("gpt-6".into()),
            effort: Some("high".into()),
            service_tier: None,
            folder: PathBuf::from("/tmp/project"),
            worktree: None,
            device: None,
            mcp_servers: Vec::new(),
            first_message: None,
            access_mode: Some("auto".into()),
        }
    }

    #[test]
    fn the_scheduler_seam_drives_start_send_steer_interrupt_wait_and_limits() {
        let runtime = MockRuntime::default();
        let sink: EventSink = Arc::new(|_| {});
        let session = start_session(&runtime, spec(), sink).expect("start");

        let turn = send_turn(
            &runtime,
            &session,
            TurnInput::text("kickoff"),
            Delivery::Start,
        )
        .expect("send");
        steer_turn(&runtime, &session, TurnInput::text("and this")).expect("steer");
        interrupt_turn(&runtime, &session).expect("interrupt");
        let outcome = wait_turn(&runtime, &session, Some(&turn), 10).expect("wait");
        assert_eq!(outcome.status, TurnStatus::Completed);
        assert!(limits_of(&runtime, "codex").expect("limits").is_none());

        assert_eq!(runtime.started().len(), 1);
        assert_eq!(runtime.started()[0].agent, "codex");
        assert_eq!(runtime.started()[0].folder, spec().folder);
        let sent = runtime.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].input.text, "kickoff");
        assert_eq!(sent[0].delivery, Delivery::Start);
    }

    #[test]
    fn access_mode_names_round_trip() {
        for mode in AccessMode::ALL {
            assert_eq!(access_mode(Some(access_mode_name(mode))), mode);
        }
        assert_eq!(access_mode(None), AccessMode::Auto);
    }
}
