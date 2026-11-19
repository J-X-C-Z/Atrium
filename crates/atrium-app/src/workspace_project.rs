//! The workspace's project side: projects, their workstreams running side by
//! side in the background, and the atrium they align through.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use atrium_core::{AgentEvent, EntryKind, GoalStatus, Item, ThreadMeta, Transcript, TurnStatus};
use codex_client::StartThread;
use gpui::Context;

use super::{CODEX, Composed, Notice, View, Workspace};
use crate::i18n::tf;
use crate::{
    agents,
    project::{
        self, ATRIUM, Project, Role, Task, TaskStatus, Workstream, controller_notice, wake_allowed,
    },
};

/// Where the user is: the everyday chats, or a project.
#[derive(Clone, Debug, PartialEq)]
pub enum Room {
    Chats,
    Project {
        root: PathBuf,
        /// The workstream whose chat is open; the overview when `None`.
        stream: Option<String>,
    },
}

/// How a workstream is doing, for its card.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamState {
    /// Its session is being made.
    Starting,
    /// Its agent is not running, so it cannot go on.
    Offline,
    Working,
    /// Waiting on the user: approvals and questions.
    NeedsYou(usize),
    Failed(String),
    Idle,
}

/// What a workstream's card shows.
#[derive(Clone, Debug)]
pub struct StreamView {
    pub stream: Workstream,
    pub state: StreamState,
    /// Steps done and total, from the agent's latest plan.
    pub plan: Option<(usize, usize)>,
    /// The step in progress.
    pub step: Option<String>,
    /// The latest thing it said, one line.
    pub said: Option<String>,
    /// Its status file's focus, when it wrote one.
    pub focus: Option<String>,
    pub unread: usize,
    /// Changes it made in others' scopes.
    pub crossings: usize,
    pub goal_status: Option<GoalStatus>,
    pub speed: Option<String>,
    pub context: Option<f64>,
    /// The task it was started for: a lead's roadmap, a worker's job.
    pub task: Option<Task>,
}

#[derive(Default)]
pub(super) struct ScheduleState {
    cleaning: HashSet<(PathBuf, String)>,
    seen: HashMap<PathBuf, usize>,
    pending: HashSet<(PathBuf, String)>,
    last_wake: HashMap<(PathBuf, String), Instant>,
    orphan_logged: HashSet<(PathBuf, String)>,
}

#[derive(Debug, PartialEq)]
enum StartDecision {
    Start,
    Wait,
    Unknown,
}

fn start_decision(known: bool, ready: bool, running: usize) -> StartDecision {
    if !known {
        StartDecision::Unknown
    } else if !ready || running >= 12 {
        StartDecision::Wait
    } else {
        StartDecision::Start
    }
}

/// Temporarily route an automated message without consuming the user's edit.
struct BackgroundDispatch {
    view: View,
    editing: Option<(String, String)>,
}

impl BackgroundDispatch {
    fn enter(view: &mut View, editing: &mut Option<(String, String)>, thread: String) -> Self {
        Self {
            view: std::mem::replace(view, View::Thread(thread)),
            editing: editing.take(),
        }
    }

    fn restore(self, view: &mut View, editing: &mut Option<(String, String)>) {
        *view = self.view;
        *editing = self.editing;
    }
}

