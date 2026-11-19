//! Projects: a folder worked on by several long-running sessions
//! ("workstreams"), each with its own goal and scope, aligning through shared
//! files in the folder's `.atrium/` — the atrium.
//!
//! Everything lives in plain files any agent can read and write with its own
//! tools, so no agent needs to know about Atrium:
//!
//! ```text
//! .atrium/project.json      the workstreams (Atrium's own bookkeeping)
//! .atrium/brief.md          shared conventions and decisions
//! .atrium/contracts/*.md    interfaces between workstreams
//! .atrium/status/<id>.md    each workstream's own status
//! .atrium/notices.md        append-only notices: "- <time> · <from> → <to>: <text>"
//! .atrium/plan.md           the controller's plan: milestones and who owns them
//! .atrium/tasks/<owner>/<id>.md
//!                           a request for a new agent one level down:
//!                           the controller asks for leads, a lead for workers
//! ```
//!
//! Three levels and no deeper: one controller splits the project and keeps
//! the plan, leads own long-running parts with a roadmap each, and workers
//! do one self-contained task for their lead. Atrium starts the agent a task
//! file asks for and tells the asker when it is done, so nobody polls.

pub mod agent;
pub mod autopilot;
pub mod mcp;
pub mod pool;
pub mod skill;

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

pub const DIR: &str = ".atrium";
/// Who signs notices Atrium posts itself, such as scope warnings.
pub const ATRIUM: &str = "atrium";
/// Who signs notices the user posts.
pub const YOU: &str = "you";

/// Where a workstream sits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Splits the project into leads and keeps the plan; does no
    /// implementation and wakes only when a lead reports.
    Controller,
    /// Owns a long-running part of the project, with a roadmap.
    #[default]
    Lead,
    /// Does one task for its lead, then reports.
    Worker,
}

impl Role {
    /// The role of the agents a task file of this role asks for.
    pub fn child(self) -> Option<Role> {
        match self {
            Role::Controller => Some(Role::Lead),
            Role::Lead => Some(Role::Worker),
            Role::Worker => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Role::Controller => "Controller",
            Role::Lead => "Lead",
            Role::Worker => "Sub-agent",
        }
    }
}

/// How far a task is, as its file says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TaskStatus {
    /// Waiting for Atrium to start an agent for it.
    #[default]
    Open,
    Running,
    Done,
    Failed,
}

