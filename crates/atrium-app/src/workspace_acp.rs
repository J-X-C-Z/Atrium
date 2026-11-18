//! The workspace's ACP side: Claude Code, DeepSeek Harness and Antigravity,
//! each its own process and event pump, their sessions mixed into the same
//! thread list and transcripts as Codex's.

use std::time::Instant;

use acp_client::{AcpClient, AcpConfig, AgentInfo};
use atrium_core::{
    AgentCommand, AgentEvent, ApprovalDecision, ConfigOption, Item, RequestId, ThreadMeta,
    Transcript,
};
use futures::StreamExt;
use gpui::{AppContext, Context, Task};

use super::{Composed, DRAFT, Notice, View, Workspace};
use crate::agents::{self, AgentProfile};

pub const CODEX: &str = "codex";

#[derive(Clone, Debug, PartialEq)]
pub enum AgentStatus {
    /// No command for it on this machine.
    NotInstalled,
    Starting,
    Ready,
    Failed(String),
}

pub struct AcpAgent {
    pub profile: AgentProfile,
    pub status: AgentStatus,
    pub info: Option<AgentInfo>,
    pub(super) client: Option<AcpClient>,
    pub(super) _pump: Option<Task<()>>,
}

impl Workspace {
    /// Finds the ACP agents installed here and starts them.
    pub(super) fn init_agents(&mut self, cx: &mut Context<Self>) {
        self.agents = agents::detect(&self.settings.agent_commands)
            .into_iter()
            .map(|profile| AcpAgent {
                status: if profile.command.is_some() {
                    AgentStatus::Starting
                } else {
                    AgentStatus::NotInstalled
                },
                profile,
                info: None,
                client: None,
                _pump: None,
            })
            .collect();
        for ix in 0..self.agents.len() {
            if self.agents[ix].profile.command.is_some() {
                self.start_agent(ix, cx);
            }
        }
    }

    pub fn agent(&self, backend: &str) -> Option<&AcpAgent> {
        self.agents.iter().find(|a| a.profile.id == backend)
    }

    fn acp(&self, backend: &str) -> Option<AcpClient> {
        self.agent(backend).and_then(|a| a.client.clone())
    }

    /// Which agent a thread belongs to.
    pub fn backend_of(&self, thread_id: &str) -> &str {
        self.threads
            .iter()
            .find(|t| t.id == thread_id)
            .map(|t| t.backend.as_str())
            .unwrap_or(CODEX)
    }

    /// The agent the shown chat (or a new chat) talks to.
    pub fn view_backend(&self) -> &str {
        match &self.view {
            View::Thread(id) => self.backend_of(id),
            View::Draft => &self.draft_backend,
        }
    }

    pub fn is_codex_view(&self) -> bool {
        self.view_backend() == CODEX
    }