impl Workspace {
    pub(super) fn init_projects(&mut self, cx: &mut Context<Self>) {
        let roots = self.settings.projects.clone();
        for root in roots {
            match Project::open(&root) {
                Ok(mut project) => {
                    match project.recover_unbound_tasks() {
                        Ok(parents) => {
                            for parent in parents {
                                self.project_schedule.pending.insert((root.clone(), parent));
                            }
                        }
                        Err(e) => log::warn!("could not recover project claims: {e:#}"),
                    }
                    self.projects.push(project);
                }
                Err(error) => log::warn!("could not open project {}: {error:#}", root.display()),
            }
        }
        for project in &self.projects {
            for stream in &project.file.streams {
                if let Some(id) = &stream.thread_id {
                    placeholder(&mut self.threads, id, stream, project);
                }
            }
        }
        if let Some(root) = self.settings.active_project.clone()
            && self.projects.iter().any(|p| p.root == root)
        {
            self.enter_project(root, cx);
        }
        self._atrium_poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                if this.update(cx, |ws, cx| ws.tick_project(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// Every couple of seconds in a project: read the atrium again if it
    /// changed, and open workstream chats whose agent has come up.
    fn tick_project(&mut self, cx: &mut Context<Self>) {
        let roots: Vec<_> = self.projects.iter().map(|p| p.root.clone()).collect();
        for root in roots {
            let changed = self.project_mut(&root).is_some_and(Project::reload);
            self.load_streams(&root, cx);
            let Some(project) = self.projects.iter().find(|p| p.root == root) else {
                continue;
            };
            let seen = self
                .project_schedule
                .seen
                .entry(root.clone())
                .or_insert_with(|| {
                    project
                        .controller()
                        .map_or(project.notices.len(), |c| c.notices_seen)
                });
            if let Some(controller) = project.controller()
                && project
                    .notices
                    .iter()
                    .skip((*seen).min(project.notices.len()))
                    .any(|n| controller_notice(n, &controller.id))
            {
                self.project_schedule
                    .pending
                    .insert((root.clone(), controller.id.clone()));
            }
            *seen = project.notices.len();
            for parent in project.pending_report_parents() {
                self.project_schedule.pending.insert((root.clone(), parent));
            }
            for task in &project.tasks {
                if project.stream(&task.owner).is_none()
                    && self
                        .project_schedule
                        .orphan_logged
                        .insert((root.clone(), task.key()))
                {
                    log::warn!("ignoring orphan task {} in {}", task.key(), root.display());
                }
            }
            let tasks: Vec<_> = project
                .startable_with(|task| {
                    let Some(owner) = project.stream(&task.owner) else {
                        return false;
                    };
                    let backend = task.agent.as_deref().unwrap_or(&owner.backend);
                    !self.scheduler_backend_known(backend) || self.backend_ready(backend)
                })
                .into_iter()
                .cloned()
                .collect();
            for task in tasks {
                let running = self
                    .stream_views_for(&root)
                    .iter()
                    .filter(|v| matches!(v.state, StreamState::Working | StreamState::Starting))
                    .count();
                let Some(project) = self.projects.iter().find(|p| p.root == root) else {
                    break;
                };
                let Some(owner) = project.stream(&task.owner) else {
                    continue;
                };
                let backend = task.agent.as_deref().unwrap_or(&owner.backend).to_string();
                let known = self.scheduler_backend_known(&backend);
                let decision = start_decision(known, self.backend_ready(&backend), running);
                if decision == StartDecision::Unknown {
                    if let Some(project) = self.project_mut(&root) {
                        if let Err(e) = project.set_task_status(&task.key(), TaskStatus::Failed) {
                            log::warn!("cannot fail task {}: {e:#}", task.key());
                            continue;
                        }
                        let _ = project.post(ATRIUM, vec![task.owner.clone()], &format!(
                            "Unknown agent '{backend}' in {}; use codex, claude, deepseek or antigravity.", task.key()));
                    }
                    self.project_schedule
                        .pending
                        .insert((root.clone(), task.owner.clone()));
                    cx.notify();
                    continue;
                }
                if decision != StartDecision::Start {
                    continue;
                }
                let Some(project) = self.project_mut(&root) else {
                    continue;
                };
                let Some(stream) = project.stream_from_task(&task, &backend) else {
                    continue;
                };
                if let Err(e) = project.set_task_status(&task.key(), TaskStatus::Running) {
                    log::warn!("cannot claim task {}: {e:#}", task.key());
                    continue;
                }
                self.start_stream(&root, stream, cx);
            }
            let reports: Vec<_> = self
                .projects
                .iter()
                .find(|p| p.root == root)
                .map(|p| {
                    p.unreported()
                        .into_iter()
                        .map(|(s, t)| (s.clone(), t.clone()))
                        .collect()
                })
                .unwrap_or_default();
            for (stream, task) in reports {
                let Some(parent) = stream.parent.clone() else {
                    continue;
                };
                // Do not stop a goal in the middle of its finishing turn.
                if stream
                    .thread_id
                    .as_ref()
                    .and_then(|id| self.transcripts.get(id))
                    .is_some_and(Transcript::is_running)
                {
                    continue;
                }
                if stream.role == Role::Worker
                    && agents::is_codex(&stream.backend)
                    && let Some(thread) = stream.thread_id.clone()
                {
                    if let Some(client) = self.codex_for(&stream.backend) {
                        // The persistent reported flag is written only after cleanup succeeds.
                        let root = root.clone();
                        let stream_id = stream.id.clone();
                        if !self
                            .project_schedule
                            .cleaning
                            .insert((root.clone(), stream_id.clone()))
                        {
                            continue;
                        }
                        cx.spawn(async move |this, cx| {
                            let cleanup = match client.goal_get(&thread).await {
                                Ok(Some(goal)) if goal.status != GoalStatus::Complete => {
                                    client
                                        .goal_set(&thread, None, Some(GoalStatus::Complete), None)
                                        .await
                                }
                                other => other,
                            };
                            match cleanup {
                                Ok(goal) => {
                                    let _ = this.update(cx, |ws, cx| {
                                        ws.project_schedule
                                            .cleaning
                                            .remove(&(root.clone(), stream_id.clone()));
                                        if let Some(goal) = goal {
                                            ws.goals.insert(thread, goal);
                                        } else {
                                            ws.goals.remove(&thread);
                                        }
                                        ws.report_task(&root, &stream_id, cx);
                                    });
                                }
                                Err(e) => {
                                    log::warn!("worker goal cleanup failed: {e:#}");
                                    let _ = this.update(cx, |ws, _| {
                                        ws.project_schedule
                                            .cleaning
                                            .remove(&(root.clone(), stream_id.clone()));
                                    });
                                }
                            }
                        })
                        .detach();
                    }
                    continue;
                }
                let _ = task;
                self.report_task(&root, &stream.id, cx);
                self.project_schedule.pending.insert((root.clone(), parent));
            }
            if changed {
                cx.notify();
            }
        }
        let pending: Vec<_> = self.project_schedule.pending.iter().cloned().collect();
        for (root, stream) in pending {
            let key = (root.clone(), stream.clone());
            if !wake_allowed(
                self.project_schedule.last_wake.get(&key).copied(),
                Instant::now(),
                false,
                false,
                false,
            ) {
                continue;
            }
            if self.deliver_notices_at(&root, &stream, cx) {
                self.project_schedule.pending.remove(&key);
                self.project_schedule.last_wake.insert(key, Instant::now());
            }
        }
    }

    fn scheduler_backend_known(&self, backend: &str) -> bool {
        backend == CODEX
            || self.agent(backend).is_some()
            || (agents::is_codex(backend)
                && agents::split_backend(backend)
                    .1
                    .is_some_and(|d| self.device(d).is_some()))
    }

    fn report_task(&mut self, root: &PathBuf, stream: &str, cx: &mut Context<Self>) {
        if let Some(project) = self.project_mut(root) {
            match project.report_finished(stream) {
                Ok(Some(parent)) => {
                    self.project_schedule.pending.insert((root.clone(), parent));
                    cx.notify();
                }
                Ok(None) => {}
                Err(e) => log::warn!("could not report task: {e:#}"),
            }
        }
    }

    pub fn project_root(&self) -> Option<PathBuf> {
        match &self.room {
            Room::Project { root, .. } => Some(root.clone()),
            Room::Chats => None,
        }
    }

    pub fn active_project(&self) -> Option<&Project> {
        let root = self.project_root()?;
        self.projects.iter().find(|p| p.root == root)
    }

    fn project_mut(&mut self, root: &PathBuf) -> Option<&mut Project> {
        self.projects.iter_mut().find(|p| p.root == *root)
    }

    /// Every thread that belongs to a workstream: kept out of the chat list.
    pub fn project_threads(&self) -> HashSet<String> {
        self.projects
            .iter()
            .flat_map(|p| p.file.streams.iter().filter_map(|s| s.thread_id.clone()))
            .collect()
    }

    /// The project and workstream a thread runs, if any.
    pub fn stream_of(&self, thread_id: &str) -> Option<(&Project, &Workstream)> {
        self.projects.iter().find_map(|p| {
            p.file
                .streams
                .iter()
                .find(|s| s.thread_id.as_deref() == Some(thread_id))
                .map(|s| (p, s))
        })
    }

    /// The workstream whose chat is shown.
    pub fn open_stream(&self) -> Option<(&Project, &Workstream)> {
        self.active_thread_id().and_then(|id| self.stream_of(id))
    }

    pub fn enter_chats(&mut self, cx: &mut Context<Self>) {
        self.room = Room::Chats;
        self.settings.active_project = None;
        self.settings.save();
        self.new_chat(cx);
    }

    /// Adds the project in `root` (making its atrium the first time) and enters it.
    pub fn add_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if !self.projects.iter().any(|p| p.root == root) {
            match Project::open(&root) {
                Ok(project) => {
                    self.projects.push(project);
                    self.settings.projects.push(root.clone());
                    self.settings.save();
                }
                Err(error) => {
                    cx.emit(Notice::Error(tf(
                        "Could not open the project: {error}",
                        &[("error", &format!("{error:#}"))],
                    )));
                    return;
                }
            }
        }
        self.enter_project(root, cx);
    }

    /// Forgets a project here. Its folder and atrium stay as they are.
    pub fn remove_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.projects.retain(|p| p.root != root);
        self.settings.projects.retain(|p| *p != root);
        if self.project_root().as_ref() == Some(&root) {
            self.enter_chats(cx);
        }
        self.settings.save();
        cx.notify();
    }

    /// Shows a project's overview.
    pub fn enter_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.room = Room::Project {
            root: root.clone(),
            stream: None,
        };
        self.settings.active_project = Some(root.clone());
        self.settings.save();
        if let Some(project) = self.project_mut(&root) {
            project.stamp = None;
            project.reload();
        }
        self.load_streams(&root, cx);
        cx.notify();
    }