impl TaskStatus {
    fn parse(text: &str) -> Self {
        match text.trim().to_lowercase().as_str() {
            "running" | "in progress" | "in-progress" | "working" | "started" => Self::Running,
            "done" | "complete" | "completed" | "finished" => Self::Done,
            "failed" | "blocked" | "error" | "abandoned" => Self::Failed,
            _ => Self::Open,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    pub fn finished(self) -> bool {
        matches!(self, Self::Done | Self::Failed)
    }
}

/// A step of a roadmap: `- [ ] text` or `- [x] text`.
#[derive(Clone, Debug, PartialEq)]
pub struct Milestone {
    pub text: String,
    pub done: bool,
}

/// A stage of a roadmap: a `### Title` and the steps under it.
#[derive(Clone, Debug, PartialEq)]
pub struct Phase {
    pub title: String,
    pub steps: Vec<Milestone>,
}

impl Phase {
    pub fn done(&self) -> bool {
        self.steps.iter().all(|m| m.done)
    }

    /// Steps done and total.
    pub fn progress(&self) -> (usize, usize) {
        (
            self.steps.iter().filter(|m| m.done).count(),
            self.steps.len(),
        )
    }
}

/// `.atrium/tasks/<owner>/<id>.md`: one agent asked for by another.
///
/// ```text
/// # Title
/// status: open
/// agent: claude
/// scope: web/**, src/ui
///
/// What to do.
///
/// ## Roadmap
/// ### First phase
/// - [ ] A step
/// - [ ] Another step
///
/// ### Next phase
/// - [ ] A step
///
/// ## Result
/// What was done, written by the agent that did it.
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Task {
    /// The workstream that asked.
    pub owner: String,
    /// The file's name without `.md`.
    pub id: String,
    pub title: String,
    pub status: TaskStatus,
    pub agent: Option<String>,
    pub scope: Vec<String>,
    /// What to do: everything before the first `## ` section.
    pub body: String,
    /// Every step of the roadmap, in order.
    pub roadmap: Vec<Milestone>,
    /// The roadmap's phases; without `###` headings each step is a phase.
    pub phases: Vec<Phase>,
    pub result: Option<String>,
    /// The pool worker working on it (`claimed_by:`).
    pub claimed_by: Option<String>,
    /// Ids of tasks of the same owner that must be done first (`after:`).
    pub after: Vec<String>,
    /// `priority: high` (1), `normal` (0) or `low` (-1); higher is claimed first.
    pub priority: i32,
}

impl Task {
    /// `owner/id`: how a workstream names the task it was started for.
    pub fn key(&self) -> String {
        format!("{}/{}", self.owner, self.id)
    }

    /// The path from the project root, for prompts.
    pub fn rel_path(&self) -> String {
        format!("{DIR}/tasks/{}/{}.md", self.owner, self.id)
    }

    /// Reads a task file leniently: agents write them by hand.
    pub fn parse(owner: &str, id: &str, text: &str) -> Self {
        let mut task = Task {
            owner: owner.to_string(),
            id: id.to_string(),
            title: id.to_string(),
            ..Default::default()
        };
        let mut lines = text.lines().peekable();
        while lines.peek().is_some_and(|l| l.trim().is_empty()) {
            lines.next();
        }
        if let Some(title) = lines.peek().and_then(|l| l.trim().strip_prefix("# ")) {
            task.title = title.trim().to_string();
            lines.next();
        }
        while let Some(line) = lines.peek() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                lines.next();
                continue;
            }
            let Some((key, value)) = trimmed.split_once(':') else {
                break;
            };
            match key.trim().to_lowercase().as_str() {
                "status" => task.status = TaskStatus::parse(value),
                "agent" => {
                    let value = value.trim().to_lowercase();
                    task.agent = (!value.is_empty()).then_some(value);
                }
                "claimed_by" | "claimed by" | "worker" => {
                    let value = value.trim();
                    task.claimed_by = (!value.is_empty()).then(|| value.to_string());
                }
                "after" | "depends on" | "blocked by" => {
                    task.after = value
                        .split(',')
                        .map(|s| {
                            s.trim()
                                .trim_matches('`')
                                .trim_end_matches(".md")
                                .to_string()
                        })
                        .filter(|s| !s.is_empty())
                        .collect()
                }
                "priority" => {
                    task.priority = match value.trim().to_lowercase().as_str() {
                        "high" | "urgent" | "p0" | "p1" | "1" => 1,
                        "low" | "p3" | "-1" => -1,
                        _ => 0,
                    }
                }
                "scope" => {
                    task.scope = value
                        .split(',')
                        .map(|s| s.trim().trim_matches('`').to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                }
                _ => break,
            }
            lines.next();
        }
        let mut section: Option<String> = None;
        let mut phases: Vec<Phase> = Vec::new();
        let mut body = String::new();
        let mut result = String::new();
        for line in lines {
            if let Some(heading) = line.trim().strip_prefix("## ") {
                section = Some(heading.trim().to_lowercase());
                continue;
            }
            match section.as_deref() {
                None => {
                    body.push_str(line);
                    body.push('\n');
                }
                Some("roadmap") | Some("milestones") => {
                    if let Some(title) = line.trim().strip_prefix("### ") {
                        phases.push(Phase {
                            title: title.trim().to_string(),
                            steps: Vec::new(),
                        });
                        continue;
                    }
                    let item = line.trim();
                    let item = item
                        .strip_prefix("- ")
                        .or_else(|| item.strip_prefix("* "))
                        .unwrap_or("");
                    let (done, text) = if let Some(rest) = item.strip_prefix("[ ]") {
                        (false, rest)
                    } else if let Some(rest) = item
                        .strip_prefix("[x]")
                        .or_else(|| item.strip_prefix("[X]"))
                    {
                        (true, rest)
                    } else if !item.is_empty() {
                        (false, item)
                    } else {
                        continue;
                    };
                    let step = Milestone {
                        text: text.trim().to_string(),
                        done,
                    };
                    match phases.last_mut() {
                        Some(phase) if !phase.title.is_empty() => phase.steps.push(step.clone()),
                        // Steps before any heading stand on their own.
                        _ => phases.push(Phase {
                            title: String::new(),
                            steps: vec![step.clone()],
                        }),
                    }
                    task.roadmap.push(step);
                }
                Some("result") | Some("report") => {
                    result.push_str(line);
                    result.push('\n');
                }
                Some(_) => {
                    body.push_str(line);
                    body.push('\n');
                }
            }
        }
        task.body = body.trim().to_string();
        task.phases = phases
            .into_iter()
            .filter(|p| !p.steps.is_empty())
            .map(|mut p| {
                if p.title.is_empty() {
                    p.title = p.steps[0].text.clone();
                }
                p
            })
            .collect();
        let result = result.trim();
        task.result = (!result.is_empty()).then(|| result.to_string());
        task
    }

    /// A task file for `title`, as the user's "add" dialog writes it.
    pub fn render(
        title: &str,
        agent: &str,
        scope: &[String],
        body: &str,
        roadmap: &[String],
    ) -> String {
        let mut text = format!("# {}\nstatus: open\n", title.trim());
        if !agent.is_empty() {
            text.push_str(&format!("agent: {agent}\n"));
        }
        if !scope.is_empty() {
            text.push_str(&format!("scope: {}\n", scope.join(", ")));
        }
        text.push('\n');
        text.push_str(body.trim());
        text.push('\n');
        if !roadmap.is_empty() {
            text.push_str("\n## Roadmap\n");
            for step in roadmap {
                text.push_str(&format!("- [ ] {}\n", step.trim()));
            }
        }
        text
    }

    /// The phase being worked on: the first not done.
    pub fn current_phase(&self) -> Option<usize> {
        self.phases.iter().position(|p| !p.done())
    }

    /// Steps done and total.
    pub fn progress(&self) -> Option<(usize, usize)> {
        (!self.roadmap.is_empty()).then(|| {
            (
                self.roadmap.iter().filter(|m| m.done).count(),
                self.roadmap.len(),
            )
        })
    }
}

/// The header keys of a task file.
const HEADER_KEYS: &[&str] = &[
    "status",
    "agent",
    "scope",
    "claimed_by",
    "claimed by",
    "worker",
    "after",
    "depends on",
    "blocked by",
    "priority",
];

/// `text` with its header line `key:` saying `value`, added under the other
/// header lines when missing, removed with `None`.
pub fn with_header(text: &str, key: &str, value: Option<&str>) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let title = lines
        .iter()
        .position(|l| l.trim().starts_with("# "))
        .map_or(0, |i| i + 1);
    // The header block: known `key: value` lines under the title, as
    // `Task::parse` reads them.
    let mut end = title;
    let mut at = title;
    let mut found = None;
    while end < lines.len() {
        let line = lines[end].trim();
        if line.is_empty() {
            end += 1;
            continue;
        }
        match line.split_once(':') {
            Some((k, _)) if HEADER_KEYS.contains(&k.trim().to_lowercase().as_str()) => {
                if k.trim().eq_ignore_ascii_case(key) {
                    found = Some(end);
                }
                end += 1;
                at = end;
            }
            _ => break,
        }
    }
    let end = at;
    match (found, value) {
        (Some(i), Some(value)) => lines[i] = format!("{key}: {value}"),
        (Some(i), None) => {
            lines.remove(i);
        }
        (None, Some(value)) => lines.insert(end, format!("{key}: {value}")),
        (None, None) => {}
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// `text` with its `status:` line saying `status`; one is added under the
/// title when there is none.
pub fn with_status(text: &str, status: TaskStatus) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    if let Some(line) = lines
        .iter_mut()
        .find(|l| l.trim().to_lowercase().starts_with("status:"))
    {
        *line = format!("status: {}", status.word());
    } else {
        let at = lines
            .iter()
            .position(|l| l.trim().starts_with("# "))
            .map_or(0, |i| i + 1);
        lines.insert(at, format!("status: {}", status.word()));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// One long-running session of a project.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Workstream {
    /// Short and file-safe: names its status file and addresses notices.
    pub id: String,
    pub name: String,
    /// `codex`, `claude`, `deepseek`, `antigravity`.
    pub backend: String,
    /// The session, once started.
    pub thread_id: Option<String>,
    /// What it keeps working toward.
    pub goal: String,
    /// Paths it owns, as globs relative to the project root (`web/**`).
    pub scope: Vec<String>,
    /// How many notices it has been handed.
    pub notices_seen: usize,
    pub created_at: i64,
    pub role: Role,
    /// Who started it: the controller for a lead, a lead for a worker.
    pub parent: Option<String>,
    /// The task it was started for, as `owner/id`.
    pub task: Option<String>,
    /// Whether its parent has been told the task finished.
    pub reported: bool,
    /// A pool worker taken out of service (its session was replaced by a
    /// fresh one, or it kept failing). Kept so its chat stays readable.
    pub retired: bool,
    /// The pull request of its branch, as the scheduler last saw it.
    pub pr: Option<PullRequest>,
}

/// A workstream's pull request and its checks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
    /// `open`, `draft`, `merged` or `closed`.
    pub state: String,
    pub checks: Checks,
}

/// How a pull request's CI stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Checks {
    #[default]
    None,
    Pending,
    Passing,
    Failing,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectFile {
    pub name: String,
    pub streams: Vec<Workstream>,
    /// How many workers one lead may have running at once.
    pub max_workers: usize,
    /// Whether leads the controller asks for wait for the user's go-ahead.
    pub approve_leads: bool,
    /// Tasks the user said yes to, as `owner/id`.
    pub approved: Vec<String>,
    /// Finished pool tasks their lead has been told about, as `owner/id`.
    pub reported_tasks: Vec<String>,
    /// Running without the user: see [`Autopilot`].
    pub autopilot: Autopilot,
    /// Each lead and pool worker works in its own git worktree
    /// ([`Project::worktree_dir`]) on its own branch ([`Project::branch_of`]).
    pub worktrees: bool,
}

/// How a project keeps going around the clock without the user, after
/// Paperclip's heartbeats and liveness recovery. Atrium's scheduler acts on
/// it; agents only read it (their kickoff says whether to stop for the user).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Autopilot {
    /// Keep every agent with unfinished work going: continue it when its
    /// turn ends, retry failures, recover stalls, and never wait on the user.
    pub on: bool,
    /// Minutes between check-ins of an idle lead that still has work.
    pub lead_heartbeat_min: u32,
    /// Minutes between check-ins of the controller while leads work.
    pub controller_heartbeat_min: u32,
    /// Seconds after a turn ends before an agent with unfinished work is
    /// told to continue.
    pub continue_after_secs: u32,
    /// Minutes without any output before a running turn counts as stalled.
    pub stall_min: u32,
    /// Tasks a pool worker does before it is replaced by a fresh session.
    pub tasks_per_session: usize,
    /// Wake a lead when fewer tasks than this are ready; 0 means one per worker.
    pub refill_at: usize,
    /// Turns the project may start in a day; 0 for no limit.
    pub daily_turns: u32,
}

