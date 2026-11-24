//! The seam between Atrium's project layer and an agent runtime.
//!
//! The scheduler, the workflow runner and the team pick talk to this trait;
//! they never hold a Codex, ACP or node client. Track A (today's clients) and
//! track B (an engine ported from T3 Code) both implement it, so the layers
//! above are written once. Nothing here touches GPUI or a provider protocol.
//!
//! See docs/native-parity-implementation.md item 32.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use atrium_core::{
    AgentEvent, ApprovalDecision, Capabilities, McpServer, RateLimits, RequestId, Transcript,
    TurnStatus,
};
use serde::{Deserialize, Serialize};

/// One agent session. Opaque to callers: only the runtime that made it reads it.
pub type SessionId = String;
/// One turn in a session.
pub type TurnId = String;

/// How a message enters a running session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// A normal next turn.
    Start,
    /// Into the turn that is running now; refused when the agent cannot steer.
    Steer,
    /// Held until the running turn ends (the queue).
    Queue,
}

/// What the user sends: text plus files it carries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnInput {
    pub text: String,
    /// Images pasted or attached, as local paths.
    pub images: Vec<PathBuf>,
    /// Other files attached to the message.
    pub attachments: Vec<PathBuf>,
}

impl TurnInput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.images.is_empty() && self.attachments.is_empty()
    }
}

/// A worktree the session should work in, instead of the project folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSpec {
    pub branch: String,
    /// An existing checkout; `None` asks the runtime to create one.
    pub path: Option<PathBuf>,
}

/// Everything a runtime needs to start one session.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionSpec {
    /// Backend id as the app names it: `codex`, `codex@nas`, `claude`, ...
    pub agent: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Fast/priority tier, when the agent offers one.
    pub service_tier: Option<String>,
    /// The folder the agent works in.
    pub folder: PathBuf,
    pub worktree: Option<WorktreeSpec>,
    /// Another computer to run on, through the node.
    pub device: Option<String>,
    /// MCP servers this session must have.
    pub mcp_servers: Vec<McpServer>,
    /// The first message, when the session starts with one.
    pub first_message: Option<TurnInput>,
    /// The agent's own approval/access mode name, when it has a choice.
    pub access_mode: Option<String>,
}

/// A session a caller can list without knowing the runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub agent: String,
    pub folder: PathBuf,
    pub device: Option<String>,
    pub title: Option<String>,
    /// A turn is running now.
    pub running: bool,
    /// Unix milliseconds, supplied by the runtime (callers never read the clock).
    pub updated_at_ms: i64,
}

/// How one turn ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnOutcome {
    pub turn_id: TurnId,
    pub status: TurnStatus,
    pub error: Option<String>,
}

/// A runtime error the project layer can act on.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// The agent or device cannot do this (no steering, no rewind, ...).
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    Failed(String),
    #[error("unknown session {0}")]
    UnknownSession(String),
    #[error("timed out after {0} ms")]
    Timeout(u64),
}

impl RuntimeError {
    pub fn failed(error: impl std::fmt::Display) -> Self {
        Self::Failed(error.to_string())
    }
}

/// Where a runtime sends the events it translated. Cheap to clone and call
/// from a provider thread.
pub type EventSink = Arc<dyn Fn(AgentEvent) + Send + Sync>;

/// What the project layer may ask of any runtime.
///
/// Calls are short (they enqueue work); provider output arrives through the
/// [`EventSink`] given to [`start`](Self::start). [`wait`](Self::wait) is the
/// one blocking call, for callers without an event loop (the scheduler, the
/// workflow runner).
pub trait AgentRuntime: Send + Sync {
    /// What this agent supports, before or after it connects.
    fn capabilities(&self, agent: &str) -> Capabilities;

    /// Start a session and stream its events into `sink`.
    fn start(&self, spec: SessionSpec, sink: EventSink) -> Result<SessionId, RuntimeError>;

    /// Send a message. Returns the turn it starts (or joins, when steering).
    fn send(
        &self,
        session: &SessionId,
        input: TurnInput,
        delivery: Delivery,
    ) -> Result<TurnId, RuntimeError>;

    /// Put a message into the running turn.
    fn steer(&self, session: &SessionId, input: TurnInput) -> Result<(), RuntimeError>;

    /// Stop the running turn.
    fn interrupt(&self, session: &SessionId) -> Result<(), RuntimeError>;

    /// Answer a permission request.
    fn answer_approval(
        &self,
        session: &SessionId,
        request: RequestId,
        decision: ApprovalDecision,
    ) -> Result<(), RuntimeError>;

    /// Block until the given turn (or the newest) ends.
    fn wait(
        &self,
        session: &SessionId,
        turn: Option<&TurnId>,
        timeout_ms: u64,
    ) -> Result<TurnOutcome, RuntimeError>;

    /// The conversation as folded so far.
    fn transcript(&self, session: &SessionId) -> Result<Transcript, RuntimeError>;

    /// Sessions this runtime knows about.
    fn sessions(&self) -> Result<Vec<SessionSummary>, RuntimeError>;