    /// Opens a workstream's chat.
    pub fn show_stream(&mut self, stream: &str, cx: &mut Context<Self>) {
        let Some(root) = self.project_root() else {
            return;
        };
        let thread = self
            .active_project()
            .and_then(|p| p.stream(stream))
            .and_then(|s| s.thread_id.clone());
        self.room = Room::Project {
            root,
            stream: Some(stream.to_string()),
        };
        match thread {
            Some(thread) => self.open_thread(thread, cx),
            None => cx.notify(),
        }
    }

    pub fn show_overview(&mut self, cx: &mut Context<Self>) {
        if let Some(root) = self.project_root() {
            self.room = Room::Project { root, stream: None };
            cx.notify();
        }
    }

    /// Opens the transcripts of a project's workstreams in the background,
    /// for those whose agent is up.
    fn load_streams(&mut self, root: &PathBuf, cx: &mut Context<Self>) {
        let Some(project) = self.projects.iter().find(|p| p.root == *root) else {
            return;
        };
        let threads: Vec<(String, String)> = project
            .file
            .streams
            .iter()
            .filter_map(|s| Some((s.thread_id.clone()?, s.backend.clone())))
            .collect();
        for (thread, backend) in threads {
            if self.transcripts.contains_key(&thread) || self.loading.contains(&thread) {
                continue;
            }
            let ready = self.backend_ready(&backend);
            if ready {
                self.load_thread(thread, cx);
            }
        }
    }

