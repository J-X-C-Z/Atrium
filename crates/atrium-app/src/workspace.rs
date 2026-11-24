//! The app's state: one connection to Codex, the threads it knows, and a
//! transcript per open thread. Views read it and call its methods; they never
//! talk to the backend themselves.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use atrium_core::{
    Account, AgentCommand, AgentEvent, ApprovalDecision, ConfigOption, Goal, GoalStatus, Item,
    RateLimits, RequestId, ThreadMeta, Transcript, TurnStatus,
};
use atrium_runtime::{AgentRuntime, Delivery, EventSink, TurnInput};
use codex_client::{
    AccessMode, Answers, CodexClient, CodexConfig, Incoming, ModelInfo, Prediction, ReviewTarget,
    StartThread, TurnRequest, translate,
};
use futures::StreamExt;
use gpui::{AppContext, Context, EventEmitter, Task};

use crate::runtime_adapters::RuntimeAdapters;

use crate::i18n::{t, t_any, tf};
use crate::settings::{Settings, ThemePref};

#[path = "notice_dispatch.rs"]
mod notice_dispatch;
use notice_dispatch::{NoticeSendResult, NoticeTarget, codex_notice_outcome};

#[path = "workspace_acp.rs"]
mod acp;
#[path = "workspace_chat_trees.rs"]
mod chat_trees;
#[path = "workspace_checkpoints.rs"]
mod checkpoints;
#[path = "workspace_devices.rs"]
mod devices;
#[path = "workspace_docs.rs"]
mod docs;
#[path = "workspace_memory.rs"]
pub mod memory;
#[path = "workspace_network.rs"]
pub mod network;
#[path = "workspace_phone.rs"]
mod phone;
#[path = "workspace_preview.rs"]
mod preview;
#[path = "workspace_project.rs"]
mod projects;
#[path = "workspace_schedules.rs"]
mod schedules;
#[path = "workspace_sync.rs"]
mod sync;
#[path = "workspace_sync_view.rs"]
mod sync_view;
#[path = "workspace_team.rs"]
pub mod team;
#[path = "workspace_updates.rs"]
mod updates;
#[path = "workspace_workflow_runner.rs"]
mod workflow_runner;
#[path = "workspace_workflows.rs"]
mod workflows;
pub use acp::{AcpAgent, AgentStatus, CODEX};
pub use devices::{Device, DeviceStatus};
pub use projects::{Activity, ActivityKind, Room, StreamState, StreamView, activities};
pub use sync::SyncRuntime;

pub use sync_view::DeviceFilter;
pub use workflows::{
    AGENT_IDS, Authoring, Editing, RunEntry, TemplateSource, draft_of, draft_report, experience,
    experience_file,
};

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Connecting,
    Ready,
    Failed(String),
}

/// Which conversation the main column shows.
#[derive(Clone, Debug, PartialEq)]
pub enum View {
    /// A fresh chat; the thread is created when the first message goes out.
    Draft,
    Thread(String),
}

impl View {
    fn key(&self) -> &str {
        match self {
            View::Draft => DRAFT,
            View::Thread(id) => id,
        }
    }
}

const DRAFT: &str = "draft";

/// A folder as one identity: symlinks resolved, so the same project opened
/// as `/var/...` and `/private/var/...` on macOS is one project. A path that
/// does not exist yet stays as it is.
pub(super) fn canonical_path(path: &Path) -> PathBuf {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    #[cfg(windows)]
    {
        // `canonicalize` returns a `\\?\` path; the rest of the app builds
        // ordinary paths, so keep that prefix off.
        if let Some(text) = canonical.to_str() {
            if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
                return PathBuf::from(format!(r"\\{rest}"));
            }
            if let Some(rest) = text.strip_prefix(r"\\?\") {
                return PathBuf::from(rest);
            }
        }
    }
    canonical
}

/// What the composer hands over.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Composed {
    pub text: String,
    pub images: Vec<PathBuf>,
    pub mentions: Vec<PathBuf>,
}

impl Composed {
    pub fn text(text: String) -> Self {
        Self {
            text,
            ..Self::default()
        }
    }

    fn attachments(&self) -> Vec<String> {
        self.images
            .iter()
            .chain(&self.mentions)
            .map(|path| path.display().to_string())
            .collect()
    }

    /// Rebuilds a message from what a transcript stored, sorting attachments
    /// back into images and mentioned files by extension.
    fn from_prompt(text: String, attachments: Vec<String>) -> Self {
        let (images, mentions) = attachments
            .into_iter()
            .map(PathBuf::from)
            .partition(|path| is_image(path));
        Self {
            text,
            images,
            mentions,
        }
    }
}

pub fn is_image(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "heic" | "tiff")
    )
}

/// Something worth a toast.
#[derive(Clone, Debug, PartialEq)]
pub enum Notice {
    Info(String),
    Success(String),
    Error(String),
}

/// A turn another Atrium sent while this owner was still resuming the thread.
pub(super) struct SyncPendingTurn {
    pub backend: String,
    pub input: atrium_sync::TurnInput,
    pub steer: bool,
}

pub(super) struct WorkflowReview {
    pub started_ms: i64,
    pub results: Vec<atrium_workflow::run::CheckResult>,
    pub next: usize,
    pub thread: Option<String>,
    pub retried: bool,
}

pub struct Workspace {
    pub status: Status,
    client: Option<CodexClient>,
    /// The one seam the project layer uses to start, message and stop agents.
    pub(super) runtime: std::sync::Arc<RuntimeAdapters>,
    /// Events the runtime itself opens and closes, for the same handler the
    /// provider pumps use.
    runtime_events: Option<futures::channel::mpsc::UnboundedReceiver<(String, AgentEvent)>>,
    runtime_events_tx: futures::channel::mpsc::UnboundedSender<(String, AgentEvent)>,
    pub threads: Vec<ThreadMeta>,
    pub view: View,
    /// Counts the user's moves between chats and project views, so the
    /// main column can play its entrance on a move and not on other changes.
    pub nav: u64,
    /// Whether Atrium's window has focus: with it, the shown thread does
    /// not notify.
    pub window_active: bool,
    alerts: crate::alerts::Alerts,
    phone: Option<phone::Phone>,
    answering: HashSet<(String, String)>,
    /// Conversations whose suggested follow-ups the user dismissed or used.
    dismissed_next: std::collections::HashSet<String>,
    transcripts: HashMap<String, Transcript>,
    /// Threads whose agent session is loaded here. A transcript may also be a
    /// `thread/read` snapshot, which must never make the next turn skip resume.
    loaded_threads: HashSet<String>,
    /// The draft's transcript, shown while its thread is being created.
    draft: Transcript,
    loading: HashSet<String>,
    pub models: Vec<ModelInfo>,
    pub settings: Settings,
    pub signed_in: Option<bool>,
    pub backend_label: String,
    /// When each reasoning item started, and how long it took once done.
    timings: HashMap<String, (Instant, Option<Duration>)>,
    /// When each live row appeared, by `view key` and row, for entrance motion.
    born: HashMap<String, Vec<Option<Instant>>>,
    /// When the running turn of each thread began.
    turn_started: HashMap<String, Instant>,
    /// The message each thread's user will likely send next.
    predictions: HashMap<String, String>,
    /// Prediction forks in flight: fork id to (source thread, text so far).
    prediction_forks: HashMap<String, (String, String)>,
    /// The turns with a checkpoint, by conversation (or a new chat's key).
    checkpoints: HashMap<String, Vec<usize>>,
    /// The checkpoint that undoes each conversation's last restore.
    restores: HashMap<String, usize>,
    /// Your saved document edits, to go to the agent with your next message,
    /// by conversation ("" for a new one).
    pub doc_reports: HashMap<String, Vec<(std::path::PathBuf, String)>>,
    /// Folded runs of steps the user opened or closed, by their first item.
    pub step_folds: HashMap<String, bool>,
    /// Which device's chats the list shows.
    pub device_filter: DeviceFilter,
    /// The pairing code shown in Settings, and when it was made.
    pub pairing: Option<(String, Instant)>,
    /// Workflow runs as last read from their folders.
    pub workflow_runs: Vec<RunEntry>,
    /// A template picked in the sidebar, for the start form to take up.
    pub pick_template: Option<String>,
    /// The "write a template with an agent" form, while it shows.
    pub authoring: Option<Authoring>,
    /// The visual template editor, while it shows.
    pub template_editing: Option<Editing>,
    /// Turns that ended in chats not open, by chat: the list shows the count.
    pub unread: HashMap<String, usize>,
    /// Steps in an open run whose full card is shown.
    pub open_steps: std::collections::HashSet<String>,
    /// A file a transcript row asked the side panel to show.
    pub preview_request: Option<std::path::PathBuf>,
    /// Steps whose checks are running, so a tick does not start them twice.
    pub(super) workflow_checking: HashSet<(PathBuf, String)>,
    /// Reviews whose separate reviewer chat has started.
    pub(super) workflow_reviews: HashMap<(PathBuf, String), WorkflowReview>,
    pub(super) workflow_recovered: HashSet<PathBuf>,
    /// Interrupted workflow tries whose still-running turn is being checked.
    pub(super) workflow_reattaching: HashSet<(PathBuf, String)>,
    _workflow_ticker: Option<Task<()>>,
    /// The shared memory: what sessions got, what was handed on.
    pub shared_memory: memory::MemoryState,
    /// Which models a project's controller can hand work to.
    pub team: team::TeamState,
    /// A newer Atrium, when one is out.
    pub update: Option<crate::updates::Update>,
    pub goals: HashMap<String, Goal>,
    /// Threads with memory switched off.
    pub memory_off: HashSet<String>,
    pub account: Option<Account>,
    /// Plan usage by backend id: `codex`, `codex@studio`, `claude`, ….
    pub rate_limits: HashMap<String, RateLimits>,
    /// Chats folded into memory, and whether memory is ready.
    pub memory: Option<(u64, bool)>,
    /// A ChatGPT sign-in in progress.
    pub login: Option<String>,
    /// The user message being edited: `(thread, turn it started)`.
    pub editing: Option<(String, String)>,
    /// Full-text search: the query and what it found.
    pub search: Option<(String, Vec<(ThreadMeta, String)>)>,
    _search: Option<Task<()>>,
    /// Claude Code, DeepSeek Harness, Antigravity: the ACP agents.
    pub agents: Vec<AcpAgent>,
    /// The agent new chats talk to.
    pub draft_backend: String,
    /// Each agent session's settings, as the agent last reported them.
    session_options: HashMap<String, Vec<ConfigOption>>,
    /// Each agent's latest settings, offered for its new chats.
    known_options: HashMap<String, Vec<ConfigOption>>,
    /// Settings picked for a new chat before its session exists, by agent.
    draft_choices: HashMap<String, HashMap<String, String>>,
    /// Slash commands by session, and each agent's latest.
    commands: HashMap<String, Vec<AgentCommand>>,
    agent_commands: HashMap<String, Vec<AgentCommand>>,
    /// A session made ahead for a new chat, by agent: `(session, folder)`.
    warm: HashMap<String, (String, PathBuf)>,
    /// Projects, and where the user is: the chats or one of them.
    pub projects: Vec<crate::project::Project>,
    pub room: Room,
    _atrium_poll: Option<Task<()>>,
    project_schedule: projects::ScheduleState,
    previews: HashMap<PathBuf, preview::ProjectPreviews>,
    /// Other devices of the tailnet that run `atrium-node`.
    pub devices: Vec<Device>,
    pub discovering: bool,
    /// Folders on devices, by `device:path`, for the folder menu.
    remote_dirs: HashMap<String, Vec<String>>,
    /// The device whose card in Settings is being renamed.
    pub renaming_device: Option<String>,
    /// How many tailnet devices are online, or why the tailnet could not be read.
    pub tailnet: Option<Result<usize, String>>,
    _discovery: Option<Task<()>>,
    _claude_usage: Option<Task<()>>,
    _pump: Option<Task<()>>,
    /// Multi-device overlay, the embedded node and remote conversations.
    pub sync: Option<SyncRuntime>,
    /// Owner-side remote turns waiting for their thread to finish resuming.
    sync_pending_turns: HashMap<String, Vec<SyncPendingTurn>>,
    /// Who claimed each approval or question first.
    answered_by: HashMap<(String, String), String>,
    _sync_poll: Option<Task<()>>,
    _sync_commands: Option<Task<()>>,
    _sync_init: Option<Task<()>>,
}

