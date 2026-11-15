//! The app's state: one connection to Codex, the threads it knows, and a
//! transcript per open thread. Views read it and call its methods; they never
//! talk to the backend themselves.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    time::{Duration, Instant},
};

use agentdesk_core::{AgentEvent, ApprovalDecision, Item, RequestId, ThreadMeta, Transcript};
use codex_client::{CodexClient, CodexConfig, Incoming, ModelInfo, StartThread, translate};
use futures::StreamExt;
use gpui::{Context, Task};

use crate::settings::Settings;

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

pub struct Workspace {
    pub status: Status,
    client: Option<CodexClient>,
    pub threads: Vec<ThreadMeta>,
    pub view: View,
    transcripts: HashMap<String, Transcript>,
    /// The draft's transcript, shown while its thread is being created.
    draft: Transcript,
    loading: HashSet<String>,
    pub models: Vec<ModelInfo>,
    pub settings: Settings,
    pub signed_in: Option<bool>,
    pub backend_label: String,
    /// When each reasoning item started, and how long it took once done.
    timings: HashMap<String, (Instant, Option<Duration>)>,
    _pump: Option<Task<()>>,
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut workspace = Self {
            status: Status::Connecting,
            client: None,
            threads: Vec::new(),
            view: View::Draft,
            transcripts: HashMap::new(),
            draft: Transcript::new(),
            loading: HashSet::new(),
            models: Vec::new(),
            settings: Settings::load(),
            signed_in: None,
            backend_label: "Codex".into(),
            timings: HashMap::new(),
            _pump: None,
        };
        workspace.connect(cx);
        workspace
    }

    /// Starts (or restarts) the Codex server and pumps its events into the state.
    pub fn connect(&mut self, cx: &mut Context<Self>) {
        self.status = Status::Connecting;
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
                    Ok(threads) => ws.threads = threads,
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
    }

    fn handle(&mut self, incoming: Incoming, cx: &mut Context<Self>) {
        let event = match incoming {
            Incoming::Notification { method, params } => translate::event(&method, &params),
            Incoming::Request { id, method, params } => {
                let event = translate::request(&id, &method, &params);
                if event.is_none() {
                    log::warn!("declining unsupported server request {method}");
                    if let Some(client) = &self.client {
                        let _ = client.reject_request(id, &method);
                    }
                }
                event
            }
            Incoming::Closed => Some(AgentEvent::Disconnected {
                reason: "Codex exited".into(),
            }),
        };
        if let Some(event) = event {
            self.apply(event);
            cx.notify();
        }
    }

    fn apply(&mut self, event: AgentEvent) {
        self.time(&event);
        match &event {
            AgentEvent::ThreadStarted(meta) => self.upsert_thread(meta.clone()),
            AgentEvent::ThreadNameUpdated { thread_id, name } => {
                if let Some(thread) = self.threads.iter_mut().find(|t| t.id == *thread_id) {
                    thread.name = name.clone();
                }
            }
            AgentEvent::TurnCompleted { thread_id, .. } => {
                if let Some(thread) = self.threads.iter_mut().find(|t| t.id == *thread_id) {
                    thread.updated_at = jiff::Timestamp::now().as_second();
                }
                self.threads
                    .sort_by_key(|t| std::cmp::Reverse(t.updated_at));
            }
            AgentEvent::Warning(message) => log::warn!("codex: {message}"),
            AgentEvent::Disconnected { reason } => {
                self.status = Status::Failed(reason.clone());
                self.client = None;
                for transcript in self.transcripts.values_mut() {
                    transcript.apply(&event);
                }
                return;
            }
            _ => {}
        }
        if let Some(thread_id) = event.thread_id()
            && let Some(transcript) = self.transcripts.get_mut(thread_id)
        {
            transcript.apply(&event);
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
    pub fn thought_for(&self, item_id: &str) -> Duration {
        match self.timings.get(item_id) {
            Some((_, Some(took))) => *took,
            Some((start, None)) => start.elapsed(),
            None => Duration::ZERO,
        }
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

    /// The transcript the main column shows.
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

    pub fn new_chat(&mut self, cx: &mut Context<Self>) {
        if !self.draft.is_running() {
            self.draft = Transcript::new();
        }
        self.view = View::Draft;
        cx.notify();
    }

    pub fn open_thread(&mut self, thread_id: String, cx: &mut Context<Self>) {
        self.view = View::Thread(thread_id.clone());
        cx.notify();
        if self.transcripts.contains_key(&thread_id) || self.loading.contains(&thread_id) {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        self.loading.insert(thread_id.clone());
        cx.spawn(async move |this, cx| {
            let resumed = client.thread_resume(&thread_id).await;
            let _ = this.update(cx, |ws, cx| {
                ws.loading.remove(&thread_id);
                match resumed {
                    Ok((meta, turns)) => {
                        ws.upsert_thread(meta);
                        ws.transcripts
                            .insert(thread_id, Transcript::from_history(turns));
                    }
                    Err(error) => {
                        let mut transcript = Transcript::new();
                        transcript
                            .fail_pending_turn(format!("Could not open this chat: {error:#}"));
                        ws.transcripts.insert(thread_id, transcript);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Sends a message in the current view, creating the thread for a draft.
    pub fn send(&mut self, text: String, cx: &mut Context<Self>) {
        let text = text.trim().to_string();
        if text.is_empty() || self.is_running() {
            return;
        }
        let Some(client) = self.client.clone() else {
            log::warn!("send: not connected");
            return;
        };
        let model = self.settings.model.clone();
        match self.view.clone() {
            View::Thread(thread_id) => {
                let transcript = self.transcripts.entry(thread_id.clone()).or_default();
                transcript.push_local_user(text.clone());
                transcript.begin_pending_turn();
                cx.notify();
                cx.spawn(async move |this, cx| {
                    let sent = client.turn_start(&thread_id, &text, model.as_deref()).await;
                    if let Err(error) = sent {
                        let _ = this.update(cx, |ws, cx| {
                            if let Some(t) = ws.transcripts.get_mut(&thread_id) {
                                t.fail_pending_turn(format!("Could not send: {error:#}"));
                            }
                            cx.notify();
                        });
                    }
                })
                .detach();
            }
            View::Draft => {
                self.draft = Transcript::new();
                self.draft.push_local_user(text.clone());
                self.draft.begin_pending_turn();
                cx.notify();
                let options = StartThread {
                    cwd: Some(self.settings.working_dir().display().to_string()),
                    model: model.clone(),
                    approval_policy: Some(
                        self.settings
                            .approval_policy
                            .clone()
                            .unwrap_or_else(|| "on-request".into()),
                    ),
                    sandbox: Some(
                        self.settings
                            .sandbox
                            .clone()
                            .unwrap_or_else(|| "workspace-write".into()),
                    ),
                };
                cx.spawn(async move |this, cx| {
                    let started = client.thread_start(options).await;
                    let thread_id = match started {
                        Ok(mut meta) => {
                            // Title the chat by its first message until Codex names it.
                            if meta.preview.trim().is_empty() {
                                meta.preview = text.clone();
                            }
                            let id = meta.id.clone();
                            let ok = this.update(cx, |ws, cx| {
                                ws.upsert_thread(meta);
                                // The draft becomes the thread's transcript.
                                let draft = std::mem::take(&mut ws.draft);
                                ws.transcripts.insert(id.clone(), draft);
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
                                ws.draft.fail_pending_turn(format!(
                                    "Could not start a chat: {error:#}"
                                ));
                                cx.notify();
                            });
                            return;
                        }
                    };
                    if let Err(error) = client.turn_start(&thread_id, &text, model.as_deref()).await
                    {
                        let _ = this.update(cx, |ws, cx| {
                            if let Some(t) = ws.transcripts.get_mut(&thread_id) {
                                t.fail_pending_turn(format!("Could not send: {error:#}"));
                            }
                            cx.notify();
                        });
                    }
                })
                .detach();
            }
        }
    }

    /// Interrupts the running turn.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        let View::Thread(thread_id) = self.view.clone() else {
            return;
        };
        let Some(turn_id) = self
            .transcripts
            .get(&thread_id)
            .and_then(|t| t.active_turn())
            .filter(|turn| !turn.is_empty())
            .map(str::to_owned)
        else {
            return;
        };
        let Some(client) = self.client.clone() else {
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
        let Some(client) = &self.client else { return };
        if let Err(error) = client.answer_approval(request_id.clone(), decision) {
            log::warn!("answering approval failed: {error:#}");
            return;
        }
        if let Some(transcript) = self.transcripts.get_mut(thread_id) {
            transcript.answer_approval(&request_id, decision);
        }
        cx.notify();
    }

    pub fn archive(&mut self, thread_id: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.threads.retain(|t| t.id != thread_id);
        self.transcripts.remove(&thread_id);
        if self.view == View::Thread(thread_id.clone()) {
            self.view = View::Draft;
            self.draft = Transcript::new();
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            if let Err(error) = client.thread_archive(&thread_id).await {
                log::warn!("thread/archive failed: {error:#}");
                let _ = this.update(cx, |ws, cx| ws.refresh(cx));
            }
        })
        .detach();
    }

    pub fn set_model(&mut self, model: Option<String>, cx: &mut Context<Self>) {
        self.settings.model = model;
        self.settings.save();
        cx.notify();
    }

    pub fn set_working_dir(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        self.settings.cwd = Some(dir);
        self.settings.save();
        cx.notify();
    }

    /// The model new turns use, for display.
    pub fn model_label(&self) -> String {
        let chosen = self
            .settings
            .model
            .clone()
            .or_else(|| self.active_thread().and_then(|t| t.model.clone()))
            .or_else(|| {
                self.models
                    .iter()
                    .find(|m| m.is_default)
                    .map(|m| m.id.clone())
            });
        match chosen {
            Some(id) => self
                .models
                .iter()
                .find(|m| m.id == id)
                .map(|m| m.display_name.clone())
                .unwrap_or(id),
            None => "Default model".into(),
        }
    }
}

/// `codex_cli_rs/0.169.0 (...)` or `agentdesk/0.169.0 (...)` to `Codex 0.169.0`.
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
    #[test]
    fn the_backend_label_names_the_version() {
        assert_eq!(
            super::backend_label("agentdesk/0.169.0 (Mac OS 15.1; arm64) x"),
            "Codex 0.169.0"
        );
        assert_eq!(super::backend_label(""), "Codex");
    }
}