    /// Starts a new workstream: its session, its goal and its kickoff.
    pub fn create_stream(
        &mut self,
        name: String,
        role: Role,
        backend: String,
        goal: String,
        scope: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.project_root() else {
            return;
        };
        if self
            .stream_views_for(&root)
            .iter()
            .filter(|v| matches!(v.state, StreamState::Working | StreamState::Starting))
            .count()
            >= 12
        {
            cx.emit(Notice::Error(
                "The project already has 12 running workstreams.".into(),
            ));
            return;
        }
        if role == Role::Controller
            && self
                .active_project()
                .is_some_and(|p| p.controller().is_some())
        {
            cx.emit(Notice::Error(
                "The project already has a controller.".into(),
            ));
            return;
        }
        let ready = self.backend_ready(&backend);
        if !ready {
            cx.emit(Notice::Error(tf(
                "{agent} is not running.",
                &[("agent", &agents::backend_label(&backend))],
            )));
            return;
        }
        let Some(project) = self.project_mut(&root) else {
            return;
        };
        let stream = Workstream {
            id: project.new_id(&name),
            name: name.trim().to_string(),
            backend: backend.clone(),
            thread_id: None,
            goal: goal.trim().to_string(),
            scope,
            notices_seen: project.notices.len(),
            created_at: jiff::Timestamp::now().as_second(),
            role,
            // Leads the user adds still report to the controller.
            parent: match role {
                Role::Lead => project.controller().map(|c| c.id.clone()),
                _ => None,
            },
            task: None,
            reported: false,
        };
        self.start_stream(&root, stream, cx);
    }