    /// Starts (or restarts) one ACP agent and pumps its events.
    pub fn start_agent(&mut self, ix: usize, cx: &mut Context<Self>) {
        let agent = &mut self.agents[ix];
        let Some((program, args)) = agent.profile.command.clone() else {
            return;
        };
        agent.status = AgentStatus::Starting;
        agent.client = None;
        let backend = agent.profile.id.to_string();
        let config = AcpConfig {
            backend: backend.clone(),
            program,
            args,
            env: Vec::new(),
        };
        cx.notify();
        agent._pump = Some(cx.spawn(async move |this, cx| {
            let started = cx
                .background_executor()
                .spawn(async move { AcpClient::start(config).await })
                .await;
            let (client, mut events) = match started {
                Ok(started) => started,
                Err(error) => {
                    log::warn!("{backend} failed to start: {error:#}");
                    let _ = this.update(cx, |ws, cx| {
                        if let Some(agent) = ws.agents.iter_mut().find(|a| a.profile.id == backend)
                        {
                            agent.status = AgentStatus::Failed(format!("{error:#}"));
                        }
                        cx.notify();
                    });
                    return;
                }
            };
            let listing = client.clone();
            let ok = this.update(cx, |ws, cx| {
                if let Some(agent) = ws.agents.iter_mut().find(|a| a.profile.id == backend) {
                    log::info!("{} {} ready", agent.profile.name, client.info.version);
                    agent.info = Some(client.info.clone());
                    agent.client = Some(client.clone());
                    agent.status = AgentStatus::Ready;
                }
                if ws.draft_backend == backend {
                    ws.warm_draft(cx);
                }
                cx.notify();
            });
            if ok.is_err() {
                return;
            }
            if let Ok(sessions) = listing.list_sessions().await {
                let _ = this.update(cx, |ws, cx| {
                    ws.merge_threads(&backend, sessions);
                    cx.notify();
                });
            }
            while let Some(incoming) = events.next().await {
                let events = client.translate(incoming);
                if this
                    .update(cx, |ws, cx| ws.apply_acp(&backend, events, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    /// Replaces `backend`'s threads in the list with `threads`.
    pub(super) fn merge_threads(&mut self, backend: &str, threads: Vec<ThreadMeta>) {
        let hidden = &self.settings.hidden_sessions;
        let kept: Vec<ThreadMeta> = self
            .threads
            .drain(..)
            .filter(|t| t.backend != backend)
            .collect();
        self.threads = kept;
        // Untitled sessions were never used (some agents store every session
        // made, such as ones made ahead for a new chat): leave them out.
        let warm: Vec<&String> = self.warm.values().map(|(id, _)| id).collect();
        self.threads.extend(threads.into_iter().filter(|t| {
            let used = t.name.is_some() || !t.preview.trim().is_empty();
            !hidden.contains(&t.id)
                && !warm.contains(&&t.id)
                && (used || self.transcripts.contains_key(&t.id))
        }));
        self.threads
            .sort_by_key(|t| std::cmp::Reverse(t.updated_at));
    }

    fn apply_acp(&mut self, backend: &str, events: Vec<AgentEvent>, cx: &mut Context<Self>) {
        for event in events {
            match event {
                AgentEvent::Disconnected { reason } => {
                    if let Some(agent) = self.agents.iter_mut().find(|a| a.profile.id == backend) {
                        agent.status = AgentStatus::Failed(reason.clone());
                        agent.client = None;
                    }
                    let event = AgentEvent::Disconnected {
                        reason: reason.clone(),
                    };
                    let ids: Vec<String> = self
                        .threads
                        .iter()
                        .filter(|t| t.backend == backend)
                        .map(|t| t.id.clone())
                        .collect();
                    for id in ids {
                        if let Some(transcript) = self.transcripts.get_mut(&id) {
                            transcript.apply(&event);
                        }
                    }
                    cx.emit(Notice::Error(reason));
                }
                AgentEvent::ConfigUpdated { thread_id, options } => {
                    self.known_options
                        .insert(backend.to_string(), options.clone());
                    self.session_options.insert(thread_id, options);
                }
                AgentEvent::CommandsUpdated {
                    thread_id,
                    commands,
                } => {
                    self.agent_commands
                        .insert(backend.to_string(), commands.clone());
                    self.commands.insert(thread_id, commands);
                }
                AgentEvent::Warning(message) => cx.emit(Notice::Info(message)),
                event => self.apply(event, cx),
            }
        }
        cx.notify();
    }

    /// The settings the shown chat's agent offers, with their current values.
    pub fn agent_options(&self) -> Vec<ConfigOption> {
        match &self.view {
            View::Thread(id) => self.session_options.get(id).cloned().unwrap_or_default(),
            View::Draft => {
                let mut options = self
                    .known_options
                    .get(&self.draft_backend)
                    .cloned()
                    .unwrap_or_default();
                if let Some(choices) = self.draft_choices.get(&self.draft_backend) {
                    for option in &mut options {
                        if let Some(value) = choices.get(&option.id) {
                            option.current = value.clone();
                        }
                    }
                }
                options
            }
        }
    }

    /// The agent's own slash commands for the shown chat.
    pub fn agent_slash_commands(&self) -> Vec<AgentCommand> {
        match &self.view {
            View::Thread(id) => self.commands.get(id).cloned().unwrap_or_default(),
            View::Draft => self
                .agent_commands
                .get(&self.draft_backend)
                .cloned()
                .unwrap_or_default(),
        }
    }

    /// Changes one of the shown chat's agent settings (for a new chat, the
    /// choice is kept and applied when its session starts).
    pub fn set_agent_option(&mut self, option: String, value: String, cx: &mut Context<Self>) {
        match self.view.clone() {
            View::Draft => {
                self.draft_choices
                    .entry(self.draft_backend.clone())
                    .or_default()
                    .insert(option, value);
                cx.notify();
            }
            View::Thread(id) => {
                let backend = self.backend_of(&id).to_string();
                let Some(client) = self.acp(&backend) else {
                    return;
                };
                if let Some(options) = self.session_options.get_mut(&id)
                    && let Some(o) = options.iter_mut().find(|o| o.id == option)
                {
                    o.current = value.clone();
                }
                cx.notify();
                cx.spawn(async move |this, cx| {
                    let result = client.set_option(&id, &option, &value).await;
                    let _ = this.update(cx, |ws, cx| match result {
                        Ok(Some(options)) => {
                            ws.known_options.insert(backend, options.clone());
                            ws.session_options.insert(id, options);
                            cx.notify();
                        }
                        Ok(None) => {}
                        Err(error) => {
                            cx.emit(Notice::Error(format!("Could not change that: {error:#}")))
                        }
                    });
                })
                .detach();
            }
        }
    }

    /// Makes a session ahead for a new chat with the draft's agent, so its
    /// settings show before the first message and the first answer starts sooner.
    pub(super) fn warm_draft(&mut self, cx: &mut Context<Self>) {
        let backend = self.draft_backend.clone();
        if backend == CODEX {
            return;
        }
        let cwd = self.settings.working_dir();
        if self.warm.get(&backend).is_some_and(|(_, dir)| *dir == cwd) {
            return;
        }
        let Some(client) = self.acp(&backend) else {
            return;
        };
        // Claim the slot so a second call does not make another session.
        self.warm
            .insert(backend.clone(), (String::new(), cwd.clone()));
        cx.spawn(async move |this, cx| {
            let started = client.new_session(&cwd).await;
            let _ = this.update(cx, |ws, cx| match started {
                Ok(start) => {
                    ws.known_options
                        .insert(backend.clone(), start.options.clone());
                    ws.session_options
                        .insert(start.session_id.clone(), start.options);
                    ws.warm.insert(backend, (start.session_id, cwd));
                    cx.notify();
                }
                Err(error) => {
                    ws.warm.remove(&backend);
                    log::warn!("could not prepare a {backend} session: {error:#}");
                }
            });
        })
        .detach();
    }

    /// Picks the agent new chats use.
    pub fn set_draft_backend(&mut self, backend: &str, cx: &mut Context<Self>) {
        self.draft_backend = backend.to_string();
        self.settings.default_agent = Some(backend.to_string());
        self.settings.save();
        self.warm_draft(cx);
        if let Some(ix) = self.agents.iter().position(|a| a.profile.id == backend)
            && matches!(self.agents[ix].status, AgentStatus::Failed(_))
        {
            self.start_agent(ix, cx);
        }
        cx.notify();
    }

    /// Whether the shown chat's agent is up.
    pub(super) fn view_agent_ready(&self) -> bool {
        match self.view_backend() {
            CODEX => self.status == super::Status::Ready,
            backend => self
                .agent(backend)
                .is_some_and(|a| a.status == AgentStatus::Ready),
        }
    }

    /// Sends a message to an ACP agent, starting its session for a new chat.
    pub(super) fn acp_send(&mut self, composed: Composed, cx: &mut Context<Self>) {
        let backend = self.view_backend().to_string();
        let Some(client) = self.acp(&backend) else {
            cx.emit(Notice::Error(format!(
                "{} is not running.",
                agents::backend_name(&backend)
            )));
            return;
        };
        if self.is_running() {
            cx.emit(Notice::Info(format!(
                "{} is still working; wait for it, or stop it first.",
                agents::backend_name(&backend)
            )));
            return;
        }
        match self.view.clone() {
            View::Thread(session) => {
                let transcript = self.transcripts.entry(session.clone()).or_default();
                let before = transcript.len();
                transcript.push_local_user(composed.text.clone(), composed.attachments());
                transcript.begin_pending_turn();
                self.note_births(&session, before);
                self.turn_started.insert(session.clone(), Instant::now());
                cx.notify();
                Self::run_prompt(client, backend, session, composed, cx);
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
                let cwd = self.settings.working_dir();
                let choices = self
                    .draft_choices
                    .get(&backend)
                    .cloned()
                    .unwrap_or_default();
                let warm = self
                    .warm
                    .remove(&backend)
                    .filter(|(id, dir)| !id.is_empty() && *dir == cwd)
                    .map(|(id, _)| id);
                let warm_options = warm
                    .as_ref()
                    .and_then(|id| self.session_options.get(id).cloned());
                cx.spawn(async move |this, cx| {
                    let started = match (warm, warm_options) {
                        (Some(session_id), Some(options)) => Ok(acp_client::SessionStart {
                            session_id,
                            options,
                        }),
                        _ => client.new_session(&cwd).await,
                    };
                    let start = match started {
                        Ok(start) => start,
                        Err(error) => {
                            let _ = this.update(cx, |ws, cx| {
                                ws.draft.fail_pending_turn(format!("{error:#}"));
                                ws.turn_started.remove(DRAFT);
                                cx.notify();
                            });
                            return;
                        }
                    };
                    let session = start.session_id.clone();
                    // Settings chosen before the session existed.
                    let mut options = start.options.clone();
                    for (option, value) in &choices {
                        let current = options
                            .iter()
                            .find(|o| o.id == *option)
                            .map(|o| o.current.clone());
                        if current.as_deref() != Some(value.as_str())
                            && let Ok(updated) = client.set_option(&session, option, value).await
                        {
                            match updated {
                                Some(updated) => options = updated,
                                None => {
                                    if let Some(o) = options.iter_mut().find(|o| o.id == *option) {
                                        o.current = value.clone();
                                    }
                                }
                            }
                        }
                    }
                    let ok = this.update(cx, |ws, cx| {
                        let now = jiff::Timestamp::now().as_second();
                        ws.upsert_thread(ThreadMeta {
                            id: session.clone(),
                            backend: backend.clone(),
                            name: None,
                            preview: composed.text.clone(),
                            cwd: cwd.display().to_string(),
                            model: None,
                            created_at: now,
                            updated_at: now,
                        });
                        ws.known_options.insert(backend.clone(), options.clone());
                        ws.session_options.insert(session.clone(), options);
                        if let Some(commands) = ws.agent_commands.get(&backend).cloned() {
                            ws.commands.insert(session.clone(), commands);
                        }
                        let draft = std::mem::take(&mut ws.draft);
                        ws.transcripts.insert(session.clone(), draft);
                        if let Some(born) = ws.born.remove(DRAFT) {
                            ws.born.insert(session.clone(), born);
                        }
                        if let Some(at) = ws.turn_started.remove(DRAFT) {
                            ws.turn_started.insert(session.clone(), at);
                        }
                        if ws.view == View::Draft {
                            ws.view = View::Thread(session.clone());
                        }
                        ws.warm_draft(cx);
                        cx.notify();
                        Self::run_prompt(
                            client.clone(),
                            backend.clone(),
                            session.clone(),
                            composed.clone(),
                            cx,
                        );
                    });
                    let _ = ok;
                })
                .detach();
            }
        }
    }

    /// Opens the turn and sends the prompt; the turn ends with its response.
    fn run_prompt(
        client: AcpClient,
        backend: String,
        session: String,
        composed: Composed,
        cx: &mut Context<Self>,
    ) {
        let (_, started) = client.begin_turn(&session);
        cx.spawn(async move |this, cx| {
            let _ = this.update(cx, |ws, cx| ws.apply_acp(&backend, started, cx));
            let ended = client
                .prompt(
                    &session,
                    &composed.text,
                    &composed.images,
                    &composed.mentions,
                )
                .await;
            let _ = this.update(cx, |ws, cx| ws.apply_acp(&backend, ended, cx));
        })
        .detach();
    }

    pub(super) fn acp_stop(&mut self, session: &str) {
        let backend = self.backend_of(session).to_string();
        if let Some(client) = self.acp(&backend)
            && let Err(error) = client.cancel(session)
        {
            log::warn!("session/cancel failed: {error:#}");
        }
    }

    pub(super) fn acp_answer(
        &mut self,
        session: &str,
        request_id: RequestId,
        decision: ApprovalDecision,
        cx: &mut Context<Self>,
    ) {
        let backend = self.backend_of(session).to_string();
        let Some(client) = self.acp(&backend) else {
            return;
        };
        if let Err(error) = client.answer_permission(request_id.clone(), decision) {
            log::warn!("answering the permission request failed: {error:#}");
            return;
        }
        if let Some(transcript) = self.transcripts.get_mut(session) {
            transcript.answer_approval(&request_id, decision);
        }
        cx.notify();
    }

    /// Reopens an agent session: its history replays into the transcript
    /// where the agent can load it; otherwise it continues without history.
    pub(super) fn acp_open(&mut self, session: String, cx: &mut Context<Self>) {
        let meta = self.threads.iter().find(|t| t.id == session).cloned();
        let Some(meta) = meta else { return };
        let Some(client) = self.acp(&meta.backend) else {
            let mut transcript = Transcript::new();
            transcript.fail_pending_turn(format!(
                "{} is not running, so this chat cannot open yet.",
                agents::backend_name(&meta.backend)
            ));
            self.transcripts.insert(session, transcript);
            return;
        };
        let replays = client.info.load_session;
        // Replayed updates fill this transcript as they arrive.
        self.transcripts.insert(session.clone(), Transcript::new());
        self.loading.insert(session.clone());
        cx.spawn(async move |this, cx| {
            let opened = client.open_session(&session, &meta.cwd).await;
            let _ = this.update(cx, |ws, cx| {
                ws.loading.remove(&session);
                match opened {
                    Ok(options) => {
                        ws.known_options.insert(meta.backend.clone(), options.clone());
                        ws.session_options.insert(session.clone(), options);
                        if !replays && let Some(transcript) = ws.transcripts.get_mut(&session) {
                            transcript.apply(&AgentEvent::ItemCompleted {
                                thread_id: session.clone(),
                                turn_id: String::new(),
                                item: Item::Other {
                                    id: format!("{session}#resumed"),
                                    label: format!(
                                        "{} does not send past messages; the chat continues from here",
                                        agents::backend_name(&meta.backend)
                                    ),
                                },
                            });
                        }
                    }
                    Err(error) => {
                        let transcript = ws.transcripts.entry(session.clone()).or_default();
                        transcript.fail_pending_turn(format!("Could not open this chat: {error:#}"));
                    }
                }
                // Rows replayed from history do not animate in.
                ws.born.remove(&session);
                cx.notify();
            });
        })
        .detach();
    }

    /// Hides an agent session and lets the agent close it.
    pub(super) fn acp_archive(&mut self, session: String, cx: &mut Context<Self>) {
        let backend = self.backend_of(&session).to_string();
        self.settings.hidden_sessions.push(session.clone());
        self.settings.save();
        if let Some(client) = self.acp(&backend) {
            cx.background_spawn(async move {
                let _ = client.close_session(&session).await;
            })
            .detach();
        }
    }
}
