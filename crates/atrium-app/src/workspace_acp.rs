//! The workspace's ACP side: Claude Code, DeepSeek Harness and Antigravity,
//! each its own process and event pump, their sessions mixed into the same
//! thread list and transcripts as Codex's.

use super::notice_dispatch::{NoticeDispatch, NoticeSendResult, NoticeTarget};
use crate::i18n::{t, tf};
use crate::project::notice_delivery::NoticeOutcome;
use std::time::Instant;

use acp_client::{AcpClient, AcpConfig, AgentInfo};
use atrium_core::{AgentCommand, AgentEvent, ConfigOption, Item, ThreadMeta, Transcript};
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
    /// Set for an agent on another device.
    pub remote: Option<super::devices::RemoteAgent>,
    /// The link to its device, while it runs there.
    pub link: Option<std::sync::Arc<std::sync::Mutex<agent_rpc::node::LinkState>>>,
    pub(super) _pump: Option<Task<()>>,
    pub(super) reopen: Vec<String>,
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
                remote: None,
                link: None,
                _pump: None,
                reopen: Vec::new(),
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

    pub(super) fn acp(&self, backend: &str) -> Option<AcpClient> {
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
        agents::is_codex(self.view_backend())
    }

    /// Notify only if the effective environment actually changed.
    pub fn save_environment(&mut self, previous: Vec<(String, String)>, cx: &mut Context<Self>) {
        self.settings.save();
        if previous != self.session_env() && self.agents.iter().any(|a| a.client.is_some()) {
            cx.emit(Notice::Info(
                t("Restart ACP agents to use the new environment.").into(),
            ));
        }
        cx.notify();
    }

    pub fn restart_acp_agents(&mut self, cx: &mut Context<Self>) {
        let running: Vec<_> = self
            .agents
            .iter()
            .enumerate()
            .filter(|(_, a)| a.client.is_some())
            .map(|(ix, _)| ix)
            .collect();
        let busy = self.draft.is_running()
            || self.threads.iter().any(|thread| {
                !agents::is_codex(&thread.backend)
                    && (self.loading.contains(&thread.id)
                        || self.transcripts.get(&thread.id).is_some_and(|chat| {
                            chat.is_running() || chat.pending_approvals().next().is_some()
                        }))
            });
        if busy {
            cx.emit(Notice::Info(
                t("Finish or stop active ACP turns before restarting agents.").into(),
            ));
            return;
        }
        for ix in running {
            let backend = self.agents[ix].profile.id.clone();
            self.agents[ix].reopen = self
                .threads
                .iter()
                .filter(|thread| {
                    thread.backend == backend && self.transcripts.contains_key(&thread.id)
                })
                .map(|thread| thread.id.clone())
                .collect();
            self.start_agent(ix, cx);
        }
    }

    /// Starts (or restarts) one ACP agent and pumps its events.
    pub fn start_agent(&mut self, ix: usize, cx: &mut Context<Self>) {
        let owner = self.settings.install_id.clone();
        let env = self.session_env();
        let agent = &mut self.agents[ix];
        let backend = agent.profile.id.to_string();
        let remote = agent.remote.clone();
        let local = agent.profile.command.clone();
        if remote.is_none() && local.is_none() {
            return;
        }
        agent.status = AgentStatus::Starting;
        agent.client = None;
        agent.link = None;
        self.runtime.forget_backend(&backend);
        self.warm.remove(&backend);
        cx.notify();
        let starting = backend.clone();
        agent._pump = Some(cx.spawn(async move |this, cx| {
            let backend = starting;
            let named = backend.clone();
            let started = cx
                .background_executor()
                .spawn(async move {
                    let backend = named;
                    match (remote, local) {
                        // On another device: the node runs it, and its
                        // stdio comes over the link.
                        (Some(remote), _) => {
                            let proc = remote
                                .node
                                .spawn_with_env(&remote.agent, &remote.cwd, &owner, &env)
                                .map_err(|e| anyhow::anyhow!("{e}"))?;
                            let attachment = remote.node.attach(&proc.id, 0);
                            let link = attachment.state.clone();
                            let keep = std::sync::Arc::new(agent_rpc::node::RemoteProc {
                                client: remote.node.clone(),
                                id: proc.id,
                            });
                            AcpClient::over(
                                backend.clone(),
                                attachment.reader,
                                attachment.writer,
                                Some(keep),
                            )
                            .await
                            .map(|(client, events)| (client, events, Some(link)))
                        }
                        (None, Some((program, args))) => AcpClient::start(AcpConfig {
                            backend: backend.clone(),
                            program,
                            args,
                            env,
                        })
                        .await
                        .map(|(client, events)| (client, events, None)),
                        (None, None) => Err(anyhow::anyhow!("nothing to start")),
                    }
                })
                .await;
            let (client, mut events, link) = match started {
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
                let Some(agent) = ws.agents.iter_mut().find(|a| a.profile.id == backend) else {
                    return;
                };
                log::info!("{} {} ready", agent.profile.name, client.info.version);
                agent.info = Some(client.info.clone());
                agent.client = Some(client.clone());
                agent.status = AgentStatus::Ready;
                agent.link = link;
                ws.runtime.register_acp(&backend, client.clone());
                let reopen = std::mem::take(&mut agent.reopen);
                for session in reopen {
                    ws.acp_open(session, cx);
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
                let permission = match &incoming {
                    agent_rpc::Incoming::Request { method, params, .. }
                        if method == "session/request_permission" =>
                    {
                        Some(params.clone())
                    }
                    _ => None,
                };
                let events = client.translate(incoming);
                if this
                    .update(cx, |ws, cx| {
                        let request = match events.as_slice() {
                            [AgentEvent::ApprovalRequested(req)] => {
                                permission.map(|wire| (req.clone(), wire))
                            }
                            _ => None,
                        };
                        for event in &events {
                            ws.runtime.observe(&backend, event);
                        }
                        ws.apply_acp(&backend, events, cx);
                        if let Some((req, wire)) = request {
                            ws.local_pilot_approval(&backend, &req, &wire, cx);
                        }
                    })
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
        // Workstreams' sessions stay even when the agent leaves them out.
        let projects = self.project_threads();
        let listed: Vec<String> = threads.iter().map(|t| t.id.clone()).collect();
        let kept: Vec<ThreadMeta> = self
            .threads
            .drain(..)
            .filter(|t| {
                t.backend != backend
                    || ((projects.contains(&t.id) || self.transcripts.contains_key(&t.id))
                        && !listed.contains(&t.id))
            })
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

    /// Applies one event the runtime owned (a sink event), by backend kind.
    pub(super) fn apply_runtime_event(
        &mut self,
        backend: &str,
        event: AgentEvent,
        cx: &mut Context<Self>,
    ) {
        if crate::agents::is_codex(backend) {
            self.apply(event, cx);
        } else {
            self.apply_acp(backend, vec![event], cx);
        }
        cx.notify();
    }

    fn apply_acp(&mut self, backend: &str, events: Vec<AgentEvent>, cx: &mut Context<Self>) {
        if let (_, Some(device)) = crate::agents::split_backend(backend)
            && !events
                .iter()
                .all(|e| matches!(e, AgentEvent::Disconnected { .. }))
        {
            let device = device.to_string();
            self.device_alive(&device);
        }
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
                        self.pilot_failure(&id, true);
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
                AgentEvent::RateLimitsUpdated(update) => self.note_limits(backend, update, cx),
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
                        Err(error) => cx.emit(Notice::Error(tf(
                            "Could not change that: {error}",
                            &[("error", &format!("{error:#}"))],
                        ))),
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
        if agents::is_codex(&backend) {
            return;
        }
        let cwd = std::path::PathBuf::from(self.folder_for(&backend));
        if self.warm.get(&backend).is_some_and(|(_, dir)| *dir == cwd) {
            return;
        }
        let Some(client) = self.acp(&backend) else {
            return;
        };
        // Claim the slot so a second call does not make another session.
        self.warm
            .insert(backend.clone(), (String::new(), cwd.clone()));
        let mcp = self.chat_mcp(&backend);
        cx.spawn(async move |this, cx| {
            let started = client.new_session_with(&cwd, &mcp).await;
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
        if let (_, Some(device)) = agents::split_backend(backend) {
            let folder = self.folder_for(backend);
            self.fetch_children(device, folder, cx);
            if agents::is_codex(backend)
                && self
                    .device(device)
                    .is_some_and(|d| matches!(d.codex_status, AgentStatus::Failed(_)))
            {
                let device = device.to_string();
                self.start_remote_codex(&device, cx);
            }
        }
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
            backend => self.backend_ready(backend),
        }
    }

    /// Sends a message to an ACP agent, starting its session for a new chat.
    pub(super) fn acp_send(
        &mut self,
        composed: Composed,
        target: Option<&NoticeTarget>,
        cx: &mut Context<Self>,
    ) -> NoticeSendResult {
        let backend = self.view_backend().to_string();
        let Some(client) = self.acp(&backend) else {
            cx.emit(Notice::Error(tf(
                "{agent} is not running.",
                &[("agent", &agents::backend_label(&backend))],
            )));
            return NoticeSendResult::NoDispatch;
        };
        if self.is_running() {
            cx.emit(Notice::Info(tf(
                "{agent} is still working; wait for it, or stop it first.",
                &[("agent", &agents::backend_label(&backend))],
            )));
            return NoticeSendResult::NoDispatch;
        }
        match self.view.clone() {
            View::Thread(session) => {
                let (composed, notice_dispatch) =
                    match self.prepare_stream_notices(&session, composed, target, cx) {
                        Ok(prepared) => prepared,
                        Err(outcome) => return outcome,
                    };

                let checkpoint = self.checkpoint_next_turn(&session, cx);
                let transcript = self.transcripts.entry(session.clone()).or_default();
                let before = transcript.len();
                transcript.push_local_user(composed.text.clone(), composed.attachments());
                transcript.begin_pending_turn();
                self.note_births(&session, before);
                self.turn_started.insert(session.clone(), Instant::now());
                cx.notify();
                cx.spawn(async move |this, cx| {
                    checkpoint.await;
                    let _ = this.update(cx, |_, cx| {
                        Self::run_prompt(client, backend, session, composed, notice_dispatch, cx)
                    });
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
                let cwd = std::path::PathBuf::from(self.folder_for(&backend));
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
                let mcp = self.chat_mcp(&backend);
                let memory = self.memory_prep(&backend);
                let (checkpoint, adopt) = self.checkpoint_new_chat(&backend, cx);
                let folder = self.new_chat_folder(&backend, &composed.text, cx);
                let mut warm = warm;
                cx.spawn(async move |this, cx| {
                    let cwd = match folder.await {
                        Ok(dir) => dir,
                        Err(error) => {
                            let _ = this.update(cx, |ws, cx| {
                                ws.draft.fail_pending_turn(error);
                                ws.turn_started.remove(DRAFT);
                                cx.notify();
                            });
                            return;
                        }
                    };
                    // A session warmed in the main folder does not fit a worktree.
                    if super::chat_trees::is_chat_tree(&cwd) {
                        warm = None;
                    }
                    checkpoint.await;
                    // What earlier sessions learned goes in front of the
                    // first message: ACP takes no instructions of its own.
                    let block = match memory {
                        Some(memory) => {
                            let (dir, query) = (cwd.clone(), composed.text.clone());
                            Some(
                                cx.background_executor()
                                    .spawn(async move { memory.block(&dir, &query) })
                                    .await,
                            )
                        }
                        None => None,
                    };
                    let started = match (warm, warm_options) {
                        (Some(session_id), Some(options)) => Ok(acp_client::SessionStart {
                            session_id,
                            options,
                        }),
                        _ => client.new_session_with(&cwd, &mcp).await,
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
                        if let Some((dir, key)) = adopt {
                            ws.adopt_checkpoints(dir, key, session.clone(), cx);
                        }
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
                        ws.loaded_threads.insert(session.clone());
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
                        let mut prompt = composed.clone();
                        if let Some(block) = block {
                            if !block.is_empty() {
                                prompt.text =
                                    atrium_memory::inject::with_memory(&block.text, &prompt.text);
                            }
                            ws.shared_memory.injected.insert(session.clone(), block);
                        }
                        Self::run_prompt(
                            client.clone(),
                            backend.clone(),
                            session.clone(),
                            prompt,
                            None,
                            cx,
                        );
                    });
                    let _ = ok;
                })
                .detach();
            }
        }
        NoticeSendResult::Submitted
    }

    /// Opens the turn and sends the prompt; the turn ends with its response.
    fn run_prompt(
        client: AcpClient,
        backend: String,
        session: String,
        composed: Composed,
        notice_dispatch: Option<NoticeDispatch>,
        cx: &mut Context<Self>,
    ) {
        let (_, started) = client.begin_turn(&session);
        cx.spawn(async move |this, cx| {
            let _ = this.update(cx, |ws, cx| ws.apply_acp(&backend, started, cx));
            let outcome = client
                .prompt_with_outcome(
                    &session,
                    &composed.text,
                    &composed.images,
                    &composed.mentions,
                )
                .await;
            let delivery = match outcome.delivery {
                acp_client::PromptDelivery::Accepted { .. } => NoticeOutcome::Accepted,
                acp_client::PromptDelivery::Uncertain { .. } => NoticeOutcome::Uncertain,
            };
            let _ = this.update(cx, |ws, cx| {
                ws.settle_stream_notices(notice_dispatch, delivery, cx);
                ws.apply_acp(&backend, outcome.events, cx);
            });
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

    /// Reopens an agent session: its history replays into the transcript
    /// where the agent can load it; otherwise it continues without history.
    pub(super) fn acp_open(&mut self, session: String, cx: &mut Context<Self>) {
        let meta = self.threads.iter().find(|t| t.id == session).cloned();
        let Some(meta) = meta else { return };
        let Some(client) = self.acp(&meta.backend) else {
            let mut transcript = Transcript::new();
            transcript.fail_pending_turn(tf(
                "{agent} is not running, so this chat cannot open yet.",
                &[("agent", &agents::backend_label(&meta.backend))],
            ));
            self.transcripts.insert(session, transcript);
            return;
        };
        let replays = client.info.load_session;
        // Replayed updates fill this transcript as they arrive.
        self.transcripts.insert(session.clone(), Transcript::new());
        self.loading.insert(session.clone());
        let mcp = self.mcp_for_thread(&session, &meta.backend);
        cx.spawn(async move |this, cx| {
            let opened = client.open_session_with(&session, &meta.cwd, &mcp).await;
            let _ = this.update(cx, |ws, cx| {
                ws.loading.remove(&session);
                match opened {
                    Ok(options) => {
                        ws.loaded_threads.insert(session.clone());
                        ws.known_options.insert(meta.backend.clone(), options.clone());
                        ws.session_options.insert(session.clone(), options);
                        if !replays && let Some(transcript) = ws.transcripts.get_mut(&session) {
                            transcript.apply(&AgentEvent::ItemCompleted {
                                thread_id: session.clone(),
                                turn_id: String::new(),
                                item: Item::Other {
                                    id: format!("{session}#resumed"),
                                    label: tf(
                                        "{agent} does not send past messages; the chat continues from here",
                                        &[("agent", &agents::backend_name(&meta.backend))],
                                    ),
                                },
                            });
                        }
                    }
                    Err(error) => {
                        ws.pilot_failure(&session, true);
                        let transcript = ws.transcripts.entry(session.clone()).or_default();
                        transcript.fail_pending_turn(tf(
"Could not open this chat: {error}", &[("error", &format!("{error:#}"))]));
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