    fn start_stream(&mut self, root: &Path, stream: Workstream, cx: &mut Context<Self>) {
        let root = root.to_path_buf();
        let backend = stream.backend.clone();
        let Some(project) = self.project_mut(&root) else {
            return;
        };
        let id = stream.id.clone();
        let title = format!("{} · {}", project.file.name, stream.name);
        project.file.streams.push(stream.clone());
        if let Err(error) = project.save() {
            self.stream_failed(&root, &id, error, cx);
            return;
        }
        let kickoff = project.kickoff(&stream);
        let cwd = root.clone();
        cx.notify();

        if agents::is_codex(&backend) {
            let Some(client) = self.codex_for(&backend) else {
                return;
            };
            let options = StartThread {
                cwd: Some(cwd.display().to_string()),
                model: if agents::split_backend(&backend).1.is_some() {
                    None
                } else {
                    self.settings.model.clone()
                },
                approval_policy: Some(self.settings.mode.approval_policy().into()),
                sandbox: Some(self.settings.mode.sandbox_mode().into()),
            };
            // Controllers are event-driven; never give them an auto-continuing goal.
            let objective = if stream.role == Role::Controller {
                String::new()
            } else {
                stream.goal.clone()
            };
            cx.spawn(async move |this, cx| {
                let meta = match client.thread_start(options).await {
                    Ok(meta) => meta,
                    Err(error) => {
                        let _ = this.update(cx, |ws, cx| ws.stream_failed(&root, &id, error, cx));
                        return;
                    }
                };
                let thread = meta.id.clone();
                let _ = client.thread_set_name(&thread, &title).await;
                let bound = this
                    .update(cx, |ws, cx| match ws.attach_thread(&root, &id, &thread) {
                        Ok(()) => true,
                        Err(error) => {
                            ws.stream_failed(&root, &id, error, cx);
                            false
                        }
                    })
                    .unwrap_or(false);
                if !bound {
                    return;
                }
                // Establish the goal before kickoff so a fast worker cannot be
                // reactivated by a late goal_set after it has already finished.
                let goal = if objective.is_empty() {
                    None
                } else {
                    client
                        .goal_set(&thread, Some(&objective), Some(GoalStatus::Active), None)
                        .await
                        .ok()
                        .flatten()
                };
                let _ = this.update(cx, |ws, cx| {
                    ws.upsert_thread(ThreadMeta {
                        name: Some(title.clone()),
                        backend: backend.clone(),
                        ..meta
                    });
                    ws.transcripts.insert(thread.clone(), Transcript::new());
                    if let Some(goal) = goal {
                        ws.goals.insert(thread.clone(), goal);
                    }
                    // The user may have removed it while goal setup was in flight.
                    if ws
                        .projects
                        .iter()
                        .any(|p| p.root == root && p.stream(&id).is_some())
                    {
                        ws.send_to_thread(thread.clone(), Composed::text(kickoff), cx);
                    }
                });
            })
            .detach();
        } else {
            let Some(client) = self.acp(&backend) else {
                return;
            };
            cx.spawn(async move |this, cx| {
                let start = match client.new_session(&cwd).await {
                    Ok(start) => start,
                    Err(error) => {
                        let _ = this.update(cx, |ws, cx| ws.stream_failed(&root, &id, error, cx));
                        return;
                    }
                };
                let _ = this.update(cx, |ws, cx| {
                    let now = jiff::Timestamp::now().as_second();
                    let thread = start.session_id.clone();
                    ws.upsert_thread(ThreadMeta {
                        id: thread.clone(),
                        backend: backend.clone(),
                        name: Some(title.clone()),
                        preview: title.clone(),
                        cwd: cwd.display().to_string(),
                        model: None,
                        created_at: now,
                        updated_at: now,
                    });
                    ws.known_options
                        .insert(backend.clone(), start.options.clone());
                    ws.session_options.insert(thread.clone(), start.options);
                    if let Some(commands) = ws.agent_commands.get(&backend).cloned() {
                        ws.commands.insert(thread.clone(), commands);
                    }
                    ws.transcripts.insert(thread.clone(), Transcript::new());
                    match ws.attach_thread(&root, &id, &thread) {
                        Ok(()) => ws.send_to_thread(thread, Composed::text(kickoff), cx),
                        Err(error) => ws.stream_failed(&root, &id, error, cx),
                    }
                });
            })
            .detach();
        }
    }

    fn attach_thread(&mut self, root: &PathBuf, stream: &str, thread: &str) -> anyhow::Result<()> {
        self.project_mut(root)
            .ok_or_else(|| anyhow::anyhow!("project was removed before startup completed"))?
            .bind_thread(stream, thread)
    }

    fn stream_failed(
        &mut self,
        root: &PathBuf,
        stream: &str,
        error: anyhow::Error,
        cx: &mut Context<Self>,
    ) {
        let mut wake_parent = None;
        if let Some(project) = self.project_mut(root) {
            match project.fail_start(stream, &format!("{error:#}")) {
                Ok(parent) => wake_parent = parent,
                Err(e) => log::warn!(
                    "could not persist startup failure; keeping workstream for recovery: {e:#}"
                ),
            }
        }
        if let Some(parent) = wake_parent {
            self.project_schedule.pending.insert((root.clone(), parent));
        }
        cx.emit(Notice::Error(tf(
            "Could not start the workstream: {error}",
            &[("error", &format!("{error:#}"))],
        )));
        cx.notify();
    }

    /// Takes a workstream out of its project. Its chat stays, as a normal chat.
    pub fn remove_stream(&mut self, stream: &str, cx: &mut Context<Self>) {
        let Some(root) = self.project_root() else {
            return;
        };
        if let Some(project) = self.project_mut(&root) {
            project.file.streams.retain(|s| s.id != stream);
            let _ = project.save();
        }
        self.show_overview(cx);
    }

    /// Sends `composed` to a thread whatever the main column shows.
    pub fn send_to_thread(&mut self, thread: String, composed: Composed, cx: &mut Context<Self>) {
        let state = BackgroundDispatch::enter(&mut self.view, &mut self.editing, thread);
        self.send(composed, cx);
        state.restore(&mut self.view, &mut self.editing);
        cx.notify();
    }

    /// When the shown chat is a workstream's: puts the notices it has not
    /// seen in front of the message, and marks them handed over.
    pub(super) fn with_stream_notices(&mut self, composed: Composed) -> Composed {
        let Some(thread) = self.active_thread_id().map(str::to_owned) else {
            return composed;
        };
        let Some((root, stream)) = self
            .stream_of(&thread)
            .map(|(p, s)| (p.root.clone(), s.id.clone()))
        else {
            return composed;
        };
        let Some(project) = self.project_mut(&root) else {
            return composed;
        };
        let unread = project.unread(&stream);
        if unread.is_empty() {
            return composed;
        }
        let text = project::with_notices(&unread, &composed.text);
        project.mark_read(&stream);
        let _ = project.save();
        Composed { text, ..composed }
    }