impl Default for Autopilot {
    fn default() -> Self {
        Self {
            on: false,
            lead_heartbeat_min: 30,
            controller_heartbeat_min: 120,
            continue_after_secs: 60,
            stall_min: 15,
            tasks_per_session: 5,
            refill_at: 0,
            daily_turns: 0,
        }
    }
}

impl Default for ProjectFile {
    fn default() -> Self {
        Self {
            name: String::new(),
            streams: Vec::new(),
            max_workers: 3,
            approve_leads: false,
            approved: Vec::new(),
            reported_tasks: Vec::new(),
            autopilot: Autopilot::default(),
            worktrees: false,
        }
    }
}

/// A line of `.atrium/notices.md`.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectNotice {
    pub at: String,
    pub from: String,
    /// Workstream ids, or `all`.
    pub to: Vec<String>,
    pub text: String,
}

impl ProjectNotice {
    /// Whether `stream` should get it: addressed to it or to all, and not its own.
    pub fn is_for(&self, stream: &str) -> bool {
        self.from != stream && self.to.iter().any(|to| to == stream || to == "all")
    }

    pub fn line(&self) -> String {
        format!(
            "- {} · {} → {}: {}",
            self.at,
            self.from,
            self.to.join(", "),
            self.text.replace('\n', "\n  ")
        )
    }
}

/// A change one workstream made inside another's scope.
#[derive(Clone, Debug, PartialEq)]
pub struct Crossing {
    pub stream: String,
    pub owner: String,
    pub path: String,
}

/// An open project: its bookkeeping plus the atrium as last read.
#[derive(Clone, Debug, Default)]
pub struct Project {
    pub root: PathBuf,
    pub file: ProjectFile,
    pub brief: String,
    /// `(file name, text)`, sorted by name.
    pub contracts: Vec<(String, String)>,
    /// Status file text by workstream id.
    pub statuses: HashMap<String, String>,
    /// `.atrium/plan.md`.
    pub plan: String,
    /// Every task file, sorted by owner and id.
    pub tasks: Vec<Task>,
    pub notices: Vec<ProjectNotice>,
    /// Scope crossings seen this session.
    pub crossings: Vec<Crossing>,
    /// The newest change to the atrium's files, to reload only when needed.
    pub stamp: Option<SystemTime>,
}

/// Added to kickoffs while the project's autopilot is on.
const UNATTENDED: &str = "This project runs unattended, around the clock: nobody is watching to answer questions. Never stop to ask the user. Make the reasonable choice, write it down in your status file (or result) and tell your parent with a notice; if something truly blocks you, say so in a notice and carry on with whatever else you can do. When Atrium tells you to continue, pick up exactly where you left off: re-read your task file and status file first.";

/// Ends every kickoff: the `atrium` MCP tools do what the files say, safely.
const TOOLS_HINT: &str = "If you have the `atrium` tools (`whoami`, `check_step`, `create_task`, `finish_task`, `notify`, `write_status`, `write_plan`), use them instead of editing these files by hand: they do the same, check the format and tell the right people. Without them, edit the files as described.";
const BRIEF: &str = "# Brief\n\nShared conventions and decisions every workstream follows. Keep it short.\n\n## Architecture\n\n## Conventions\n\n## Decisions\n";
const CONTRACTS: &str = "# Contracts\n\nOne file per interface between workstreams (an API, a schema, an event).\nThe workstream that changes an interface updates its file and posts a notice.\n";

impl Project {
    pub fn dir(&self) -> PathBuf {
        self.root.join(DIR)
    }

