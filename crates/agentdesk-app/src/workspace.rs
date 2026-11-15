//! The app's state: one connection to Codex, the threads it knows, and a
//! transcript per open thread. Views read it and call its methods; they never
//! talk to the backend themselves.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    time::{Duration, Instant},
};

use agentdesk_core::{AgentEvent, ApprovalDecision, Item, RequestId, ThreadMeta, Transcript};
use codex_client::{
    AccessMode, CodexClient, CodexConfig, Incoming, ModelInfo, ReviewTarget, StartThread,
    TurnRequest, translate,
};
use futures::StreamExt;
use gpui::{AppContext, Context, EventEmitter, Task};

use crate::settings::{Settings, ThemePref};

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

/// What the composer hands over.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Composed {
    pub text: String,
    pub images: Vec<PathBuf>,
    pub mentions: Vec<PathBuf>,
}

impl Composed {
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
    /// When each live row appeared, by `view key` and row, for entrance motion.
    born: HashMap<String, Vec<Option<Instant>>>,
    /// When the running turn of each thread began.
    turn_started: HashMap<String, Instant>,
    _pump: Option<Task<()>>,
}

impl EventEmitter<Notice> for Workspace {}

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
            born: HashMap::new(),
            turn_started: HashMap::new(),
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
            self.apply(event, cx);
            cx.notify();
        }
    }

    fn apply(&mut self, event: AgentEvent, cx: &mut Context<Self>) {
        self.time(&event);
        match &event {
            AgentEvent::ThreadStarted(meta) => self.upsert_thread(meta.clone()),
            AgentEvent::ThreadNameUpdated { thread_id, name } => {
                if let Some(thread) = self.threads.iter_mut().find(|t| t.id == *thread_id) {
                    thread.name = name.clone();
                }
            }
            AgentEvent::TurnStarted { thread_id, .. } => {
                self.turn_started
                    .entry(thread_id.clone())
                    .or_insert_with(Instant::now);
            }
            AgentEvent::TurnCompleted { thread_id, .. } => {
                self.turn_started.remove(thread_id);
                if let Some(thread) = self.threads.iter_mut().find(|t| t.id == *thread_id) {
                    thread.updated_at = jiff::Timestamp::now().as_second();
                }
                self.threads
                    .sort_by_key(|t| std::cmp::Reverse(t.updated_at));
            }
            AgentEvent::ItemCompleted {
                item: Item::Other { label, .. },
                ..
            } if label == "Context compacted" => {
                cx.emit(Notice::Success("Conversation compacted".into()));
            }
            AgentEvent::Warning(message) => log::warn!("codex: {message}"),
            AgentEvent::Disconnected { reason } => {
                self.status = Status::Failed(reason.clone());
                self.client = None;
                self.turn_started.clear();
                for transcript in self.transcripts.values_mut() {
                    transcript.apply(&event);
                }
                cx.emit(Notice::Error(format!(
                    "{reason}. Restart it from the sidebar."
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

    pub fn is_ready(&self) -> bool {
        self.status == Status::Ready
    }

    /// Whether the shown chat is waiting on its first sign of life: a turn
    /// runs and nothing has come back since the user's message.
    pub fn awaiting_first_output(&self) -> bool {
        let Some(transcript) = self.transcript() else {
            return false;
        };
        transcript.is_running()
            && transcript.entries().last().is_some_and(|entry| {
                matches!(
                    entry.kind,
                    agentdesk_core::EntryKind::Item(Item::User { .. })
                )
            })
    }

    /// The folder the shown chat runs in.
    pub fn folder(&self) -> PathBuf {
        match self.active_thread() {
            Some(thread) if !thread.cwd.is_empty() => PathBuf::from(&thread.cwd),
            _ => self.settings.working_dir(),
        }
    }

    pub fn new_chat(&mut self, cx: &mut Context<Self>) {
        if !self.draft.is_running() {
            self.draft = Transcript::new();
            self.born.remove(DRAFT);
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

    fn turn_request(&self, composed: &Composed) -> TurnRequest {
        TurnRequest {
            text: composed.text.clone(),
            images: composed.images.clone(),
            mentions: composed.mentions.clone(),
            model: self.settings.model.clone(),
            effort: self.settings.effort.clone(),
            mode: Some(self.settings.mode),
        }
    }

    /// Sends a message in the current view, creating the thread for a draft.
    pub fn send(&mut self, composed: Composed, cx: &mut Context<Self>) {
        let composed = Composed {
            text: composed.text.trim().to_string(),
            ..composed
        };
        if (composed.text.is_empty() && composed.images.is_empty()) || self.is_running() {
            return;
        }
        let Some(client) = self.client.clone() else {
            cx.emit(Notice::Error("Codex is not running.".into()));
            return;
        };
        let request = self.turn_request(&composed);
        match self.view.clone() {
            View::Thread(thread_id) => {
                let transcript = self.transcripts.entry(thread_id.clone()).or_default();
                let before = transcript.len();
                transcript.push_local_user(composed.text.clone(), composed.attachments());
                transcript.begin_pending_turn();
                self.note_births(&thread_id, before);
                self.turn_started.insert(thread_id.clone(), Instant::now());
                cx.notify();
                cx.spawn(async move |this, cx| {
                    let sent = client.turn_start(&thread_id, request).await;
                    if let Err(error) = sent {
                        let _ = this.update(cx, |ws, cx| {
                            if let Some(t) = ws.transcripts.get_mut(&thread_id) {
                                t.fail_pending_turn(format!("Could not send: {error:#}"));
                            }
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
                let options = StartThread {
                    cwd: Some(self.settings.working_dir().display().to_string()),
                    model: self.settings.model.clone(),
                    approval_policy: Some(self.settings.mode.approval_policy().into()),
                    sandbox: Some(self.settings.mode.sandbox_mode().into()),
                };
                let title = composed.text.clone();
                cx.spawn(async move |this, cx| {
                    let started = client.thread_start(options).await;
                    let thread_id = match started {
                        Ok(mut meta) => {
                            // Title the chat by its first message until Codex names it.
                            if meta.preview.trim().is_empty() {
                                meta.preview = title;
                            }
                            let id = meta.id.clone();
                            let ok = this.update(cx, |ws, cx| {
                                ws.upsert_thread(meta);
                                // The draft becomes the thread's transcript.
                                let draft = std::mem::take(&mut ws.draft);
                                ws.transcripts.insert(id.clone(), draft);
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
                                ws.draft.fail_pending_turn(format!(
                                    "Could not start a chat: {error:#}"
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
                                t.fail_pending_turn(format!("Could not send: {error:#}"));
                            }
                            ws.turn_started.remove(&thread_id);
                            cx.notify();
                        });
                    }
                })
                .detach();
            }
        }
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
        self.settings.pinned.retain(|id| *id != thread_id);
        if self.view == View::Thread(thread_id.clone()) {
            self.view = View::Draft;
            self.draft = Transcript::new();
        }
        cx.emit(Notice::Info("Chat archived".into()));
        cx.notify();
        cx.spawn(async move |this, cx| {
            if let Err(error) = client.thread_archive(&thread_id).await {
                log::warn!("thread/archive failed: {error:#}");
                let _ = this.update(cx, |ws, cx| {
                    cx.emit(Notice::Error(format!("Could not archive: {error:#}")));
                    ws.refresh(cx);
                });
            }
        })
        .detach();
    }

    pub fn rename(&mut self, thread_id: String, name: String, cx: &mut Context<Self>) {
        let name = name.trim().to_string();
        let Some(client) = self.client.clone() else {
            return;
        };
        if name.is_empty() {
            return;
        }
        if let Some(thread) = self.threads.iter_mut().find(|t| t.id == thread_id) {
            thread.name = Some(name.clone());
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            if let Err(error) = client.thread_set_name(&thread_id, &name).await {
                let _ = this.update(cx, |_, cx| {
                    cx.emit(Notice::Error(format!("Could not rename: {error:#}")))
                });
            }
        })
        .detach();
    }

    pub fn toggle_pin(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        self.settings.toggle_pin(thread_id);
        self.settings.save();
        cx.notify();
    }

    /// Asks Codex to summarize the shown chat's history to free context.
    pub fn compact(&mut self, cx: &mut Context<Self>) {
        let (Some(thread_id), Some(client)) = (
            self.active_thread_id().map(str::to_owned),
            self.client.clone(),
        ) else {
            cx.emit(Notice::Info("Start a chat first, then compact it.".into()));
            return;
        };
        cx.emit(Notice::Info("Compacting the conversation…".into()));
        cx.spawn(async move |this, cx| {
            let result = client.thread_compact(&thread_id).await;
            let _ = this.update(cx, |_, cx| match result {
                Ok(()) => {}
                Err(error) => cx.emit(Notice::Error(format!("Could not compact: {error:#}"))),
            });
        })
        .detach();
    }

    /// Asks Codex to review the uncommitted changes in the chat's folder.
    pub fn review(&mut self, instructions: Option<String>, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
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
                let options = StartThread {
                    cwd: Some(self.settings.working_dir().display().to_string()),
                    model: self.settings.model.clone(),
                    approval_policy: Some(AccessMode::ReadOnly.approval_policy().into()),
                    sandbox: Some(AccessMode::ReadOnly.sandbox_mode().into()),
                };
                cx.spawn(async move |this, cx| {
                    let meta = match client.thread_start(options).await {
                        Ok(mut meta) => {
                            meta.preview = "Code review".into();
                            meta
                        }
                        Err(error) => {
                            let _ = this.update(cx, |_, cx| {
                                cx.emit(Notice::Error(format!(
                                    "Could not start a review: {error:#}"
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
                            cx.emit(Notice::Error(format!(
                                "Could not start a review: {error:#}"
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
                        t.fail_pending_turn(format!("Could not start a review: {error:#}"));
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Files in the shown chat's folder matching `query`, as absolute paths.
    pub fn search_files(&self, query: String, cx: &mut Context<Self>) -> Task<Vec<PathBuf>> {
        let Some(client) = self.client.clone() else {
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
        // A model may not take the chosen effort; fall back to its default.
        if let Some(effort) = &self.settings.effort {
            let accepts = self
                .model_info(model.as_deref())
                .is_none_or(|info| info.efforts.is_empty() || info.efforts.contains(effort));
            if !accepts {
                self.settings.effort = None;
            }
        }
        self.settings.model = model;
        self.settings.save();
        cx.notify();
    }

    pub fn set_effort(&mut self, effort: Option<String>, cx: &mut Context<Self>) {
        self.settings.effort = effort;
        self.settings.save();
        cx.notify();
    }

    pub fn set_mode(&mut self, mode: AccessMode, cx: &mut Context<Self>) {
        self.settings.mode = mode;
        self.settings.save();
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
        cx.notify();
    }

    pub fn set_sidebar_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        self.settings.sidebar_collapsed = collapsed;
        self.settings.save();
        cx.notify();
    }

    /// The model new turns use: chosen, else the chat's, else Codex's default.
    pub fn model_id(&self) -> Option<String> {
        self.settings
            .model
            .clone()
            .or_else(|| self.active_thread().and_then(|t| t.model.clone()))
            .or_else(|| {
                self.models
                    .iter()
                    .find(|m| m.is_default)
                    .map(|m| m.id.clone())
            })
    }

    fn model_info(&self, id: Option<&str>) -> Option<&ModelInfo> {
        let id = id.map(str::to_owned).or_else(|| self.model_id())?;
        self.models.iter().find(|m| m.id == id)
    }

    /// The model new turns use, for display.
    pub fn model_label(&self) -> String {
        match self.model_id() {
            Some(id) => self
                .models
                .iter()
                .find(|m| m.id == id)
                .map(|m| m.display_name.clone())
                .unwrap_or(id),
            None => "Default model".into(),
        }
    }

    /// Efforts the current model accepts, and the one in force.
    pub fn efforts(&self) -> (Vec<String>, Option<String>) {
        let info = self.model_info(None);
        let efforts = info.map(|m| m.efforts.clone()).unwrap_or_default();
        let current = self
            .settings
            .effort
            .clone()
            .or_else(|| info.and_then(|m| m.default_effort.clone()));
        (efforts, current)
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
    use super::*;

    #[test]
    fn the_backend_label_names_the_version() {
        assert_eq!(
            backend_label("agentdesk/0.169.0 (Mac OS 15.1; arm64) x"),
            "Codex 0.169.0"
        );
        assert_eq!(backend_label(""), "Codex");
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