    /// Hands a workstream its new notices now, asking it to carry on.
    pub fn deliver_notices(&mut self, stream: &str, cx: &mut Context<Self>) {
        if let Some(root) = self.project_root() {
            self.deliver_notices_at(&root, stream, cx);
        }
    }

    fn deliver_notices_at(&mut self, root: &PathBuf, stream: &str, cx: &mut Context<Self>) -> bool {
        let Some(project) = self.projects.iter().find(|p| p.root == *root) else {
            return true;
        };
        if project.unread(stream).is_empty() {
            return true;
        }
        let Some(thread) = project.stream(stream).and_then(|s| s.thread_id.clone()) else {
            return false;
        };
        if !self.backend_ready(self.backend_of(&thread)) {
            return false;
        }
        let Some(transcript) = self.transcripts.get(&thread) else {
            return false;
        };
        if !wake_allowed(
            None,
            Instant::now(),
            waiting_on_user(transcript) > 0,
            transcript.is_running(),
            agents::is_codex(self.backend_of(&thread)),
        ) {
            return false;
        }
        self.send_to_thread(
            thread,
            Composed::text("Take these into account and carry on toward your goal.".into()),
            cx,
        );
        true
    }

    /// Asks an idle workstream to keep going toward its goal.
    pub fn continue_stream(&mut self, stream: &str, cx: &mut Context<Self>) {
        let Some(thread) = self
            .active_project()
            .and_then(|p| p.stream(stream))
            .and_then(|s| s.thread_id.clone())
        else {
            return;
        };
        self.send_to_thread(
            thread,
            Composed::text(
                "Carry on toward your goal. Update your status file when you reach a milestone."
                    .into(),
            ),
            cx,
        );
    }

    /// Leads the controller asked for and the user has not answered yet.
    pub fn proposed_leads(&self) -> Vec<Task> {
        self.active_project().map_or_else(Vec::new, |p| {
            p.tasks
                .iter()
                .filter(|t| p.awaits_approval(t))
                .cloned()
                .collect()
        })
    }

    /// Tasks waiting for an agent to be started: queued under a lead, or
    /// approved and not yet picked up.
    pub fn queued_tasks(&self, owner: &str) -> Vec<Task> {
        self.active_project().map_or_else(Vec::new, |p| {
            p.tasks
                .iter()
                .filter(|t| {
                    t.owner == owner
                        && t.status == TaskStatus::Open
                        && p.stream_for_task(&t.key()).is_none()
                        && !p.awaits_approval(t)
                })
                .cloned()
                .collect()
        })
    }

    /// The user says yes to a lead the controller asked for; Atrium starts
    /// it at its next look at the tasks.
    pub fn approve_task(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(root) = self.project_root() else {
            return;
        };
        if let Some(project) = self.project_mut(&root) {
            if !project.file.approved.iter().any(|k| k == key) {
                project.file.approved.push(key.to_string());
            }
            if let Err(error) = project.save() {
                cx.emit(Notice::Error(format!("{error:#}")));
            }
        }
        cx.notify();
    }

    /// The user says no: the task is marked failed and its asker told.
    pub fn reject_task(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(root) = self.project_root() else {
            return;
        };
        if let Some(project) = self.project_mut(&root) {
            let owner = project
                .task(key)
                .map(|t| (t.owner.clone(), t.title.clone()));
            if let Err(error) = project.set_task_status(key, TaskStatus::Failed) {
                cx.emit(Notice::Error(format!("{error:#}")));
            }
            if let Some((owner, title)) = owner {
                let _ = project.post(
                    project::YOU,
                    vec![owner],
                    &format!("I turned down \"{title}\" (`{key}`). Rethink that part of the plan."),
                );
            }
        }
        cx.notify();
    }

    /// Whether to hold the controller's leads for approval.
    pub fn set_approve_leads(&mut self, on: bool, cx: &mut Context<Self>) {
        let Some(root) = self.project_root() else {
            return;
        };
        if let Some(project) = self.project_mut(&root) {
            project.file.approve_leads = on;
            let _ = project.save();
        }
        cx.notify();
    }

    /// Posts a notice from the user.
    pub fn post_notice(&mut self, to: Vec<String>, text: String, cx: &mut Context<Self>) {
        let Some(root) = self.project_root() else {
            return;
        };
        if text.trim().is_empty() {
            return;
        }
        if let Some(project) = self.project_mut(&root)
            && let Err(error) = project.post(project::YOU, to, &text)
        {
            cx.emit(Notice::Error(format!("{error:#}")));
        }
        cx.notify();
    }

