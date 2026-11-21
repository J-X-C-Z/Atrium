//! Multi-device sync for the desktop: the embedded node, the owner-side
//! host that answers other devices, and the follower side that lists,
//! reads and continues their conversations.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use atrium_core::{AgentEvent, ApprovalDecision, Item, ThreadMeta, Transcript};
use atrium_node::{EmbeddedNode, HostError, HostFuture, SyncHost, SyncState};
use atrium_project::repo;
use atrium_sync::{
    AnswerInput, ApprovalAnswer, DeviceDirectory, Lease, LeaseDecision, MovePackage, PeerClient,
    SettingsView, ThreadAnnouncement, ThreadCache, ThreadSnapshot, TurnInput, lease_can_be_taken,
    migrate_settings, thread_key,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use gpui::Context;
use tokio::sync::{mpsc, oneshot};

use super::{Composed, Notice, View, Workspace};

pub struct SyncRuntime {
    pub state: Arc<SyncState>,
    pub _node: EmbeddedNode,
    pub cache: Arc<ThreadCache>,
    /// Conversation id to the install that owns its agent session.
    pub owners: HashMap<String, String>,
    /// Install id to the device id used in backend ids.
    pub devices: HashMap<String, String>,
    /// The last transcript cursor seen from each remote conversation.
    pub cursors: HashMap<String, u64>,
    /// Conversations that came from another device, by device id.
    pub remote: HashMap<String, Vec<ThreadAnnouncement>>,
    pub polling: bool,
    pub project_ids: HashMap<PathBuf, String>,
    pub lease_renewed: HashMap<String, i64>,
}

struct AppliedMove {
    package: MovePackage,
    folder: PathBuf,
    old_owner: String,
    device: String,
    install_id: String,
    backend: String,
    resumed: bool,
}

enum Command {
    Threads {
        since: Option<i64>,
        reply: oneshot::Sender<Result<Vec<ThreadAnnouncement>, HostError>>,
    },
    Snapshot {
        backend: String,
        id: String,
        reply: oneshot::Sender<Result<ThreadSnapshot, HostError>>,
    },
    Turn {
        backend: String,
        id: String,
        input: TurnInput,
        reply: oneshot::Sender<Result<(), HostError>>,
    },
    Steer {
        backend: String,
        id: String,
        input: TurnInput,
        reply: oneshot::Sender<Result<(), HostError>>,
    },
    Interrupt {
        backend: String,
        id: String,
        reply: oneshot::Sender<Result<(), HostError>>,
    },
    Approve {
        backend: String,
        id: String,
        request: serde_json::Value,
        answer: ApprovalAnswer,
        reply: oneshot::Sender<Result<(), HostError>>,
    },
    Answer {
        backend: String,
        id: String,
        request: serde_json::Value,
        input: AnswerInput,
        reply: oneshot::Sender<Result<(), HostError>>,
    },
    MovePackage {
        backend: String,
        id: String,
        reply: oneshot::Sender<Result<MovePackage, HostError>>,
    },
    MoveRelease {
        backend: String,
        id: String,
        reply: oneshot::Sender<Result<(), HostError>>,
    },
}

#[derive(Clone)]
struct SyncHostClient {
    commands: mpsc::Sender<Command>,
}

impl SyncHostClient {
    async fn ask<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T, HostError>>) -> Command,
    ) -> Result<T, HostError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(make(reply))
            .await
            .map_err(|_| HostError::Unavailable("the desktop stopped".into()))?;
        answer
            .await
            .map_err(|_| HostError::Unavailable("the desktop did not answer".into()))?
    }
}

impl SyncHost for SyncHostClient {
    fn threads<'a>(&'a self, since_ms: Option<i64>) -> HostFuture<'a, Vec<ThreadAnnouncement>> {
        Box::pin(async move {
            self.ask(|reply| Command::Threads {
                since: since_ms,
                reply,
            })
            .await
        })
    }

    fn snapshot<'a>(&'a self, backend: &'a str, id: &'a str) -> HostFuture<'a, ThreadSnapshot> {
        let backend = backend.to_string();
        let id = id.to_string();
        Box::pin(async move {
            self.ask(|reply| Command::Snapshot { backend, id, reply })
                .await
        })
    }

    fn turn<'a>(&'a self, backend: &'a str, id: &'a str, input: TurnInput) -> HostFuture<'a, ()> {
        let backend = backend.to_string();
        let id = id.to_string();
        Box::pin(async move {
            self.ask(|reply| Command::Turn {
                backend,
                id,
                input,
                reply,
            })
            .await
        })
    }

    fn steer<'a>(&'a self, backend: &'a str, id: &'a str, input: TurnInput) -> HostFuture<'a, ()> {
        let backend = backend.to_string();
        let id = id.to_string();
        Box::pin(async move {
            self.ask(|reply| Command::Steer {
                backend,
                id,
                input,
                reply,
            })
            .await
        })
    }

    fn interrupt<'a>(&'a self, backend: &'a str, id: &'a str) -> HostFuture<'a, ()> {
        let backend = backend.to_string();
        let id = id.to_string();
        Box::pin(async move {
            self.ask(|reply| Command::Interrupt { backend, id, reply })
                .await
        })
    }

    fn approve<'a>(
        &'a self,
        backend: &'a str,
        id: &'a str,
        request: serde_json::Value,
        answer: ApprovalAnswer,
    ) -> HostFuture<'a, ()> {
        let backend = backend.to_string();
        let id = id.to_string();
        Box::pin(async move {
            self.ask(|reply| Command::Approve {
                backend,
                id,
                request,
                answer,
                reply,
            })
            .await
        })
    }

    fn answer<'a>(
        &'a self,
        backend: &'a str,
        id: &'a str,
        request: serde_json::Value,
        input: AnswerInput,
    ) -> HostFuture<'a, ()> {
        let backend = backend.to_string();
        let id = id.to_string();
        Box::pin(async move {
            self.ask(|reply| Command::Answer {
                backend,
                id,
                request,
                input,
                reply,
            })
            .await
        })
    }

    fn move_package<'a>(&'a self, backend: &'a str, id: &'a str) -> HostFuture<'a, MovePackage> {
        let backend = backend.to_string();
        let id = id.to_string();
        Box::pin(async move {
            self.ask(|reply| Command::MovePackage { backend, id, reply })
                .await
        })
    }

    fn move_release<'a>(&'a self, backend: &'a str, id: &'a str) -> HostFuture<'a, ()> {
        let backend = backend.to_string();
        let id = id.to_string();
        Box::pin(async move {
            self.ask(|reply| Command::MoveRelease { backend, id, reply })
                .await
        })
    }
}

