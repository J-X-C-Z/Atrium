//! The workspace's devices: other machines of the user's tailnet that run
//! `atrium-node`. Their agents join the same lists as this machine's, under
//! backend ids such as `claude@studio`; a Codex there gets its own client.

use crate::i18n::{t, tf};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use agent_rpc::node::{self, LinkState, NodeClient, NodeInfo, RemoteProc};
use atrium_core::AgentEvent;
use codex_client::{CodexClient, Incoming, translate};
use futures::StreamExt;
use gpui::{Context, Task};

use super::{AcpAgent, AgentStatus, Notice, Workspace, usage};
use crate::agents::{self, AgentProfile};

/// Whether a device answers.
#[derive(Clone, Debug, PartialEq)]
pub enum DeviceStatus {
    Online,
    /// Missed a poll or lost its link, but not for long enough to call it
    /// offline: still tried, shown as reconnecting.
    Reconnecting,
    Offline(String),
}

impl DeviceStatus {
    /// Worth talking to: answers, or may be about to again.
    pub fn is_reachable(&self) -> bool {
        !matches!(self, DeviceStatus::Offline(_))
    }
}

/// Consecutive missed polls after which a device is called offline.
pub const MISS_LIMIT: u32 = 3;
/// With at least two misses, this long without any sign of life is enough.
pub const SILENCE_WINDOW: Duration = Duration::from_secs(45);

/// What the live links to a device say.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LinkHealth {
    /// An attach connection is up: proof the node answers.
    pub alive: bool,
    /// A link is trying to get back.
    pub reconnecting: bool,
}

/// The verdict on a device.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Verdict {
    Online,
    Reconnecting,
    Offline,
}

/// Decides a device's state from consecutive missed polls, how long it has
/// been silent (no poll answered, no RPC or event) and its links.
pub fn judge(misses: u32, silent_for: Duration, link: LinkHealth) -> Verdict {
    if link.alive {
        return Verdict::Online;
    }
    if misses == 0 {
        return if link.reconnecting {
            Verdict::Reconnecting
        } else {
            Verdict::Online
        };
    }
    if misses >= MISS_LIMIT || (misses >= 2 && silent_for >= SILENCE_WINDOW) {
        Verdict::Offline
    } else {
        Verdict::Reconnecting
    }
}

/// When a device last showed life, and how many polls missed since.
#[derive(Clone, Copy, Debug)]
pub struct Liveness {
    pub misses: u32,
    pub last_seen: Instant,
}

impl Liveness {
    pub fn new(now: Instant) -> Self {
        Self {
            misses: 0,
            last_seen: now,
        }
    }

    /// A poll, RPC, event or link proved the device is there.
    pub fn alive(&mut self, now: Instant) {
        self.misses = 0;
        self.last_seen = now;
    }

    /// A poll did not find it.
    pub fn missed(&mut self) {
        self.misses = self.misses.saturating_add(1);
    }

    pub fn verdict(&self, now: Instant, link: LinkHealth) -> Verdict {
        judge(
            self.misses,
            now.saturating_duration_since(self.last_seen),
            link,
        )
    }
}

/// Another device's node and what runs there for Atrium.
pub struct Device {
    /// Names its backends: `codex@<id>`.
    pub id: String,
    pub node: NodeClient,
    pub info: NodeInfo,
    pub status: DeviceStatus,
    pub live: Liveness,
    pub codex: Option<CodexClient>,
    pub codex_status: AgentStatus,
    pub codex_link: Option<Arc<Mutex<LinkState>>>,
    /// The models that device's Codex offers.
    pub models: Vec<codex_client::ModelInfo>,
    _codex_pump: Option<Task<()>>,
}

impl Device {
    /// Records whether a poll or call reached the device and sets its
    /// status. Returns true when it was offline and is back.
    pub fn observe(&mut self, reached: bool, now: Instant, link: LinkHealth) -> bool {
        let was_offline = !self.status.is_reachable();
        if reached || link.alive {
            self.live.alive(now);
        } else {
            self.live.missed();
        }
        self.status = match self.live.verdict(now, link) {
            Verdict::Online => DeviceStatus::Online,
            Verdict::Reconnecting => DeviceStatus::Reconnecting,
            Verdict::Offline => match &self.status {
                DeviceStatus::Offline(why) => DeviceStatus::Offline(why.clone()),
                _ => DeviceStatus::Offline("did not answer".into()),
            },
        };
        was_offline && self.status.is_reachable()
    }