    /// Opens the project in `root`, creating the atrium the first time.
    pub fn open(root: &Path) -> Result<Self> {
        let dir = root.join(DIR);
        fs::create_dir_all(dir.join("contracts"))
            .and_then(|()| fs::create_dir_all(dir.join("status")))
            .and_then(|()| fs::create_dir_all(dir.join("tasks")))
            .with_context(|| format!("could not create {}", dir.display()))?;
        let write_new = |path: PathBuf, text: &str| -> Result<()> {
            if !path.exists() {
                fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
            }
            Ok(())
        };
        write_new(dir.join("brief.md"), BRIEF)?;
        write_new(dir.join("contracts").join("README.md"), CONTRACTS)?;
        write_new(dir.join("notices.md"), "# Notices\n\n")?;
        let file = match fs::read_to_string(dir.join("project.json")) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("reading {}", dir.join("project.json").display()))?,
            Err(_) => ProjectFile {
                name: root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Project".into()),
                ..ProjectFile::default()
            },
        };
        let mut project = Self {
            root: root.to_path_buf(),
            file,
            ..Self::default()
        };
        project.save()?;
        project.reload();
        Ok(project)
    }

    pub fn save(&self) -> Result<()> {
        let path = self.dir().join("project.json");
        let text = serde_json::to_string_pretty(&self.file)?;
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
    }

    /// When the atrium's files last changed.
    pub fn latest_change(&self) -> Option<SystemTime> {
        let dir = self.dir();
        let mut paths = vec![
            dir.join("brief.md"),
            dir.join("notices.md"),
            dir.join("plan.md"),
        ];
        for sub in ["contracts", "status", "tasks"] {
            paths.push(dir.join(sub));
            if let Ok(entries) = fs::read_dir(dir.join(sub)) {
                paths.extend(entries.flatten().map(|e| e.path()));
            }
        }
        if let Ok(owners) = fs::read_dir(dir.join("tasks")) {
            for owner in owners.flatten() {
                if let Ok(entries) = fs::read_dir(owner.path()) {
                    paths.extend(entries.flatten().map(|e| e.path()));
                }
            }
        }
        paths
            .iter()
            .filter_map(|p| fs::metadata(p).and_then(|m| m.modified()).ok())
            .max()
    }

    /// Reads the atrium again. Returns whether anything changed on disk.
    pub fn reload(&mut self) -> bool {
        let stamp = self.latest_change();
        if stamp.is_some() && stamp == self.stamp {
            return false;
        }
        self.stamp = stamp;
        let dir = self.dir();
        self.brief = fs::read_to_string(dir.join("brief.md")).unwrap_or_default();
        self.notices =
            parse_notices(&fs::read_to_string(dir.join("notices.md")).unwrap_or_default());
        self.contracts = read_dir_texts(&dir.join("contracts"));
        self.statuses = read_dir_texts(&dir.join("status"))
            .into_iter()
            .map(|(name, text)| (name.trim_end_matches(".md").to_string(), text))
            .collect();
        self.plan = fs::read_to_string(dir.join("plan.md")).unwrap_or_default();
        self.tasks = read_tasks(&dir.join("tasks"));
        true
    }

    pub fn task(&self, key: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.key() == key)
    }

    /// Writes `status` into a task's file.
    pub fn set_task_status(&mut self, key: &str, status: TaskStatus) -> Result<()> {
        let _lock = pool::TaskLock::take(self)?;
        let Some(task) = self.task(key) else {
            return Ok(());
        };
        let path = self.root.join(task.rel_path());
        let text = fs::read_to_string(&path).unwrap_or_default();
        fs::write(&path, with_status(&text, status))
            .with_context(|| format!("writing {}", path.display()))?;
        self.stamp = None;
        self.reload();
        Ok(())
    }

    /// Writes a new task file for `owner`; returns its key.
    pub fn add_task(&mut self, owner: &str, title: &str, text: &str) -> Result<String> {
        let dir = self.dir().join("tasks").join(owner);
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let base = slug(title).unwrap_or_else(|| "task".into());
        let mut id = base.clone();
        let mut n = 2;
        while dir.join(format!("{id}.md")).exists() {
            id = format!("{base}-{n}");
            n += 1;
        }
        let path = dir.join(format!("{id}.md"));
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        self.stamp = None;
        self.reload();
        Ok(format!("{owner}/{id}"))
    }

    /// Where workstream `id` works when the project uses worktrees:
    /// `.atrium/worktrees/<id>`, kept out of git by the project's exclude file.
    pub fn worktree_dir(&self, id: &str) -> PathBuf {
        self.dir().join("worktrees").join(id)
    }

    /// The latest screenshot of workstream `id`'s work, when one was saved
    /// to `.atrium/previews/<id>.png` (by the agent, or by Atrium's preview).
    pub fn preview_of(&self, id: &str) -> Option<PathBuf> {
        let path = self.dir().join("previews").join(format!("{id}.png"));
        path.is_file().then_some(path)
    }

    /// The branch of workstream `id`'s worktree: `atrium/<id>`.
    pub fn branch_of(id: &str) -> String {
        format!("atrium/{id}")
    }

    /// The folder workstream `id`'s session runs in: its worktree when the
    /// project uses them, the project root otherwise.
    pub fn work_dir(&self, id: &str) -> PathBuf {
        let own = self.stream(id);
        // The controller only plans; it stays in the main checkout.
        let isolated = self.file.worktrees && own.is_some_and(|s| s.role != Role::Controller);
        if isolated {
            self.worktree_dir(id)
        } else {
            self.root.clone()
        }
    }

    pub fn controller(&self) -> Option<&Workstream> {
        self.file
            .streams
            .iter()
            .find(|s| s.role == Role::Controller)
    }

    /// The workstreams `id` started.
    pub fn children<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Workstream> + 'a {
        self.file
            .streams
            .iter()
            .filter(move |s| s.parent.as_deref() == Some(id))
    }

    /// The workstream started for a task.
    pub fn stream_for_task(&self, key: &str) -> Option<&Workstream> {
        self.file
            .streams
            .iter()
            .find(|s| s.task.as_deref() == Some(key))
    }

    /// The task a workstream was started for, or for a pool worker the task
    /// it holds now.
    pub fn task_of(&self, stream: &Workstream) -> Option<&Task> {
        match stream.task.as_deref() {
            Some(key) => self.task(key),
            None if stream.role == Role::Worker => self.tasks.iter().find(|t| {
                t.status == TaskStatus::Running && t.claimed_by.as_deref() == Some(&stream.id)
            }),
            None => None,
        }
    }

    /// Whether a task waits for the user's go-ahead before its agent starts.
    pub fn awaits_approval(&self, task: &Task) -> bool {
        self.file.approve_leads
            && task.status == TaskStatus::Open
            && self
                .stream(&task.owner)
                .is_some_and(|o| o.role == Role::Controller)
            && !self.file.approved.contains(&task.key())
    }

    /// The controller's open tasks Atrium should start a lead for now:
    /// nobody works on them yet and the user said yes where needed.
    pub fn startable(&self) -> Vec<&Task> {
        self.tasks
            .iter()
            .filter(|task| {
                // A lead's tasks go to its pool (see `pool`), not to agents of their own.
                self.stream(&task.owner)
                    .is_some_and(|o| o.role == Role::Controller)
                    && task.status == TaskStatus::Open
                    && self.stream_for_task(&task.key()).is_none()
                    && !self.awaits_approval(task)
            })
            .collect()
    }

    /// Workstreams whose task has finished and whose parent has not been told.
    pub fn unreported(&self) -> Vec<(&Workstream, &Task)> {
        self.file
            .streams
            .iter()
            .filter(|s| !s.reported && s.parent.is_some())
            .filter_map(|s| Some((s, self.task_of(s)?)))
            .filter(|(_, task)| task.status.finished())
            .collect()
    }

    /// A new workstream for `task`: its role, parent, goal and scope.
    pub fn stream_from_task(&self, task: &Task, backend: &str) -> Option<Workstream> {
        let owner = self.stream(&task.owner)?;
        let role = owner.role.child()?;
        let scope = if task.scope.is_empty() && role == Role::Worker {
            owner.scope.clone()
        } else {
            task.scope.clone()
        };
        Some(Workstream {
            id: self.new_id(&match role {
                Role::Worker => format!("{}-{}", owner.id, task.id),
                _ => task.id.clone(),
            }),
            name: task.title.clone(),
            backend: backend.to_string(),
            thread_id: None,
            goal: task.body.clone(),
            scope,
            notices_seen: self.notices.len(),
            created_at: jiff::Timestamp::now().as_second(),
            role,
            parent: Some(owner.id.clone()),
            task: Some(task.key()),
            ..Workstream::default()
        })
    }

    /// Appends a notice and reads it back.
    pub fn post(&mut self, from: &str, to: Vec<String>, text: &str) -> Result<()> {
        let notice = ProjectNotice {
            at: now_label(),
            from: from.to_string(),
            to,
            text: text.trim().to_string(),
        };
        let path = self.dir().join("notices.md");
        let mut body = fs::read_to_string(&path).unwrap_or_default();
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(&notice.line());
        body.push('\n');
        fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
        self.stamp = None;
        self.reload();
        Ok(())
    }

    pub fn stream(&self, id: &str) -> Option<&Workstream> {
        self.file.streams.iter().find(|s| s.id == id)
    }

    pub fn stream_mut(&mut self, id: &str) -> Option<&mut Workstream> {
        self.file.streams.iter_mut().find(|s| s.id == id)
    }

    /// A new workstream id from its name, unique in the project.
    pub fn new_id(&self, name: &str) -> String {
        let base = match slug(name) {
            Some(base) if base != "all" && base != ATRIUM && base != YOU => base,
            _ => format!("stream-{}", self.file.streams.len() + 1),
        };
        let mut id = base.clone();
        let mut n = 2;
        while self.stream(&id).is_some() {
            id = format!("{base}-{n}");
            n += 1;
        }
        id
    }

    /// Notices `stream` has not been handed yet.
    pub fn unread(&self, stream: &str) -> Vec<&ProjectNotice> {
        let Some(own) = self.stream(stream) else {
            return Vec::new();
        };
        let worker = own.role == Role::Worker;
        self.notices
            .iter()
            .skip(own.notices_seen)
            .filter(|n| n.is_for(stream))
            .filter(|n| !worker || n.to.iter().any(|to| to == stream))
            .collect()
    }

    /// Marks every notice so far as handed to `stream`.
    pub fn mark_read(&mut self, stream: &str) {
        let count = self.notices.len();
        if let Some(s) = self.stream_mut(stream) {
            s.notices_seen = count;
        }
    }

    /// Who owns `path` (absolute, or relative to the root) when it is not
    /// `stream` itself: a scope crossing.
    pub fn crossing(&self, stream: &str, path: &str) -> Option<Crossing> {
        let relative = Path::new(path)
            .strip_prefix(&self.root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.trim_start_matches("./").to_string());
        let own = self.stream(stream)?;
        if own.scope.iter().any(|glob| glob_match(glob, &relative)) {
            return None;
        }
        let parent = own.parent.as_deref().and_then(|id| self.stream(id));
        // A worker inside its lead's scope is doing its lead's work.
        if own.role == Role::Worker
            && parent.is_some_and(|p| p.scope.iter().any(|glob| glob_match(glob, &relative)))
        {
            return None;
        }
        let owner = self.file.streams.iter().find(|s| {
            s.id != stream
                && Some(s.id.as_str()) != own.parent.as_deref()
                && s.parent.as_deref() != Some(stream)
                && s.scope.iter().any(|glob| glob_match(glob, &relative))
        })?;
        Some(Crossing {
            stream: stream.to_string(),
            owner: owner.id.clone(),
            path: relative,
        })
    }

    /// What a workstream is told when it starts.
    pub fn kickoff(&self, stream: &Workstream) -> String {
        let text = match stream.role {
            Role::Controller => self.controller_kickoff(stream),
            Role::Lead => self.lead_kickoff(stream),
            Role::Worker => self.worker_kickoff(stream),
        };
        let unattended = if self.file.autopilot.on {
            format!("\n\n{UNATTENDED}")
        } else {
            String::new()
        };
        format!("{text}{unattended}\n\n{TOOLS_HINT}")
    }

    /// The leads, one line each, leaving out `except`.
    fn lead_lines(&self, except: &str) -> String {
        let lines: Vec<String> = self
            .file
            .streams
            .iter()
            .filter(|s| s.role == Role::Lead && s.id != except)
            .map(|s| {
                format!(
                    "- {} (`{}`): {} — scope {}",
                    s.name,
                    s.id,
                    first_line(&s.goal),
                    scope_label(&s.scope)
                )
            })
            .collect();
        if lines.is_empty() {
            "None yet; others may join later.".to_string()
        } else {
            lines.join("\n")
        }
    }

    fn controller_kickoff(&self, stream: &Workstream) -> String {
        format!(
            "You are the controller of the project \"{project}\" (id `{id}`). You do not implement anything yourself: you split the project into a few leads, give each a roadmap, keep the plan, and step in only when a lead reports.\n\n\
             The project's goal:\n{goal}\n\n\
             The leads so far:\n{leads}\n\n\
             How to work:\n\
             1. Read the repository enough to plan. Write `{dir}/plan.md`: the goal, the milestones in order, which lead owns each, and the decisions that cut across leads. Put shared conventions in `{dir}/brief.md` and interfaces between leads in `{dir}/contracts/`.\n\
             2. Split the work into 2–5 leads with clear, non-overlapping scopes. Each lead is one long-running session that owns its part. For each, write `{dir}/tasks/{id}/<lead-id>.md`:\n\
             ```\n# <Lead name>\nstatus: open\nagent: <codex, claude, deepseek or antigravity; leave out to use yours>\nscope: <paths it owns, as globs, such as web/**, src/ui>\n\n<Its goal: what done looks like, and what it must not touch.>\n\n## Roadmap\n### <Phase 1 title>\n- [ ] <step small enough to check>\n- [ ] <step>\n\n### <Phase 2 title>\n- [ ] <step>\n```\n\
             The roadmap is that lead's share of `{dir}/plan.md`: 2–6 phases in order, each a few checkable steps. The user follows every lead's progress phase by phase, so name phases by outcome (\"Timeline editing\"), not by activity.\n\
             Atrium starts a lead for each file{approval} and hands it the roadmap; the lead ticks steps as it goes.\n\
             3. Then stop. You are woken when a lead posts you a notice (at a milestone, or blocked on a decision across leads) or finishes its task. When woken: check its work against the plan, update `{dir}/plan.md`, and post notices to re-align leads if needed. Keep these turns short; do not take over a lead's work.\n\n\
             To tell leads something, append one line to `{dir}/notices.md`: `- YYYY-MM-DD HH:MM · {id} → <lead id or all>: <message>`.",
            project = self.file.name,
            id = stream.id,
            goal = stream.goal.trim(),
            leads = self.lead_lines(&stream.id),
            dir = DIR,
            approval = if self.file.approve_leads {
                " once the user approves it"
            } else {
                ""
            },
        )
    }

    fn lead_kickoff(&self, stream: &Workstream) -> String {
        let task = self.task_of(stream);
        let roadmap = task
            .filter(|t| !t.roadmap.is_empty())
            .map(|t| {
                let steps: Vec<String> = t
                    .phases
                    .iter()
                    .map(|p| {
                        let mut text = format!("### {}", p.title);
                        for m in &p.steps {
                            text.push_str(&format!(
                                "\n- [{}] {}",
                                if m.done { "x" } else { " " },
                                m.text
                            ));
                        }
                        text
                    })
                    .collect();
                format!(
                    "Your roadmap from the controller, in `{path}` under Roadmap:\n{steps}\nWork through it phase by phase, in order. As soon as you finish a step, change its `- [ ]` to `- [x]` in that file: the user watches your progress there. When a phase is done, post the controller a notice. If the roadmap needs to change, tell the controller rather than editing the phases yourself.\n\n",
                    path = t.rel_path(),
                    steps = steps.join("\n")
                )
            })
            .unwrap_or_default();
        let controller = match self.controller() {
            Some(c) => format!(
                "The controller (`{}`) keeps the plan in `{DIR}/plan.md`. Post it a notice when you reach a milestone, when you are blocked on a decision that crosses leads, and when you are done; it is woken by your notices, so keep them short and concrete.{}\n\n",
                c.id,
                task.map_or(String::new(), |t| format!(
                    " When your whole goal is done, write a summary under `## Result` in `{}` and set its `status:` to `done`.",
                    t.rel_path()
                ))
            ),
            None => String::new(),
        };
        format!(
            "You are the \"{name}\" lead (id `{id}`) of the project \"{project}\": one of several long-lived sessions working on this repository side by side, each owning its part.\n\n\
             Your goal:\n{goal}\n\n\
             Your scope: {scope}. Change files outside it only when you must, and tell their owner with a notice.\n\n\
             {roadmap}\
             The other leads:\n{others}\n\n\
             {controller}\
             The atrium, `{dir}/` in the repository root, is how everyone stays aligned:\n\
             - `{dir}/brief.md`: shared conventions and decisions. Read it first and follow it.\n\
             - `{dir}/contracts/`: interfaces between leads. Read the ones you depend on; if you change one, update its file and post a notice.\n\
             - `{dir}/status/{id}.md`: your status. Keep it current with the headings Focus, Done, Next, Touching (files you are changing) and Questions.\n\
             - `{dir}/notices.md`: append-only. To tell others something, append one line: `- YYYY-MM-DD HH:MM · {id} → <id or all>: <message>`.\n\
             New notices for you are added to your next message, so there is no need to check for them.\n\n\
             You have a task pool and up to {max} sub-agents to work it. Break the current phase into self-contained, checkable tasks and add them all at once (`create_tasks`, or one file each as `{dir}/tasks/{id}/<task-id>.md` with a `# Title`, `status: open`, optionally `scope:` inside yours, `after: <ids>` for tasks that must be done first and `priority: high`, then what to do and how it will be checked). Sub-agents start on their own; each claims a task, finishes it and claims the next, without you. You are woken with their results when fewer than one task per sub-agent is left: review them, tick your roadmap, and add the next batch so they never run dry. Do the parts that need your judgement yourself.\n\n\
             If your part has a user interface, save a screenshot of it to `{dir}/previews/{id}.png` at the end of each phase (overwrite the last one): the user sees it beside your progress.\n\n\
             Read the brief and the contracts, write your status file, then work toward your goal on your own. Stop to ask only when a decision really needs me.",
            name = stream.name,
            id = stream.id,
            project = self.file.name,
            goal = stream.goal.trim(),
            scope = scope_label(&stream.scope),
            others = self.lead_lines(&stream.id),
            dir = DIR,
            max = self.file.max_workers.max(1),
        )
    }

    fn pool_worker_kickoff(&self, stream: &Workstream) -> String {
        let lead = stream.parent.as_deref().unwrap_or("your lead");
        let first = match self.task_of(stream) {
            Some(task) => format!(
                "Your first task, already claimed for you:\n\n{}",
                agent::task_brief(task)
            ),
            None => "Start by calling `claim_next`.".to_string(),
        };
        format!(
            "You are a pool sub-agent (id `{id}`) of the lead `{lead}` in the project \"{project}\". The lead keeps a pool of tasks in `{dir}/tasks/{lead}/`; you and other sub-agents work through it, one task at a time, without waiting for anyone.\n\n\
             Your scope: {scope}, unless a task names a narrower one. Do not change files outside it.\n\n\
             Read `{dir}/brief.md` and follow it. Do not edit the brief, the contracts, the plan or task files other than the one you hold, and do not start sub-agents.\n\n\
             The loop: do the task you hold, then `finish_task` with what you changed, how you checked it and anything left open (status `failed` with the reason if you cannot). `finish_task` claims your next task and gives it to you: carry straight on with it. Stop only when it says the pool is empty. Keep each task's work separate; do not rely on what you remember from earlier tasks without checking the files.\n\n\
             Without the tools: claim a task by setting `status: running` and adding `claimed_by: {id}` under its title, in the first file whose `status:` is `open`, which has no `claimed_by:` and whose `after:` tasks are all `done`; finish it by adding `## Result` and setting `status: done`.\n\n\
             {first}",
            id = stream.id,
            project = self.file.name,
            scope = scope_label(&stream.scope),
            dir = DIR,
        )
    }

    fn worker_kickoff(&self, stream: &Workstream) -> String {
        if stream.task.is_none() {
            return self.pool_worker_kickoff(stream);
        }
        let lead = stream
            .parent
            .as_deref()
            .and_then(|id| self.stream(id))
            .map_or_else(
                || "your lead".to_string(),
                |l| format!("the \"{}\" lead (`{}`)", l.name, l.id),
            );
        let path = self
            .task_of(stream)
            .map_or_else(|| format!("{DIR}/tasks/"), Task::rel_path);
        format!(
            "You are a sub-agent (id `{id}`) of {lead} in the project \"{project}\". You have one task; others work on the same repository at the same time.\n\n\
             Your task, from `{path}`:\n{goal}\n\n\
             Your scope: {scope}. Do not change files outside it.\n\n\
             Read `{dir}/brief.md` and follow it. Do not edit the brief, the contracts, the plan or other task files, and do not start sub-agents of your own; if something there needs to change, say so in your result.\n\n\
             When you are done, add a short report under `## Result` at the end of `{path}`: what you changed, how you checked it, and anything left open. Then change its `status:` line to `done`, or to `failed` with the reason in the result if you could not do it. Your lead is told as soon as you do.",
            id = stream.id,
            project = self.file.name,
            goal = stream.goal.trim(),
            scope = scope_label(&stream.scope),
            dir = DIR,
        )
    }
}

