//! The workspace's project side: projects, their workstreams running side by
//! side in the background, and the atrium they align through.

#[path = "workspace_project_pool.rs"]
mod pool;
#[path = "workspace_pr_status.rs"]
mod pr_status;

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::project::notice_delivery::{NoticeOutcome, NoticeResolution};
use atrium_core::{
    AgentEvent, EntryKind, GoalStatus, Item, ItemStatus, McpServer, ThreadMeta, Transcript,
    TurnStatus,
};
use codex_client::StartThread;
use gpui::Context;

use super::notice_dispatch::{
    NoticeDispatch, NoticeSendResult, NoticeTarget, automatic_notice_dispatch_allowed,
};
use super::{CODEX, Composed, Notice, View, Workspace};
use crate::i18n::tf;
use crate::{
    agents,
    project::{
        self, ATRIUM, Project, Role, Task, TaskStatus, Workstream, controller_notice, wake_allowed,
    },
};

#[path = "workspace_autopilot.rs"]
mod autopilot;
#[path = "workspace_worktrees.rs"]
mod worktrees;

/// Where the user is: the everyday chats, or a project.
#[derive(Clone, Debug, PartialEq)]
pub enum Room {
    Chats,
    Project {
        root: PathBuf,
        /// The workstream whose chat is open; the overview when `None`.
        stream: Option<String>,
    },
    /// Workflows: the run shown, or the templates to start one from.
    Workflows {
        run: Option<PathBuf>,
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

/// What the watch pane shows of a conversation.
#[derive(Clone, Debug)]
pub struct Glance {
    pub title: String,
    pub state: StreamState,
    pub said: Option<String>,
    pub running: Vec<Activity>,
}

/// Something a conversation is doing in the background, or did lately.
#[derive(Clone, Debug, PartialEq)]
pub struct Activity {
    pub kind: ActivityKind,
    pub label: String,
    pub detail: Option<String>,
    pub live: bool,
    pub failed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityKind {
    Command,
    Tool,
    SubAgent,
    Search,
}

/// Commands, tools, sub-agents and searches of `transcript`, newest first:
/// the running ones, then the last few finished.
pub fn activities(transcript: &Transcript) -> Vec<Activity> {
    let mut out: Vec<Activity> = transcript
        .entries()
        .iter()
        .rev()
        .filter_map(|e| {
            let EntryKind::Item(item) = &e.kind else {
                return None;
            };
            let (kind, label, detail, status) = match item {
                Item::Command {
                    command,
                    status,
                    exit_code,
                    ..
                } => (
                    ActivityKind::Command,
                    command.clone(),
                    exit_code.filter(|c| *c != 0).map(|c| format!("exit {c}")),
                    *status,
                ),
                Item::Tool { name, status, .. } => {
                    (ActivityKind::Tool, name.clone(), None, *status)
                }
                Item::SubAgent {
                    label,
                    status,
                    detail,
                    ..
                } => (
                    ActivityKind::SubAgent,
                    label.clone(),
                    detail.clone(),
                    *status,
                ),
                Item::WebSearch { query, .. } => (
                    ActivityKind::Search,
                    query.clone(),
                    None,
                    if e.live {
                        ItemStatus::InProgress
                    } else {
                        ItemStatus::Completed
                    },
                ),
                _ => return None,
            };
            Some(Activity {
                kind,
                label,
                detail,
                live: status == ItemStatus::InProgress,
                failed: matches!(status, ItemStatus::Failed | ItemStatus::Declined),
            })
        })
        .take(40)
        .collect();
    out.sort_by_key(|a| !a.live);
    out
}

fn running_now(transcript: &Transcript) -> Vec<Activity> {
    activities(transcript)
        .into_iter()
        .filter(|a| a.live)
        .collect()
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
    task_pool: pool::TaskPoolSchedule,
    pr_poll: pr_status::Poll,
    pilot: autopilot::Pilot,
    inhibitor: crate::sleep_inhibitor::SleepInhibitor,
    inhibitor_failed: bool,
    cleaning: HashSet<(PathBuf, String)>,
    seen: HashMap<PathBuf, usize>,
    pending: HashSet<(PathBuf, String)>,
    last_wake: HashMap<(PathBuf, String), Instant>,
    tree_jobs: HashSet<PathBuf>,
    orphan_logged: HashSet<(PathBuf, String)>,
    #[cfg(all(test, unix, feature = "workspace-ticker-test"))]
    test_wake_checks: usize,
}

#[cfg(all(test, unix, feature = "workspace-ticker-test"))]
impl ScheduleState {
    pub(super) fn test_requeue_after_backoff(&mut self, root: PathBuf, stream: String) {
        let key = (root, stream);
        let elapsed = Instant::now().checked_sub(Duration::from_secs(31)).unwrap();
        assert!(
            self.last_wake.insert(key.clone(), elapsed).is_some(),
            "requeue must follow the fixture's real initial dispatch"
        );
        self.pending.insert(key);
        self.test_wake_checks = 0;
    }

    pub(super) fn test_queue_first_wake(&mut self, root: PathBuf, stream: String) {
        let key = (root, stream);
        assert!(!self.last_wake.contains_key(&key));
        assert_eq!(self.test_wake_checks, 0);
        assert!(self.pending.insert(key));
    }

    pub(super) fn assert_test_wake_attempt_still_pending(&self, root: PathBuf, stream: String) {
        let key = (root, stream);
        assert!(
            self.pending.contains(&key),
            "held notice wake must remain pending"
        );
        assert!(
            self.test_wake_checks > 0,
            "ticker must have reached dispatch preflight"
        );
    }
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
        cx.on_app_quit(|ws, _| {
            ws.previews.clear();
            async {}
        })
        .detach();
        let roots = self.settings.projects.clone();
        for root in roots {
            match Project::open(&root) {
                Ok(mut project) => {
                    if let Err(error) = project.recover_notice_deliveries() {
                        log::warn!("could not recover notice delivery journal: {error:#}");
                        cx.emit(Notice::Error(format!(
                            "Notice delivery recovery needs review: {error:#}"
                        )));
                    }
                    let held = project
                        .file
                        .streams
                        .iter()
                        .filter(|stream| project.notice_delivery_blocked(&stream.id))
                        .count();
                    if held > 0 {
                        cx.emit(Notice::Info(format!("{}: {held} workstream(s) have unresolved notice delivery. Automatic notice retries are paused; review notice delivery in each affected workstream.", root.display())));
                    }
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
        self.init_autopilot();
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
        self.tick_pr_badges(cx);
        let roots: Vec<_> = self.projects.iter().map(|p| p.root.clone()).collect();
        for root in roots {
            let wants_scheduler = self
                .projects
                .iter()
                .find(|project| project.root == root)
                .is_some_and(|project| {
                    project.file.autopilot.on
                        || project
                            .file
                            .streams
                            .iter()
                            .any(|stream| project.has_work(stream))
                });
            let scheduler_allowed = !wants_scheduler || self.sync_scheduler_allowed(&root);
            let changed = self.project_mut(&root).is_some_and(Project::reload);
            self.load_streams(&root, cx);
            self.tick_worktrees(&root, cx);
            let Some(project) = self.projects.iter().find(|p| p.root == root) else {
                continue;
            };
            if let Some(approved) = self.settings.preview_commands.get(&root) {
                self.previews
                    .entry(root.clone())
                    .or_default()
                    .tick(project, approved, cx);
            }
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
            // Only one device runs a project's scheduler. The lease is
            // renewed here, before any task can start.
            if !scheduler_allowed {
                continue;
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
                let running = self.running_project_streams(&root);
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
                if decision != StartDecision::Start || !self.scheduler_turn_slot(&root) {
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
            self.tick_task_pools(&root, cx);
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
            self.tick_autopilot(&root, cx);
            if changed {
                cx.notify();
            }
        }
        self.update_sleep_request();
        let pending: Vec<_> = self.project_schedule.pending.iter().cloned().collect();
        for (root, stream) in pending {
            let key = (root.clone(), stream.clone());
            if let Some(p) = self
                .projects
                .iter()
                .find(|p| p.root == root && p.file.autopilot.on)
            {
                if p.unread(&stream).is_empty() || p.stream(&stream).is_none_or(|s| s.retired) {
                    self.project_schedule.pending.remove(&key);
                }
                continue;
            }
            if !wake_allowed(
                self.project_schedule.last_wake.get(&key).copied(),
                Instant::now(),
                false,
                false,
                false,
            ) {
                continue;
            }
            #[cfg(all(test, unix, feature = "workspace-ticker-test"))]
            {
                self.project_schedule.test_wake_checks += 1;
            }
            if self.deliver_notices_at(&root, &stream, cx) {
                self.project_schedule.pending.remove(&key);
                self.project_schedule.last_wake.insert(key, Instant::now());
            }
        }
    }

    fn update_sleep_request(&mut self) {
        let needed = self
            .projects
            .iter()
            .any(|p| p.file.autopilot.on && p.file.streams.iter().any(|s| p.has_work(s)));
        match self.project_schedule.inhibitor.set_needed(needed) {
            Ok(()) => self.project_schedule.inhibitor_failed = false,
            Err(error) if !self.project_schedule.inhibitor_failed => {
                log::warn!("could not inhibit idle sleep: {error}");
                self.project_schedule.inhibitor_failed = true;
            }
            Err(_) => {}
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
            Room::Chats | Room::Workflows { .. } => None,
        }
    }

    pub fn active_project(&self) -> Option<&Project> {
        let root = self.project_root()?;
        self.projects.iter().find(|p| p.root == root)
    }

    fn project_mut(&mut self, root: &PathBuf) -> Option<&mut Project> {
        self.projects.iter_mut().find(|p| p.root == *root)
    }

    /// The `atrium` MCP server for workstream `stream` of the project in
    /// `root`: this executable run as `atrium mcp`. None for agents on other
    /// devices, which cannot run it.
    pub fn mcp_servers(root: &Path, stream: &str, backend: &str) -> Vec<McpServer> {
        if agents::split_backend(backend).1.is_some() {
            return Vec::new();
        }
        let Ok(exe) = std::env::current_exe() else {
            return Vec::new();
        };
        vec![McpServer {
            name: "atrium".into(),
            command: exe.display().to_string(),
            args: vec![
                "mcp".into(),
                "--project".into(),
                root.display().to_string(),
                "--stream".into(),
                stream.into(),
            ],
            env: Vec::new(),
        }]
    }

    /// The MCP servers the user set up in Atrium for every agent. None for
    /// agents on other devices, where their commands may not exist.
    pub fn user_mcp(&self, backend: &str) -> Vec<McpServer> {
        if agents::split_backend(backend).1.is_some() {
            return Vec::new();
        }
        self.settings
            .mcp_servers
            .iter()
            .filter(|m| !m.off)
            .map(|m| McpServer {
                name: m.name.clone(),
                command: m.command.clone(),
                args: m.args.clone(),
                env: m.env.clone(),
            })
            .collect()
    }

    /// The variables of the environment new sessions run with.
    pub fn session_env(&self) -> Vec<(String, String)> {
        let Some(name) = &self.settings.environment else {
            return Vec::new();
        };
        self.settings
            .environments
            .iter()
            .find(|e| &e.name == name)
            .map(|e| e.vars.clone())
            .unwrap_or_default()
    }

    /// The MCP servers a session of `thread` gets: the user's own, plus the
    /// `atrium` server for the workstream it belongs to, if any.
    pub fn mcp_for_thread(&self, thread: &str, backend: &str) -> Vec<McpServer> {
        let mut servers = self.user_mcp(backend);
        servers.extend(self.project_mcp_for_thread(thread, backend));
        servers
    }

    fn project_mcp_for_thread(&self, thread: &str, backend: &str) -> Vec<McpServer> {
        self.projects
            .iter()
            .find_map(|p| {
                let stream = p
                    .file
                    .streams
                    .iter()
                    .find(|s| s.thread_id.as_deref() == Some(thread))?;
                Some(Self::mcp_servers(&p.root, &stream.id, backend))
            })
            .unwrap_or_default()
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
        self.nav += 1;
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
        self.previews.remove(&root);
        self.settings.preview_commands.remove(&root);
        self.clear_autopilot(&root);
        self.projects.retain(|p| p.root != root);
        self.update_sleep_request();
        self.settings.projects.retain(|p| *p != root);
        if self.project_root().as_ref() == Some(&root) {
            self.enter_chats(cx);
        }
        self.settings.save();
        cx.notify();
    }

    /// Shows a project's overview.
    pub fn enter_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.nav += 1;
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
        self.nav += 1;
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
        self.nav += 1;
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
        let threads: Vec<_> = project
            .file
            .streams
            .iter()
            .filter(|s| {
                !self
                    .project_schedule
                    .task_pool
                    .starting
                    .contains(&(root.clone(), s.id.clone()))
            })
            .filter_map(|s| Some((s.thread_id.clone()?, s.backend.clone(), s.id.clone())))
            .collect();
        for (thread, backend, id) in threads {
            if self.transcripts.contains_key(&thread) || self.loading.contains(&thread) {
                continue;
            }
            let ready = self.backend_ready(&backend);
            if ready && self.pilot_resume_slot(root, &id, Instant::now()) {
                self.load_thread(thread, cx);
            }
        }
    }

    /// Starts a new workstream: its session, its goal and its kickoff.
    /// Starts a conversation with `text` in the background, with the shown
    /// conversation's agent and folder, without leaving the shown one. For
    /// "start all" on suggested follow-ups. Agents on other devices get a
    /// new chat with the text filled in instead.
    pub fn start_chat(&mut self, text: String, cx: &mut Context<Self>) {
        let backend = self.view_backend().to_string();
        let cwd = PathBuf::from(
            self.active_thread()
                .map(|t| t.cwd.clone())
                .unwrap_or_else(|| self.settings.working_dir().display().to_string()),
        );
        self.start_chat_in(text, backend, cwd, false, cx);
    }

    /// Starts a chat with `backend` in `cwd` in the background and sends
    /// `text`; with `open`, shows it once it starts.
    pub fn start_chat_in(
        &mut self,
        text: String,
        backend: String,
        cwd: PathBuf,
        open: bool,
        cx: &mut Context<Self>,
    ) {
        let title: String = text.chars().take(60).collect();
        if backend == CODEX {
            let Some(client) = self.client.clone() else {
                return;
            };
            let options = StartThread {
                cwd: Some(cwd.display().to_string()),
                model: self.settings.model.clone(),
                approval_policy: Some(self.settings.mode.approval_policy().into()),
                sandbox: Some(self.settings.mode.sandbox_mode().into()),
                mcp_servers: self.user_mcp(&backend),
                env: self.session_env(),
                developer_instructions: None,
            };
            cx.spawn(async move |this, cx| {
                let meta = match client.thread_start(options).await {
                    Ok(meta) => meta,
                    Err(error) => {
                        let _ =
                            this.update(cx, |_, cx| cx.emit(Notice::Error(format!("{error:#}"))));
                        return;
                    }
                };
                let thread = meta.id.clone();
                let _ = client.thread_set_name(&thread, &title).await;
                let _ = this.update(cx, |ws, cx| {
                    ws.upsert_thread(ThreadMeta {
                        name: Some(title.clone()),
                        ..meta
                    });
                    ws.transcripts.insert(thread.clone(), Transcript::new());
                    ws.send_to_thread(thread.clone(), Composed::text(text), cx);
                    if open {
                        ws.show_started(thread, cx);
                    }
                });
            })
            .detach();
            return;
        }
        let Some(client) = self.acp(&backend) else {
            self.new_chat(cx);
            cx.emit(super::Fill(Composed::text(text)));
            return;
        };
        let mcp = self.user_mcp(&backend);
        cx.spawn(async move |this, cx| {
            let start = match client.new_session_with(&cwd, &mcp).await {
                Ok(start) => start,
                Err(error) => {
                    let _ = this.update(cx, |_, cx| cx.emit(Notice::Error(format!("{error:#}"))));
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
                ws.session_options.insert(thread.clone(), start.options);
                if let Some(commands) = ws.agent_commands.get(&backend).cloned() {
                    ws.commands.insert(thread.clone(), commands);
                }
                ws.transcripts.insert(thread.clone(), Transcript::new());
                ws.send_to_thread(thread.clone(), Composed::text(text), cx);
                if open {
                    ws.show_started(thread, cx);
                }
            });
        })
        .detach();
    }

    /// Shows a chat started in the background, in the chats room.
    fn show_started(&mut self, thread: String, cx: &mut Context<Self>) {
        if self.room != Room::Chats || self.settings.active_project.is_some() {
            self.room = Room::Chats;
            self.settings.active_project = None;
            self.settings.save();
        }
        self.open_thread(thread, cx);
    }

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
        if self.running_project_streams(&root) >= 12 {
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
            ..Workstream::default()
        };
        self.start_stream(&root, stream, cx);
    }

    fn start_stream(&mut self, root: &Path, stream: Workstream, cx: &mut Context<Self>) {
        let root = root.to_path_buf();
        let Some(project) = self.project_mut(&root) else {
            return;
        };
        let id = stream.id.clone();
        let title = format!("{} · {}", project.file.name, stream.name);
        let kickoff = if pool::is_pool_worker(&stream) {
            match project.prepare_pool_start(&stream) {
                Ok(Some(kickoff)) => kickoff,
                Ok(None) => {
                    // Another worker took the last task before we acquired the lock.
                    if let Err(error) = project.retire_pool_worker(&id, None) {
                        log::warn!("could not retire empty pool start: {error:#}");
                    }
                    return;
                }
                Err(error) => {
                    self.stream_failed(&root, &id, error, cx);
                    return;
                }
            }
        } else {
            project.file.streams.push(stream.clone());
            if let Err(error) = project.save() {
                self.stream_failed(&root, &id, error, cx);
                return;
            }
            project.kickoff(&stream)
        };
        let isolated = project.file.worktrees;
        if pool::is_pool_worker(&stream) {
            self.project_schedule
                .task_pool
                .starting
                .insert((root.clone(), id.clone()));
        }
        cx.notify();
        if isolated {
            self.prepare_stream_checkout(root, stream, title, kickoff, cx);
        } else {
            self.launch_stream(root.clone(), stream, title, kickoff, root, cx);
        }
    }

    fn launch_stream(
        &mut self,
        root: PathBuf,
        stream: Workstream,
        title: String,
        kickoff: String,
        cwd: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let backend = stream.backend.clone();
        let id = stream.id.clone();
        let unattended = self
            .projects
            .iter()
            .any(|p| p.root == root && p.file.autopilot.on);
        if unattended
            && agents::split_backend(&backend).1.is_none()
            && !cwd
                .canonicalize()
                .ok()
                .zip(root.canonicalize().ok())
                .is_some_and(|(dir, root)| dir.is_dir() && dir.starts_with(root))
        {
            self.stream_failed(
                &root,
                &id,
                anyhow::anyhow!("unavailable or unauthorized project work directory"),
                cx,
            );
            return;
        }
        if agents::is_codex(&backend) {
            let Some(client) = self.codex_for(&backend) else {
                self.stream_failed(
                    &root,
                    &id,
                    anyhow::anyhow!("backend {backend} disconnected"),
                    cx,
                );
                return;
            };
            let mode = self.project_access_mode(unattended);
            let options = StartThread {
                cwd: Some(cwd.display().to_string()),
                model: if agents::split_backend(&backend).1.is_some() {
                    None
                } else {
                    self.settings.model.clone()
                },
                approval_policy: Some(mode.approval_policy().into()),
                sandbox: Some(mode.sandbox_mode().into()),
                mcp_servers: {
                    let mut servers = self.user_mcp(&backend);
                    servers.extend(Self::mcp_servers(&root, &id, &backend));
                    servers
                },
                env: self.session_env(),
                developer_instructions: None,
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
                    ws.project_schedule
                        .task_pool
                        .starting
                        .remove(&(root.clone(), id.clone()));
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
                    if ws.projects.iter().any(|p| {
                        p.root == root
                            && p.stream(&id).is_some_and(|s| {
                                !s.retired && s.thread_id.as_ref() == Some(&thread)
                            })
                    }) {
                        ws.send_stream_kickoff(&root, &id, thread, kickoff, cx);
                    }
                });
            })
            .detach();
        } else {
            let Some(client) = self.acp(&backend) else {
                self.stream_failed(
                    &root,
                    &id,
                    anyhow::anyhow!("backend {backend} disconnected"),
                    cx,
                );
                return;
            };
            let mut mcp = self.user_mcp(&backend);
            mcp.extend(Self::mcp_servers(&root, &id, &backend));
            cx.spawn(async move |this, cx| {
                let start = match client.new_session_with(&cwd, &mcp).await {
                    Ok(start) => start,
                    Err(error) => {
                        let _ = this.update(cx, |ws, cx| ws.stream_failed(&root, &id, error, cx));
                        return;
                    }
                };
                let _ = this.update(cx, |ws, cx| {
                    let now = jiff::Timestamp::now().as_second();
                    let thread = start.session_id.clone();
                    ws.project_schedule
                        .task_pool
                        .starting
                        .remove(&(root.clone(), id.clone()));
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
                        Ok(()) => ws.send_stream_kickoff(&root, &id, thread, kickoff, cx),
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
        self.project_schedule
            .task_pool
            .starting
            .remove(&(root.clone(), stream.to_owned()));
        if let Some(parent) = self
            .projects
            .iter()
            .find(|p| p.root == *root)
            .and_then(|p| p.stream(stream))
            .filter(|s| pool::is_pool_worker(s))
            .and_then(|s| s.parent.clone())
        {
            self.project_schedule
                .task_pool
                .last_start_failure
                .insert((root.clone(), parent), Instant::now());
        }
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
        self.clean_removed_checkout(&root, stream, cx);
        if let Some(project) = self.project_mut(&root) {
            project.file.streams.retain(|s| s.id != stream);
            let _ = project.save();
        }
        self.show_overview(cx);
    }

    /// Sends `composed` to a thread whatever the main column shows.
    pub fn send_to_thread(&mut self, thread: String, composed: Composed, cx: &mut Context<Self>) {
        self.submit_to_thread(thread, composed, cx);
    }

    pub(super) fn submit_to_thread(
        &mut self,
        thread: String,
        composed: Composed,
        cx: &mut Context<Self>,
    ) -> bool {
        let state = BackgroundDispatch::enter(&mut self.view, &mut self.editing, thread);
        let submitted = self.send_for_notice(composed, None, cx) == NoticeSendResult::Submitted;
        state.restore(&mut self.view, &mut self.editing);
        cx.notify();
        submitted
    }

    /// Reserve a captured prefix immediately before dispatch. Held notices
    /// are never attached again; manual text may still be sent without them.
    pub(super) fn prepare_stream_notices(
        &mut self,
        thread: &str,
        composed: Composed,
        target: Option<&NoticeTarget>,
        cx: &mut Context<Self>,
    ) -> Result<(Composed, Option<NoticeDispatch>), NoticeSendResult> {
        let Some((root, stream)) = self
            .stream_of(thread)
            .map(|(p, s)| (p.root.clone(), s.id.clone()))
        else {
            return if target.is_some() {
                Err(NoticeSendResult::NoDispatch)
            } else {
                Ok((composed, None))
            };
        };
        if target.is_some_and(|t| t.root != root || t.stream != stream || t.thread != thread) {
            return Err(NoticeSendResult::NoDispatch);
        }
        let project = self
            .project_mut(&root)
            .ok_or(NoticeSendResult::NoDispatch)?;
        let prepared = match target {
            Some(target) => {
                NoticeDispatch::prepare_automatic(project, target, thread, composed.text.clone())
            }
            None => {
                NoticeDispatch::prepare(project, &stream, thread, composed.text.clone()).map(Ok)
            }
        };
        match prepared {
            Ok(Err(outcome)) => Err(outcome),
            Ok(Ok(prepared)) => {
                if prepared.held {
                    cx.emit(Notice::Info("Earlier project notices are awaiting delivery confirmation. Sending only your current text; held notices are not retried.".into()));
                }
                Ok((
                    Composed {
                        text: prepared.text,
                        ..composed
                    },
                    prepared.dispatch,
                ))
            }
            Err(error) => {
                cx.emit(Notice::Error(format!(
                    "Could not reserve project notices; nothing was sent: {error:#}"
                )));
                Err(NoticeSendResult::NoDispatch)
            }
        }
    }

    pub(super) fn settle_stream_notices(
        &mut self,
        dispatch: Option<NoticeDispatch>,
        outcome: NoticeOutcome,
        cx: &mut Context<Self>,
    ) {
        let Some(dispatch) = dispatch else {
            return;
        };
        let Some(project) = self.project_mut(&dispatch.root) else {
            return;
        };
        match dispatch.settle(project, outcome) {
            Ok(false) => {},
            Ok(true) => {
                if project.notice_delivery_uncertain(&dispatch.stream) {
                    cx.emit(Notice::Error("Project notice delivery is uncertain. Automatic retry is paused; explicitly mark the notices received or authorize retrying notices only.".into()));
                }
            }
            Err(error) => cx.emit(Notice::Error(format!("Could not save notice delivery confirmation. Automatic retry remains blocked: {error:#}"))),
        }
        cx.notify();
    }

    /// Requires an explicit user choice. Never replays the original manual text.
    pub fn resolve_stream_notice_delivery(
        &mut self,
        root: &PathBuf,
        stream: &str,
        retry_notices: bool,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<bool> {
        let resolution = if retry_notices {
            NoticeResolution::RetryNotices
        } else {
            NoticeResolution::MarkReceived
        };
        let changed = self
            .project_mut(root)
            .ok_or_else(|| anyhow::anyhow!("project is not open"))?
            .resolve_notice_delivery(stream, resolution)?;
        if changed {
            self.project_schedule
                .pending
                .insert((root.clone(), stream.to_owned()));
            cx.notify();
        }
        Ok(changed)
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
        if !automatic_notice_dispatch_allowed(project, stream) {
            return false;
        }
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
        if !transcript.is_running() && self.running_project_streams(root) >= 12 {
            return false;
        }
        if !wake_allowed(
            None,
            Instant::now(),
            waiting_on_user(transcript) > 0,
            transcript.is_running(),
            agents::is_codex(self.backend_of(&thread)),
        ) {
            return false;
        }
        let target = NoticeTarget {
            root: root.clone(),
            stream: stream.to_owned(),
            thread: thread.clone(),
        };
        // Rate-limit actual attempts (including persistence refusals) without
        // dropping their pending wake or spamming an error every two seconds.
        self.project_schedule
            .last_wake
            .insert((root.clone(), stream.to_owned()), Instant::now());
        let state = BackgroundDispatch::enter(&mut self.view, &mut self.editing, thread);
        let result = self.send_for_notice(
            Composed::text("Take these into account and carry on toward your goal.".into()),
            Some(&target),
            cx,
        );
        state.restore(&mut self.view, &mut self.editing);
        cx.notify();
        matches!(
            result,
            NoticeSendResult::Empty | NoticeSendResult::Submitted
        )
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
    /// A lead's task pool in the shown project.
    pub fn pool(&self, lead: &str) -> Option<project::pool::Pool> {
        self.active_project().map(|p| p.pool(lead))
    }

    /// Turns the shown project's autopilot on or off: whether Atrium keeps
    /// its agents going around the clock without the user.
    pub fn set_autopilot(&mut self, on: bool, cx: &mut Context<Self>) {
        let Some(root) = self.project_root() else {
            return;
        };
        if let Some(project) = self.project_mut(&root) {
            let previous = project.file.autopilot.on;
            project.file.autopilot.on = on;
            if let Err(e) = project.save() {
                project.file.autopilot.on = previous;
                cx.emit(Notice::Error(format!("{e:#}")));
                return;
            }
        }
        if !on {
            self.clear_autopilot(&root);
        }
        self.update_sleep_request();
        cx.notify();
    }

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
    /// A conversation at a glance, for watching it beside another: its
    /// title, state, last words and what it is running now.
    pub fn glance(&self, thread: &str) -> Option<Glance> {
        let meta = self.threads.iter().find(|t| t.id == thread);
        let stream = self.projects.iter().find_map(|p| {
            p.file
                .streams
                .iter()
                .find(|s| s.thread_id.as_deref() == Some(thread))
                .map(|s| format!("{} · {}", p.file.name, s.name))
        });
        let title =
            stream.or_else(|| meta.map(|m| m.name.clone().unwrap_or_else(|| m.preview.clone())))?;
        let transcript = self.transcripts.get(thread);
        let state = match transcript {
            None => StreamState::Idle,
            Some(t) if waiting_on_user(t) > 0 => StreamState::NeedsYou(waiting_on_user(t)),
            Some(t) if t.is_running() => StreamState::Working,
            Some(t) => last_failure(t).map_or(StreamState::Idle, StreamState::Failed),
        };
        let said = transcript.and_then(|t| {
            t.entries().iter().rev().find_map(|e| match &e.kind {
                EntryKind::Item(Item::Agent { text, .. }) if !text.trim().is_empty() => {
                    Some(text.trim().to_string())
                }
                _ => None,
            })
        });
        Some(Glance {
            title,
            state,
            said,
            running: transcript.map(running_now).unwrap_or_default(),
        })
    }

    pub fn stream_views(&self) -> Vec<StreamView> {
        let Some(root) = self.project_root() else {
            return Vec::new();
        };
        self.stream_views_for(&root)
    }

    pub(super) fn stream_state(
        &self,
        stream: &Workstream,
        transcript: Option<&Transcript>,
    ) -> StreamState {
        let agent_up = self.backend_ready(&stream.backend);
        let waiting = transcript.map_or(0, waiting_on_user);
        match (transcript, &stream.thread_id) {
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
        }
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
                let state = self.stream_state(stream, transcript);
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
        cwd: project.session_checkout(&stream.id).display().to_string(),
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