impl Workspace {
    pub(super) fn init_sync(&mut self, cx: &mut Context<Self>) {
        if !self.settings.sync_enabled() || std::env::var("ATRIUM_NO_SYNC").is_ok() {
            return;
        }
        if self.settings.install_id.is_empty() {
            self.settings.install_id = format!("install-{}", uuid::Uuid::new_v4().simple());
        }
        if self.settings.device_name.trim().is_empty() {
            self.settings.device_name = atrium_node::config::hostname();
        }
        self.settings.save();
        let data = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Atrium");
        let cache = match ThreadCache::open(data.join("sync/cache"), 500 * 1024 * 1024) {
            Ok(cache) => cache,
            Err(error) => {
                log::warn!("sync cache: {error}");
                return;
            }
        };
        let (commands, receiving) = mpsc::channel(64);
        let host = Arc::new(SyncHostClient {
            commands: commands.clone(),
        });
        let (ready, ready_rx) = tokio::sync::oneshot::channel();
        let config = self.sync_config();
        let install_id = self.settings.install_id.clone();
        let device_name = self.settings.device_name.clone();
        let sync_dir = data.join("sync");
        std::thread::Builder::new()
            .name("atrium-sync".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready.send(Err(error.to_string()));
                        return;
                    }
                };
                runtime.block_on(async move {
                    let path = atrium_node::config::login_path().await;
                    let owner = atrium_node::auth::owner(&path).await;
                    let directory = DeviceDirectory {
                        install_id,
                        device_name,
                        os: std::env::consts::OS.into(),
                        app_version: env!("CARGO_PKG_VERSION").into(),
                        sync_version: atrium_sync::SYNC_VERSION,
                        owner,
                    };
                    let state = match SyncState::open(
                        &sync_dir,
                        directory,
                        Some(host as Arc<dyn SyncHost>),
                    ) {
                        Ok(state) => state,
                        Err(error) => {
                            let _ = ready.send(Err(error.to_string()));
                            return;
                        }
                    };
                    let node =
                        match EmbeddedNode::start(Arc::new(config), path, state.clone()).await {
                            Ok(node) => node,
                            Err(error) => {
                                let _ = ready.send(Err(error.to_string()));
                                return;
                            }
                        };
                    let _ = ready.send(Ok((state, node)));
                    std::future::pending::<()>().await;
                });
            })
            .ok();
        self._sync_commands = Some(cx.spawn(async move |this, cx| {
            let mut receiving = receiving;
            while let Some(command) = receiving.recv().await {
                let result = this.update(cx, |workspace, cx| workspace.sync_command(command, cx));
                if result.is_err() {
                    break;
                }
            }
        }));
        let _init = cx.spawn(async move |this, cx| {
            let Ok(ready) = ready_rx.await else {
                return;
            };
            let _ = this.update(cx, |workspace, cx| match ready {
                Ok((state, node)) => {
                    workspace.sync = Some(SyncRuntime {
                        state,
                        _node: node,
                        cache: Arc::new(cache),
                        owners: HashMap::new(),
                        devices: HashMap::new(),
                        cursors: HashMap::new(),
                        remote: HashMap::new(),
                        polling: false,
                        project_ids: HashMap::new(),
                        lease_renewed: HashMap::new(),
                    });
                    workspace.sync_migrate();
                    workspace.sync_refresh_overlay();
                    workspace.sync_poll(cx);
                    cx.notify();
                }
                Err(error) => {
                    log::warn!("multi-device sync is off: {error}");
                    cx.emit(Notice::Info(crate::i18n::tf(
                        "Multi-device sync is off: {error}",
                        &[("error", &error)],
                    )));
                }
            });
        });
        self._sync_init = Some(_init);
    }

    fn sync_config(&self) -> atrium_node::config::Config {
        let mut roots = Vec::new();
        roots.extend(self.settings.recent_folders.iter().cloned());
        roots.extend(self.settings.projects.iter().cloned());
        roots.retain(|path| path.is_dir());
        roots.dedup();
        let mut config = atrium_node::config::Config {
            port: std::env::var("ATRIUM_NODE_PORT")
                .ok()
                .and_then(|port| port.parse().ok())
                .unwrap_or(7381),
            name: self.settings.device_name.clone(),
            token: self.settings.node_token.clone().unwrap_or_default(),
            loopback_only: std::env::var("ATRIUM_NODE_LOOPBACK_ONLY").is_ok(),
            roots,
            ..Default::default()
        };
        for (id, command, protocol) in [
            ("codex", vec!["codex", "app-server"], "codex-app-server"),
            ("claude", vec!["claude-agent-acp"], "acp"),
            ("deepseek", vec!["dsh", "--profile", "acp"], "acp"),
            ("antigravity", vec!["agy", "acp"], "acp"),
        ] {
            config.agents.insert(
                id.into(),
                atrium_node::config::Agent {
                    command: command.into_iter().map(String::from).collect(),
                    protocol: protocol.into(),
                },
            );
        }
        config
    }

    fn sync_command(&mut self, command: Command, cx: &mut Context<Self>) {
        match command {
            Command::Threads { since, reply } => {
                let _ = reply.send(Ok(self.sync_local_threads(since)));
            }
            Command::Snapshot { backend, id, reply } => {
                self.sync_answer_snapshot(backend, id, reply, cx)
            }
            Command::Turn {
                backend,
                id,
                input,
                reply,
            } => {
                let _ = reply.send(self.sync_owner_turn(backend, id, input, false, cx));
            }
            Command::Steer {
                backend,
                id,
                input,
                reply,
            } => {
                let _ = reply.send(self.sync_owner_turn(backend, id, input, true, cx));
            }
            Command::Interrupt { backend, id, reply } => {
                let _ = reply.send(self.sync_owner_interrupt(backend, id, cx));
            }
            Command::Approve {
                backend,
                id,
                request,
                answer,
                reply,
            } => {
                let decision = match answer.decision.as_str() {
                    "accept" => ApprovalDecision::Accept,
                    "accept_for_session" => ApprovalDecision::AcceptForSession,
                    "decline" => ApprovalDecision::Decline,
                    _ => {
                        let _ = reply.send(Err(HostError::BadRequest(
                            "unknown approval decision".into(),
                        )));
                        return;
                    }
                };
                if self.thread_exists(&backend, &id) {
                    self.answer(&id, request, decision, cx);
                    let _ = reply.send(Ok(()));
                } else {
                    let _ = reply.send(Err(HostError::NotFound));
                }
            }
            Command::Answer {
                backend,
                id,
                request,
                input,
                reply,
            } => {
                if self.thread_exists(&backend, &id) {
                    let answers: Vec<(String, Vec<String>)> = input.answers.into_iter().collect();
                    self.answer_questions(&id, request, answers, cx);
                    let _ = reply.send(Ok(()));
                } else {
                    let _ = reply.send(Err(HostError::NotFound));
                }
            }
            Command::MovePackage { backend, id, reply } => {
                if self.transcripts.contains_key(&id) {
                    let _ = reply.send(self.sync_owner_move_package(&backend, &id));
                } else if crate::agents::is_codex(&backend)
                    && let Some(client) = self.thread_codex(&id)
                {
                    let backend_for_read = backend.clone();
                    cx.spawn(async move |this, cx| {
                        let read = client.thread_read(&id).await;
                        let answer = match read {
                            Ok((_meta, turns)) => this
                                .update(cx, |workspace, _| {
                                    workspace
                                        .transcripts
                                        .insert(id.clone(), Transcript::from_history(turns));
                                    workspace.sync_owner_move_package(&backend_for_read, &id)
                                })
                                .unwrap_or_else(|_| {
                                    Err(HostError::Unavailable("the desktop stopped".into()))
                                }),
                            Err(error) => Err(HostError::Unavailable(format!("{error:#}"))),
                        };
                        let _ = reply.send(answer);
                    })
                    .detach();
                } else {
                    let _ = reply.send(Err(HostError::NotFound));
                }
            }
            Command::MoveRelease { backend, id, reply } => {
                let _ = reply.send(self.sync_owner_move_release(&backend, &id, cx));
            }
        }
    }

    fn thread_exists(&self, backend: &str, id: &str) -> bool {
        self.threads
            .iter()
            .any(|thread| thread.id == id && thread.backend == backend)
    }

    fn sync_local_threads(&self, since: Option<i64>) -> Vec<ThreadAnnouncement> {
        let owner = self.settings.install_id.clone();
        self.threads
            .iter()
            .filter(|thread| !thread.backend.contains('@'))
            .filter(|thread| since.is_none_or(|since| thread.updated_at >= since))
            .map(|thread| {
                ThreadAnnouncement::from_meta(
                    thread.clone(),
                    owner.clone(),
                    self.sync_thread_state(thread),
                )
            })
            .collect()
    }

    fn sync_thread_state(&self, thread: &ThreadMeta) -> &'static str {
        let Some(transcript) = self.transcripts.get(&thread.id) else {
            return "idle";
        };
        if transcript.is_running() {
            return "running";
        }
        if transcript.pending_approvals().next().is_some() {
            return "waiting_for_approval";
        }
        if transcript.pending_questions().next().is_some() {
            return "waiting_for_answer";
        }
        "idle"
    }

    fn sync_answer_snapshot(
        &mut self,
        backend: String,
        id: String,
        reply: oneshot::Sender<Result<ThreadSnapshot, HostError>>,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = self
            .threads
            .iter()
            .find(|thread| thread.id == id && thread.backend == backend)
            .cloned()
        else {
            let _ = reply.send(Err(HostError::NotFound));
            return;
        };
        if let Some(transcript) = self.transcripts.get(&id) {
            let _ = reply.send(Ok(ThreadSnapshot {
                thread: ThreadAnnouncement::from_meta(
                    thread,
                    self.settings.install_id.clone(),
                    self.sync_thread_state_from(transcript),
                ),
                items: transcript_items(transcript),
                approvals: transcript.pending_approvals().cloned().collect(),
                questions: transcript.pending_questions().cloned().collect(),
                cursor: transcript.version(),
            }));
            return;
        }
        let Some(client) = self.thread_codex(&id) else {
            let _ = reply.send(Err(HostError::Unavailable(
                "the conversation is not open and its agent cannot read it".into(),
            )));
            return;
        };
        let owner = self.settings.install_id.clone();
        cx.spawn(async move |this, cx| {
            let read = client.thread_read(&id).await;
            let result = this.update(cx, |workspace, cx| {
                let answer = match read {
                    Ok((meta, turns)) => {
                        let transcript = Transcript::from_history(turns);
                        let snapshot = ThreadSnapshot {
                            thread: ThreadAnnouncement::from_meta(meta, owner, "idle"),
                            items: transcript_items(&transcript),
                            approvals: Vec::new(),
                            questions: Vec::new(),
                            cursor: transcript.version(),
                        };
                        workspace.transcripts.insert(id, transcript);
                        Ok(snapshot)
                    }
                    Err(error) => Err(HostError::Unavailable(format!("{error:#}"))),
                };
                cx.notify();
                answer
            });
            let _ = reply.send(match result {
                Ok(answer) => answer,
                Err(_) => Err(HostError::Unavailable("the desktop stopped".into())),
            });
        })
        .detach();
    }

    fn sync_thread_state_from(&self, transcript: &Transcript) -> &'static str {
        if transcript.is_running() {
            return "running";
        }
        if transcript.pending_approvals().next().is_some() {
            return "waiting_for_approval";
        }
        if transcript.pending_questions().next().is_some() {
            return "waiting_for_answer";
        }
        "idle"
    }

    fn sync_owner_turn(
        &mut self,
        backend: String,
        id: String,
        input: TurnInput,
        steer: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), HostError> {
        if !self.thread_exists(&backend, &id) {
            return Err(HostError::NotFound);
        }
        if let Some(model) = input.model.clone() {
            self.settings.model = Some(model);
        }
        if let Some(effort) = input.effort.clone() {
            self.settings.effort = Some(effort);
        }
        self.view = View::Thread(id.clone());
        self.nav += 1;
        if !self.transcripts.contains_key(&id) {
            self.load_thread(id.clone(), cx);
        }
        let composed = Composed::from_prompt(input.text, input.attachments);
        if composed.text.is_empty() && composed.images.is_empty() {
            return Err(HostError::BadRequest("empty turn".into()));
        }
        let _ = steer;
        self.send_turn(composed, None, false, cx);
        cx.notify();
        Ok(())
    }

    fn sync_owner_interrupt(
        &mut self,
        backend: String,
        id: String,
        cx: &mut Context<Self>,
    ) -> Result<(), HostError> {
        if !self.thread_exists(&backend, &id) {
            return Err(HostError::NotFound);
        }
        if crate::agents::is_codex(&backend) {
            self.view = View::Thread(id.clone());
            self.stop(cx);
            return Ok(());
        }
        Err(HostError::Unavailable(
            "this agent cannot be interrupted remotely".into(),
        ))
    }

    /// Migrates this device's existing pins, hidden sessions, projects and
    /// names into the overlay once. A later start sees the marker.
    fn sync_migrate(&mut self) {
        let Some(sync) = &self.sync else { return };
        let marker = format!("sync/migrated/{}", self.settings.install_id);
        if sync
            .state
            .store()
            .value(&marker)
            .is_some_and(|value| value == &serde_json::json!(true))
        {
            return;
        }
        let view = SettingsView {
            pinned: self.settings.pinned.clone(),
            hidden: self.settings.hidden_sessions.clone(),
            projects: self
                .settings
                .projects
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
            environments: self
                .settings
                .environments
                .iter()
                .map(|environment| environment.name.clone())
                .collect(),
            mcp_servers: self
                .settings
                .mcp_servers
                .iter()
                .map(|server| server.name.clone())
                .collect(),
        };
        let ops = migrate_settings(
            &self.settings.install_id,
            "codex",
            &view,
            atrium_node::sync::now_ms(),
        );
        if let Err(error) = sync.state.merge(ops) {
            log::warn!("sync migration: {error}");
        }
        let _ = sync.state.set(marker, serde_json::json!(true));
    }

    /// Applies the replicated titles, pins and hidden flags on top of the
    /// local lists. The overlay wins over each device's settings file.
    pub(super) fn sync_refresh_overlay(&mut self) {
        let Some(sync) = &self.sync else { return };
        let mut pinned = HashSet::new();
        let mut hidden = HashSet::new();
        let mut titles = HashMap::new();
        let mut moved = HashMap::new();
        for (key, register) in sync.state.store().registers() {
            let Some((device, backend, thread, field)) = atrium_sync::parse_thread_key(key) else {
                continue;
            };
            let visible = register.value.as_ref().and_then(|value| value.as_bool());
            match field {
                "title" => {
                    if let Some(title) = register.value.as_ref().and_then(|value| value.as_str())
                        && !title.trim().is_empty()
                    {
                        titles.insert(thread.to_string(), title.to_string());
                    }
                }
                "pinned" if visible == Some(true) => {
                    pinned.insert(thread.to_string());
                }
                "hidden" if visible == Some(true) => {
                    hidden.insert(thread.to_string());
                }
                "moved_to" => {
                    if let Some(target) = register.value.as_ref().and_then(|value| value.as_str()) {
                        moved.insert(thread.to_string(), (device.to_string(), target.to_string()));
                    }
                }
                _ => {
                    let _ = backend;
                }
            }
        }
        for thread in &self.threads {
            let owner = sync
                .owners
                .get(&thread.id)
                .cloned()
                .unwrap_or_else(|| self.settings.install_id.clone());
            if moved
                .get(&thread.id)
                .is_some_and(|(old, _target)| *old == owner)
            {
                hidden.insert(thread.id.clone());
            }
        }
        let mut changed = false;
        for thread in &mut self.threads {
            if let Some(title) = titles.get(&thread.id)
                && thread.name.as_deref() != Some(title)
            {
                thread.name = Some(title.clone());
                changed = true;
            }
        }
        if !hidden.is_empty() {
            let before = self.threads.len();
            self.threads.retain(|thread| !hidden.contains(&thread.id));
            changed |= before != self.threads.len();
        }
        let mut pinned: Vec<String> = pinned.into_iter().collect();
        pinned.sort();
        if self.settings.pinned != pinned {
            self.settings.pinned = pinned;
            changed = true;
        }
        if changed {
            self.settings.save();
        }
    }

    /// Writes one overlay field for a conversation, whichever device owns it.
    pub(super) fn sync_set_thread_field(
        &mut self,
        thread_id: &str,
        field: &str,
        value: serde_json::Value,
    ) {
        let Some(thread) = self.threads.iter().find(|thread| thread.id == thread_id) else {
            return;
        };
        let backend = crate::agents::split_backend(&thread.backend).0.to_string();
        let owner = self
            .sync
            .as_ref()
            .and_then(|sync| sync.owners.get(thread_id).cloned())
            .unwrap_or_else(|| self.settings.install_id.clone());
        let key = thread_key(&owner, &backend, thread_id, field);
        if let Some(sync) = &self.sync
            && let Err(error) = sync.state.set(key, value)
        {
            log::warn!("sync overlay: {error}");
        }
        self.sync_refresh_overlay();
    }

    /// Whether this device may run a project's scheduler and autopilot.
    ///
    /// The lease is renewed every thirty seconds. Another device's live
    /// lease stops scheduling here; once it has been expired past the grace
    /// window this device takes over and says so.
    pub(super) fn sync_scheduler_allowed(&mut self, root: &PathBuf) -> bool {
        let Some(sync) = &mut self.sync else {
            return true;
        };
        let id = sync
            .project_ids
            .entry(root.clone())
            .or_insert_with(|| {
                atrium_project::repo::identity(root).unwrap_or_else(|| {
                    use std::hash::{Hash, Hasher};
                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                    root.hash(&mut hasher);
                    format!("{:016x}", hasher.finish())
                })
            })
            .clone();
        let key = format!("project/{id}/scheduler");
        let now = atrium_node::sync::now_ms();
        let lease = sync
            .state
            .store()
            .value(&key)
            .and_then(|value| serde_json::from_value::<Lease>(value.clone()).ok());
        let decision = lease_can_be_taken(lease.as_ref(), &self.settings.install_id, now, false);
        match decision {
            LeaseDecision::Wait => false,
            LeaseDecision::ConfirmWithOwner => false,
            LeaseDecision::Keep | LeaseDecision::CanTakeOver => {
                let renewed = sync
                    .lease_renewed
                    .get(&key)
                    .is_some_and(|last| now.saturating_sub(*last) < 30_000);
                if !renewed {
                    let value = Lease {
                        device: self.settings.install_id.clone(),
                        until_ms: now + atrium_sync::LEASE_RENEW_MS,
                    };
                    if let Err(error) = sync
                        .state
                        .set(key.clone(), serde_json::to_value(value).unwrap_or_default())
                    {
                        log::warn!("scheduler lease: {error}");
                    }
                    sync.lease_renewed.insert(key, now);
                }
                if decision == LeaseDecision::CanTakeOver
                    && lease
                        .as_ref()
                        .is_some_and(|lease| lease.device != self.settings.install_id)
                {
                    log::info!(
                        "scheduler lease for {} moved to this computer",
                        root.display()
                    );
                }
                true
            }
        }
    }

    /// Starts the five-second peer round. Each round exchanges the overlay
    /// and asks every device for its conversation list.
    pub(super) fn sync_poll(&mut self, cx: &mut Context<Self>) {
        if self.sync.is_none() {
            return;
        }
        self._sync_poll = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(Some((state, devices, me))) = this.update(cx, |workspace, _| {
                    let Some(sync) = &mut workspace.sync else {
                        return None;
                    };
                    if sync.polling {
                        return None;
                    }
                    sync.polling = true;
                    let devices: Vec<(String, PeerClient)> = workspace
                        .devices
                        .iter()
                        .filter(|device| device.status == super::DeviceStatus::Online)
                        .map(|device| (device.id.clone(), sync_peer(&device.node)))
                        .collect();
                    Some((
                        sync.state.clone(),
                        devices,
                        workspace.settings.install_id.clone(),
                    ))
                }) else {
                    cx.background_executor().timer(Duration::from_secs(2)).await;
                    continue;
                };
                let rounds = cx
                    .background_executor()
                    .spawn(async move {
                        let mut rounds = Vec::new();
                        for (device, peer) in devices {
                            let Ok(directory) = peer.directory() else {
                                continue;
                            };
                            if directory.install_id == me {
                                continue;
                            }
                            let vector = peer.vector().unwrap_or_default();
                            let sending = state.missing_for(&vector);
                            let merged = match peer.exchange(&sending) {
                                Ok(ops) => state.merge(ops).map(|applied| applied.len()),
                                Err(error) => {
                                    log::info!("sync with {device}: {error}");
                                    continue;
                                }
                            };
                            let threads = peer.threads(None).unwrap_or_default();
                            rounds.push((device, directory.install_id, threads, merged.ok()));
                        }
                        rounds
                    })
                    .await;
                let _ = this.update(cx, |workspace, cx| {
                    for (device, install, threads, applied) in rounds {
                        if applied.is_some() {
                            workspace.sync_merge_remote(device, install, threads);
                        }
                    }
                    if let Some(sync) = &mut workspace.sync {
                        sync.polling = false;
                    }
                    workspace.sync_refresh_overlay();
                    cx.notify();
                });
                cx.background_executor().timer(Duration::from_secs(5)).await;
            }
        }));
    }

    fn sync_merge_remote(
        &mut self,
        device: String,
        install: String,
        announcements: Vec<ThreadAnnouncement>,
    ) {
        let Some(sync) = &mut self.sync else { return };
        if install == self.settings.install_id {
            return;
        }
        sync.devices.insert(install.clone(), device.clone());
        sync.remote.insert(device.clone(), announcements.clone());
        let mut titles = HashMap::new();
        let mut hidden = HashSet::new();
        for announcement in &announcements {
            let base = crate::agents::split_backend(&announcement.backend).0;
            let title_key = thread_key(&install, base, &announcement.id, "title");
            let hidden_key = thread_key(&install, base, &announcement.id, "hidden");
            if let Some(title) = sync
                .state
                .store()
                .value(&title_key)
                .and_then(|value| value.as_str())
            {
                titles.insert(announcement.id.clone(), title.to_string());
            }
            if sync
                .state
                .store()
                .value(&hidden_key)
                .and_then(|value| value.as_bool())
                == Some(true)
            {
                hidden.insert(announcement.id.clone());
            }
            sync.owners.insert(announcement.id.clone(), install.clone());
        }
        let listed: HashSet<String> = announcements
            .iter()
            .map(|announcement| announcement.id.clone())
            .collect();
        let projects = self.project_threads();
        self.threads.retain(|thread| {
            let remote = thread.backend.strip_suffix(&format!("@{device}")).is_some();
            !remote
                || listed.contains(&thread.id)
                || projects.contains(&thread.id)
                || self.transcripts.contains_key(&thread.id)
        });
        for announcement in announcements {
            if hidden.contains(&announcement.id) {
                continue;
            }
            let mut meta = remote_thread_meta(announcement, &device);
            if let Some(title) = titles.get(&meta.id) {
                meta.name = Some(title.clone());
            }
            match self.threads.iter_mut().find(|thread| thread.id == meta.id) {
                Some(existing) => *existing = meta,
                None => self.threads.push(meta),
            }
        }
        self.threads
            .sort_by_key(|thread| std::cmp::Reverse(thread.updated_at));
    }

    pub(super) fn sync_owner(&self, thread_id: &str) -> Option<String> {
        self.sync
            .as_ref()
            .and_then(|sync| sync.owners.get(thread_id).cloned())
    }

    /// Whether the device that owns `thread_id` can be reached now.
    pub fn sync_owner_online(&self, thread_id: &str) -> bool {
        let Some(device) = self
            .sync_owner(thread_id)
            .and_then(|owner| self.sync.as_ref()?.devices.get(&owner).cloned())
        else {
            return false;
        };
        self.device(&device)
            .is_some_and(|d| d.status == super::DeviceStatus::Online)
    }

    pub fn sync_is_remote(&self, thread_id: &str) -> bool {
        self.sync_owner(thread_id)
            .is_some_and(|owner| owner != self.settings.install_id)
    }

    /// Opens another device's conversation without resuming it there. True
    /// when this path took over; false falls back to the old remote Codex.
    pub(super) fn sync_open_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) -> bool {
        let Some(owner) = self.sync_owner(thread_id) else {
            return false;
        };
        if owner == self.settings.install_id {
            return false;
        }
        let Some(sync) = &self.sync else { return false };
        let Some(device) = sync.devices.get(&owner).cloned() else {
            return false;
        };
        let Some(client) = self.device(&device).map(|device| device.node.clone()) else {
            return false;
        };
        let backend = self
            .threads
            .iter()
            .find(|thread| thread.id == thread_id)
            .map(|thread| crate::agents::split_backend(&thread.backend).0.to_string())
            .unwrap_or_else(|| "codex".into());
        let peer = sync_peer(&client);
        let cache = sync.cache.clone();
        let id = thread_id.to_string();
        let device_for_cache = device.clone();
        self.loading.insert(id.clone());
        cx.spawn(async move |this, cx| {
            let backend_for_read = backend.clone();
            let id_for_read = id.clone();
            let device_for_read = device_for_cache.clone();
            let cache_for_read = cache.clone();
            let (answer, error) = cx
                .background_executor()
                .spawn(async move {
                    let remote = peer.snapshot(&backend_for_read, &id_for_read);
                    let error = remote.as_ref().err().map(|error| error.to_string());
                    let entry = remote.as_ref().ok().map(|snapshot| {
                        (
                            snapshot.items.clone(),
                            snapshot.approvals.clone(),
                            snapshot.questions.clone(),
                            snapshot.cursor,
                            snapshot.thread.updated_at,
                        )
                    });
                    let cached = if entry.is_none() {
                        cache_for_read
                            .get(&device_for_read, &backend_for_read, &id_for_read)
                            .map(|entry| {
                                (
                                    entry.items,
                                    entry.approvals,
                                    entry.questions,
                                    entry.cursor,
                                    0,
                                )
                            })
                    } else {
                        None
                    };
                    (entry.or(cached), error)
                })
                .await;
            let _ = this.update(cx, |workspace, cx| {
                workspace.loading.remove(&id);
                match answer {
                    Some((items, approvals, questions, cursor, updated_at)) => {
                        let mut transcript = Transcript::from_items(items);
                        for request in approvals {
                            transcript.apply(&AgentEvent::ApprovalRequested(request));
                        }
                        for request in questions {
                            transcript.apply(&AgentEvent::UserInputRequested(request));
                        }
                        workspace.transcripts.insert(id.clone(), transcript);
                        workspace.sync_cursor(&id, cursor);
                        if updated_at > 0
                            && let Some(thread) =
                                workspace.threads.iter_mut().find(|thread| thread.id == id)
                        {
                            thread.updated_at = updated_at;
                        }
                        workspace.sync_watch_thread(
                            id.clone(),
                            device_for_cache.clone(),
                            backend.clone(),
                            cx,
                        );
                    }
                    None => {
                        cx.emit(Notice::Error(crate::i18n::tf(
                            "Could not read this conversation from {device}: {error}",
                            &[
                                ("device", &device_for_cache),
                                ("error", &error.unwrap_or_else(|| "no cached copy".into())),
                            ],
                        )));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        true
    }

    fn sync_cursor(&mut self, thread_id: &str, cursor: u64) {
        if let Some(sync) = &mut self.sync {
            sync.cursors.insert(thread_id.to_string(), cursor);
        }
    }

    fn sync_watch_thread(
        &mut self,
        id: String,
        device: String,
        backend: String,
        cx: &mut Context<Self>,
    ) {
        let Some(sync) = &self.sync else { return };
        let Some(node) = self.device(&device).map(|device| device.node.clone()) else {
            return;
        };
        let peer = sync_peer(&node);
        let cache = sync.cache.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let peer = peer.clone();
                let cache = cache.clone();
                let device_for_read = device.clone();
                let backend_for_read = backend.clone();
                let id_for_read = id.clone();
                let snapshot = cx
                    .background_executor()
                    .spawn(async move {
                        let snapshot = peer.snapshot(&backend_for_read, &id_for_read).ok()?;
                        let _ = cache.put(
                            &device_for_read,
                            &backend_for_read,
                            &id_for_read,
                            &atrium_sync::CacheEntry {
                                items: snapshot.items.clone(),
                                approvals: snapshot.approvals.clone(),
                                questions: snapshot.questions.clone(),
                                cursor: snapshot.cursor,
                                fetched_at_ms: atrium_node::sync::now_ms(),
                            },
                        );
                        Some(snapshot)
                    })
                    .await;
                let Some(snapshot) = snapshot else { continue };
                let keep = this.update(cx, |workspace, cx| {
                    if workspace.view != View::Thread(id.clone()) {
                        return false;
                    }
                    let cursor = workspace
                        .sync
                        .as_ref()
                        .and_then(|sync| sync.cursors.get(&id).copied())
                        .unwrap_or_default();
                    if snapshot.cursor == cursor {
                        return true;
                    }
                    let running = snapshot.thread.state == "running";
                    let approvals = snapshot.approvals;
                    let questions = snapshot.questions;
                    if let Some(transcript) = workspace.transcripts.get_mut(&id) {
                        transcript.apply_snapshot(snapshot.items, running);
                        for request in approvals {
                            transcript.apply(&AgentEvent::ApprovalRequested(request));
                        }
                        for request in questions {
                            transcript.apply(&AgentEvent::UserInputRequested(request));
                        }
                    } else {
                        let mut transcript = Transcript::from_items(snapshot.items);
                        for request in approvals {
                            transcript.apply(&AgentEvent::ApprovalRequested(request));
                        }
                        for request in questions {
                            transcript.apply(&AgentEvent::UserInputRequested(request));
                        }
                        workspace.transcripts.insert(id.clone(), transcript);
                    }
                    workspace.sync_cursor(&id, snapshot.cursor);
                    cx.notify();
                    true
                });
                if !matches!(keep, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Sends a follower's turn to the owner's Atrium. True when handled.
    pub(super) fn sync_send_remote(
        &mut self,
        thread_id: &str,
        composed: &Composed,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(owner) = self.sync_owner(thread_id) else {
            return false;
        };
        if owner == self.settings.install_id {
            return false;
        }
        let Some(sync) = &self.sync else { return false };
        let Some(device) = sync.devices.get(&owner).cloned() else {
            return false;
        };
        let Some(node) = self.device(&device).map(|device| device.node.clone()) else {
            return false;
        };
        let backend = self
            .threads
            .iter()
            .find(|thread| thread.id == thread_id)
            .map(|thread| crate::agents::split_backend(&thread.backend).0.to_string())
            .unwrap_or_else(|| "codex".into());
        let peer = sync_peer(&node);
        let text = composed.text.clone();
        let local_attachments = composed.attachments();
        let model = self.settings.model.clone();
        let effort = self.settings.effort.clone();
        let id = thread_id.to_string();
        if let Some(transcript) = self.transcripts.get_mut(thread_id) {
            transcript.begin_pending_turn();
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let device_for_send = device.clone();
            let id_for_send = id.clone();
            let backend_for_send = backend.clone();
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let mut remote_attachments = Vec::new();
                    for attachment in &local_attachments {
                        let path = PathBuf::from(attachment);
                        let name = path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("attachment");
                        let uploaded = std::fs::read(&path)
                            .map_err(|error| error.to_string())
                            .and_then(|bytes| {
                                peer.upload(name, &bytes).map_err(|error| error.to_string())
                            });
                        match uploaded {
                            Ok(path) => remote_attachments.push(path),
                            Err(error) => {
                                return Err(format!(
                                    "Could not upload {name} to {device_for_send}: {error}"
                                ));
                            }
                        }
                    }
                    let input = TurnInput {
                        text,
                        attachments: remote_attachments,
                        model,
                        effort,
                        mode: None,
                    };
                    peer.turn(&backend_for_send, &id_for_send, &input)
                        .map_err(|error| format!("Could not send to {device_for_send}: {error}"))
                })
                .await;
            match outcome {
                Ok(()) => {
                    let _ = this.update(cx, |workspace, cx| {
                        workspace.sync_watch_thread(
                            id.clone(),
                            device.clone(),
                            backend.clone(),
                            cx,
                        );
                        cx.notify();
                    });
                }
                Err(error) => {
                    let _ = this.update(cx, |workspace, cx| {
                        if let Some(transcript) = workspace.transcripts.get_mut(&id) {
                            transcript.fail_pending_turn(error);
                        }
                        cx.notify();
                    });
                }
            }
        })
        .detach();
        true
    }

    pub(super) fn sync_remote_approval(
        &mut self,
        thread_id: &str,
        request: serde_json::Value,
        decision: ApprovalDecision,
        cx: &mut Context<Self>,
    ) {
        let Some((peer, device, backend)) = self.sync_peer_for(thread_id) else {
            return;
        };
        let decision_value = decision;
        let decision = match decision {
            ApprovalDecision::Accept => "accept",
            ApprovalDecision::AcceptForSession => "accept_for_session",
            ApprovalDecision::Decline => "decline",
            ApprovalDecision::Cancel => "cancel",
        };
        if let Some(transcript) = self.transcripts.get_mut(thread_id) {
            transcript.answer_approval(&request, decision_value);
        }
        let id = thread_id.to_string();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    peer.approve(&backend, &id, &request, decision)
                        .map_err(|error| error.to_string())
                })
                .await;
            if let Err(error) = result {
                let _ = this.update(cx, |_, cx| {
                    cx.emit(Notice::Error(format!(
                        "Could not answer on {device}: {error}"
                    )));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub(super) fn sync_remote_answers(
        &mut self,
        thread_id: &str,
        request: serde_json::Value,
        answers: Vec<(String, Vec<String>)>,
        cx: &mut Context<Self>,
    ) {
        let Some((peer, device, backend)) = self.sync_peer_for(thread_id) else {
            return;
        };
        if let Some(transcript) = self.transcripts.get_mut(thread_id) {
            transcript.answer_questions(&request, answers.clone());
        }
        let answers = answers.into_iter().collect();
        let id = thread_id.to_string();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    peer.answer(&backend, &id, &request, &answers)
                        .map_err(|error| error.to_string())
                })
                .await;
            if let Err(error) = result {
                let _ = this.update(cx, |_, cx| {
                    cx.emit(Notice::Error(format!(
                        "Could not answer on {device}: {error}"
                    )));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub(super) fn sync_remote_interrupt(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let Some((peer, device, backend)) = self.sync_peer_for(thread_id) else {
            return;
        };
        let id = thread_id.to_string();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    peer.interrupt(&backend, &id)
                        .map_err(|error| error.to_string())
                })
                .await;
            if let Err(error) = result {
                let _ = this.update(cx, |_, cx| {
                    cx.emit(Notice::Error(format!(
                        "Could not stop the turn on {device}: {error}"
                    )));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Packages and releases a local conversation for transfer.
    fn sync_owner_move_package(
        &mut self,
        backend: &str,
        id: &str,
    ) -> Result<MovePackage, HostError> {
        let thread = self
            .threads
            .iter()
            .find(|thread| thread.id == id && thread.backend == backend)
            .cloned()
            .ok_or(HostError::NotFound)?;
        let transcript = self
            .transcripts
            .get(id)
            .ok_or_else(|| HostError::Unavailable("the conversation is not open".into()))?;
        if transcript.is_running() {
            return Err(HostError::Conflict(
                "stop the running turn before moving the conversation".into(),
            ));
        }
        if !crate::agents::is_codex(backend) {
            return Err(HostError::Unavailable(
                "moving an ACP conversation is not implemented yet".into(),
            ));
        }
        let codex_home = self
            .client
            .as_ref()
            .map(|client| PathBuf::from(&client.codex_home))
            .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
            .ok_or_else(|| HostError::Unavailable("no Codex home".into()))?;
        let (session_relative, session) = find_session_file(&codex_home, id).ok_or_else(|| {
            HostError::Unavailable("Codex has no session file for this conversation".into())
        })?;
        let git = repo::root(Path::new(&thread.cwd))
            .map(|_| repo::move_bundle(Path::new(&thread.cwd), id))
            .transpose()
            .map_err(|error| HostError::Unavailable(format!("{error:#}")))?;
        if session.len() > 32 * 1024 * 1024
            || git
                .as_ref()
                .is_some_and(|(bundle, ..)| bundle.len() > 32 * 1024 * 1024)
        {
            return Err(HostError::Unavailable(
                "the session or working tree is too large to move (32 MB limit)".into(),
            ));
        }
        let (bundle, git_ref, git_commit, identity) = match git {
            Some((bundle, reference, commit, identity)) => (
                Some(BASE64.encode(bundle)),
                Some(reference),
                Some(commit),
                Some(identity),
            ),
            None => (None, None, None, None),
        };
        Ok(MovePackage {
            thread: ThreadAnnouncement::from_meta(thread, self.settings.install_id.clone(), "idle"),
            items: transcript_items(transcript),
            approvals: transcript.pending_approvals().cloned().collect(),
            questions: transcript.pending_questions().cloned().collect(),
            session_relative: Some(session_relative),
            session_base64: Some(BASE64.encode(session)),
            git_identity: identity,
            git_bundle_base64: bundle,
            git_ref,
            git_commit,
        })
    }

    fn sync_owner_move_release(
        &mut self,
        _backend: &str,
        id: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), HostError> {
        self.threads.retain(|thread| thread.id != id);
        self.transcripts.remove(id);
        self.turn_started.remove(id);
        self.loading.remove(id);
        if let Some(sync) = &mut self.sync {
            sync.owners.remove(id);
            sync.cursors.remove(id);
        }
        if self.view == View::Thread(id.to_string()) {
            self.view = View::Draft;
        }
        // Codex keeps its writer lock per app-server. Releasing the whole
        // process is blunt, but it is the reliable way to let the target
        // resume the same thread; the other chats reconnect immediately.
        self.client = None;
        self.connect(cx);
        cx.notify();
        Ok(())
    }

    /// Moves a remote conversation and its working tree onto this computer.
    pub fn move_to_this_computer(&mut self, thread_id: String, cx: &mut Context<Self>) {
        let Some((peer, device, backend)) = self.sync_peer_for(&thread_id) else {
            return;
        };
        let Some(owner) = self.sync_owner(&thread_id) else {
            return;
        };
        let Some(local_client) = self.client.clone() else {
            cx.emit(Notice::Error("Codex is not running.".into()));
            return;
        };
        let candidates: Vec<PathBuf> = self
            .settings
            .recent_folders
            .iter()
            .chain(self.settings.projects.iter())
            .cloned()
            .collect();
        let install_id = self.settings.install_id.clone();
        let codex_home = PathBuf::from(&local_client.codex_home);
        let mcp = self.mcp_for_thread(&thread_id, &backend);
        let id = thread_id.clone();
        cx.emit(Notice::Info(crate::i18n::tf(
            "Moving the conversation from {device}…",
            &[("device", &device)],
        )));
        cx.spawn(async move |this, cx| {
            let backend_for_move = backend.clone();
            let job = cx.background_executor().spawn(async move {
                let package = peer
                    .move_package(&backend_for_move, &id)
                    .map_err(|error| format!("{error}"))?;
                let identity = package
                    .git_identity
                    .clone()
                    .ok_or_else(|| "the conversation's folder is not a git project".to_string())?;
                let repo = candidates
                    .iter()
                    .find(|candidate| repo::identity(candidate).as_deref() == Some(&identity))
                    .cloned()
                    .ok_or_else(|| {
                        "no checkout of this project on this computer; clone it first".to_string()
                    })?;
                let session = BASE64
                    .decode(
                        package
                            .session_base64
                            .as_deref()
                            .ok_or_else(|| "the package has no session".to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
                let relative = package
                    .session_relative
                    .as_deref()
                    .ok_or_else(|| "the package has no session path".to_string())?;
                write_session_file(&codex_home, relative, &session)?;
                let bundle = BASE64
                    .decode(
                        package
                            .git_bundle_base64
                            .as_deref()
                            .ok_or_else(|| "the package has no code".to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
                let reference = package
                    .git_ref
                    .as_deref()
                    .ok_or_else(|| "the package has no code ref".to_string())?;
                let folder = repo::apply_move_bundle(
                    &repo,
                    &bundle,
                    reference,
                    &id,
                    &super::chat_trees::worktrees_base(),
                )
                .map_err(|error| format!("{error:#}"))?;
                // The owner must drop its writer before the target resumes.
                peer.move_release(&backend_for_move, &id)
                    .map_err(|error| format!("{error}"))?;
                let resumed = local_client
                    .thread_resume_in_folder(&id, &mcp, &folder)
                    .await;
                Ok::<_, String>((package, folder, resumed.is_ok()))
            });
            let result = job.await;
            let _ = this.update(cx, |workspace, cx| match result {
                Ok((package, folder, resumed)) => {
                    workspace.finish_move(
                        AppliedMove {
                            package,
                            folder,
                            old_owner: owner,
                            device,
                            install_id,
                            backend,
                            resumed,
                        },
                        cx,
                    );
                }
                Err(error) => {
                    cx.emit(Notice::Error(crate::i18n::tf(
                        "Could not move the conversation: {error}",
                        &[("error", &error)],
                    )));
                }
            });
        })
        .detach();
    }

    fn finish_move(&mut self, applied: AppliedMove, cx: &mut Context<Self>) {
        let AppliedMove {
            package,
            folder,
            old_owner,
            device,
            install_id,
            backend,
            resumed,
        } = applied;
        let id = package.thread.id.clone();
        let mut meta = package.thread.into_meta();
        meta.cwd = folder.display().to_string();
        meta.backend = backend.clone();
        if let Some(thread) = self.threads.iter_mut().find(|thread| thread.id == id) {
            *thread = meta;
        } else {
            self.threads.push(meta);
        }
        let mut transcript = Transcript::from_items(package.items);
        for request in package.approvals {
            transcript.apply(&AgentEvent::ApprovalRequested(request));
        }
        for request in package.questions {
            transcript.apply(&AgentEvent::UserInputRequested(request));
        }
        self.transcripts.insert(id.clone(), transcript);
        if let Some(sync) = &mut self.sync {
            sync.owners.insert(id.clone(), install_id.clone());
            sync.cursors.insert(id.clone(), 0);
            let key = thread_key(&old_owner, &backend, &id, "moved_to");
            if let Err(error) = sync.state.set(key, serde_json::json!(install_id)) {
                log::warn!("move overlay: {error}");
            }
        }
        self.view = View::Thread(id.clone());
        self.nav += 1;
        self.sync_refresh_overlay();
        cx.emit(Notice::Success(crate::i18n::tf(
            "Moved the conversation from {device}.",
            &[("device", &device)],
        )));
        if !resumed {
            cx.emit(Notice::Error(
                "The files moved, but Codex could not reopen the session; its history is shown read-only."
                    .into(),
            ));
        }
        cx.notify();
    }

    fn sync_peer_for(&self, thread_id: &str) -> Option<(PeerClient, String, String)> {
        let owner = self.sync_owner(thread_id)?;
        if owner == self.settings.install_id {
            return None;
        }
        let sync = self.sync.as_ref()?;
        let device = sync.devices.get(&owner)?.clone();
        let node = self.device(&device)?.node.clone();
        let backend = self
            .threads
            .iter()
            .find(|thread| thread.id == thread_id)
            .map(|thread| crate::agents::split_backend(&thread.backend).0.to_string())
            .unwrap_or_else(|| "codex".into());
        Some((sync_peer(&node), device, backend))
    }
}

fn transcript_items(transcript: &Transcript) -> Vec<Item> {
    transcript
        .entries()
        .iter()
        .filter_map(|entry| match &entry.kind {
            atrium_core::EntryKind::Item(item) => Some(item.clone()),
            _ => None,
        })
        .collect()
}

fn sync_peer(node: &agent_rpc::node::NodeClient) -> PeerClient {
    PeerClient::new(node.base.clone(), node.token())
}

fn remote_thread_meta(announcement: ThreadAnnouncement, device: &str) -> ThreadMeta {
    let mut meta = announcement.into_meta();
    let base = crate::agents::split_backend(&meta.backend).0;
    meta.backend = format!("{base}@{device}");
    meta
}

fn find_session_file(home: &Path, id: &str) -> Option<(String, Vec<u8>)> {
    let sessions = home.join("sessions");
    let mut found = Vec::new();
    let mut stack = vec![sessions.clone()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path.file_name()?.to_string_lossy();
            if name.contains(id) && (name.ends_with(".jsonl") || name.ends_with(".jsonl.zst")) {
                let modified = entry
                    .metadata()
                    .ok()
                    .and_then(|meta| meta.modified().ok())
                    .unwrap_or(std::time::UNIX_EPOCH);
                found.push((modified, path));
            }
        }
    }
    found.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    let (_, path) = found.into_iter().next()?;
    let relative = path
        .strip_prefix(&sessions)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    Some((relative, std::fs::read(path).ok()?))
}

fn write_session_file(home: &Path, relative: &str, bytes: &[u8]) -> Result<(), String> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err("the session path is outside Codex's sessions folder".into());
    }
    let target = home.join("sessions").join(relative);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temp = target.with_extension("part");
    std::fs::write(&temp, bytes).map_err(|error| error.to_string())?;
    std::fs::rename(temp, target).map_err(|error| error.to_string())
}