    /// Watches what workstreams do: a change in another's scope is posted
    /// to its owner; a finished turn may have written to the atrium.
    pub(super) fn watch_streams(&mut self, event: &AgentEvent, cx: &mut Context<Self>) {
        let Some(thread) = event.thread_id() else {
            return;
        };
        let Some((root, stream)) = self
            .stream_of(thread)
            .map(|(p, s)| (p.root.clone(), s.id.clone()))
        else {
            return;
        };
        match event {
            AgentEvent::ItemCompleted {
                item: Item::FileChange { changes, .. },
                ..
            } => {
                let Some(project) = self.project_mut(&root) else {
                    return;
                };
                for change in changes {
                    let Some(crossing) = project.crossing(&stream, &change.path) else {
                        continue;
                    };
                    if project.crossings.contains(&crossing) {
                        continue;
                    }
                    let name = |id: &str| {
                        project
                            .stream(id)
                            .map(|s| s.name.clone())
                            .unwrap_or_else(|| id.to_string())
                    };
                    let said = format!(
                        "{} changed `{}`, which is in your scope. Check it fits your plans.",
                        name(&crossing.stream),
                        crossing.path
                    );
                    cx.emit(Notice::Info(tf(
                        "{stream} changed {path} in {owner}'s scope",
                        &[
                            ("stream", &name(&crossing.stream)),
                            ("path", &crossing.path),
                            ("owner", &name(&crossing.owner)),
                        ],
                    )));
                    let owner = crossing.owner.clone();
                    project.crossings.push(crossing);
                    if let Err(error) = project.post(ATRIUM, vec![owner], &said) {
                        log::warn!("could not post a notice: {error:#}");
                    }
                }
            }
            AgentEvent::TurnCompleted { status, .. } => {
                if let Some(project) = self.project_mut(&root) {
                    project.reload();
                }
                if *status == TurnStatus::Failed {
                    let name = self
                        .projects
                        .iter()
                        .find(|p| p.root == root)
                        .and_then(|p| p.stream(&stream))
                        .map_or(stream.clone(), |s| s.name.clone());
                    cx.emit(Notice::Error(tf("{name} hit an error", &[("name", &name)])));
                }
            }
            _ => {}
        }
    }

    /// What each workstream of the shown project is doing.
    pub fn stream_views(&self) -> Vec<StreamView> {
        let Some(root) = self.project_root() else {
            return Vec::new();
        };
        self.stream_views_for(&root)
    }

    fn stream_views_for(&self, root: &PathBuf) -> Vec<StreamView> {
        let Some(project) = self.projects.iter().find(|p| p.root == *root) else {
            return Vec::new();
        };
        let now = std::time::Instant::now();
        project
            .file
            .streams
            .iter()
            .map(|stream| {
                let transcript = stream
                    .thread_id
                    .as_ref()
                    .and_then(|id| self.transcripts.get(id));
                let agent_up = self.backend_ready(&stream.backend);
                let waiting = transcript.map_or(0, waiting_on_user);
                let state = match (transcript, &stream.thread_id) {
                    (_, None) => StreamState::Starting,
                    (None, Some(_)) if !agent_up => StreamState::Offline,
                    (None, Some(_)) => StreamState::Starting,
                    (Some(_), _) if waiting > 0 => StreamState::NeedsYou(waiting),
                    (Some(t), _) if t.is_running() => StreamState::Working,
                    (Some(t), _) => match last_failure(t) {
                        Some(error) => StreamState::Failed(error),
                        None if !agent_up => StreamState::Offline,
                        None => StreamState::Idle,
                    },
                };
                let (plan, step) = transcript.map_or((None, None), latest_plan);
                let said = transcript.and_then(|t| {
                    t.entries().iter().rev().find_map(|e| match &e.kind {
                        EntryKind::Item(Item::Agent { text, .. }) if !text.trim().is_empty() => {
                            Some(
                                project::first_line(text)
                                    .trim_start_matches('#')
                                    .trim()
                                    .to_string(),
                            )
                        }
                        _ => None,
                    })
                });
                let focus = project
                    .statuses
                    .get(&stream.id)
                    .and_then(|text| focus_of(text));
                let pace = transcript.and_then(|t| t.latest_pace(now));
                StreamView {
                    unread: project.unread(&stream.id).len(),
                    crossings: project
                        .crossings
                        .iter()
                        .filter(|c| c.stream == stream.id)
                        .count(),
                    goal_status: stream
                        .thread_id
                        .as_ref()
                        .and_then(|id| self.goals.get(id))
                        .map(|g| g.status),
                    speed: pace
                        .filter(|p| p.writing_now || p.done)
                        .as_ref()
                        .and_then(crate::ui::format_speed),
                    context: transcript
                        .and_then(|t| t.usage())
                        .and_then(|u| u.context_share()),
                    task: project.task_of(stream).cloned(),
                    stream: stream.clone(),
                    state,
                    plan,
                    step,
                    said,
                    focus,
                }
            })
            .collect()
    }
}