/// `text` with the notices `stream` has not seen in front of it.
pub fn with_notices(notices: &[&ProjectNotice], text: &str) -> String {
    if notices.is_empty() {
        return text.to_string();
    }
    let lines: Vec<String> = notices
        .iter()
        .map(|n| format!("- {} · from {}: {}", n.at, n.from, n.text))
        .collect();
    format!(
        "[Atrium] New notices for you:\n{}\n\n{}",
        lines.join("\n"),
        text
    )
}

pub fn scope_label(scope: &[String]) -> String {
    if scope.is_empty() {
        "the whole repository".into()
    } else {
        scope
            .iter()
            .map(|s| format!("`{s}`"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// `name` lowercased with runs of other characters turned into one `-`.
fn slug(name: &str) -> Option<String> {
    let slug = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    (!slug.is_empty()).then_some(slug)
}

/// Every `tasks/<owner>/<id>.md`.
fn read_tasks(dir: &Path) -> Vec<Task> {
    let mut tasks = Vec::new();
    let Ok(owners) = fs::read_dir(dir) else {
        return tasks;
    };
    for owner in owners.flatten().filter(|e| e.path().is_dir()) {
        let owner_id = owner.file_name().to_string_lossy().into_owned();
        for (name, text) in read_dir_texts(&owner.path()) {
            let id = name.trim_end_matches(".md");
            tasks.push(Task::parse(&owner_id, id, &text));
        }
    }
    tasks.sort_by_key(Task::key);
    tasks
}

fn now_label() -> String {
    let now = jiff::Zoned::now();
    now.strftime("%Y-%m-%d %H:%M").to_string()
}

fn read_dir_texts(dir: &Path) -> Vec<(String, String)> {
    let mut texts: Vec<(String, String)> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
                .filter_map(|e| {
                    let text = fs::read_to_string(e.path()).ok()?;
                    Some((e.file_name().to_string_lossy().into_owned(), text))
                })
                .collect()
        })
        .unwrap_or_default();
    texts.sort();
    texts
}

/// Reads notice lines, leniently: agents write them by hand. Indented lines
/// continue the notice above.
pub fn parse_notices(text: &str) -> Vec<ProjectNotice> {
    let mut notices: Vec<ProjectNotice> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            if let Some(notice) = parse_notice(rest) {
                notices.push(notice);
            }
        } else if (line.starts_with("  ") || line.starts_with('\t'))
            && let Some(last) = notices.last_mut()
        {
            last.text.push('\n');
            last.text.push_str(line.trim());
        }
    }
    notices
}