    /// The folder agents start in, and new chats run in, unless chosen.
    pub fn default_folder(&self) -> String {
        self.info
            .roots
            .first()
            .cloned()
            .unwrap_or_else(|| self.info.home.clone())
    }
}

/// An ACP agent on another device: where to start it.
#[derive(Clone, Debug)]
pub struct RemoteAgent {
    pub node: NodeClient,
    pub device: String,
    /// The agent's id on the node.
    pub agent: String,
    pub cwd: String,
}

/// What discovery found: the nodes, and how the tailnet answered.
pub struct Found {
    nodes: Vec<(NodeClient, NodeInfo)>,
    tailnet: Result<usize, String>,
}

/// A device name as it appears in backend ids: lower case, letters,
/// digits and dashes.
fn device_id(name: &str) -> String {
    let id: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let id = id.trim_matches('-').to_string();
    if id.is_empty() { "device".into() } else { id }
}

impl Workspace {
    pub(super) fn init_devices(&mut self, cx: &mut Context<Self>) {
        if self.settings.install_id.is_empty() {
            self.settings.install_id = format!(
                "{:x}{:x}",
                jiff::Timestamp::now().as_nanosecond(),
                std::process::id()
            );
            self.settings.save();
        }
        self.init_claude_usage(cx);
        self._discovery = Some(cx.spawn(async move |this, cx| {
            loop {
                if this.update(cx, |ws, cx| ws.discover(cx)).is_err() {
                    break;
                }
                // Poll faster while a device is being doubted, so a blip
                // clears (or is confirmed) in well under three minutes.
                let wait = this
                    .read_with(cx, |ws, _| {
                        if ws.devices.iter().any(|d| d.live.misses > 0) {
                            15
                        } else {
                            60
                        }
                    })
                    .unwrap_or(60);
                cx.background_executor()
                    .timer(Duration::from_secs(wait))
                    .await;
            }
        }));
    }