/// A stand-in thread for a workstream whose agent has not listed it yet, so
/// its backend is known.
fn placeholder(threads: &mut Vec<ThreadMeta>, id: &str, stream: &Workstream, project: &Project) {
    if threads.iter().any(|t| t.id == id) {
        return;
    }
    let title = format!("{} · {}", project.file.name, stream.name);
    threads.push(ThreadMeta {
        id: id.to_string(),
        backend: stream.backend.clone(),
        name: Some(title.clone()),
        preview: title,
        cwd: project.root.display().to_string(),
        model: None,
        created_at: stream.created_at,
        updated_at: stream.created_at,
    });
}

/// Approvals and questions still waiting for an answer.
pub fn waiting_on_user(transcript: &Transcript) -> usize {
    transcript
        .entries()
        .iter()
        .filter(|e| match &e.kind {
            EntryKind::Approval {
                answer, resolved, ..
            } => answer.is_none() && !resolved,
            EntryKind::Question {
                answers, resolved, ..
            } => answers.is_none() && !resolved,
            _ => false,
        })
        .count()
}

fn last_failure(transcript: &Transcript) -> Option<String> {
    let last = transcript.entries().last()?;
    let summary = transcript.turn_summary(&last.turn_id)?;
    if summary.status != TurnStatus::Failed {
        return None;
    }
    transcript
        .entries()
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            EntryKind::Error(message) if e.turn_id == last.turn_id => Some(message.clone()),
            _ => None,
        })
}

fn latest_plan(transcript: &Transcript) -> (Option<(usize, usize)>, Option<String>) {
    transcript
        .entries()
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            EntryKind::Plan { steps, .. } if !steps.is_empty() => {
                let done = steps.iter().filter(|s| s.done).count();
                let step = steps
                    .iter()
                    .find(|s| s.active)
                    .or_else(|| steps.iter().find(|s| !s.done))
                    .map(|s| s.step.clone());
                Some((Some((done, steps.len())), step))
            }
            _ => None,
        })
        .unwrap_or((None, None))
}

/// The first line under a status file's "Focus" heading.
fn focus_of(status: &str) -> Option<String> {
    let mut lines = status.lines();
    lines.find(|l| {
        let l = l.trim().trim_start_matches('#').trim().to_lowercase();
        l == "focus" || l.starts_with("focus") || l == "当前" || l == "焦点"
    })?;
    lines
        .map(|l| l.trim().trim_start_matches(['-', '*']).trim())
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_focus_is_read_from_a_status_file() {
        let status = "# Frontend\n\n## Focus\n\n- Timeline drag and drop\n\n## Done\n- Shell\n";
        assert_eq!(focus_of(status).as_deref(), Some("Timeline drag and drop"));
        assert_eq!(focus_of("nothing here"), None);
    }
    #[test]
    fn automated_dispatch_cannot_resend_an_edit_and_restores_the_draft() {
        for target in ["shown", "background"] {
            let mut view = View::Thread("shown".into());
            let mut editing = Some(("shown".into(), "old-turn".into()));
            let state = BackgroundDispatch::enter(&mut view, &mut editing, target.into());
            assert!(matches!(&view, View::Thread(id) if id == target));
            // send's resend_edited branch requires Some, even for the same thread.
            assert!(editing.is_none());
            state.restore(&mut view, &mut editing);
            assert!(matches!(&view, View::Thread(id) if id == "shown"));
            assert_eq!(editing, Some(("shown".into(), "old-turn".into())));
        }
    }

    #[test]
    fn scheduler_distinguishes_unknown_offline_and_project_cap() {
        assert_eq!(start_decision(false, false, 0), StartDecision::Unknown);
        assert_eq!(start_decision(true, false, 0), StartDecision::Wait);
        assert_eq!(start_decision(true, true, 12), StartDecision::Wait);
        assert_eq!(start_decision(true, true, 11), StartDecision::Start);
    }

    #[test]
    fn only_direct_external_notices_wake_controller() {
        let mut notice = project::ProjectNotice {
            at: String::new(),
            from: "lead".into(),
            to: vec!["all".into()],
            text: "milestone".into(),
        };
        assert!(!controller_notice(&notice, "boss"));
        notice.to = vec!["boss".into()];
        assert!(controller_notice(&notice, "boss"));
        notice.from = "boss".into();
        assert!(!controller_notice(&notice, "boss"));
    }

    #[test]
    fn wake_debounces_and_never_overrides_user_attention() {
        let now = Instant::now();
        assert!(!wake_allowed(
            Some(now),
            now + Duration::from_secs(29),
            false,
            false,
            false
        ));
        assert!(wake_allowed(
            Some(now),
            now + Duration::from_secs(30),
            false,
            false,
            false
        ));
        assert!(!wake_allowed(None, now, true, false, true));
        assert!(!wake_allowed(None, now, false, true, false));
        assert!(wake_allowed(None, now, false, true, true));
    }
}