impl EventEmitter<Notice> for Workspace {}

/// Puts a message in the composer, such as one being edited.
#[derive(Clone, Debug, PartialEq)]
pub struct Fill(pub Composed);

impl EventEmitter<Fill> for Workspace {}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut workspace = Self::with_settings(Settings::load());
        workspace.connect(cx);
        workspace.init_agents(cx);
        workspace.init_projects(cx);
        workspace.init_devices(cx);
        workspace.init_sync(cx);
        workspace.init_workflow_runner(cx);
        workspace.init_memory(cx);
        workspace.sync_phone(cx);
        workspace.start_schedules(cx);
        workspace.start_update_checks(cx);
        cx.observe_self(|ws, cx| ws.sync_phone(cx)).detach();
        if let Some(agent) = workspace.settings.default_agent.clone()
            && (agent == CODEX
                || workspace
                    .agent(&agent)
                    .is_some_and(|a| a.status != AgentStatus::NotInstalled))
        {
            workspace.draft_backend = agent;
        }
        workspace
    }

    fn with_settings(settings: Settings) -> Self {
        let (runtime_events_tx, runtime_events) = futures::channel::mpsc::unbounded();
        Self {
            status: Status::Connecting,
            client: None,
            runtime: std::sync::Arc::new(RuntimeAdapters::new()),
            runtime_events: Some(runtime_events),
            runtime_events_tx,
            threads: Vec::new(),
            view: View::Draft,
            nav: 0,
            window_active: true,
            alerts: Default::default(),
            phone: None,
            answering: HashSet::new(),
            dismissed_next: Default::default(),
            transcripts: HashMap::new(),
            loaded_threads: HashSet::new(),
            draft: Transcript::new(),
            loading: HashSet::new(),
            models: Vec::new(),
            settings,
            signed_in: None,
            backend_label: "Codex".into(),
            timings: HashMap::new(),
            born: HashMap::new(),
            turn_started: HashMap::new(),
            predictions: HashMap::new(),
            prediction_forks: HashMap::new(),
            checkpoints: HashMap::new(),
            restores: HashMap::new(),
            doc_reports: HashMap::new(),
            step_folds: HashMap::new(),
            unread: HashMap::new(),
            device_filter: DeviceFilter::default(),
            pairing: None,
            workflow_runs: Vec::new(),
            pick_template: None,
            authoring: None,
            template_editing: None,
            open_steps: Default::default(),
            preview_request: None,
            workflow_checking: HashSet::new(),
            workflow_reviews: HashMap::new(),
            workflow_recovered: HashSet::new(),
            workflow_reattaching: HashSet::new(),
            _workflow_ticker: None,
            shared_memory: Default::default(),
            team: team::TeamState::load(),
            update: None,
            goals: HashMap::new(),
            memory_off: HashSet::new(),
            account: None,
            rate_limits: HashMap::new(),
            memory: None,
            login: None,
            editing: None,
            search: None,
            _search: None,
            agents: Vec::new(),
            draft_backend: CODEX.into(),
            session_options: HashMap::new(),
            known_options: HashMap::new(),
            draft_choices: HashMap::new(),
            commands: HashMap::new(),
            agent_commands: HashMap::new(),
            warm: HashMap::new(),
            projects: Vec::new(),
            room: Room::Chats,
            _atrium_poll: None,
            project_schedule: projects::ScheduleState::default(),
            previews: HashMap::new(),
            devices: Vec::new(),
            discovering: false,
            remote_dirs: HashMap::new(),
            renaming_device: None,
            tailnet: None,
            _discovery: None,
            _claude_usage: None,
            _pump: None,
            sync: None,
            sync_pending_turns: HashMap::new(),
            answered_by: HashMap::new(),
            _sync_poll: None,
            _sync_commands: None,
            _sync_init: None,
        }
    }

    /// Starts (or restarts) the Codex server and pumps its events into the state.
    pub fn connect(&mut self, cx: &mut Context<Self>) {
        self.status = Status::Connecting;
        self.runtime.forget_backend(CODEX);
        self.client = None;
        cx.notify();
        let config = CodexConfig {
            binary: self.settings.codex_binary.clone(),
            ..Default::default()
        };
        self._pump = Some(cx.spawn(async move |this, cx| {
            // Finding the binary asks the login shell for PATH: off the UI thread.
            let started = cx
                .background_executor()
                .spawn(async move { CodexClient::start(config).await })
                .await;
            let mut events = match started {
                Ok((client, events)) => {
                    let ok = this.update(cx, |ws, cx| {
                        log::info!("connected to {}", client.user_agent);
                        ws.backend_label = backend_label(&client.user_agent);
                        let env = ws.session_env();
                        ws.runtime.register_codex(CODEX, client.clone(), env);
                        ws.client = Some(client);
                        ws.status = Status::Ready;
                        ws.refresh(cx);
                        cx.notify();
                    });
                    if ok.is_err() {
                        return;
                    }
                    events
                }
                Err(error) => {
                    log::error!("codex failed to start: {error:#}");
                    let _ = this.update(cx, |ws, cx| {
                        ws.status = Status::Failed(format!("{error:#}"));
                        cx.notify();
                    });
                    return;
                }
            };
            while let Some(incoming) = events.next().await {
                if this.update(cx, |ws, cx| ws.handle(incoming, cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// Hands every connected client to the runtime seam. Cheap when nothing
    /// changed: registration replaces the same clone.
    pub(super) fn ensure_runtime_clients(&mut self) {
        let env = self.session_env();
        if let Some(client) = &self.client {
            self.runtime
                .register_codex(CODEX, client.clone(), env.clone());
        }
        for device in &self.devices {
            if let Some(client) = &device.codex {
                self.runtime.register_codex(
                    &crate::agents::backend_id("codex", Some(&device.id)),
                    client.clone(),
                    env.clone(),
                );
            }
        }
        for agent in &self.agents {
            if let Some(client) = &agent.client {
                self.runtime.register_acp(&agent.profile.id, client.clone());
            }
        }
    }

    /// Starts the one task that carries events the runtime itself opens and
    /// closes into the app's normal handler. Idempotent.
    pub(super) fn ensure_runtime_events(&mut self, cx: &mut Context<Self>) {
        let Some(mut events) = self.runtime_events.take() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            while let Some((backend, event)) = events.next().await {
                if this
                    .update(cx, |ws, cx| {
                        ws.apply_runtime_event(&backend, event, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// A sink that sends a backend's runtime-owned events into the app.
    pub(super) fn runtime_sink(&self, backend: &str) -> EventSink {
        let tx = self.runtime_events_tx.clone();
        let backend = backend.to_string();
        std::sync::Arc::new(move |event| {
            let _ = tx.unbounded_send((backend.clone(), event));
        })
    }

    /// Reloads the thread list, the models and whether Codex is signed in.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let threads = client.thread_list(100).await;
            let models = client.model_list().await;
            let signed_in = client.signed_in().await;
            let _ = this.update(cx, |ws, cx| {
                match threads {
                    Ok(threads) => ws.merge_threads(CODEX, threads),
                    Err(error) => log::warn!("thread/list failed: {error:#}"),
                }
                match models {
                    Ok(models) => ws.models = models,
                    Err(error) => log::warn!("model/list failed: {error:#}"),
                }
                ws.signed_in = signed_in;
                cx.notify();
            });
        })
        .detach();
        self.refresh_account(cx);
    }

    /// The plan usage of `backend`'s account, when known.
    pub fn limits_for(&self, backend: &str) -> Option<&RateLimits> {
        self.rate_limits.get(backend)
    }

    /// Merges a usage update for `backend`, tells the team when a limit is
    /// hit or lifted, and warns once when it is reached.
    pub(super) fn note_limits(
        &mut self,
        backend: &str,
        update: RateLimits,
        cx: &mut Context<Self>,
    ) {
        let limits = self.rate_limits.entry(backend.to_string()).or_default();
        let was_reached = limits.reached;
        limits.merge(update);
        let reached = limits.reached;
        let resets_at = [limits.primary, limits.secondary]
            .into_iter()
            .flatten()
            .filter(|w| w.used_percent >= 100.0 || reached)
            .filter_map(|w| w.resets_at)
            .max();
        self.team_limit(backend, reached, resets_at);
        if reached && !was_reached {
            cx.emit(Notice::Error(
                t("Usage limit reached. It resets soon; see Usage.").into(),
            ));
        }
        cx.notify();
    }

    /// Reloads who is signed in, their usage, and memory's state.
    pub fn refresh_account(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let account = client.account().await;
            let signed_in = matches!(&account, Ok((Some(_), _)));
            let limits = if signed_in {
                client.rate_limits().await.ok()
            } else {
                None
            };
            let memory = client.memory_status().await.ok();
            let _ = this.update(cx, |ws, cx| {
                if let Ok((account, requires)) = account {
                    ws.signed_in = Some(account.is_some() || !requires);
                    ws.account = account;
                }
                match limits {
                    Some(limits) => {
                        ws.rate_limits.insert(CODEX.to_string(), limits);
                    }
                    None => {
                        ws.rate_limits.remove(CODEX);
                    }
                }
                ws.memory = memory;
                cx.notify();
            });
        })
        .detach();
    }

    fn handle(&mut self, incoming: Incoming, cx: &mut Context<Self>) {
        let event = match incoming {
            Incoming::Notification { method, params } => {
                let event = translate::event(&method, &params);
                if let Some(event) = &event {
                    self.runtime.observe(CODEX, event);
                }
                event
            }
            Incoming::Request { id, method, params } => {
                let event = translate::request(&id, &method, &params);
                if let Some(event) = &event {
                    self.runtime.observe(CODEX, event);
                }
                if let Some(AgentEvent::ApprovalRequested(req)) = &event {
                    self.apply(AgentEvent::ApprovalRequested(req.clone()), cx);
                    self.local_pilot_approval(CODEX, req, &params, cx);
                    cx.notify();
                    return;
                }
                if event.is_none() {
                    log::warn!("declining unsupported server request {method}");
                    if let Some(client) = &self.client {
                        let _ = client.reject_request(id, &method);
                    }
                }
                event
            }
            Incoming::Closed => {
                let event = AgentEvent::Disconnected {
                    reason: "Codex exited".into(),
                };
                self.runtime.observe(CODEX, &event);
                Some(event)
            }
        };
        if let Some(event) = event {
            self.apply(event, cx);
            cx.notify();
        }
    }

    fn apply(&mut self, event: AgentEvent, cx: &mut Context<Self>) {
        // A prediction fork's events are its guess, not a chat to show.
        if let Some(fork) = event
            .thread_id()
            .filter(|id| self.prediction_forks.contains_key(*id))
        {
            let fork = fork.to_string();
            self.collect_prediction(&fork, &event, cx);
            return;
        }
        self.time(&event);
        self.watch_streams(&event, cx);
        self.pilot_event(&event);
        self.alert(&event);
        match &event {
            AgentEvent::PredictionUpdated { thread_id, text } => {
                self.set_prediction(thread_id, text.clone());
                return;
            }
            AgentEvent::GoalUpdated { thread_id, goal } => {
                let finished = goal
                    .as_ref()
                    .is_some_and(|g| g.status == GoalStatus::Complete)
                    && self
                        .goals
                        .get(thread_id)
                        .is_some_and(|g| g.status != GoalStatus::Complete);
                match goal {
                    Some(goal) => self.goals.insert(thread_id.clone(), goal.clone()),
                    None => self.goals.remove(thread_id),
                };
                if finished {
                    cx.emit(Notice::Success(t("Goal complete").into()));
                }
                return;
            }
            AgentEvent::RateLimitsUpdated(update) => {
                self.note_limits(CODEX, update.clone(), cx);
                return;
            }
            AgentEvent::AccountChanged => {
                self.refresh_account(cx);
                return;
            }
            AgentEvent::LoginCompleted { success, error } => {
                self.login = None;
                if *success {
                    cx.emit(Notice::Success(t("Signed in to ChatGPT").into()));
                    self.refresh(cx);
                } else {
                    cx.emit(Notice::Error(tf(
                        "Sign-in failed: {error}",
                        &[(
                            "error",
                            &error.clone().unwrap_or_else(|| t("unknown error").into()),
                        )],
                    )));
                }
                return;
            }
            AgentEvent::ThreadStarted(meta) => self.upsert_thread(meta.clone()),
            AgentEvent::ThreadNameUpdated { thread_id, name } => {
                if let Some(thread) = self.threads.iter_mut().find(|t| t.id == *thread_id) {
                    thread.name = name.clone();
                }
            }
            AgentEvent::TurnStarted { thread_id, .. } => {
                self.dismissed_next.remove(thread_id);
                self.turn_started
                    .entry(thread_id.clone())
                    .or_insert_with(Instant::now);
            }
            AgentEvent::TurnCompleted {
                thread_id,
                turn_id,
                status,
                ..
            } => {
                self.turn_started.remove(thread_id);
                if !self.loading.contains(thread_id) && self.active_thread_id() != Some(thread_id) {
                    *self.unread.entry(thread_id.clone()).or_default() += 1;
                }
                if *status == TurnStatus::Completed
                    && crate::agents::is_codex(self.backend_of(thread_id))
                {
                    self.predict(thread_id.clone(), turn_id.clone(), cx);
                }
                // A replayed history is not new activity.
                if !self.loading.contains(thread_id)
                    && let Some(thread) = self.threads.iter_mut().find(|t| t.id == *thread_id)
                {
                    thread.updated_at = jiff::Timestamp::now().as_second();
                }
                self.threads
                    .sort_by_key(|t| std::cmp::Reverse(t.updated_at));
            }
            AgentEvent::ItemCompleted {
                item: Item::Other { label, .. },
                ..
            } if label == "Context compacted" => {
                cx.emit(Notice::Success(t("Conversation compacted").into()));
            }
            AgentEvent::Warning(message) => log::warn!("codex: {message}"),
            AgentEvent::Disconnected { reason } => {
                self.status = Status::Failed(reason.clone());
                self.client = None;
                let codex: Vec<String> = self
                    .threads
                    .iter()
                    .filter(|t| t.backend == CODEX)
                    .map(|t| t.id.clone())
                    .collect();
                for id in codex {
                    self.pilot_failure(&id, true);
                    self.turn_started.remove(&id);
                    if let Some(transcript) = self.transcripts.get_mut(&id) {
                        transcript.apply(&event);
                    }
                }
                cx.emit(Notice::Error(tf(
                    "{reason}. Restart it from the sidebar.",
                    &[("reason", &t_any(reason))],
                )));
                return;
            }
            _ => {}
        }
        if let Some(thread_id) = event.thread_id().map(str::to_owned)
            && let Some(transcript) = self.transcripts.get_mut(&thread_id)
        {
            let before = transcript.len();
            transcript.apply(&event);
            self.note_births(&thread_id, before);
        }
    }

    /// A system notification for `event` when it concerns a conversation
    /// the user is not looking at.
    fn alert(&mut self, event: &AgentEvent) {
        let Some((thread, alert)) = crate::alerts::alert_for(event) else {
            return;
        };
        if !self.loading.contains(thread)
            && self.settings.remote_view
            && let Some(phone) = &self.phone
            && let Some(topic) = self
                .settings
                .push_topic
                .as_deref()
                .filter(|s| !s.trim().is_empty())
            && let Some((ix, project, stream)) =
                self.projects.iter().enumerate().find_map(|(ix, p)| {
                    p.file
                        .streams
                        .iter()
                        .find(|s| s.thread_id.as_deref() == Some(thread))
                        .map(|s| (ix, p, s))
                })
        {
            let lead_done = stream.role == crate::project::Role::Lead
                && project
                    .task_of(stream)
                    .is_none_or(|task| task.status == crate::project::TaskStatus::Done);
            if let Some(title) = self.alerts.phone_title(thread, event, alert, lead_done) {
                phone.server.notify(atrium_phone::push::Notification {
                    topic: topic.to_owned(),
                    title,
                    ix,
                    project: project.file.name.clone(),
                    stream: stream.name.clone(),
                    id: stream.id.clone(),
                });
            }
        }
        let shown = self.window_active && self.active_thread_id() == Some(thread);
        if self.settings.quiet_notifications || shown || self.loading.contains(thread) {
            return;
        }
        if !self.alerts.allow(thread, alert, Instant::now()) {
            return;
        }
        let stream = self.projects.iter().find_map(|p| {
            p.file
                .streams
                .iter()
                .find(|s| s.thread_id.as_deref() == Some(thread))
                .map(|s| format!("{} · {}", p.file.name, s.name))
        });
        let title = stream.unwrap_or_else(|| {
            self.threads
                .iter()
                .find(|t| t.id == thread)
                .map(|t| t.name.clone().unwrap_or_else(|| t.preview.clone()))
                .unwrap_or_else(|| "Atrium".into())
        });
        let title: String = title.chars().take(80).collect();
        crate::alerts::show(&title, t(alert.body()));
    }

    /// Records when rows past `before` appeared in the transcript under `key`.
    fn note_births(&mut self, key: &str, before: usize) {
        let len = match key {
            DRAFT => self.draft.len(),
            id => self.transcripts.get(id).map_or(0, Transcript::len),
        };
        let born = self.born.entry(key.to_string()).or_default();
        if born.len() < before {
            born.resize(before, None);
        }
        let now = Instant::now();
        while born.len() < len {
            born.push(Some(now));
        }
    }

    /// How far row `ix` of the shown transcript is through its entrance, 0 to 1.
    pub fn entrance(&self, ix: usize) -> f32 {
        const LENGTH: f32 = 0.32;
        let born = self
            .born
            .get(self.view.key())
            .and_then(|born| born.get(ix).copied().flatten());
        match born {
            Some(at) => (at.elapsed().as_secs_f32() / LENGTH).min(1.),
            None => 1.,
        }
    }

    fn time(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::ItemStarted {
                item: Item::Reasoning { id, .. },
                ..
            } => {
                self.timings
                    .entry(id.clone())
                    .or_insert((Instant::now(), None));
            }
            AgentEvent::ReasoningDelta { item_id, .. } => {
                self.timings
                    .entry(item_id.clone())
                    .or_insert((Instant::now(), None));
            }
            AgentEvent::ItemCompleted {
                item: Item::Reasoning { id, .. },
                ..
            } => {
                if let Some((start, took)) = self.timings.get_mut(id) {
                    took.get_or_insert(start.elapsed());
                }
            }
            _ => {}
        }
    }

    /// How long a reasoning item thought, or has been thinking.
    /// The follow-ups the agent proposed at the end of its last answer in the
    /// shown conversation, once it has finished, unless dismissed.
    pub fn suggested_next(&self) -> Vec<String> {
        let Some(thread) = self.active_thread_id() else {
            return Vec::new();
        };
        let Some(transcript) = self.transcripts.get(thread) else {
            return Vec::new();
        };
        if transcript.is_running() || self.dismissed_next.contains(thread) {
            return Vec::new();
        }
        match transcript.entries().last().map(|e| &e.kind) {
            Some(atrium_core::EntryKind::Item(Item::Agent { text, .. })) => next_steps(text),
            _ => Vec::new(),
        }
    }

    /// Hides the shown conversation's suggested follow-ups until its next answer.
    pub fn dismiss_next(&mut self, cx: &mut Context<Self>) {
        if let Some(thread) = self.active_thread_id().map(str::to_owned) {
            self.dismissed_next.insert(thread);
            cx.notify();
        }
    }

    /// The transcript of `thread`, when loaded.
    pub fn transcript_of(&self, thread: &str) -> Option<&Transcript> {
        self.transcripts.get(thread)
    }

    pub fn thought_for(&self, item_id: &str) -> Duration {
        match self.timings.get(item_id) {
            Some((_, Some(took))) => *took,
            Some((start, None)) => start.elapsed(),
            None => Duration::ZERO,
        }
    }

    /// How long the shown chat's turn has been running.
    pub fn running_for(&self) -> Option<Duration> {
        if !self.is_running() {
            return None;
        }
        self.turn_started.get(self.view.key()).map(Instant::elapsed)
    }

    fn upsert_thread(&mut self, mut meta: ThreadMeta) {
        match self.threads.iter_mut().find(|t| t.id == meta.id) {
            Some(existing) => {
                // A fresh thread's notifications carry no title yet; keep ours.
                if meta.preview.trim().is_empty() {
                    meta.preview = std::mem::take(&mut existing.preview);
                }
                if meta.name.is_none() {
                    meta.name = existing.name.take();
                }
                // Codex reports its own threads as plain `codex`; one on
                // another device keeps its device.
                if meta.backend == CODEX && crate::agents::is_codex(&existing.backend) {
                    meta.backend = std::mem::take(&mut existing.backend);
                }
                *existing = meta;
            }
            None => self.threads.insert(0, meta),
        }
    }

    pub fn active_thread(&self) -> Option<&ThreadMeta> {
        match &self.view {
            View::Thread(id) => self.threads.iter().find(|t| t.id == *id),
            View::Draft => None,
        }
    }

    pub fn active_thread_id(&self) -> Option<&str> {
        match &self.view {
            View::Thread(id) => Some(id),
            View::Draft => None,
        }
    }

    /// The transcript the main column shows.
    /// What a chat in the list shows it is doing.
    pub fn chat_status(&self, thread_id: &str) -> ely_gpui_component::chat::ConversationStatus {
        use ely_gpui_component::chat::ConversationStatus;
        let transcript = self.transcripts.get(thread_id);
        if transcript.is_some_and(|t| t.pending_approvals().next().is_some()) {
            return ConversationStatus::Waiting;
        }
        if transcript.is_some_and(Transcript::is_running)
            || self.turn_started.contains_key(thread_id)
        {
            return ConversationStatus::Working;
        }
        match self.unread.get(thread_id) {
            Some(n) if *n > 0 && self.active_thread_id() != Some(thread_id) => {
                ConversationStatus::Unread(*n)
            }
            _ => ConversationStatus::Idle,
        }
    }

    pub fn transcript(&self) -> Option<&Transcript> {
        match &self.view {
            View::Draft => (!self.draft.is_empty()).then_some(&self.draft),
            View::Thread(id) => self.transcripts.get(id),
        }
    }

    pub fn is_loading(&self) -> bool {
        matches!(&self.view, View::Thread(id) if self.loading.contains(id))
    }

    pub fn is_running(&self) -> bool {
        self.transcript().is_some_and(Transcript::is_running)
    }

    /// Whether the shown chat's agent can take a message.
    pub fn is_ready(&self) -> bool {
        // Another device's conversation goes to its owner, which runs the agent.
        if let View::Thread(id) = &self.view
            && self.sync_is_remote(id)
        {
            return self.sync_owner_online(id);
        }
        self.view_agent_ready()
    }

    /// Whether the shown chat is waiting on its first sign of life: a turn
    /// runs and nothing has come back since the user's message.
    pub fn awaiting_first_output(&self) -> bool {
        let Some(transcript) = self.transcript() else {
            return false;
        };
        transcript.is_running()
            && transcript.entries().last().is_some_and(|entry| {
                matches!(entry.kind, atrium_core::EntryKind::Item(Item::User { .. }))
            })
    }

    /// Whether the shown chat's agent runs on another device.
    pub fn is_remote_view(&self) -> bool {
        crate::agents::split_backend(self.view_backend())
            .1
            .is_some()
    }

    /// The folder the shown chat runs in.
    pub fn folder(&self) -> PathBuf {
        match self.active_thread() {
            Some(thread) if !thread.cwd.is_empty() => PathBuf::from(&thread.cwd),
            _ => PathBuf::from(self.folder_for(self.view_backend())),
        }
    }

    pub fn new_chat(&mut self, cx: &mut Context<Self>) {
        self.nav += 1;
        if !self.draft.is_running() {
            self.draft = Transcript::new();
            self.born.remove(DRAFT);
        }
        self.view = View::Draft;
        self.warm_draft(cx);
        cx.notify();
    }

    pub fn open_thread(&mut self, thread_id: String, cx: &mut Context<Self>) {
        self.nav += 1;
        self.view = View::Thread(thread_id.clone());
        self.unread.remove(&thread_id);
        cx.notify();
        self.load_thread(thread_id, cx);
    }

    /// Reads a thread's history into its transcript, without showing it.
    fn load_thread(&mut self, thread_id: String, cx: &mut Context<Self>) {
        if self.loaded_threads.contains(&thread_id) || self.loading.contains(&thread_id) {
            return;
        }
        // A follower's snapshot is the conversation; its owner keeps the
        // writer lock, so there is nothing local to resume.
        if self.transcripts.contains_key(&thread_id) && self.sync_is_remote(&thread_id) {
            return;
        }
        if self.sync_open_thread(&thread_id, cx) {
            return;
        }
        if !crate::agents::is_codex(self.backend_of(&thread_id)) {
            self.acp_open(thread_id, cx);
            return;
        }
        let Some(client) = self.thread_codex(&thread_id) else {
            return;
        };
        self.loading.insert(thread_id.clone());
        let mcp = self.mcp_for_thread(&thread_id, self.backend_of(&thread_id));
        let mode = self
            .stream_of(&thread_id)
            .filter(|(p, _)| p.file.autopilot.on)
            .map(|_| self.project_access_mode(true));
        cx.spawn(async move |this, cx| {
            let resumed = client.thread_resume_in_mode(&thread_id, &mcp, mode).await;
            let goal = if mode.is_some() && resumed.is_ok() {
                client.goal_get(&thread_id).await.ok().flatten()
            } else {
                None
            };
            let _ = this.update(cx, |ws, cx| {
                ws.loading.remove(&thread_id);
                match resumed {
                    Ok((meta, turns)) => {
                        ws.upsert_thread(meta);
                        ws.transcripts
                            .insert(thread_id.clone(), Transcript::from_history(turns));
                        ws.loaded_threads.insert(thread_id.clone());
                        let pending = ws.sync_pending_turns.remove(&thread_id).unwrap_or_default();
                        for turn in pending {
                            if let Err(error) = ws.sync_deliver_owner_turn(&thread_id, turn, cx) {
                                cx.emit(Notice::Error(tf(
                                    "Could not deliver a turn from another computer: {error}",
                                    &[("error", &error.to_string())],
                                )));
                            }
                        }
                        if mode.is_none() {
                            ws.load_goal(thread_id, cx);
                        } else if let Some(goal) = goal {
                            ws.goals.insert(thread_id, goal);
                        } else {
                            ws.goals.remove(&thread_id);
                        }
                    }
                    Err(error) => {
                        ws.pilot_failure(&thread_id, true);
                        if let Some(pending) = ws.sync_pending_turns.remove(&thread_id) {
                            for _ in pending {
                                cx.emit(Notice::Error(tf(
                                    "Could not open this chat: {error}",
                                    &[("error", &format!("{error:#}"))],
                                )));
                            }
                        }
                        let mut transcript = Transcript::new();
                        transcript.fail_pending_turn(tf(
                            "Could not open this chat: {error}",
                            &[("error", &format!("{error:#}"))],
                        ));
                        ws.transcripts.insert(thread_id, transcript);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_goal(&mut self, thread_id: String, cx: &mut Context<Self>) {
        let Some(client) = self.thread_codex(&thread_id) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            if let Ok(Some(goal)) = client.goal_get(&thread_id).await {
                let _ = this.update(cx, |ws, cx| {
                    ws.goals.insert(thread_id, goal);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn project_access_mode(&self, unattended: bool) -> AccessMode {
        match (unattended, self.settings.mode) {
            (true, AccessMode::FullAccess) => AccessMode::Auto,
            (_, mode) => mode,
        }
    }

    fn turn_request(&self, composed: &Composed) -> TurnRequest {
        let unattended = self
            .active_thread_id()
            .and_then(|id| self.stream_of(id))
            .is_some_and(|(p, _)| p.file.autopilot.on);
        let mode = self.project_access_mode(unattended);
        // A device's Codex: the model chosen from that device's list.
        let mut request = if self.is_remote_view() {
            let choice = self.model_choice();
            TurnRequest {
                text: composed.text.clone(),
                images: composed.images.clone(),
                mentions: composed.mentions.clone(),
                model: choice.model,
                effort: choice.effort,
                mode: Some(mode),
                service_tier: None,
                plan_mode: false,
            }
        } else {
            // Plan mode names its model, so resolve the default when none is chosen.
            let model = match self.settings.plan_mode {
                true => self.model_id(),
                false => self.settings.model.clone(),
            };
            TurnRequest {
                text: composed.text.clone(),
                images: composed.images.clone(),
                mentions: composed.mentions.clone(),
                model,
                effort: self.settings.effort.clone(),
                mode: Some(mode),
                service_tier: self.settings.fast.then(|| self.fast_tier()).flatten(),
                plan_mode: self.settings.plan_mode,
            }
        };
        // A project session started for a team member keeps that member's
        // model and effort on its turns; each field alone when it names one.
        if let Some(choice) = self
            .active_thread_id()
            .and_then(|id| self.stream_of(id))
            .and_then(|(p, s)| self.team.starts.get(&(p.root.clone(), s.id.clone())))
        {
            if choice.model.is_some() {
                request.model.clone_from(&choice.model);
            }
            if choice.effort.is_some() {
                request.effort.clone_from(&choice.effort);
            }
        }
        request
    }

    /// The Fast tier the current model offers, if any.
    pub fn fast_tier(&self) -> Option<String> {
        self.model_info(None)
            .and_then(|m| m.service_tiers.first().map(|(id, _, _)| id.clone()))
    }

    // ---- Prediction ----------------------------------------------------

    /// The likely next message in the shown chat.
    pub fn prediction(&self) -> Option<&str> {
        self.active_thread_id()
            .and_then(|id| self.predictions.get(id))
            .map(String::as_str)
    }

    /// Forgets the shown chat's prediction, once used or typed over.
    pub fn dismiss_prediction(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.active_thread_id().map(str::to_owned)
            && self.predictions.remove(&id).is_some()
        {
            cx.notify();
        }
    }

    fn set_prediction(&mut self, thread_id: &str, text: Option<String>) {
        match text.map(|t| clean_prediction(&t)).filter(|t| !t.is_empty()) {
            Some(text) => self.predictions.insert(thread_id.to_string(), text),
            None => self.predictions.remove(thread_id),
        };
    }

    /// Asks for a guess at the user's next message after `turn_id`.
    fn predict(&mut self, thread_id: String, turn_id: String, cx: &mut Context<Self>) {
        if self
            .stream_of(&thread_id)
            .is_some_and(|(p, _)| p.file.autopilot.on)
            || self.settings.no_predictions
            || self.prediction_forks.values().any(|(t, _)| *t == thread_id)
        {
            return;
        }
        let Some(client) = self.thread_codex(&thread_id) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            match client.request_prediction(&thread_id, &turn_id).await {
                Ok(Prediction::Notified) => {}
                Ok(Prediction::Fork(fork)) => {
                    let _ = this.update(cx, |ws, _| {
                        ws.prediction_forks.insert(fork, (thread_id, String::new()));
                    });
                }
                Err(error) => log::debug!("no prediction: {error:#}"),
            }
        })
        .detach();
    }

    /// Gathers a prediction fork's answer; its end sets the prediction.
    fn collect_prediction(&mut self, fork: &str, event: &AgentEvent, cx: &mut Context<Self>) {
        match event {
            AgentEvent::AgentMessageDelta { delta, .. } => {
                if let Some((_, text)) = self.prediction_forks.get_mut(fork) {
                    text.push_str(delta);
                }
            }
            AgentEvent::ItemCompleted {
                item: Item::Agent { text: full, .. },
                ..
            } if !full.is_empty() => {
                if let Some((_, text)) = self.prediction_forks.get_mut(fork) {
                    *text = full.clone();
                }
            }
            AgentEvent::TurnCompleted { .. }
            | AgentEvent::Error {
                will_retry: false, ..
            } => {
                let source = self
                    .prediction_forks
                    .get(fork)
                    .map(|(thread, _)| self.backend_of(thread).to_string())
                    .unwrap_or_else(|| CODEX.to_string());
                if let Some((thread, text)) = self.prediction_forks.remove(fork) {
                    // Only if the user has not moved on in the meantime.
                    if !self
                        .transcripts
                        .get(&thread)
                        .is_some_and(Transcript::is_running)
                    {
                        self.set_prediction(&thread, Some(text));
                        cx.notify();
                    }
                }
                if let Some(client) = self.codex_for(&source) {
                    let fork = fork.to_string();
                    cx.background_spawn(async move {
                        let _ = client.thread_unsubscribe(&fork).await;
                    })
                    .detach();
                }
            }
            _ => {}
        }
    }

    // ---- Editing, branching, steering ----------------------------------

    /// Puts the user message that started `turn_id` back in the composer;
    /// sending it replaces that turn and everything after.
    pub fn start_edit(&mut self, turn_id: &str, cx: &mut Context<Self>) -> Option<Composed> {
        let thread = self.active_thread_id()?.to_string();
        let (text, attachments) = self.transcript()?.turn_prompt(turn_id)?;
        self.editing = Some((thread, turn_id.to_string()));
        let composed = Composed::from_prompt(text, attachments);
        cx.emit(Fill(composed.clone()));
        cx.notify();
        Some(composed)
    }

    pub fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            cx.notify();
        }
    }

    /// Copies the chat through `turn_id` into a new chat and opens it.
    pub fn branch(&mut self, turn_id: Option<String>, cx: &mut Context<Self>) {
        let (Some(thread), Some(client)) = (
            self.active_thread_id().map(str::to_owned),
            self.view_codex(),
        ) else {
            return;
        };
        let backend = self.view_backend().to_string();
        cx.spawn(async move |this, cx| {
            let forked = client.thread_fork(&thread, turn_id.as_deref()).await;
            let _ = this.update(cx, |ws, cx| match forked {
                Ok(meta) => {
                    let id = meta.id.clone();
                    ws.upsert_thread(ThreadMeta { backend, ..meta });
                    cx.emit(Notice::Success(t("Branched into a new chat").into()));
                    ws.open_thread(id, cx);
                }
                Err(error) => cx.emit(Notice::Error(tf(
                    "Could not branch: {error}",
                    &[("error", &format!("{error:#}"))],
                ))),
            });
        })
        .detach();
    }

    /// Adds a message to the running turn.
    fn steer(
        &mut self,
        thread_id: String,
        composed: Composed,
        target: Option<&NoticeTarget>,
        cx: &mut Context<Self>,
    ) -> NoticeSendResult {
        self.ensure_runtime_clients();
        if self.runtime.has_session(&thread_id) {
            return self.steer_via_runtime(thread_id, composed, target, cx);
        }
        let Some(client) = self.thread_codex(&thread_id) else {
            return NoticeSendResult::NoDispatch;
        };
        let Some(turn) = self
            .transcripts
            .get(&thread_id)
            .and_then(|t| t.active_turn())
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
        else {
            cx.emit(Notice::Info(
                t("Codex is still starting this turn; try again in a moment.").into(),
            ));
            return NoticeSendResult::NoDispatch;
        };
        let (composed, notice_dispatch) =
            match self.prepare_stream_notices(&thread_id, composed, target, cx) {
                Ok(prepared) => prepared,
                Err(outcome) => return outcome,
            };
        if let Some(transcript) = self.transcripts.get_mut(&thread_id) {
            let before = transcript.len();
            transcript.push_local_user(composed.text.clone(), composed.attachments());
            self.note_births(&thread_id, before);
        }
        let request = self.turn_request(&composed);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = client.turn_steer(&thread_id, &turn, request).await;
            let outcome = codex_notice_outcome(&result, Some(&turn));
            let _ = this.update(cx, |ws, cx| {
                ws.settle_stream_notices(notice_dispatch, outcome, cx)
            });
            if let Err(error) = result {
                let _ = this.update(cx, |_, cx| {
                    cx.emit(Notice::Error(tf(
                        "Could not add that to the running turn: {error}",
                        &[("error", &format!("{error:#}"))],
                    )))
                });
            }
        })
        .detach();
        NoticeSendResult::Submitted
    }

    /// Replaces the edited turn (and those after it) with `composed`.
    fn resend_edited(
        &mut self,
        thread_id: String,
        turn_id: String,
        composed: Composed,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.thread_codex(&thread_id) else {
            return;
        };
        self.editing = None;
        let mcp = self.mcp_for_thread(&thread_id, self.backend_of(&thread_id));
        cx.spawn(async move |this, cx| {
            let reverted = client.thread_revert(&thread_id, &turn_id).await;
            let resumed = match reverted {
                Ok(()) => client.thread_resume_with(&thread_id, &mcp).await,
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |ws, cx| match resumed {
                Ok((_, turns)) => {
                    ws.transcripts
                        .insert(thread_id.clone(), Transcript::from_history(turns));
                    ws.born.remove(&thread_id);
                    ws.send_to_thread(thread_id.clone(), composed, cx);
                }
                Err(error) => cx.emit(Notice::Error(tf(
                    "Could not edit the message: {error}",
                    &[("error", &format!("{error:#}"))],
                ))),
            });
        })
        .detach();
    }

    // ---- Goals, memory, questions ---------------------------------------

    /// The shown chat's goal.
    pub fn goal(&self) -> Option<&Goal> {
        self.active_thread_id().and_then(|id| self.goals.get(id))
    }

    /// Sets the shown chat's goal and starts working toward it.
    pub fn set_goal(&mut self, objective: String, budget: Option<u64>, cx: &mut Context<Self>) {
        let (Some(thread), Some(client)) = (
            self.active_thread_id().map(str::to_owned),
            self.view_codex(),
        ) else {
            cx.emit(Notice::Info(
                t("Start a chat first, then give it a goal.").into(),
            ));
            return;
        };
        cx.spawn(async move |this, cx| {
            let set = client
                .goal_set(&thread, Some(&objective), Some(GoalStatus::Active), budget)
                .await;
            let _ = this.update(cx, |ws, cx| match set {
                Ok(Some(goal)) => {
                    ws.goals.insert(thread, goal);
                    cx.emit(Notice::Success(
                        t("Goal set. Codex keeps working toward it.").into(),
                    ));
                    cx.notify();
                }
                Ok(None) => {}
                Err(error) => cx.emit(Notice::Error(tf(
                    "Could not set the goal: {error}",
                    &[("error", &format!("{error:#}"))],
                ))),
            });
        })
        .detach();
    }

    pub fn set_goal_status(&mut self, status: GoalStatus, cx: &mut Context<Self>) {
        let (Some(thread), Some(client)) = (
            self.active_thread_id().map(str::to_owned),
            self.view_codex(),
        ) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let set = client.goal_set(&thread, None, Some(status), None).await;
            let _ = this.update(cx, |ws, cx| {
                if let Ok(Some(goal)) = set {
                    ws.goals.insert(thread, goal);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn clear_goal(&mut self, cx: &mut Context<Self>) {
        let (Some(thread), Some(client)) = (
            self.active_thread_id().map(str::to_owned),
            self.view_codex(),
        ) else {
            return;
        };
        self.goals.remove(&thread);
        cx.notify();
        cx.background_spawn(async move {
            let _ = client.goal_clear(&thread).await;
        })
        .detach();
    }

    pub fn memory_enabled(&self) -> bool {
        self.active_thread_id()
            .is_none_or(|id| !self.memory_off.contains(id))
    }

    /// Lets the shown chat use and add to memory, or not.
    pub fn toggle_memory(&mut self, cx: &mut Context<Self>) {
        let (Some(thread), Some(client)) = (
            self.active_thread_id().map(str::to_owned),
            self.view_codex(),
        ) else {
            return;
        };
        let enabled = self.memory_off.contains(&thread);
        if enabled {
            self.memory_off.remove(&thread);
        } else {
            self.memory_off.insert(thread.clone());
        }
        cx.emit(Notice::Info(
            if enabled {
                t("Memory on for this chat")
            } else {
                t("Memory off for this chat")
            }
            .into(),
        ));
        cx.notify();
        cx.background_spawn(async move {
            let _ = client.memory_mode_set(&thread, enabled).await;
        })
        .detach();
    }

    pub fn answer_questions(
        &mut self,
        thread_id: &str,
        request_id: RequestId,
        answers: Answers,
        cx: &mut Context<Self>,
    ) {
        if self.sync_is_remote(thread_id) {
            self.sync_remote_answers(thread_id, request_id, answers, cx);
            return;
        }
        if let Ok(task) =
            self.submit_answer(thread_id, request_id, phone::Answer::Questions(answers), cx)
        {
            task.detach();
        }
    }

    // ---- Account -------------------------------------------------------

    /// Signs in with ChatGPT in the browser.
    pub fn sign_in(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let started = client.login_chatgpt().await;
            let _ = this.update(cx, |ws, cx| match started {
                Ok((login, url)) => {
                    ws.login = Some(login);
                    cx.open_url(&url);
                    cx.emit(Notice::Info(t("Finish signing in in your browser.").into()));
                    cx.notify();
                }
                Err(error) => cx.emit(Notice::Error(tf(
                    "Could not start sign-in: {error}",
                    &[("error", &format!("{error:#}"))],
                ))),
            });
        })
        .detach();
    }

    pub fn cancel_sign_in(&mut self, cx: &mut Context<Self>) {
        let (Some(login), Some(client)) = (self.login.take(), self.client.clone()) else {
            return;
        };
        cx.notify();
        cx.background_spawn(async move {
            let _ = client.login_cancel(&login).await;
        })
        .detach();
    }

    pub fn sign_out(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = client.logout().await;
            let _ = this.update(cx, |ws, cx| match result {
                Ok(()) => {
                    ws.account = None;
                    ws.rate_limits.remove(CODEX);
                    cx.emit(Notice::Info(t("Signed out").into()));
                    ws.refresh_account(cx);
                }
                Err(error) => cx.emit(Notice::Error(tf(
                    "Could not sign out: {error}",
                    &[("error", &format!("{error:#}"))],
                ))),
            });
        })
        .detach();
    }

    /// Reloads usage now (also refreshed after every turn by Codex).
    pub fn refresh_usage(&mut self, cx: &mut Context<Self>) {
        self.refresh_account(cx);
    }

    // ---- Search --------------------------------------------------------

    /// Searches every chat's content for `query`, a moment after typing stops.
    pub fn search_chats(&mut self, query: String, cx: &mut Context<Self>) {
        let query = query.trim().to_string();
        if query.chars().count() < 2 {
            self.search = None;
            self._search = None;
            return;
        }
        if self.search.as_ref().is_some_and(|(q, _)| *q == query) {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        self._search = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(220))
                .await;
            let found = match client.thread_search(&query, 30).await {
                Ok(found) => found,
                Err(error) => {
                    log::warn!("thread/search failed: {error:#}");
                    Vec::new()
                }
            };
            log::debug!("search {query:?}: {} chats", found.len());
            let _ = this.update(cx, |ws, cx| {
                ws.search = Some((query, found));
                cx.notify();
            });
        }));
    }

    /// Sends a message in the current view, creating the thread for a draft.
    pub fn send(&mut self, composed: Composed, cx: &mut Context<Self>) {
        self.send_for_notice(composed, None, cx);
    }

    fn send_for_notice(
        &mut self,
        composed: Composed,
        target: Option<&NoticeTarget>,
        cx: &mut Context<Self>,
    ) -> NoticeSendResult {
        self.send_turn(composed, target, false, cx)
    }

    fn send_turn(
        &mut self,
        composed: Composed,
        target: Option<&NoticeTarget>,
        reactivate: bool,
        cx: &mut Context<Self>,
    ) -> NoticeSendResult {
        if target
            .is_some_and(|target| !matches!(&self.view, View::Thread(id) if id == &target.thread))
        {
            return NoticeSendResult::NoDispatch;
        }
        let composed = Composed {
            text: composed.text.trim().to_string(),
            ..composed
        };
        if composed.text.is_empty() && composed.images.is_empty() {
            return NoticeSendResult::NoDispatch;
        }
        let composed = if target.is_none() {
            self.with_doc_reports(composed)
        } else {
            composed
        };
        if let View::Thread(thread_id) = self.view.clone()
            && self.sync_send_remote(&thread_id, &composed, cx)
        {
            return NoticeSendResult::NoDispatch;
        }
        // Project sessions talk to the agent through the runtime seam.
        if let View::Thread(thread_id) = self.view.clone() {
            self.ensure_runtime_clients();
            if self.runtime.has_session(&thread_id) {
                self.predictions.remove(&thread_id);
                let running = self.is_running();
                let delivery = if running {
                    if self.runtime.capabilities(self.backend_of(&thread_id)).steer {
                        Delivery::Steer
                    } else {
                        Delivery::Queue
                    }
                } else {
                    Delivery::Start
                };
                let editing = self
                    .editing
                    .as_ref()
                    .is_some_and(|(thread, _)| thread == &thread_id);
                if delivery != Delivery::Queue && !(editing && self.is_codex_view()) {
                    return self
                        .send_via_runtime(thread_id, composed, target, delivery, reactivate, cx);
                }
            }
        }
        if !self.is_codex_view() {
            return self.acp_send(composed, target, cx);
        }
        if self.view_codex().is_none() {
            cx.emit(Notice::Error(t("Codex is not running.").into()));
            return NoticeSendResult::NoDispatch;
        }
        if let View::Thread(thread_id) = self.view.clone() {
            self.predictions.remove(&thread_id);
            // A message while Codex works joins the running turn.
            if self.is_running() {
                return self.steer(thread_id, composed, target, cx);
            }
            if let Some((editing, turn)) = self.editing.clone()
                && editing == thread_id
            {
                if target.is_some() {
                    return NoticeSendResult::NoDispatch;
                }
                self.resend_edited(thread_id, turn, composed, cx);
                return NoticeSendResult::NoDispatch;
            }
        } else if self.is_running() {
            return NoticeSendResult::NoDispatch;
        }
        let Some(client) = self.view_codex() else {
            return NoticeSendResult::NoDispatch;
        };
        let (composed, notice_dispatch) = match self.view.clone() {
            View::Thread(thread) => {
                match self.prepare_stream_notices(&thread, composed, target, cx) {
                    Ok(prepared) => prepared,
                    Err(outcome) => return outcome,
                }
            }
            View::Draft => (composed, None),
        };
        let request = self.turn_request(&composed);
        match self.view.clone() {
            View::Thread(thread_id) => {
                let checkpoint = self.checkpoint_next_turn(&thread_id, cx);
                let transcript = self.transcripts.entry(thread_id.clone()).or_default();
                let before = transcript.len();
                transcript.push_local_user(composed.text.clone(), composed.attachments());
                transcript.begin_pending_turn();
                self.note_births(&thread_id, before);
                self.turn_started.insert(thread_id.clone(), Instant::now());
                cx.notify();
                cx.spawn(async move |this, cx| {
                    checkpoint.await;
                    if reactivate
                        && let Err(e) = client
                            .goal_set(&thread_id, None, Some(GoalStatus::Active), None)
                            .await
                    {
                        log::warn!("could not reactivate goal: {e:#}");
                    }
                    let sent = client.turn_start(&thread_id, request).await;
                    let outcome = codex_notice_outcome(&sent, None);
                    let _ = this.update(cx, |ws, cx| {
                        ws.settle_stream_notices(notice_dispatch, outcome, cx)
                    });
                    if let Err(error) = sent {
                        let _ = this.update(cx, |ws, cx| {
                            if let Some(t) = ws.transcripts.get_mut(&thread_id) {
                                t.fail_pending_turn(tf(
                                    "Could not send: {error}",
                                    &[("error", &format!("{error:#}"))],
                                ));
                            }
                            ws.pilot_failure(&thread_id, false);
                            ws.turn_started.remove(&thread_id);
                            cx.notify();
                        });
                    }
                })
                .detach();
            }
            View::Draft => {
                self.draft = Transcript::new();
                self.born.remove(DRAFT);
                self.draft
                    .push_local_user(composed.text.clone(), composed.attachments());
                self.draft.begin_pending_turn();
                self.note_births(DRAFT, 0);
                self.turn_started.insert(DRAFT.into(), Instant::now());
                cx.notify();
                let backend = self.view_backend().to_string();
                let options = StartThread {
                    cwd: Some(self.folder_for(&backend)),
                    // Each device's Codex takes a model from its own list.
                    model: self.model_choice().model,
                    approval_policy: Some(self.settings.mode.approval_policy().into()),
                    sandbox: Some(self.settings.mode.sandbox_mode().into()),
                    mcp_servers: self.chat_mcp(&backend),
                    env: self.session_env(),
                    developer_instructions: None,
                };
                let title = composed.text.clone();
                let memory = self.memory_prep(&backend);
                let (checkpoint, adopt) = self.checkpoint_new_chat(&backend, cx);
                let folder = self.new_chat_folder(&backend, &title, cx);
                let mut options = options;
                cx.spawn(async move |this, cx| {
                    match folder.await {
                        Ok(dir) => options.cwd = Some(dir.display().to_string()),
                        Err(error) => {
                            let _ = this.update(cx, |ws, cx| {
                                ws.draft.fail_pending_turn(error);
                                ws.turn_started.remove(DRAFT);
                                cx.notify();
                            });
                            return;
                        }
                    }
                    checkpoint.await;
                    // What earlier sessions learned, once, at the start.
                    let block = match (memory, options.cwd.clone()) {
                        (Some(memory), Some(cwd)) => {
                            let query = title.clone();
                            Some(
                                cx.background_executor()
                                    .spawn(async move {
                                        memory.block(std::path::Path::new(&cwd), &query)
                                    })
                                    .await,
                            )
                        }
                        _ => None,
                    };
                    if let Some(block) = &block
                        && !block.is_empty()
                    {
                        options.developer_instructions = Some(block.text.clone());
                    }
                    let started = client.thread_start(options).await;
                    let thread_id = match started {
                        Ok(mut meta) => {
                            meta.backend = backend;
                            // Title the chat by its first message until Codex names it.
                            if meta.preview.trim().is_empty() {
                                meta.preview = title;
                            }
                            let id = meta.id.clone();
                            let ok = this.update(cx, |ws, cx| {
                                if let Some(block) = block {
                                    ws.shared_memory.injected.insert(id.clone(), block);
                                }
                                if let Some((dir, key)) = adopt {
                                    ws.adopt_checkpoints(dir, key, id.clone(), cx);
                                }
                                ws.upsert_thread(meta);
                                // The draft becomes the thread's transcript.
                                let draft = std::mem::take(&mut ws.draft);
                                ws.transcripts.insert(id.clone(), draft);
                                ws.loaded_threads.insert(id.clone());
                                if let Some(born) = ws.born.remove(DRAFT) {
                                    ws.born.insert(id.clone(), born);
                                }
                                if let Some(at) = ws.turn_started.remove(DRAFT) {
                                    ws.turn_started.insert(id.clone(), at);
                                }
                                if ws.view == View::Draft {
                                    ws.view = View::Thread(id.clone());
                                }
                                cx.notify();
                            });
                            if ok.is_err() {
                                return;
                            }
                            id
                        }
                        Err(error) => {
                            let _ = this.update(cx, |ws, cx| {
                                ws.draft.fail_pending_turn(tf(
                                    "Could not start a chat: {error}",
                                    &[("error", &format!("{error:#}"))],
                                ));
                                ws.turn_started.remove(DRAFT);
                                cx.notify();
                            });
                            return;
                        }
                    };
                    if let Err(error) = client.turn_start(&thread_id, request).await {
                        let _ = this.update(cx, |ws, cx| {
                            if let Some(t) = ws.transcripts.get_mut(&thread_id) {
                                t.fail_pending_turn(tf(
                                    "Could not send: {error}",
                                    &[("error", &format!("{error:#}"))],
                                ));
                            }
                            ws.turn_started.remove(&thread_id);
                            cx.notify();
                        });
                    }
                })
                .detach();
            }
        }
        NoticeSendResult::Submitted
    }

    /// Sends a message to a project session through the agent-runtime seam.
    fn send_via_runtime(
        &mut self,
        thread_id: String,
        composed: Composed,
        target: Option<&NoticeTarget>,
        delivery: Delivery,
        reactivate: bool,
        cx: &mut Context<Self>,
    ) -> NoticeSendResult {
        let backend = self.backend_of(&thread_id).to_string();
        let (composed, notice_dispatch) =
            match self.prepare_stream_notices(&thread_id, composed, target, cx) {
                Ok(prepared) => prepared,
                Err(outcome) => return outcome,
            };
        let input = TurnInput {
            text: composed.text.clone(),
            images: composed.images.clone(),
            attachments: composed.mentions.clone(),
        };
        let steering = delivery == Delivery::Steer;
        let checkpoint = (!steering).then(|| self.checkpoint_next_turn(&thread_id, cx));
        if let Some(transcript) = self.transcripts.get_mut(&thread_id) {
            let before = transcript.len();
            transcript.push_local_user(composed.text.clone(), composed.attachments());
            if !steering {
                transcript.begin_pending_turn();
            }
            self.note_births(&thread_id, before);
        }
        if !steering {
            self.turn_started.insert(thread_id.clone(), Instant::now());
        }
        cx.notify();
        let runtime = self.runtime.clone();
        let reactivate_client = if reactivate && crate::agents::is_codex(&backend) {
            self.codex_for(&backend)
        } else {
            None
        };
        let call_id = thread_id.clone();
        let update_id = thread_id;
        cx.spawn(async move |this, cx| {
            if let Some(checkpoint) = checkpoint {
                checkpoint.await;
            }
            if let Some(client) = reactivate_client {
                let _ = client
                    .goal_set(&call_id, None, Some(GoalStatus::Active), None)
                    .await;
            }
            let sent = cx
                .background_executor()
                .spawn(async move {
                    crate::runtime_adapters::send_turn(&*runtime, &call_id, input, delivery)
                })
                .await;
            let outcome = match &sent {
                Ok(turn) if !turn.trim().is_empty() => {
                    crate::project::notice_delivery::NoticeOutcome::Accepted
                }
                _ => crate::project::notice_delivery::NoticeOutcome::Uncertain,
            };
            let _ = this.update(cx, |ws, cx| {
                ws.settle_stream_notices(notice_dispatch, outcome, cx);
                if let Err(error) = &sent {
                    if !steering {
                        if let Some(transcript) = ws.transcripts.get_mut(&update_id) {
                            transcript.fail_pending_turn(tf(
                                "Could not send: {error}",
                                &[("error", &format!("{error:#}"))],
                            ));
                        }
                        ws.pilot_failure(&update_id, false);
                        ws.turn_started.remove(&update_id);
                    }
                    cx.emit(Notice::Error(tf(
                        "Could not send: {error}",
                        &[("error", &format!("{error:#}"))],
                    )));
                }
                cx.notify();
            });
        })
        .detach();
        NoticeSendResult::Submitted
    }

    /// Adds a message to a project session's running turn through the seam.
    fn steer_via_runtime(
        &mut self,
        thread_id: String,
        composed: Composed,
        target: Option<&NoticeTarget>,
        cx: &mut Context<Self>,
    ) -> NoticeSendResult {
        let (composed, notice_dispatch) =
            match self.prepare_stream_notices(&thread_id, composed, target, cx) {
                Ok(prepared) => prepared,
                Err(outcome) => return outcome,
            };
        let input = TurnInput {
            text: composed.text.clone(),
            images: composed.images.clone(),
            attachments: composed.mentions.clone(),
        };
        if let Some(transcript) = self.transcripts.get_mut(&thread_id) {
            let before = transcript.len();
            transcript.push_local_user(composed.text.clone(), composed.attachments());
            self.note_births(&thread_id, before);
        }
        cx.notify();
        let runtime = self.runtime.clone();
        let call_id = thread_id.clone();
        cx.spawn(async move |this, cx| {
            let steered = cx
                .background_executor()
                .spawn(
                    async move { crate::runtime_adapters::steer_turn(&*runtime, &call_id, input) },
                )
                .await;
            let outcome = if steered.is_ok() {
                crate::project::notice_delivery::NoticeOutcome::Accepted
            } else {
                crate::project::notice_delivery::NoticeOutcome::Uncertain
            };
            let _ = this.update(cx, |ws, cx| {
                ws.settle_stream_notices(notice_dispatch, outcome, cx);
                if let Err(error) = steered {
                    cx.emit(Notice::Error(tf(
                        "Could not add that to the running turn: {error}",
                        &[("error", &format!("{error:#}"))],
                    )));
                }
            });
        })
        .detach();
        NoticeSendResult::Submitted
    }

    /// Sends the message that started `turn_id` again.
    pub fn retry(&mut self, turn_id: &str, cx: &mut Context<Self>) {
        let prompt = self
            .transcript()
            .and_then(|transcript| transcript.turn_prompt(turn_id));
        if let Some((text, attachments)) = prompt {
            self.send(Composed::from_prompt(text, attachments), cx);
        }
    }

    /// Interrupts the running turn.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        let View::Thread(thread_id) = self.view.clone() else {
            return;
        };
        if self.sync_is_remote(&thread_id) {
            self.sync_remote_interrupt(&thread_id, cx);
            return;
        }
        self.ensure_runtime_clients();
        if self.runtime.has_session(&thread_id) {
            let runtime = self.runtime.clone();
            let id = thread_id;
            cx.background_executor()
                .spawn(async move {
                    let _ = crate::runtime_adapters::interrupt_turn(&*runtime, &id);
                })
                .detach();
            return;
        }
        if !crate::agents::is_codex(self.backend_of(&thread_id)) {
            self.acp_stop(&thread_id);
            return;
        }
        let Some(turn_id) = self
            .transcripts
            .get(&thread_id)
            .and_then(|t| t.active_turn())
            .filter(|turn| !turn.is_empty())
            .map(str::to_owned)
        else {
            return;
        };
        let Some(client) = self.thread_codex(&thread_id) else {
            return;
        };
        cx.spawn(async move |_, _| {
            if let Err(error) = client.turn_interrupt(&thread_id, &turn_id).await {
                log::warn!("turn/interrupt failed: {error:#}");
            }
        })
        .detach();
    }

    pub fn answer(
        &mut self,
        thread_id: &str,
        request_id: RequestId,
        decision: ApprovalDecision,
        cx: &mut Context<Self>,
    ) {
        if self.sync_is_remote(thread_id) {
            self.sync_remote_approval(thread_id, request_id, decision, cx);
            return;
        }
        match self.submit_answer(
            thread_id,
            request_id.clone(),
            phone::Answer::Approval(decision),
            cx,
        ) {
            Ok(task) => task.detach(),
            Err(atrium_phone::Error::Stale) => {
                if let Some(device) = self.answered_by(thread_id, &request_id) {
                    cx.emit(Notice::Info(tf(
                        "Already answered on {device}.",
                        &[("device", &device)],
                    )));
                }
            }
            Err(_) => {}
        }
    }

    pub fn archive(&mut self, thread_id: String, cx: &mut Context<Self>) {
        if !crate::agents::is_codex(self.backend_of(&thread_id)) {
            self.acp_archive(thread_id.clone(), cx);
            self.threads.retain(|t| t.id != thread_id);
            self.transcripts.remove(&thread_id);
            self.loaded_threads.remove(&thread_id);
            self.settings.pinned.retain(|id| *id != thread_id);
            if self.view == View::Thread(thread_id) {
                self.view = View::Draft;
                self.draft = Transcript::new();
            }
            cx.emit(Notice::Info(t("Chat archived").into()));
            cx.notify();
            return;
        }
        let Some(client) = self.thread_codex(&thread_id) else {
            return;
        };
        self.threads.retain(|t| t.id != thread_id);
        self.transcripts.remove(&thread_id);
        self.loaded_threads.remove(&thread_id);
        self.settings.pinned.retain(|id| *id != thread_id);
        if self.view == View::Thread(thread_id.clone()) {
            self.view = View::Draft;
            self.draft = Transcript::new();
        }
        cx.emit(Notice::Info(t("Chat archived").into()));
        cx.notify();
        cx.spawn(async move |this, cx| {
            if let Err(error) = client.thread_archive(&thread_id).await {
                log::warn!("thread/archive failed: {error:#}");
                let _ = this.update(cx, |ws, cx| {
                    cx.emit(Notice::Error(tf(
                        "Could not archive: {error}",
                        &[("error", &format!("{error:#}"))],
                    )));
                    ws.refresh(cx);
                });
            }
        })
        .detach();
    }

    pub fn rename(&mut self, thread_id: String, name: String, cx: &mut Context<Self>) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        if self.sync_is_remote(&thread_id) {
            if let Some(thread) = self.threads.iter_mut().find(|t| t.id == thread_id) {
                thread.name = Some(name.clone());
            }
            self.sync_set_thread_field(&thread_id, "title", serde_json::json!(name));
            self.settings.save();
            cx.notify();
            return;
        }
        let Some(client) = self.thread_codex(&thread_id) else {
            return;
        };
        if let Some(thread) = self.threads.iter_mut().find(|t| t.id == thread_id) {
            thread.name = Some(name.clone());
        }
        self.sync_set_thread_field(&thread_id, "title", serde_json::json!(name));
        cx.notify();
        cx.spawn(async move |this, cx| {
            if let Err(error) = client.thread_set_name(&thread_id, &name).await {
                let _ = this.update(cx, |_, cx| {
                    cx.emit(Notice::Error(tf(
                        "Could not rename: {error}",
                        &[("error", &format!("{error:#}"))],
                    )))
                });
            }
        })
        .detach();
    }

    pub fn toggle_pin(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        self.settings.toggle_pin(thread_id);
        self.settings.save();
        let pinned = self.settings.is_pinned(thread_id);
        self.sync_set_thread_field(thread_id, "pinned", serde_json::json!(pinned));
        cx.notify();
    }

    /// Asks Codex to summarize the shown chat's history to free context.
    pub fn compact(&mut self, cx: &mut Context<Self>) {
        // ACP agents compact through their own `/compact` command.
        if !self.is_codex_view() {
            if self.can_compact() {
                self.send(Composed::text("/compact".into()), cx);
            } else {
                cx.emit(Notice::Info(
                    t("This agent cannot compact a conversation.").into(),
                ));
            }
            return;
        }
        let (Some(thread_id), Some(client)) = (
            self.active_thread_id().map(str::to_owned),
            self.view_codex(),
        ) else {
            cx.emit(Notice::Info(
                t("Start a chat first, then compact it.").into(),
            ));
            return;
        };
        cx.emit(Notice::Info(t("Compacting the conversation…").into()));
        cx.spawn(async move |this, cx| {
            let result = client.thread_compact(&thread_id).await;
            let _ = this.update(cx, |_, cx| match result {
                Ok(()) => {}
                Err(error) => cx.emit(Notice::Error(tf(
                    "Could not compact: {error}",
                    &[("error", &format!("{error:#}"))],
                ))),
            });
        })
        .detach();
    }

    /// Asks Codex to review the uncommitted changes in the chat's folder.
    pub fn review(&mut self, instructions: Option<String>, cx: &mut Context<Self>) {
        let Some(client) = self.view_codex() else {
            return;
        };
        let target = match instructions {
            Some(text) if !text.trim().is_empty() => ReviewTarget::Custom(text),
            _ => ReviewTarget::UncommittedChanges,
        };
        let thread_id = match self.active_thread_id() {
            Some(id) => id.to_string(),
            None => {
                // A review needs a thread: start one in the working folder.
                let backend = self.view_backend().to_string();
                let options = StartThread {
                    cwd: Some(self.folder_for(&backend)),
                    model: self.settings.model.clone(),
                    approval_policy: Some(AccessMode::ReadOnly.approval_policy().into()),
                    sandbox: Some(AccessMode::ReadOnly.sandbox_mode().into()),
                    mcp_servers: Vec::new(),
                    env: Vec::new(),
                    developer_instructions: None,
                };
                cx.spawn(async move |this, cx| {
                    let meta = match client.thread_start(options).await {
                        Ok(mut meta) => {
                            meta.preview = t("Code review").into();
                            meta.backend = backend;
                            meta
                        }
                        Err(error) => {
                            let _ = this.update(cx, |_, cx| {
                                cx.emit(Notice::Error(tf(
                                    "Could not start a review: {error}",
                                    &[("error", &format!("{error:#}"))],
                                )))
                            });
                            return;
                        }
                    };
                    let id = meta.id.clone();
                    let _ = this.update(cx, |ws, cx| {
                        ws.upsert_thread(meta);
                        ws.transcripts.insert(id.clone(), Transcript::new());
                        ws.view = View::Thread(id.clone());
                        ws.turn_started.insert(id.clone(), Instant::now());
                        cx.notify();
                    });
                    if let Err(error) = client.review_start(&id, target).await {
                        let _ = this.update(cx, |_, cx| {
                            cx.emit(Notice::Error(tf(
                                "Could not start a review: {error}",
                                &[("error", &format!("{error:#}"))],
                            )))
                        });
                    }
                })
                .detach();
                return;
            }
        };
        if let Some(transcript) = self.transcripts.get_mut(&thread_id) {
            transcript.begin_pending_turn();
        }
        self.turn_started.insert(thread_id.clone(), Instant::now());
        cx.notify();
        cx.spawn(async move |this, cx| {
            if let Err(error) = client.review_start(&thread_id, target).await {
                let _ = this.update(cx, |ws, cx| {
                    if let Some(t) = ws.transcripts.get_mut(&thread_id) {
                        t.fail_pending_turn(tf(
                            "Could not start a review: {error}",
                            &[("error", &format!("{error:#}"))],
                        ));
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Files in the shown chat's folder matching `query`, as absolute paths.
    pub fn search_files(&self, query: String, cx: &mut Context<Self>) -> Task<Vec<PathBuf>> {
        let Some(client) = self.view_codex() else {
            return Task::ready(Vec::new());
        };
        let root = self.folder();
        cx.background_spawn(async move {
            let root_text = root.display().to_string();
            match client.fuzzy_file_search(&query, &root_text).await {
                Ok(found) => found
                    .into_iter()
                    .take(40)
                    .map(|path| root.join(path))
                    .collect(),
                Err(error) => {
                    log::debug!("file search failed: {error:#}");
                    Vec::new()
                }
            }
        })
    }

    pub fn set_model(&mut self, model: Option<String>, cx: &mut Context<Self>) {
        let mut choice = self.model_choice();
        // A model may not take the chosen effort; fall back to its default.
        if let Some(effort) = &choice.effort {
            let accepts = self
                .model_info(model.as_deref())
                .is_none_or(|info| info.efforts.is_empty() || info.efforts.contains(effort));
            if !accepts {
                choice.effort = None;
            }
        }
        choice.model = model;
        self.set_model_choice(choice, cx);
    }

    pub fn set_effort(&mut self, effort: Option<String>, cx: &mut Context<Self>) {
        let mut choice = self.model_choice();
        choice.effort = effort;
        self.set_model_choice(choice, cx);
    }

    /// The device whose Codex the shown chat uses; none for this machine.
    fn view_device(&self) -> Option<String> {
        crate::agents::split_backend(self.view_backend())
            .1
            .map(str::to_owned)
    }

    /// The models the shown chat's Codex offers: this machine's, or the
    /// device's it runs on.
    pub fn view_models(&self) -> &[ModelInfo] {
        match self.view_device() {
            Some(id) => self
                .devices
                .iter()
                .find(|d| d.id == id)
                .map(|d| d.models.as_slice())
                .unwrap_or(&[]),
            None => &self.models,
        }
    }

    /// The model and effort chosen for the shown chat's Codex.
    pub fn model_choice(&self) -> crate::settings::ModelChoice {
        self.model_choice_on(self.view_device().as_deref())
    }

    /// The model and effort chosen for Codex on `device` (none: here).
    pub fn model_choice_on(&self, device: Option<&str>) -> crate::settings::ModelChoice {
        match device {
            Some(id) => self
                .settings
                .device_models
                .get(id)
                .cloned()
                .unwrap_or_default(),
            None => crate::settings::ModelChoice {
                model: self.settings.model.clone(),
                effort: self.settings.effort.clone(),
            },
        }
    }

    fn set_model_choice(&mut self, choice: crate::settings::ModelChoice, cx: &mut Context<Self>) {
        match self.view_device() {
            Some(id) => {
                self.settings.device_models.insert(id, choice);
            }
            None => {
                self.settings.model = choice.model;
                self.settings.effort = choice.effort;
            }
        }
        self.settings.save();
        cx.notify();
    }

    pub fn set_mode(&mut self, mode: AccessMode, cx: &mut Context<Self>) {
        self.settings.mode = mode;
        let folder = self.settings.working_dir();
        self.settings.folder_modes.insert(folder, mode);
        self.settings.save();
        cx.notify();
    }

    /// Whether the shown conversation can be compacted: any Codex thread,
    /// or an ACP agent offering `/compact`.
    pub fn can_compact(&self) -> bool {
        let Some(thread) = self.active_thread_id() else {
            return false;
        };
        self.is_codex_view()
            || self
                .commands
                .get(thread)
                .is_some_and(|c| c.iter().any(|c| c.name == "compact"))
    }

    /// Shows the next of normal, thinking and verbose views of the chat.
    pub fn cycle_transcript_mode(&mut self, cx: &mut Context<Self>) {
        let mode = self.settings.transcript_mode.next();
        self.settings.transcript_mode = mode;
        self.settings.save();
        cx.emit(Notice::Success(t(mode.label()).into()));
        cx.notify();
    }

    /// Switches the interface's language; it shows at once.
    pub fn set_language(&mut self, language: crate::i18n::Language, cx: &mut Context<Self>) {
        self.settings.language = language;
        self.settings.save();
        crate::apply_language(language, cx);
        cx.notify();
    }

    pub fn set_theme(&mut self, theme: ThemePref, cx: &mut Context<Self>) {
        self.settings.theme = theme;
        self.settings.save();
        cx.notify();
    }

    pub fn set_codex_binary(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.settings.codex_binary != path {
            self.settings.codex_binary = path;
            self.settings.save();
            self.connect(cx);
        }
    }

    pub fn set_working_dir(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        self.settings.use_folder(dir);
        self.settings.save();
        self.warm_draft(cx);
        cx.notify();
    }

    pub fn set_sidebar_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        self.settings.sidebar_collapsed = collapsed;
        self.settings.save();
        cx.notify();
    }

    /// The model new turns use: chosen, else the chat's, else Codex's default.
    pub fn model_id(&self) -> Option<String> {
        self.model_choice()
            .model
            .or_else(|| self.active_thread().and_then(|t| t.model.clone()))
            .or_else(|| {
                self.view_models()
                    .iter()
                    .find(|m| m.is_default)
                    .map(|m| m.id.clone())
            })
    }

    fn model_info(&self, id: Option<&str>) -> Option<&ModelInfo> {
        let id = id.map(str::to_owned).or_else(|| self.model_id())?;
        self.view_models().iter().find(|m| m.id == id)
    }

    /// The model new turns use, for display.
    pub fn model_label(&self) -> String {
        match self.model_id() {
            Some(id) => self
                .view_models()
                .iter()
                .find(|m| m.id == id)
                .map(|m| m.display_name.clone())
                .unwrap_or(id),
            None => t("Default model").into(),
        }
    }

    /// Efforts the current model accepts, and the one in force.
    pub fn efforts(&self) -> (Vec<String>, Option<String>) {
        let info = self.model_info(None);
        let efforts = info.map(|m| m.efforts.clone()).unwrap_or_default();
        let current = self
            .model_choice()
            .effort
            .or_else(|| info.and_then(|m| m.default_effort.clone()));
        (efforts, current)
    }
}

/// A prediction as one clean line: no quotes, no "User:" prefix.
fn clean_prediction(text: &str) -> String {
    let line = text
        .trim()
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let line = line
        .strip_prefix("User:")
        .or_else(|| line.strip_prefix("Me:"))
        .unwrap_or(line)
        .trim();
    line.trim_matches(|c| c == '"' || c == '“' || c == '”')
        .trim()
        .to_string()
}

/// `codex_cli_rs/0.169.0 (...)` or `atrium/0.169.0 (...)` to `Codex 0.169.0`.
fn backend_label(user_agent: &str) -> String {
    let version = user_agent
        .split_whitespace()
        .next()
        .and_then(|product| product.split('/').nth(1));
    match version {
        Some(version) => format!("Codex {version}"),
        None => "Codex".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backend_label_names_the_version() {
        assert_eq!(
            backend_label("atrium/0.169.0 (Mac OS 15.1; arm64) x"),
            "Codex 0.169.0"
        );
        assert_eq!(backend_label(""), "Codex");
    }

    #[test]
    fn predictions_read_as_one_clean_line() {
        assert_eq!(clean_prediction("  \"Run the tests\"\n"), "Run the tests");
        assert_eq!(clean_prediction("User: ship it"), "ship it");
        assert_eq!(clean_prediction("\n\n"), "");
    }

    #[test]
    fn a_retried_prompt_sorts_images_from_files() {
        let composed = Composed::from_prompt(
            "look".into(),
            vec!["/a/shot.PNG".into(), "/a/src/main.rs".into()],
        );
        assert_eq!(composed.images, vec![PathBuf::from("/a/shot.PNG")]);
        assert_eq!(composed.mentions, vec![PathBuf::from("/a/src/main.rs")]);
    }
}

#[cfg(all(test, unix, feature = "workspace-ticker-test"))]
#[path = "workspace_ticker_test.rs"]
mod ticker_test;

/// The follow-ups an answer proposes: the list under a "Next steps" (or
/// "Follow-ups", "Suggested next", "下一步", "后续") line, at most five.
pub fn next_steps(text: &str) -> Vec<String> {
    const CUES: [&str; 8] = [
        "next step",
        "next steps",
        "follow-up",
        "follow up",
        "suggested next",
        "下一步",
        "后续",
        "接下来可以",
    ];
    let mut out = Vec::new();
    let mut in_list = false;
    for line in text.lines() {
        let trimmed = line.trim();
        let plain = trimmed
            .trim_start_matches('#')
            .trim()
            .trim_matches('*')
            .trim()
            .to_lowercase();
        let is_cue = !plain.is_empty()
            && plain.chars().count() <= 40
            && CUES.iter().any(|cue| plain.starts_with(cue))
            && (trimmed.starts_with('#')
                || plain.ends_with(':')
                || plain.ends_with('：')
                || trimmed.starts_with("**"));
        if is_cue {
            out.clear();
            in_list = true;
            continue;
        }
        if !in_list {
            continue;
        }
        let item = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| {
                let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
                (digits > 0)
                    .then(|| {
                        trimmed[digits..]
                            .strip_prefix(". ")
                            .or_else(|| trimmed[digits..].strip_prefix(") "))
                    })
                    .flatten()
            });
        match item {
            Some(item) if !item.trim().is_empty() => {
                let item = item
                    .trim()
                    .trim_start_matches("[ ]")
                    .trim()
                    .replace("**", "");
                out.push(item);
            }
            None if trimmed.is_empty() => {}
            // Anything else ends the list.
            _ => {
                if !out.is_empty() {
                    in_list = false;
                }
            }
        }
    }
    out.truncate(5);
    out
}

#[cfg(test)]
mod next_step_tests {
    use super::next_steps;

    #[test]
    fn follow_ups_are_read_from_the_end_of_an_answer() {
        let text = "Done.\n\n**Next steps:**\n- Add tests for the parser\n- Wire it into the UI\n\nThat's it.";
        assert_eq!(
            next_steps(text),
            vec!["Add tests for the parser", "Wire it into the UI"]
        );
        let zh = "改好了。\n\n## 下一步\n1. 补测试\n2. 更新文档\n";
        assert_eq!(next_steps(zh), vec!["补测试", "更新文档"]);
        assert!(
            next_steps("Here is a list:\n- a\n- b").is_empty(),
            "no cue, no suggestions"
        );
        assert!(
            next_steps("The next step in the pipeline parses input.").is_empty(),
            "a sentence is not a heading"
        );
    }
}

#[cfg(test)]
mod path_tests {
    use super::canonical_path;

    #[cfg(unix)]
    #[test]
    fn a_project_reached_through_a_symlink_has_one_identity() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("project");
        std::fs::create_dir_all(&real).unwrap();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();

        assert_eq!(canonical_path(&alias), canonical_path(&real));
        assert_eq!(canonical_path(&alias), real.canonicalize().unwrap());
        // A path that does not exist yet keeps its spelling.
        let missing = alias.join("not/created");
        assert_eq!(canonical_path(&missing), missing);
    }
}