    /// Every five minutes, reads the plan usage of each Claude Code in use:
    /// this machine's through its own login, a device's through its node.
    pub(super) fn init_claude_usage(&mut self, cx: &mut Context<Self>) {
        let local = Arc::new(agent_rpc::claude_usage::ClaudeUsage::system());
        self._claude_usage = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(5)).await;
            loop {
                let Ok(targets) = this.read_with(cx, |ws, _| ws.claude_usage_targets()) else {
                    break;
                };
                for (backend, node) in targets {
                    let local = local.clone();
                    let usage = cx
                        .background_executor()
                        .spawn(async move {
                            match node {
                                Some(node) => node.claude_usage().ok().flatten(),
                                None => local.get(),
                            }
                        })
                        .await;
                    let Some(value) = usage else {
                        continue;
                    };
                    let checked_at = usage::iso_at(usage::now_ms()).unwrap_or_default();
                    let probe = usage::claude_oauth_probe(&value, &checked_at);
                    if probe.limits.unavailable.is_some() {
                        continue;
                    }
                    if this
                        .update(cx, |ws, cx| ws.note_claude_probe(&backend, probe, cx))
                        .is_err()
                    {
                        return;
                    }
                }
                cx.background_executor()
                    .timer(Duration::from_secs(300))
                    .await;
            }
        }));
    }

    /// The running Claude Code backends to ask about usage; a device's carry
    /// its node.
    fn claude_usage_targets(&self) -> Vec<(String, Option<NodeClient>)> {
        self.agents
            .iter()
            .filter(|a| agents::is_claude(&a.profile.id) && a.status == AgentStatus::Ready)
            .map(|a| {
                (
                    a.profile.id.clone(),
                    a.remote.as_ref().map(|r| r.node.clone()),
                )
            })
            .collect()
    }

    /// Looks for nodes: the tailnet's devices, plus addresses added by hand.
    pub fn discover(&mut self, cx: &mut Context<Self>) {
        if self.discovering {
            return;
        }
        self.discovering = true;
        cx.notify();
        let manual: Vec<String> = self
            .settings
            .nodes
            .iter()
            .cloned()
            .chain(
                std::env::var("ATRIUM_NODES")
                    .ok()
                    .into_iter()
                    .flat_map(|v| v.split(',').map(str::to_owned).collect::<Vec<_>>()),
            )
            .filter(|a| !a.trim().is_empty())
            .collect();
        let token = self.settings.node_token.clone();
        let paired = self.settings.paired.clone();
        let use_tailnet = std::env::var("ATRIUM_NO_TAILSCALE").is_err();
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move {
                    let mut nodes: Vec<(NodeClient, NodeInfo)> = manual
                        .iter()
                        .filter_map(|address| {
                            let token = paired.get(address).cloned().or_else(|| token.clone());
                            let client = NodeClient::new(address, token);
                            match client.info() {
                                Ok(info) => Some((client, info)),
                                Err(error) => {
                                    log::info!("no node at {address}: {error}");
                                    None
                                }
                            }
                        })
                        .collect();
                    let tailnet = if use_tailnet {
                        match node::tailnet_peers() {
                            Ok(peers) => {
                                let online = peers.iter().filter(|p| p.online).count();
                                for found in node::find_nodes(&peers, token.clone()) {
                                    if !nodes
                                        .iter()
                                        .any(|(_, i)| i.device.name == found.1.device.name)
                                    {
                                        nodes.push(found);
                                    }
                                }
                                Ok(online)
                            }
                            Err(error) => Err(error),
                        }
                    } else {
                        Err(t("Tailscale discovery is off").into())
                    };
                    Found { nodes, tailnet }
                })
                .await;
            let _ = this.update(cx, |ws, cx| ws.found_devices(found, cx));
        })
        .detach();
    }

    fn found_devices(&mut self, found: Found, cx: &mut Context<Self>) {
        self.discovering = false;
        self.tailnet = Some(found.tailnet);
        let now = Instant::now();
        let mut seen = Vec::new();
        for (client, info) in found.nodes {
            let id = device_id(if info.device.name.is_empty() {
                &info.device.hostname
            } else {
                &info.device.name
            });
            seen.push(id.clone());
            let health = self.link_health(&id);
            match self.devices.iter_mut().find(|d| d.id == id) {
                Some(device) => {
                    device.node = client;
                    device.info = info;
                    if device.observe(true, now, health) {
                        self.start_device(&id, cx);
                    }
                }
                None => {
                    log::info!("found {} at {}", id, client.base);
                    self.devices.push(Device {
                        id: id.clone(),
                        node: client,
                        info,
                        status: DeviceStatus::Online,
                        live: Liveness::new(now),
                        codex: None,
                        codex_status: AgentStatus::NotInstalled,
                        codex_link: None,
                        models: Vec::new(),
                        _codex_pump: None,
                    });
                    self.start_device(&id, cx);
                }
            }
        }
        // A missed poll is only a miss: links and recent calls still count.
        let missing: Vec<String> = self
            .devices
            .iter()
            .filter(|d| !seen.contains(&d.id))
            .map(|d| d.id.clone())
            .collect();
        for id in missing {
            let health = self.link_health(&id);
            if let Some(device) = self.devices.iter_mut().find(|d| d.id == id) {
                device.observe(false, now, health);
            }
        }
        cx.notify();
    }

    /// Adds a node by its address and looks for it at once.
    pub fn add_node(&mut self, address: String, cx: &mut Context<Self>) {
        let address = address.trim().to_string();
        if address.is_empty() || self.settings.nodes.contains(&address) {
            return;
        }
        self.settings.nodes.push(address);
        self.settings.save();
        self.discover(cx);
    }

    pub fn remove_node(&mut self, address: &str, cx: &mut Context<Self>) {
        self.settings.nodes.retain(|a| a != address);
        self.settings.save();
        cx.notify();
    }

    /// What the open attach links to `id` say about it.
    pub fn link_health(&self, id: &str) -> LinkHealth {
        let mut health = LinkHealth::default();
        let links = self
            .device(id)
            .and_then(|d| d.codex_link.clone())
            .into_iter()
            .chain(
                self.agents
                    .iter()
                    .filter(|a| a.remote.as_ref().is_some_and(|r| r.device == id))
                    .filter_map(|a| a.link.clone()),
            );
        for link in links {
            match &*link.lock().expect("link") {
                LinkState::Connected => health.alive = true,
                LinkState::Reconnecting { .. } => health.reconnecting = true,
                _ => {}
            }
        }
        health
    }

    /// A call or event from `id` just succeeded: proof of life.
    pub fn device_alive(&mut self, id: &str) {
        if let Some(device) = self.devices.iter_mut().find(|d| d.id == id) {
            device.live.alive(Instant::now());
            if device.status != DeviceStatus::Online {
                device.status = DeviceStatus::Online;
            }
        }
    }

    /// The device's status with its live links applied: online, but a link
    /// is trying to get back, shows as reconnecting.
    pub fn device_state(&self, id: &str) -> DeviceStatus {
        let Some(device) = self.device(id) else {
            return DeviceStatus::Offline("did not answer".into());
        };
        match &device.status {
            DeviceStatus::Online => {
                let health = self.link_health(id);
                if health.reconnecting && !health.alive {
                    DeviceStatus::Reconnecting
                } else {
                    DeviceStatus::Online
                }
            }
            other => other.clone(),
        }
    }

    pub fn device(&self, id: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.id == id)
    }

    #[cfg(all(test, unix, feature = "workspace-ticker-test"))]
    pub(super) fn attach_test_node(&mut self, address: &str, token: Option<String>) -> String {
        let client = NodeClient::new(address, token);
        let info = client.info().expect("test node answers");
        let id = device_id(if info.device.name.is_empty() {
            &info.device.hostname
        } else {
            &info.device.name
        });
        self.devices.push(Device {
            id: id.clone(),
            node: client,
            info,
            status: DeviceStatus::Online,
            live: Liveness::new(Instant::now()),
            codex: None,
            codex_status: AgentStatus::NotInstalled,
            codex_link: None,
            models: Vec::new(),
            _codex_pump: None,
        });
        id
    }

    /// Starts what a device offers that is not running yet.
    fn start_device(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(device) = self.devices.iter().find(|d| d.id == id) else {
            return;
        };
        let node = device.node.clone();
        let cwd = device.default_folder();
        let offered: Vec<(String, String, bool)> = device
            .info
            .agents
            .iter()
            .map(|a| (a.id.clone(), a.protocol.clone(), a.available))
            .collect();
        for (agent, protocol, available) in offered {
            if !available {
                continue;
            }
            if protocol == "codex-app-server" || agents::is_codex(&agent) {
                let running = self
                    .device(id)
                    .is_some_and(|d| d.codex.is_some() || d.codex_status == AgentStatus::Starting);
                if !running {
                    self.start_remote_codex(id, cx);
                }
                continue;
            }
            if protocol != "acp" {
                continue;
            }
            let backend = agents::backend_id(&agent, Some(id));
            let existing = self.agents.iter().position(|a| a.profile.id == backend);
            let ix = match existing {
                Some(ix) => {
                    if self.agents[ix].client.is_some()
                        || self.agents[ix].status == AgentStatus::Starting
                    {
                        continue;
                    }
                    ix
                }
                None => {
                    let local = agents::detect(&Default::default())
                        .into_iter()
                        .find(|p| p.id == agent);
                    let (name, short) = match &local {
                        Some(p) => (p.name.clone(), p.short.clone()),
                        None => (agent.clone(), agent.clone()),
                    };
                    self.agents.push(AcpAgent {
                        profile: AgentProfile {
                            id: backend.clone(),
                            name,
                            short,
                            command: None,
                            install: String::new(),
                        },
                        status: AgentStatus::Starting,
                        info: None,
                        client: None,
                        remote: Some(RemoteAgent {
                            node: node.clone(),
                            device: id.to_string(),
                            agent: agent.clone(),
                            cwd: cwd.clone(),
                        }),
                        link: None,
                        _pump: None,
                        reopen: Vec::new(),
                    });
                    self.agents.len() - 1
                }
            };
            self.start_agent(ix, cx);
        }
    }

    /// Runs `codex app-server` on a device and pumps its events.
    pub fn start_remote_codex(&mut self, id: &str, cx: &mut Context<Self>) {
        let owner = self.settings.install_id.clone();
        let Some(device) = self.devices.iter_mut().find(|d| d.id == id) else {
            return;
        };
        let agent = device
            .info
            .agents
            .iter()
            .find(|a| a.protocol == "codex-app-server" || agents::is_codex(&a.id))
            .map(|a| a.id.clone())
            .unwrap_or_else(|| "codex".into());
        let node = device.node.clone();
        let cwd = device.default_folder();
        let device_id = id.to_string();
        let backend = agents::backend_id("codex", Some(id));
        device.codex_status = AgentStatus::Starting;
        device.codex = None;
        cx.notify();
        device._codex_pump = Some(cx.spawn(async move |this, cx| {
            let started = cx
                .background_executor()
                .spawn(async move {
                    let proc = node
                        .spawn(&agent, &cwd, &owner)
                        .map_err(|e| e.to_string())?;
                    let attachment = node.attach(&proc.id, 0);
                    let link = attachment.state.clone();
                    let keep = Arc::new(RemoteProc {
                        client: node.clone(),
                        id: proc.id,
                    });
                    CodexClient::remote(attachment.reader, attachment.writer, keep)
                        .await
                        .map(|(client, events)| (client, events, link))
                        .map_err(|e| format!("{e:#}"))
                })
                .await;
            let (client, mut events, link) = match started {
                Ok(started) => started,
                Err(error) => {
                    let _ = this.update(cx, |ws, cx| {
                        if let Some(d) = ws.devices.iter_mut().find(|d| d.id == device_id) {
                            d.codex_status = AgentStatus::Failed(error);
                        }
                        cx.notify();
                    });
                    return;
                }
            };
            let listing = client.clone();
            let ok = this.update(cx, |ws, cx| {
                if let Some(d) = ws.devices.iter_mut().find(|d| d.id == device_id) {
                    log::info!("Codex on {device_id} ready ({})", client.user_agent);
                    d.codex = Some(client.clone());
                    d.codex_status = AgentStatus::Ready;
                    d.codex_link = Some(link);
                }
                cx.notify();
            });
            if ok.is_err() {
                return;
            }
            if let Ok(limits) = listing.rate_limits().await {
                let _ = this.update(cx, |ws, cx| ws.note_limits(&backend, limits, cx));
            }
            match listing.model_list().await {
                Ok(models) => {
                    let _ = this.update(cx, |ws, cx| {
                        if let Some(d) = ws.devices.iter_mut().find(|d| d.id == device_id) {
                            d.models = models;
                        }
                        cx.notify();
                    });
                }
                Err(error) => log::warn!("model/list on {device_id} failed: {error:#}"),
            }
            if let Ok(threads) = listing.thread_list(50).await {
                let threads = threads
                    .into_iter()
                    .map(|mut t| {
                        t.backend = backend.clone();
                        t
                    })
                    .collect();
                let _ = this.update(cx, |ws, cx| {
                    ws.merge_threads(&backend, threads);
                    cx.notify();
                });
            }
            while let Some(incoming) = events.next().await {
                if this
                    .update(cx, |ws, cx| {
                        ws.handle_remote_codex(&device_id, incoming, cx)
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    /// One message from a device's Codex: its threads are that device's,
    /// and account news is not this machine's.
    fn handle_remote_codex(&mut self, device: &str, incoming: Incoming, cx: &mut Context<Self>) {
        if !matches!(incoming, Incoming::Closed) {
            self.device_alive(device);
        }
        let backend = agents::backend_id("codex", Some(device));
        let event = match incoming {
            Incoming::Notification { method, params } => translate::event(&method, &params),
            Incoming::Request { id, method, params } => {
                let event = translate::request(&id, &method, &params);
                if event.is_none()
                    && let Some(client) = self.device(device).and_then(|d| d.codex.clone())
                {
                    let _ = client.reject_request(id, &method);
                }
                event
            }
            Incoming::Closed => {
                let reason = tf("Codex on {device} stopped", &[("device", &device)]);
                if let Some(d) = self.devices.iter_mut().find(|d| d.id == device) {
                    d.codex = None;
                    d.codex_status = AgentStatus::Failed(reason.clone());
                }
                let ids: Vec<String> = self
                    .threads
                    .iter()
                    .filter(|t| t.backend == backend)
                    .map(|t| t.id.clone())
                    .collect();
                let event = AgentEvent::Disconnected {
                    reason: reason.clone(),
                };
                for id in ids {
                    self.pilot_failure(&id, true);
                    self.turn_started.remove(&id);
                    if let Some(transcript) = self.transcripts.get_mut(&id) {
                        transcript.apply(&event);
                    }
                }
                cx.emit(Notice::Error(reason));
                cx.notify();
                return;
            }
        };
        let Some(event) = event else { return };
        let event = match event {
            AgentEvent::ThreadStarted(mut meta) => {
                meta.backend = backend.clone();
                AgentEvent::ThreadStarted(meta)
            }
            AgentEvent::RateLimitsUpdated(update) => {
                self.note_limits(&backend, update, cx);
                return;
            }
            AgentEvent::AccountChanged | AgentEvent::LoginCompleted { .. } => return,
            event => event,
        };
        self.runtime.observe(&backend, &event);
        self.apply(event, cx);
        cx.notify();
    }

    /// The Codex a backend id names: this machine's, or a device's.
    pub fn codex_for(&self, backend: &str) -> Option<CodexClient> {
        match agents::split_backend(backend) {
            (_, None) => self.client.clone(),
            (_, Some(device)) => self.device(device).and_then(|d| d.codex.clone()),
        }
    }

    /// The Codex a thread runs on.
    pub fn thread_codex(&self, thread_id: &str) -> Option<CodexClient> {
        self.codex_for(self.backend_of(thread_id))
    }

    /// The Codex the shown chat (or a new one) talks to.
    pub fn view_codex(&self) -> Option<CodexClient> {
        self.codex_for(self.view_backend())
    }

    /// Whether a backend's agent is up, here or on its device.
    pub fn backend_ready(&self, backend: &str) -> bool {
        match agents::split_backend(backend) {
            ("codex", None) => self.client.is_some(),
            ("codex", Some(device)) => self.device(device).is_some_and(|d| d.codex.is_some()),
            _ => self
                .agent(backend)
                .is_some_and(|a| a.status == AgentStatus::Ready),
        }
    }

    /// How the link to a backend's device is doing, for remote backends.
    pub fn link_state(&self, backend: &str) -> Option<LinkState> {
        let (agent, device) = agents::split_backend(backend);
        let device = device?;
        let link = if agent == "codex" {
            self.device(device)?.codex_link.clone()
        } else {
            self.agent(backend)?.link.clone()
        };
        link.map(|l| l.lock().expect("link").clone())
    }

    /// Where new chats with `backend` run: this machine's chosen folder, or
    /// the device's.
    pub fn folder_for(&self, backend: &str) -> String {
        match agents::split_backend(backend).1 {
            None => self.settings.working_dir().display().to_string(),
            Some(device) => self
                .settings
                .device_folders
                .get(device)
                .cloned()
                .or_else(|| self.device(device).map(Device::default_folder))
                .unwrap_or_default(),
        }
    }

    /// Another computer's Atrium Workspace folder: set in settings, else
    /// the root of its node named after it.
    pub fn device_workspace(&self, device: &str) -> Option<String> {
        self.settings
            .device_workspaces
            .get(device)
            .cloned()
            .or_else(|| {
                atrium_project::spread::workspace_in_roots(&self.device(device)?.info.roots)
            })
    }

    /// Where a project of this computer's workspace is on `device`, when
    /// the project is inside the workspace and the device has one.
    pub fn project_on_device(&self, root: &std::path::Path, device: &str) -> Option<String> {
        let relative = atrium_project::spread::relative(root, &self.settings.atrium_workspace())?;
        Some(atrium_project::spread::join_on(
            &self.device_workspace(device)?,
            &relative,
        ))
    }

    pub fn set_workspace_dir(&mut self, dir: std::path::PathBuf, cx: &mut Context<Self>) {
        let _ = std::fs::create_dir_all(&dir);
        self.settings.workspace_dir = Some(dir);
        self.settings.save();
        cx.notify();
    }

    pub fn set_device_folder(&mut self, device: &str, folder: String, cx: &mut Context<Self>) {
        self.settings
            .device_folders
            .insert(device.to_string(), folder.clone());
        self.settings.save();
        self.fetch_children(device, folder, cx);
        cx.notify();
    }

    /// The folders in `path` on `device`, once fetched.
    pub fn remote_children(&self, device: &str, path: &str) -> Option<&Vec<String>> {
        self.remote_dirs.get(&format!("{device}:{path}"))
    }

    /// Drops what was fetched for `path` on `device`, so the next look asks again.
    pub fn forget_children(&mut self, device: &str, path: &str) {
        self.remote_dirs.remove(&format!("{device}:{path}"));
    }

    /// Fetches the folders in `path` on `device` for the folder menu.
    pub fn fetch_children(&mut self, device: &str, path: String, cx: &mut Context<Self>) {
        let key = format!("{device}:{path}");
        if self.remote_dirs.contains_key(&key) {
            return;
        }
        let Some(node) = self.device(device).map(|d| d.node.clone()) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let listed = cx
                .background_executor()
                .spawn(async move { node.list(&path) })
                .await;
            let dirs = match listed {
                Ok(entries) => entries
                    .into_iter()
                    .filter(|e| e.dir)
                    .map(|e| e.name)
                    .collect(),
                Err(error) => {
                    log::info!("could not list a folder: {error}");
                    Vec::new()
                }
            };
            let _ = this.update(cx, |ws, cx| {
                ws.remote_dirs.insert(key, dirs);
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: LinkHealth = LinkHealth {
        alive: false,
        reconnecting: false,
    };

    #[test]
    fn one_missed_poll_is_not_offline() {
        assert_eq!(judge(0, Duration::ZERO, NONE), Verdict::Online);
        assert_eq!(
            judge(1, Duration::from_secs(60), NONE),
            Verdict::Reconnecting
        );
    }

    #[test]
    fn offline_after_three_misses_or_silence() {
        assert_eq!(judge(3, Duration::from_secs(1), NONE), Verdict::Offline);
        assert_eq!(
            judge(2, Duration::from_secs(10), NONE),
            Verdict::Reconnecting
        );
        assert_eq!(judge(2, Duration::from_secs(45), NONE), Verdict::Offline);
    }

    #[test]
    fn a_live_link_is_proof_of_life() {
        let alive = LinkHealth {
            alive: true,
            reconnecting: false,
        };
        assert_eq!(judge(9, Duration::from_secs(900), alive), Verdict::Online);
        let retrying = LinkHealth {
            alive: false,
            reconnecting: true,
        };
        assert_eq!(judge(0, Duration::ZERO, retrying), Verdict::Reconnecting);
        assert_eq!(judge(3, Duration::ZERO, retrying), Verdict::Offline);
    }

    #[test]
    fn liveness_resets_on_proof() {
        let t0 = Instant::now();
        let mut live = Liveness::new(t0);
        live.missed();
        live.missed();
        let later = t0 + Duration::from_secs(50);
        assert_eq!(live.verdict(later, NONE), Verdict::Offline);
        live.alive(later);
        assert_eq!(live.misses, 0);
        assert_eq!(live.verdict(later, NONE), Verdict::Online);
    }

    #[test]
    fn reachable_means_not_offline() {
        assert!(DeviceStatus::Online.is_reachable());
        assert!(DeviceStatus::Reconnecting.is_reachable());
        assert!(!DeviceStatus::Offline("x".into()).is_reachable());
    }

    #[test]
    fn device_ids_are_tidy() {
        assert_eq!(super::device_id("Studio Mac"), "studio-mac");
        assert_eq!(super::device_id("NAS_01"), "nas-01");
        assert_eq!(super::device_id("!!"), "device");
    }
}