    /// The agent's plan windows, when it reports them.
    fn limits(&self, agent: &str) -> Result<Option<RateLimits>, RuntimeError>;
}

/// A runtime that records calls and ends every turn at once.
///
/// The scheduler and workflow tests use it; it never spawns a process.
#[derive(Clone, Debug)]
pub struct MockRuntime {
    inner: Arc<Mutex<MockState>>,
    caps: Capabilities,
}

#[derive(Debug, Default)]
struct MockState {
    next: u64,
    started: Vec<SessionSpec>,
    sent: Vec<SentMessage>,
}

/// One message the mock received.
#[derive(Clone, Debug, PartialEq)]
pub struct SentMessage {
    pub session: SessionId,
    pub input: TurnInput,
    pub delivery: Delivery,
}

impl MockRuntime {
    pub fn new(caps: Capabilities) -> Self {
        Self {
            inner: Arc::new(Mutex::new(MockState::default())),
            caps,
        }
    }

    pub fn started(&self) -> Vec<SessionSpec> {
        self.inner
            .lock()
            .expect("mock runtime poisoned")
            .started
            .clone()
    }

    pub fn sent(&self) -> Vec<SentMessage> {
        self.inner
            .lock()
            .expect("mock runtime poisoned")
            .sent
            .clone()
    }
}

impl Default for MockRuntime {
    fn default() -> Self {
        Self::new(Capabilities::everything())
    }
}

impl AgentRuntime for MockRuntime {
    fn capabilities(&self, _agent: &str) -> Capabilities {
        self.caps.clone()
    }

    fn start(&self, spec: SessionSpec, _sink: EventSink) -> Result<SessionId, RuntimeError> {
        let mut state = self.inner.lock().expect("mock runtime poisoned");
        state.next += 1;
        let id = format!("mock-session-{}", state.next);
        state.started.push(spec);
        Ok(id)
    }

    fn send(
        &self,
        session: &SessionId,
        input: TurnInput,
        delivery: Delivery,
    ) -> Result<TurnId, RuntimeError> {
        let mut state = self.inner.lock().expect("mock runtime poisoned");
        state.sent.push(SentMessage {
            session: session.clone(),
            input,
            delivery,
        });
        Ok(format!("mock-turn-{}", state.sent.len()))
    }

    fn steer(&self, _session: &SessionId, _input: TurnInput) -> Result<(), RuntimeError> {
        Ok(())
    }

    fn interrupt(&self, _session: &SessionId) -> Result<(), RuntimeError> {
        Ok(())
    }

    fn answer_approval(
        &self,
        _session: &SessionId,
        _request: RequestId,
        _decision: ApprovalDecision,
    ) -> Result<(), RuntimeError> {
        Ok(())
    }

    fn wait(
        &self,
        _session: &SessionId,
        turn: Option<&TurnId>,
        _timeout_ms: u64,
    ) -> Result<TurnOutcome, RuntimeError> {
        Ok(TurnOutcome {
            turn_id: turn.map_or("mock-turn", |turn| turn.as_str()).to_string(),
            status: TurnStatus::Completed,
            error: None,
        })
    }

    fn transcript(&self, _session: &SessionId) -> Result<Transcript, RuntimeError> {
        Ok(Transcript::default())
    }

    fn sessions(&self) -> Result<Vec<SessionSummary>, RuntimeError> {
        let state = self.inner.lock().expect("mock runtime poisoned");
        Ok(state
            .started
            .iter()
            .enumerate()
            .map(|(ix, spec)| SessionSummary {
                session_id: format!("mock-session-{}", ix + 1),
                agent: spec.agent.clone(),
                folder: spec.folder.clone(),
                device: spec.device.clone(),
                title: spec.first_message.as_ref().map(|input| input.text.clone()),
                running: false,
                updated_at_ms: 0,
            })
            .collect())
    }

    fn limits(&self, _agent: &str) -> Result<Option<RateLimits>, RuntimeError> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            first_message: Some(TurnInput::text("hello")),
            access_mode: None,
        }
    }

    #[test]
    fn mock_records_what_the_project_layer_asked() {
        let runtime = MockRuntime::default();
        let session = runtime.start(spec(), Arc::new(|_| {})).expect("start");
        let turn = runtime
            .send(&session, TurnInput::text("next"), Delivery::Start)
            .expect("send");
        assert_eq!(turn, "mock-turn-1");
        assert_eq!(runtime.started()[0].agent, "codex");
        assert_eq!(runtime.sent()[0].input.text, "next");
        assert_eq!(runtime.sessions().expect("sessions").len(), 1);
        let outcome = runtime.wait(&session, Some(&turn), 10).expect("wait");
        assert_eq!(outcome.status, TurnStatus::Completed);
    }

    #[test]
    fn empty_input_is_empty() {
        assert!(TurnInput::default().is_empty());
        assert!(TurnInput::text("  ").is_empty());
        assert!(!TurnInput::text("hi").is_empty());
    }
}