fn parse_notice(line: &str) -> Option<ProjectNotice> {
    let (at, rest) = line.split_once('·')?;
    let (route, text) = rest.split_once(':')?;
    let (from, to) = route.split_once('→').or_else(|| route.split_once("->"))?;
    let to: Vec<String> = to
        .split(',')
        .map(|t| t.trim().trim_matches('`').to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    let from = from.trim().trim_matches('`').to_lowercase();
    if from.is_empty() || to.is_empty() {
        return None;
    }
    Some(ProjectNotice {
        at: at.trim().to_string(),
        from,
        to,
        text: text.trim().to_string(),
    })
}

/// Matches a path against a glob: `*` within a segment, `**` across them,
/// `?` for one character. A pattern without wildcards also matches anything
/// under it, so `web` means `web/**`.
pub fn glob_match(glob: &str, path: &str) -> bool {
    let glob = glob.trim().trim_start_matches("./").trim_end_matches('/');
    let path = path.trim_start_matches("./");
    if glob.is_empty() {
        return false;
    }
    if !glob.contains(['*', '?']) {
        return path == glob || path.starts_with(&format!("{glob}/"));
    }
    let glob: Vec<&str> = glob.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    match_segments(&glob, &path)
}

fn match_segments(glob: &[&str], path: &[&str]) -> bool {
    match glob.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| match_segments(rest, &path[skip..])),
        Some((first, rest)) => match path.split_first() {
            Some((segment, tail)) => match_one(first, segment) && match_segments(rest, tail),
            None => false,
        },
    }
}

fn match_one(glob: &str, text: &str) -> bool {
    let (g, t): (Vec<char>, Vec<char>) = (glob.chars().collect(), text.chars().collect());
    fn go(g: &[char], t: &[char]) -> bool {
        match g.split_first() {
            None => t.is_empty(),
            Some(('*', rest)) => (0..=t.len()).any(|skip| go(rest, &t[skip..])),
            Some(('?', rest)) => !t.is_empty() && go(rest, &t[1..]),
            Some((c, rest)) => t.first() == Some(c) && go(rest, &t[1..]),
        }
    }
    go(&g, &t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("web/**", "web/src/app.tsx"));
        assert!(glob_match("web", "web/src/app.tsx"));
        assert!(!glob_match("web", "website/index.html"));
        assert!(glob_match("server/**/*.rs", "server/api/render.rs"));
        assert!(glob_match("server/**/*.rs", "server/main.rs"));
        assert!(!glob_match("server/**/*.rs", "server/README.md"));
        assert!(glob_match("*.md", "README.md"));
        assert!(!glob_match("*.md", "docs/README.md"));
    }

    #[test]
    fn notices_parse_leniently() {
        let text = "# Notices\n\n\
            - 2026-11-18 10:42 · backend → frontend: `/render` now returns `progress`\n  \
              (0 to 1, polled every second)\n\
            - 2026-11-18 11:00 · you -> all: use zod for validation\n\
            - just a note\n\
            * 2026-11-18 11:05 · Frontend → backend, core: thanks\n";
        let notices = parse_notices(text);
        assert_eq!(notices.len(), 3);
        assert_eq!(notices[0].from, "backend");
        assert_eq!(notices[0].to, vec!["frontend"]);
        assert!(notices[0].text.ends_with("polled every second)"));
        assert_eq!(notices[1].to, vec!["all"]);
        assert_eq!(notices[2].to, vec!["backend", "core"]);
        assert!(notices[0].is_for("frontend") && !notices[0].is_for("backend"));
        assert!(notices[1].is_for("backend"));
    }

    fn stream(id: &str, scope: &[&str]) -> Workstream {
        Workstream {
            id: id.into(),
            name: id.into(),
            backend: "codex".into(),
            goal: format!("Build the {id}"),
            scope: scope.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_project_round_trips_and_tracks_notices_and_crossings() {
        let root = std::env::temp_dir().join(format!("atrium-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let mut project = Project::open(&root).unwrap();
        assert!(root.join(".atrium/brief.md").exists());
        project.file.streams.push(stream("frontend", &["web/**"]));
        project.file.streams.push(stream("backend", &["server/**"]));
        project.save().unwrap();
        assert_eq!(project.new_id("Frontend"), "frontend-2");
        assert_eq!(project.new_id("后端 优化"), "后端-优化");

        project
            .post(
                "backend",
                vec!["frontend".into()],
                "render returns progress",
            )
            .unwrap();
        project.post(YOU, vec!["all".into()], "use zod").unwrap();
        assert_eq!(project.unread("frontend").len(), 2);
        assert_eq!(project.unread("backend").len(), 1);
        project.mark_read("frontend");
        assert!(project.unread("frontend").is_empty());

        let crossing = project
            .crossing(
                "frontend",
                &root.join("server/api.rs").display().to_string(),
            )
            .unwrap();
        assert_eq!(crossing.owner, "backend");
        assert_eq!(crossing.path, "server/api.rs");
        assert!(project.crossing("frontend", "web/app.tsx").is_none());
        assert!(project.crossing("frontend", "README.md").is_none());

        let kickoff = project.kickoff(project.stream("frontend").unwrap());
        assert!(kickoff.contains("Build the frontend") && kickoff.contains("`backend`"));

        let reopened = Project::open(&root).unwrap();
        assert_eq!(reopened.file.streams.len(), 2);
        assert_eq!(
            reopened.file.streams[0].notices_seen, 0,
            "marks are saved by the caller"
        );
        assert_eq!(reopened.notices.len(), 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn task_files_parse_leniently_and_take_a_new_status() {
        let text = "\n# Editor timeline\nStatus: In progress\nagent: Claude\nscope: web/editor/**, `web/ui`\n\nBuild the timeline.\nKeep it fast.\n\n## Roadmap\n- [x] Track model\n- [ ] Drag and drop\n* Snap to grid\n\n## Result\nDone the model.\n";
        let task = Task::parse("frontend", "timeline", text);
        assert_eq!(task.title, "Editor timeline");
        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(task.agent.as_deref(), Some("claude"));
        assert_eq!(task.scope, vec!["web/editor/**", "web/ui"]);
        assert_eq!(task.body, "Build the timeline.\nKeep it fast.");
        assert_eq!(task.progress(), Some((1, 3)));
        assert_eq!(task.result.as_deref(), Some("Done the model."));
        assert_eq!(task.key(), "frontend/timeline");

        let done = Task::parse("f", "t", &with_status(text, TaskStatus::Done));
        assert_eq!(done.status, TaskStatus::Done);
        assert_eq!(done.body, task.body);
        let bare = with_status("# Only a title\n\nDo it.", TaskStatus::Running);
        assert_eq!(Task::parse("f", "t", &bare).status, TaskStatus::Running);
        let none = Task::parse("f", "t", "Just words, no title.");
        assert_eq!((none.title.as_str(), none.status), ("t", TaskStatus::Open));
        assert_eq!(none.body, "Just words, no title.");

        let written = Task::render(
            "API",
            "codex",
            &["server/**".into()],
            "Build it.",
            &["Routes".into()],
        );
        let back = Task::parse("lead", "api", &written);
        assert_eq!(back.agent.as_deref(), Some("codex"));
        assert_eq!(
            back.roadmap,
            vec![Milestone {
                text: "Routes".into(),
                done: false
            }]
        );
    }

    #[test]
    fn tasks_start_agents_one_level_down_and_report_back() {
        let root = std::env::temp_dir().join(format!("atrium-tasks-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let mut project = Project::open(&root).unwrap();
        project.file.max_workers = 1;
        project.file.approve_leads = true;
        let mut boss = stream("boss", &[]);
        boss.role = Role::Controller;
        project.file.streams.push(boss);

        // The controller asks for a lead: it waits for the user.
        let lead_text = Task::render(
            "Web",
            "",
            &["web/**".into()],
            "Own the web app.",
            &["Shell".into(), "Editor".into()],
        );
        let key = project.add_task("boss", "Web", &lead_text).unwrap();
        assert_eq!(key, "boss/web");
        assert!(project.awaits_approval(project.task(&key).unwrap()));
        assert!(project.startable().is_empty());
        project.file.approved.push(key.clone());
        let task = project.startable()[0].clone();
        let lead = project.stream_from_task(&task, "claude").unwrap();
        assert_eq!(
            (lead.role, lead.parent.as_deref()),
            (Role::Lead, Some("boss"))
        );
        assert_eq!(lead.scope, vec!["web/**"]);
        project.file.streams.push(lead.clone());
        project.set_task_status(&key, TaskStatus::Running).unwrap();
        assert!(project.startable().is_empty());
        let kickoff = project.kickoff(&lead);
        assert!(kickoff.contains("- [ ] Shell") && kickoff.contains("`boss`"));
        assert_eq!(
            project.task(&key).unwrap().phases.len(),
            2,
            "steps without headings are phases"
        );
        assert!(
            project
                .kickoff(project.stream("boss").unwrap())
                .contains("plan.md")
        );

        // The lead's tasks go to its pool: no agent of their own.
        project
            .add_task(
                "web",
                "Shell",
                "# Shell\nstatus: open\n\nLay out the shell.",
            )
            .unwrap();
        project
            .add_task("web", "Theme", "# Theme\nscope: web/theme/**\n\nColours.")
            .unwrap();
        assert!(project.startable().is_empty());
        assert_eq!(project.workers_needed("web"), 1, "max_workers is 1 here");
        let worker = project.pool_worker("web", "codex").unwrap();
        assert_eq!(worker.id, "web-w1");
        assert_eq!(
            worker.scope,
            vec!["web/**"],
            "a pool worker gets its lead's scope"
        );
        assert!(project.kickoff(&worker).contains("claim_next"));
        project.file.streams.push(worker);
        project.save().unwrap();
        assert_eq!(project.claim_next("web-w1").unwrap().unwrap().id, "shell");

        // Its changes inside the lead's scope are no crossing.
        project.file.streams.push(stream("server", &["server/**"]));
        assert!(project.crossing("web-w1", "web/app.tsx").is_none());
        assert_eq!(
            project.crossing("web-w1", "server/a.rs").unwrap().owner,
            "server"
        );
        assert!(project.crossing("web", "web/app.tsx").is_none());

        // Workers do not get notices for all.
        project.post(YOU, vec!["all".into()], "freeze").unwrap();
        assert!(project.unread("web-w1").is_empty());
        assert_eq!(project.unread("web").len(), 1);

        // Pool tasks are reported in batches (see `pool`), not one by one.
        project
            .finish_task("web-w1", true, "Shell laid out.")
            .unwrap();
        assert!(project.unreported().is_empty());
        assert_eq!(
            project.task("web/theme").unwrap().claimed_by.as_deref(),
            Some("web-w1")
        );

        // The lead finishing is reported to the controller once.
        project.set_task_status(&key, TaskStatus::Done).unwrap();
        let unreported: Vec<String> = project
            .unreported()
            .iter()
            .map(|(s, _)| s.id.clone())
            .collect();
        assert_eq!(unreported, vec!["web"]);
        project.stream_mut("web").unwrap().reported = true;
        assert!(project.unreported().is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn worktrees_are_per_workstream_except_the_controller() {
        let mut project = Project {
            root: PathBuf::from("/r"),
            ..Default::default()
        };
        let mut boss = stream("boss", &[]);
        boss.role = Role::Controller;
        project.file.streams.push(boss);
        project.file.streams.push(stream("web", &["web/**"]));
        assert_eq!(project.work_dir("web"), PathBuf::from("/r"));
        project.file.worktrees = true;
        assert_eq!(
            project.work_dir("web"),
            PathBuf::from("/r/.atrium/worktrees/web")
        );
        assert_eq!(project.work_dir("boss"), PathBuf::from("/r"));
        assert_eq!(Project::branch_of("web"), "atrium/web");
    }

    #[test]
    fn roadmaps_split_into_phases() {
        let text = "# Web\n\nOwn it.\n\n## Roadmap\n- [x] Loose step\n### Shell\n- [x] Layout\n- [x] Routing\n\n### Timeline\n- [x] Model\n- [ ] Drag\n### Empty\n### Export\n- [ ] MP4\n";
        let task = Task::parse("boss", "web", text);
        let titles: Vec<&str> = task.phases.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, vec!["Loose step", "Shell", "Timeline", "Export"]);
        assert_eq!(task.current_phase(), Some(2));
        assert_eq!(task.phases[2].progress(), (1, 2));
        assert!(task.phases[1].done() && !task.phases[3].done());
        assert_eq!(task.progress(), Some((4, 6)));
        assert_eq!(task.body, "Own it.");
    }
}
