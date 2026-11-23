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
pub mod devices;
mod git;
pub mod mcp;
pub mod notice_delivery;
pub mod pool;
pub mod pr;
pub mod preview;
pub mod repo;
pub mod scheduler;
pub mod skill;
pub mod spread;
pub mod worktrees;

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use notice_delivery::{
    Commit, Delivery, NoticeOutcome, NoticePreparation, NoticeResolution, NoticeTicket,
    Phase as DeliveryPhase,
};
use sha2::{Digest, Sha256};

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
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    notice_deliveries: HashMap<String, Delivery>,
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
            notice_deliveries: HashMap::new(),
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

// Replace only after a complete, synced write. A failed write/rename leaves
// the previous document intact; temporary files are not part of the format.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    atomic_write_at(path, bytes, &tmp)
}

fn atomic_write_at(path: &Path, bytes: &[u8], tmp: &Path) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // Failure here returns before cleanup: a collision belongs to someone else.
    let mut file = options
        .open(tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    let result = (|| -> Result<()> {
        if let Ok(metadata) = fs::metadata(path) {
            file.set_permissions(metadata.permissions())?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(tmp, path)?;
        #[cfg(unix)]
        fs::File::open(path.parent().expect("document has a parent"))?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result.with_context(|| format!("writing {}", path.display()))
}

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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => ProjectFile {
                name: root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Project".into()),
                ..ProjectFile::default()
            },
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("reading {}", dir.join("project.json").display()));
            }
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
        atomic_write(&path, text.as_bytes())
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
        let task = self
            .task(key)
            .ok_or_else(|| anyhow::anyhow!("task {key} is missing or unreadable"))?;
        let path = self.root.join(task.rel_path());
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        atomic_write(&path, with_status(&text, status).as_bytes())?;
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
        self.startable_with(|_| true)
    }

    /// Like [`Self::startable`], with a caller-supplied eligibility check.
    pub fn startable_with(&self, eligible: impl Fn(&Task) -> bool) -> Vec<&Task> {
        self.tasks
            .iter()
            .filter(|task| {
                // A lead's tasks go to its pool (see `pool`), not to agents of their own.
                self.stream(&task.owner)
                    .is_some_and(|o| o.role == Role::Controller)
                    && task.status == TaskStatus::Open
                    && self.stream_for_task(&task.key()).is_none()
                    && !self.awaits_approval(task)
                    && eligible(task)
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
        let task_key = task.key();
        let failed_ids: HashSet<_> = self
            .notices
            .iter()
            .filter_map(|notice| startup_failure(notice, &owner.id))
            .filter_map(|(key, (id, _))| (key == task_key).then_some(id))
            .collect();
        let name = match role {
            Role::Worker => format!("{}-{}", owner.id, task.id),
            _ => task.id.clone(),
        };
        Some(Workstream {
            id: self.new_id_avoiding(&name, |id| failed_ids.contains(id)),
            name: task.title.clone(),
            backend: backend.to_string(),
            thread_id: None,
            goal: task.body.clone(),
            scope,
            notices_seen: self.notices.len(),
            created_at: jiff::Timestamp::now().as_second(),
            role,
            parent: Some(owner.id.clone()),
            task: Some(task_key),
            ..Workstream::default()
        })
    }

    /// Persist a completion receipt. The deterministic notice is checked before
    /// appending, so a crash between the notice write and reported flag is safe.
    pub fn report_finished(&mut self, id: &str) -> Result<Option<String>> {
        let Some(stream) = self.stream(id).cloned() else {
            return Ok(None);
        };
        if stream.reported {
            return Ok(None);
        }
        let Some(parent) = stream.parent.clone() else {
            return Ok(None);
        };
        if !self
            .notices
            .iter()
            .any(|notice| is_completion_receipt(notice, &stream))
        {
            let Some(task) = self.task_of(&stream) else {
                return Ok(None);
            };
            if !task.status.finished() {
                return Ok(None);
            }
            let text = format!(
                "{} {}",
                completion_text(&stream, task),
                completion_marker(&stream).expect("task and parent exist")
            );
            self.post(ATRIUM, vec![parent.clone()], &text)?;
        }
        self.stream_mut(id)
            .expect("stream remains present")
            .reported = true;
        if let Err(e) = self.save() {
            self.stream_mut(id)
                .expect("stream remains present")
                .reported = false;
            return Err(e);
        }
        Ok(Some(parent))
    }

    /// Bind before kickoff. On failure callers must not send any work.
    pub fn bind_thread(&mut self, stream: &str, thread: &str) -> Result<()> {
        let target = self
            .stream_mut(stream)
            .ok_or_else(|| anyhow::anyhow!("workstream was removed before startup completed"))?;
        let previous = target.thread_id.replace(thread.to_string());
        if let Err(e) = self.save() {
            self.stream_mut(stream).unwrap().thread_id = previous;
            return Err(e);
        }
        Ok(())
    }

    /// Preserve evidence until both task failure and its notice have persisted.
    pub fn fail_start(&mut self, id: &str, reason: &str) -> Result<Option<String>> {
        let Some(stream) = self.stream(id).cloned() else {
            return Ok(None);
        };
        if stream.thread_id.is_some() {
            return Ok(None);
        }
        if stream.role == Role::Worker
            && stream.task.is_none()
            && let Some(parent) = &stream.parent
        {
            let held = self.task_of(&stream).map(Task::key);
            if let Some(task) = &held {
                self.record_start_failure(parent, task, (&stream.id, stream.created_at), reason)?;
            }
            self.retire_pool_worker(id, held.as_deref())?;
            return Ok(held.map(|_| parent.clone()));
        }
        if let (Some(task), Some(parent)) = (&stream.task, &stream.parent) {
            self.record_start_failure(parent, task, (&stream.id, stream.created_at), reason)?;
            if !self.task(task).is_some_and(|task| task.status.finished()) {
                self.set_task_status(task, TaskStatus::Failed)?;
            }
        }
        let previous = self.file.streams.clone();
        self.file.streams.retain(|s| s.id != id);
        if let Err(e) = self.save() {
            self.file.streams = previous;
            return Err(e);
        }
        Ok(stream.parent)
    }

    fn record_start_failure(
        &mut self,
        parent: &str,
        task: &str,
        claim: (&str, i64),
        reason: &str,
    ) -> Result<()> {
        let marker = start_failure_marker(parent, task, claim);
        let prefix = format!("Could not start an agent for {task}: ");
        if !self.notices.iter().any(|notice| {
            notice.from == ATRIUM
                && notice.to == [parent]
                && notice.text.starts_with(&prefix)
                && notice.text.ends_with(&format!(" {marker}"))
        }) {
            self.post(
                ATRIUM,
                vec![parent.to_owned()],
                &format!("{prefix}{reason} {marker}"),
            )?;
        }
        Ok(())
    }

    /// Recover interrupted launches of task-bound workstreams. Pool claims
    /// have their own release and resume lifecycle. A failed launch is
    /// reported to its owner for review before retry.
    pub fn recover_unbound_tasks(&mut self) -> Result<Vec<String>> {
        // A durable receipt may precede the reported flag and outlive its task
        // file. Reconcile it before scanning tasks, otherwise archival hides it.
        let receipts: Vec<_> = self
            .file
            .streams
            .iter()
            .filter(|s| !s.reported && self.notices.iter().any(|n| is_completion_receipt(n, s)))
            .map(|s| s.id.clone())
            .collect();
        let mut parents = Vec::new();
        for id in receipts {
            if let Some(parent) = self.report_finished(&id)? {
                parents.push(parent);
            }
        }

        let tasks: Vec<_> = self
            .tasks
            .iter()
            .filter(|task| {
                task.status == TaskStatus::Running
                    && task.claimed_by.is_none()
                    && self.stream_for_task(&task.key()).is_none()
                    && self
                        .stream(&task.owner)
                        .is_some_and(|s| s.role.child().is_some())
            })
            .cloned()
            .collect();
        let unstarted: Vec<_> = self
            .file
            .streams
            .iter()
            .filter(|s| {
                s.thread_id.is_none()
                    && s.task
                        .as_deref()
                        .and_then(|key| self.task(key))
                        .is_some_and(|t| {
                            t.status == TaskStatus::Running
                                || (t.status == TaskStatus::Failed
                                    && s.parent.as_ref().is_some_and(|parent| {
                                        self.notices.iter().any(|notice| {
                                            startup_failure(notice, parent).is_some_and(
                                                |(task, stream)| {
                                                    s.task.as_ref() == Some(&task)
                                                        && stream == (s.id.clone(), s.created_at)
                                                },
                                            )
                                        })
                                    }))
                        })
            })
            .map(|s| s.id.clone())
            .collect();
        for id in unstarted {
            if let Some(parent) = self.fail_start(
                &id,
                "interrupted before session binding; review before retrying",
            )? {
                parents.push(parent);
            }
        }
        for task in tasks {
            let text = format!(
                "Could not start an agent for {}: interrupted before the workstream was persisted. Review and reopen the task to retry.",
                task.key()
            );
            // Post first: replay is deduplicated if interrupted before Failed.
            if !self
                .notices
                .iter()
                .any(|n| n.from == ATRIUM && n.text == text)
            {
                self.post(ATRIUM, vec![task.owner.clone()], &text)?;
            }
            self.set_task_status(&task.key(), TaskStatus::Failed)?;
            parents.push(task.owner);
        }
        Ok(parents)
    }

    /// A saved receipt may outlive the in-memory wake queue after restart.
    /// Re-enqueue only recognized scheduler receipts not yet handed over.
    pub fn pending_report_parents(&self) -> Vec<String> {
        let mut parents: Vec<_> = self
            .file
            .streams
            .iter()
            .filter(|s| s.reported)
            .filter_map(|stream| {
                let parent = stream.parent.as_ref()?;
                self.unread(parent)
                    .iter()
                    .any(|notice| is_completion_receipt(notice, stream))
                    .then(|| parent.clone())
            })
            .collect();
        for parent in self
            .file
            .streams
            .iter()
            .filter(|s| s.role.child().is_some())
        {
            if !parents.contains(&parent.id)
                && self.unread(&parent.id).iter().any(|notice| {
                    startup_failure(notice, &parent.id).is_some_and(|(task, _)| {
                        self.task(&task)
                            .is_none_or(|task| task.status == TaskStatus::Failed)
                    })
                })
            {
                parents.push(parent.id.clone());
            }
        }
        parents
    }

    /// Appends a notice and reads it back.
    pub fn post(&mut self, from: &str, to: Vec<String>, text: &str) -> Result<()> {
        self.post_at(from, to, text, now_label())
    }

    fn post_at(&mut self, from: &str, to: Vec<String>, text: &str, at: String) -> Result<()> {
        let result = Self::append_notice_at(&self.root, from, to, text, at);
        self.stamp = None;
        self.reload();
        result
    }

    /// Append without touching the in-memory project, for background writers.
    pub fn append_notice(root: &Path, from: &str, to: Vec<String>, text: &str) -> Result<()> {
        Self::append_notice_at(root, from, to, text, now_label())
    }

    fn append_notice_at(
        root: &Path,
        from: &str,
        to: Vec<String>,
        text: &str,
        at: String,
    ) -> Result<()> {
        let notice = ProjectNotice {
            at,
            from: from.to_string(),
            to,
            text: text.trim().to_string(),
        };
        let path = root.join(DIR).join("notices.md");
        let body =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let separator = if !body.is_empty() && !body.ends_with('\n') {
            "\n"
        } else {
            ""
        };
        let line = format!("{separator}{}\n", notice.line());
        // Append rather than replacing other agents' concurrent notices.
        let mut file = fs::OpenOptions::new().append(true).open(&path)?;
        file.write_all(line.as_bytes())
            .and_then(|()| file.sync_all())
            .with_context(|| format!("appending {}", path.display()))
    }

    pub fn stream(&self, id: &str) -> Option<&Workstream> {
        self.file.streams.iter().find(|s| s.id == id)
    }

    pub fn stream_mut(&mut self, id: &str) -> Option<&mut Workstream> {
        self.file.streams.iter_mut().find(|s| s.id == id)
    }

    /// A new workstream id from its name, unique in the project.
    pub fn new_id(&self, name: &str) -> String {
        self.new_id_avoiding(name, |_| false)
    }

    fn new_id_avoiding(&self, name: &str, unavailable: impl Fn(&str) -> bool) -> String {
        let base = match slug(name) {
            Some(base) if base != "all" && base != ATRIUM && base != YOU => base,
            _ => format!("stream-{}", self.file.streams.len() + 1),
        };
        let mut id = base.clone();
        let mut n = 2;
        while self.stream(&id).is_some() || unavailable(&id) {
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

    fn notice_prefix(notices: &[ProjectNotice]) -> [u8; 32] {
        let mut hash = Sha256::new();
        for n in notices {
            let record = serde_json::to_vec(&(&n.at, &n.from, &n.to, &n.text))
                .expect("notice strings serialize");
            hash.update((record.len() as u64).to_be_bytes());
            hash.update(record);
        }
        hash.finalize().into()
    }

    fn notices_from_disk(&self) -> Result<Vec<ProjectNotice>> {
        let path = self.dir().join("notices.md");
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        Ok(parse_notices(&text))
    }

    fn persist_delivery_file(&mut self, next: ProjectFile) -> Result<()> {
        self.persist_delivery_file_with(next, atomic_write)
    }

    fn persist_delivery_file_with(
        &mut self,
        next: ProjectFile,
        write: impl FnOnce(&Path, &[u8]) -> Result<()>,
    ) -> Result<()> {
        let text = serde_json::to_string_pretty(&next)?;
        write(&self.dir().join("project.json"), text.as_bytes())?;
        self.file = next;
        Ok(())
    }

    fn delivery_prefix_matches(&self, ticket: &NoticeTicket) -> bool {
        let Some(stream) = self.stream(&ticket.stream) else {
            return false;
        };
        if !ticket.is_for(stream)
            || stream.notices_seen != ticket.from
            || ticket.from >= ticket.through
        {
            return false;
        }
        self.notices_from_disk().is_ok_and(|notices| {
            notices
                .get(..ticket.through)
                .is_some_and(|n| Self::notice_prefix(n) == ticket.prefix_sha256)
        })
    }

    /// Persist before dispatch. The caller sends no auto wake if this fails.
    pub fn prepare_notice_delivery(
        &mut self,
        stream: &str,
        thread: &str,
    ) -> Result<NoticePreparation> {
        if self.file.notice_deliveries.contains_key(stream) {
            return Ok(NoticePreparation::Blocked);
        }
        let Some(target) = self
            .stream(stream)
            .filter(|s| s.thread_id.as_deref() == Some(thread))
        else {
            return Ok(NoticePreparation::Blocked);
        };
        let notices = self.notices_from_disk()?;
        let unread = notices
            .get(target.notices_seen..)
            .context("notice journal no longer contains the saved cursor")?
            .iter()
            .filter(|n| n.is_for(stream))
            .filter(|n| target.role != Role::Worker || n.to.iter().any(|to| to == stream))
            .collect::<Vec<_>>();
        if unread.is_empty() {
            return Ok(NoticePreparation::Empty);
        }
        let ticket =
            NoticeTicket::new(target, thread, notices.len(), Self::notice_prefix(&notices));
        let text = with_notices(&unread, "");
        let mut next = self.file.clone();
        next.notice_deliveries
            .insert(stream.to_owned(), Delivery::reserved(ticket.clone()));
        self.persist_delivery_file(next)?;
        Ok(NoticePreparation::Ready { ticket, text })
    }

    /// True means the exact callback was processed; an accepted RPC whose
    /// captured prefix no longer matches becomes an unresolved hold instead.
    pub fn settle_notice_delivery(
        &mut self,
        ticket: &NoticeTicket,
        outcome: NoticeOutcome,
    ) -> Result<bool> {
        let Some(delivery) = self.file.notice_deliveries.get(&ticket.stream) else {
            return Ok(false);
        };
        if &delivery.ticket != ticket
            || !self
                .stream(&ticket.stream)
                .is_some_and(|s| ticket.is_for(s))
        {
            return Ok(false);
        }
        let Some(action) = delivery.settle(
            outcome,
            outcome == NoticeOutcome::Accepted && self.delivery_prefix_matches(ticket),
        ) else {
            return Ok(false);
        };
        self.commit_notice_delivery(&ticket.stream, action)?;
        Ok(true)
    }

    fn commit_notice_delivery(&mut self, stream: &str, action: Commit) -> Result<()> {
        let mut next = self.file.clone();
        match action {
            Commit::Advance(through) => {
                next.streams
                    .iter_mut()
                    .find(|s| s.id == stream)
                    .context("notice stream no longer exists")?
                    .notices_seen = through;
                next.notice_deliveries.remove(stream);
            }
            Commit::Release => {
                next.notice_deliveries.remove(stream);
            }
            Commit::Hold => {
                next.notice_deliveries
                    .get_mut(stream)
                    .context("notice delivery no longer exists")?
                    .phase = DeliveryPhase::Uncertain;
            }
        }
        self.persist_delivery_file(next)
    }

    /// Run once when opening projects in a fresh Workspace, never on reload.
    pub fn recover_notice_deliveries(&mut self) -> Result<()> {
        if self
            .file
            .notice_deliveries
            .values()
            .any(|d| d.phase == DeliveryPhase::Reserved)
        {
            let mut next = self.file.clone();
            for delivery in next.notice_deliveries.values_mut() {
                delivery.phase = DeliveryPhase::Uncertain;
            }
            self.persist_delivery_file(next)?;
        }
        Ok(())
    }

    /// Requires explicit user choice. Reserved live attempts cannot be reset.
    pub fn resolve_notice_delivery(
        &mut self,
        stream: &str,
        resolution: NoticeResolution,
    ) -> Result<bool> {
        let Some(delivery) = self.file.notice_deliveries.get(stream) else {
            return Ok(false);
        };
        let matches = resolution == NoticeResolution::MarkReceived
            && self.delivery_prefix_matches(&delivery.ticket);
        let Some(action) = delivery.resolve(resolution, matches) else {
            return Ok(false);
        };
        self.commit_notice_delivery(stream, action)?;
        Ok(true)
    }

    pub fn notice_delivery_blocked(&self, stream: &str) -> bool {
        self.file.notice_deliveries.contains_key(stream)
    }

    pub fn notice_delivery_uncertain(&self, stream: &str) -> bool {
        self.file
            .notice_deliveries
            .get(stream)
            .is_some_and(|d| d.phase == DeliveryPhase::Uncertain)
    }

    /// Marks every notice so far as handed to `stream`.
    pub fn mark_read(&mut self, stream: &str) {
        if self.file.notice_deliveries.contains_key(stream) {
            return;
        }
        let count = self.notices.len();
        if let Some(s) = self.stream_mut(stream) {
            s.notices_seen = count;
        }
    }

    /// Who owns `path` (absolute, or relative to the root) when it is not
    /// `stream` itself: a scope crossing.
    pub fn crossing(&self, stream: &str, path: &str) -> Option<Crossing> {
        let relative = Path::new(path)
            .strip_prefix(self.session_checkout(stream))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.trim_start_matches("./").to_string());
        #[cfg(windows)]
        let relative = relative.replace('\\', "/");
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
             Without the tools: work only on the task Atrium claimed for you. Finish it by adding `## Result` and setting `status: done` (or `failed` with the reason), keeping `claimed_by: {id}`. End your turn; Atrium assigns the next task.\n\n\
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

pub fn controller_notice(notice: &ProjectNotice, controller: &str) -> bool {
    notice.from != controller && notice.to.iter().any(|to| to == controller)
}

pub fn wake_allowed(
    last: Option<Instant>,
    now: Instant,
    needs_user: bool,
    running: bool,
    codex: bool,
) -> bool {
    !needs_user
        && (!running || codex)
        && last.is_none_or(|last| now.duration_since(last) >= Duration::from_secs(30))
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

// One workstream owns one task lifecycle. Do not include mutable
// display name, task status or Result text in its persisted receipt identity.
// This is single-writer crash/retry deduplication, not cross-process locking.
fn completion_marker(stream: &Workstream) -> Option<String> {
    let identity = serde_json::to_string(&(
        &stream.id,
        stream.created_at,
        stream.task.as_ref()?,
        stream.parent.as_ref()?,
    ))
    .expect("receipt identity contains only JSON primitives");
    Some(format!("[atrium-completion:{identity}]"))
}

fn is_completion_receipt(notice: &ProjectNotice, stream: &Workstream) -> bool {
    let Some(parent) = stream.parent.as_ref() else {
        return false;
    };
    if notice.from != ATRIUM || notice.to != [parent.clone()] {
        return false;
    }
    let Some(marker) = completion_marker(stream) else {
        return false;
    };
    if notice.text.ends_with(&format!(" {marker}")) {
        return true;
    }
    // Marker-like text inside a legacy Result is ordinary prose. A different
    // new marker cannot pass the legacy matcher because it follows its suffix.
    // Compatibility with receipts written before identity markers. Require
    // the exact old sender, recipient, role, name, task path and trailing
    // sentence; never treat an arbitrary mention of a task as its receipt.
    let path = format!("{DIR}/tasks/{}.md", stream.task.as_ref().unwrap());
    notice
        .text
        .ends_with(". Read its Result and review its changes.")
        && ["finished", "could not finish"].iter().any(|verb| {
            notice.text.starts_with(&format!(
                "{} \"{}\" {verb} {path}: ",
                stream.role.label(),
                stream.name
            ))
        })
}

const START_FAILURE_TAG: &str = " [atrium-start-failure:";
fn start_failure_marker(parent: &str, task: &str, claim: (&str, i64)) -> String {
    let identity = serde_json::to_string(&(parent, task, claim))
        .expect("startup failure identity contains only JSON primitives");
    format!("[atrium-start-failure:{identity}]")
}

fn startup_failure(notice: &ProjectNotice, parent: &str) -> Option<(String, (String, i64))> {
    if notice.from != ATRIUM || notice.to != [parent] {
        return None;
    }
    let (text, marker) = notice.text.rsplit_once(START_FAILURE_TAG)?;
    let (recipient, task, stream): (String, String, (String, i64)) =
        serde_json::from_str(marker.strip_suffix(']')?).ok()?;
    let (owner, id) = task.split_once('/')?;
    if recipient != parent
        || owner != parent
        || id.is_empty()
        || id.contains('/')
        || !text.starts_with(&format!("Could not start an agent for {task}: "))
    {
        return None;
    }
    Some((task, stream))
}

fn completion_text(stream: &Workstream, task: &Task) -> String {
    let verb = if task.status == TaskStatus::Done {
        "finished"
    } else {
        "could not finish"
    };
    let reason = task
        .result
        .as_deref()
        .map(first_line)
        .filter(|s| !s.is_empty())
        .unwrap_or("no reason given");
    format!(
        "{} \"{}\" {verb} {}: {reason}. Read its Result and review its changes.",
        stream.role.label(),
        stream.name,
        task.rel_path()
    )
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

    fn unique_root(label: &str) -> PathBuf {
        use std::hash::{BuildHasher, Hasher};
        for _ in 0..128 {
            let random = std::collections::hash_map::RandomState::new()
                .build_hasher()
                .finish();
            let path = std::env::temp_dir().join(format!("{label}-{random:016x}"));
            match fs::create_dir(&path) {
                Ok(()) => return path,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("create isolated fixture: {e}"),
            }
        }
        panic!("could not allocate an isolated fixture");
    }

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

    fn delivery_fixture(label: &str) -> (PathBuf, Project) {
        let root = unique_root(label);
        let mut project = Project::open(&root).unwrap();
        let mut lead = stream("lead", &[]);
        lead.thread_id = Some("mock-session".into());
        project.file.streams.push(lead);
        project.save().unwrap();
        project
            .post(ATRIUM, vec!["lead".into()], "first notice")
            .unwrap();
        (root, project)
    }

    fn reserve_delivery(project: &mut Project) -> NoticeTicket {
        let NoticePreparation::Ready { ticket, text } = project
            .prepare_notice_delivery("lead", "mock-session")
            .unwrap()
        else {
            panic!("expected a new notice reservation");
        };
        assert!(text.contains("first notice"));
        ticket
    }

    #[test]
    fn notice_delivery_acceptance_commits_only_captured_prefix() {
        let (root, mut p) = delivery_fixture("atrium-delivery-watermark");
        let ticket = reserve_delivery(&mut p);
        p.mark_read("lead"); // Older helper cannot clobber an active lease.
        assert_eq!(p.stream("lead").unwrap().notices_seen, 0);
        p.post(ATRIUM, vec!["lead".into()], "later notice").unwrap();
        assert!(
            p.settle_notice_delivery(&ticket, NoticeOutcome::Accepted)
                .unwrap()
        );
        let p = Project::open(&root).unwrap();
        assert_eq!(p.stream("lead").unwrap().notices_seen, 1);
        assert_eq!(
            p.unread("lead")
                .iter()
                .map(|n| n.text.as_str())
                .collect::<Vec<_>>(),
            ["later notice"]
        );
        let raw = fs::read_to_string(p.dir().join("project.json")).unwrap();
        assert!(!raw.contains("notice_deliveries"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejected_delivery_stays_unread_and_old_callbacks_are_stale() {
        let (root, mut p) = delivery_fixture("atrium-delivery-rejected");
        let first = reserve_delivery(&mut p);
        assert!(
            !p.resolve_notice_delivery("lead", NoticeResolution::RetryNotices)
                .unwrap()
        );
        assert!(
            p.settle_notice_delivery(&first, NoticeOutcome::Rejected)
                .unwrap()
        );
        let second = reserve_delivery(&mut p);
        assert_ne!(first, second);
        assert!(
            !p.settle_notice_delivery(&first, NoticeOutcome::Accepted)
                .unwrap()
        );
        p.bind_thread("lead", "new-session").unwrap();
        assert!(
            !p.settle_notice_delivery(&second, NoticeOutcome::Accepted)
                .unwrap()
        );
        p.recover_notice_deliveries().unwrap();
        assert!(
            !p.resolve_notice_delivery("lead", NoticeResolution::MarkReceived)
                .unwrap()
        );
        assert!(
            p.resolve_notice_delivery("lead", NoticeResolution::RetryNotices)
                .unwrap()
        );
        assert_eq!(Project::open(&root).unwrap().unread("lead").len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persisted_reserved_delivery_is_held_after_restart_until_user_resolves() {
        let (root, mut p) = delivery_fixture("atrium-delivery-restart");
        let ticket = reserve_delivery(&mut p);
        let raw = fs::read_to_string(p.dir().join("project.json")).unwrap();
        assert!(raw.contains("reserved") && !raw.contains("first notice"));
        let mut p = Project::open(&root).unwrap();
        assert!(matches!(
            p.prepare_notice_delivery("lead", "mock-session").unwrap(),
            NoticePreparation::Blocked
        ));
        p.recover_notice_deliveries().unwrap();
        assert!(p.notice_delivery_uncertain("lead"));
        assert!(
            !p.settle_notice_delivery(&ticket, NoticeOutcome::Accepted)
                .unwrap()
        );
        assert!(
            p.resolve_notice_delivery("lead", NoticeResolution::MarkReceived)
                .unwrap()
        );
        assert!(Project::open(&root).unwrap().unread("lead").is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    mod notice_crash {
        use super::*;
        use std::{
            io::Read,
            os::unix::process::ExitStatusExt,
            process::{Command, Stdio},
        };

        const FIXTURE_ROOT: &str = "ATRIUM_NOTICE_CRASH_FIXTURE_ROOT";

        #[test]
        fn killed_after_prepare_blocks_replay_and_preserves_unread() {
            let (root, project) = delivery_fixture("atrium-delivery-crash");
            drop(project);
            let executable = std::env::current_exe().unwrap();
            let (_, module) = module_path!().split_once("::").unwrap();
            let subprocess = |test: &str| {
                let mut cmd = Command::new(&executable);
                cmd.args(["--ignored", "--exact", "--nocapture"])
                    .arg(format!("{module}::{test}"))
                    .env(FIXTURE_ROOT, &root);
                cmd
            };

            let mut writer = subprocess("prepare_then_wait")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .unwrap();
            let ready = root.join("prepared");
            let deadline = Instant::now() + Duration::from_secs(10);
            let prepared = loop {
                if ready.exists() {
                    break true;
                }
                if writer.try_wait().unwrap().is_some() || Instant::now() >= deadline {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            let killed = writer.kill();
            let status = writer.wait().unwrap();
            assert!(prepared, "writer never finished prepare ({status})");
            killed.unwrap();
            assert_eq!(status.signal(), Some(9), "writer must die by SIGKILL");

            let status = subprocess("reopen_and_verify").status().unwrap();
            assert!(status.success(), "recovery subprocess failed ({status})");
            assert!(root.join("verified").exists(), "recovery test did not run");
            fs::remove_dir_all(root).unwrap();
        }

        #[test]
        #[ignore = "subprocess entry point for the notice crash test"]
        fn prepare_then_wait() {
            let Some(root) = std::env::var_os(FIXTURE_ROOT).map(PathBuf::from) else {
                return;
            };
            let mut project = Project::open(&root).unwrap();
            reserve_delivery(&mut project);
            fs::write(root.join("prepared"), []).unwrap();
            // Keep the reservation live until killed, or exit if the supervisor disappears.
            let _ = std::io::stdin().read(&mut [0]);
        }

        #[test]
        #[ignore = "subprocess entry point for the notice crash test"]
        fn reopen_and_verify() {
            let Some(root) = std::env::var_os(FIXTURE_ROOT).map(PathBuf::from) else {
                return;
            };
            let mut project = Project::open(&root).unwrap();
            assert_eq!(
                project.file.notice_deliveries["lead"].phase,
                DeliveryPhase::Reserved
            );
            project.recover_notice_deliveries().unwrap();
            assert!(project.notice_delivery_uncertain("lead"));
            let lead = project.stream("lead").unwrap();
            assert_eq!(lead.thread_id.as_deref(), Some("mock-session"));
            assert_eq!(lead.notices_seen, 0);
            assert!(matches!(
                project
                    .prepare_notice_delivery("lead", "mock-session")
                    .unwrap(),
                NoticePreparation::Blocked
            ));
            let unread = project.unread("lead");
            assert_eq!(unread.len(), 1);
            assert_eq!(unread[0].text, "first notice");
            fs::write(root.join("verified"), []).unwrap();
        }
    }

    #[test]
    fn changed_notice_prefix_remains_unread_despite_accepted_rpc() {
        let (root, mut p) = delivery_fixture("atrium-delivery-prefix-edit");
        let ticket = reserve_delivery(&mut p);
        let notice_file = p.dir().join("notices.md");
        let prior = fs::read_to_string(&notice_file).unwrap();
        fs::write(
            &notice_file,
            prior.replace("first notice", "replacement notice"),
        )
        .unwrap();
        assert!(
            p.settle_notice_delivery(&ticket, NoticeOutcome::Accepted)
                .unwrap()
        );
        assert!(p.notice_delivery_uncertain("lead"));
        assert!(
            !p.resolve_notice_delivery("lead", NoticeResolution::MarkReceived)
                .unwrap()
        );
        assert!(
            p.resolve_notice_delivery("lead", NoticeResolution::RetryNotices)
                .unwrap()
        );
        assert_eq!(
            Project::open(&root).unwrap().unread("lead")[0].text,
            "replacement notice"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn delivery_save_reported_failed_after_publication_does_not_redispatch() {
        let (root, mut p) = delivery_fixture("atrium-delivery-after-rename");
        reserve_delivery(&mut p);
        // Simulate an Accepted cursor+clear that got through replacement but
        // returned an error synchronizing the containing directory afterwards.
        let mut next = p.file.clone();
        next.streams[0].notices_seen = 1;
        next.notice_deliveries.clear();
        let result = p.persist_delivery_file_with(next, |path, bytes| {
            atomic_write(path, bytes)?;
            anyhow::bail!("simulated directory sync error after rename");
        });
        assert!(result.is_err());
        assert!(p.notice_delivery_blocked("lead"));
        assert_eq!(p.stream("lead").unwrap().notices_seen, 0);
        assert!(Project::open(&root).unwrap().unread("lead").is_empty());
        p.reload();
        assert!(p.notice_delivery_blocked("lead"));
        p.save().unwrap(); // May regress cursor but preserves conservative hold.
        let mut p = Project::open(&root).unwrap();
        p.recover_notice_deliveries().unwrap();
        assert!(p.notice_delivery_uncertain("lead"));
        assert!(matches!(
            p.prepare_notice_delivery("lead", "mock-session").unwrap(),
            NoticePreparation::Blocked
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn delivery_persistence_failure_neither_sends_nor_advances_cursor() {
        let (root, mut p) = delivery_fixture("atrium-delivery-io");
        let path = p.dir().join("project.json");
        let backup = p.dir().join("project.backup");
        let block = || {
            fs::rename(&path, &backup).unwrap();
            fs::create_dir(&path).unwrap();
        };
        let unblock = || {
            fs::remove_dir(&path).unwrap();
            fs::rename(&backup, &path).unwrap();
        };
        block();
        assert!(p.prepare_notice_delivery("lead", "mock-session").is_err());
        assert!(p.file.notice_deliveries.is_empty());
        unblock();
        let ticket = reserve_delivery(&mut p);
        block();
        assert!(
            p.settle_notice_delivery(&ticket, NoticeOutcome::Accepted)
                .is_err()
        );
        assert!(
            p.settle_notice_delivery(&ticket, NoticeOutcome::Rejected)
                .is_err()
        );
        assert!(
            p.settle_notice_delivery(&ticket, NoticeOutcome::Uncertain)
                .is_err()
        );
        assert_eq!(p.stream("lead").unwrap().notices_seen, 0);
        assert!(matches!(
            p.prepare_notice_delivery("lead", "mock-session").unwrap(),
            NoticePreparation::Blocked
        ));
        unblock();
        let mut p = Project::open(&root).unwrap();
        p.recover_notice_deliveries().unwrap();
        assert!(p.notice_delivery_uncertain("lead"));
        block();
        assert!(
            p.resolve_notice_delivery("lead", NoticeResolution::MarkReceived)
                .is_err()
        );
        assert!(
            p.resolve_notice_delivery("lead", NoticeResolution::RetryNotices)
                .is_err()
        );
        assert_eq!(p.stream("lead").unwrap().notices_seen, 0);
        assert!(p.notice_delivery_uncertain("lead"));
        unblock();
        assert!(
            p.resolve_notice_delivery("lead", NoticeResolution::RetryNotices)
                .unwrap()
        );
        assert_eq!(Project::open(&root).unwrap().unread("lead").len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_project_round_trips_and_tracks_notices_and_crossings() {
        let root = unique_root("atrium-test");
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
        let root = unique_root("atrium-tasks");
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
    #[test]
    fn receipt_identity_scopes_lifecycle_and_legacy_matching_is_exact() {
        let mut worker = stream("worker", &[]);
        worker.task = Some("lead/job".into());
        worker.parent = Some("lead".into());
        worker.thread_id = Some("session-1".into());
        let task = Task::parse(
            "lead",
            "job",
            "# Job\nstatus: done\n\n## Result\nFirst result",
        );
        let legacy = ProjectNotice {
            at: String::new(),
            from: ATRIUM.into(),
            to: vec!["lead".into()],
            text: completion_text(&worker, &task),
        };
        assert!(is_completion_receipt(&legacy, &worker));
        let mut forged = legacy.clone();
        forged.from = YOU.into();
        assert!(!is_completion_receipt(&forged, &worker));
        forged = legacy.clone();
        forged.to.push("all".into());
        assert!(!is_completion_receipt(&forged, &worker));
        forged = legacy.clone();
        forged.text = format!("A mention: {}", forged.text);
        assert!(!is_completion_receipt(&forged, &worker));
        let marked = ProjectNotice {
            text: format!("{} {}", legacy.text, completion_marker(&worker).unwrap()),
            ..legacy
        };
        assert!(is_completion_receipt(&marked, &worker));
        worker.name = "Renamed".into();
        assert!(is_completion_receipt(&marked, &worker));
        worker.task = Some("lead/another-job".into());
        assert!(!is_completion_receipt(&marked, &worker));
        worker.task = Some("lead/job".into());
        // Session binding can finish after a manually completed task; it is
        // not a new task lifecycle and must not invalidate a persisted receipt.
        worker.thread_id = None;
        assert!(is_completion_receipt(&marked, &worker));
        worker.thread_id = Some("session-2".into());
        assert!(is_completion_receipt(&marked, &worker));
        worker.created_at += 1;
        assert!(!is_completion_receipt(&marked, &worker));
    }

    #[test]
    fn receipt_after_failed_save_is_recovered_when_task_was_archived() {
        let root = unique_root("atrium-partial-archive");
        let mut p = Project::open(&root).unwrap();
        p.file.streams.push(stream("lead", &[]));
        let key = p
            .add_task("lead", "job", "# Job\nstatus: done\n\n## Result\nFinished")
            .unwrap();
        let worker = p.stream_from_task(p.task(&key).unwrap(), "codex").unwrap();
        let id = worker.id.clone();
        p.file.streams.push(worker);
        p.save().unwrap();
        let path = root.join(p.task(&key).unwrap().rel_path());
        let metadata = p.dir().join("project.json");
        let backup = p.dir().join("project.backup");
        fs::rename(&metadata, &backup).unwrap();
        fs::create_dir(&metadata).unwrap();
        assert!(p.report_finished(&id).is_err());
        assert_eq!(p.notices.len(), 1);
        fs::remove_dir(&metadata).unwrap();
        fs::rename(&backup, &metadata).unwrap();
        fs::remove_file(path).unwrap();
        let mut p = Project::open(&root).unwrap();
        assert_eq!(p.report_finished(&id).unwrap(), Some("lead".into()));
        assert!(p.stream(&id).unwrap().reported);
        assert_eq!(p.notices.len(), 1);
        assert_eq!(p.pending_report_parents(), vec!["lead"]);
        // Repeat the missing-flag boundary and exercise the actual startup hook.
        p.stream_mut(&id).unwrap().reported = false;
        p.save().unwrap();
        let mut p = Project::open(&root).unwrap();
        assert_eq!(p.recover_unbound_tasks().unwrap(), vec!["lead"]);
        assert!(p.stream(&id).unwrap().reported);
        assert_eq!(p.notices.len(), 1);
        assert!(p.recover_unbound_tasks().unwrap().is_empty());
        // With no receipt and no task, do not manufacture a new completion.
        p.notices.clear();
        p.stream_mut(&id).unwrap().reported = false;
        assert!(p.report_finished(&id).unwrap().is_none());
        assert!(!p.stream(&id).unwrap().reported);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_receipt_marker_text_in_result_is_not_a_new_identity() {
        let root = unique_root("atrium-legacy-marker");
        let mut p = Project::open(&root).unwrap();
        p.file.streams.push(stream("lead", &[]));
        let key = p
            .add_task(
                "lead",
                "job",
                "# Job\nstatus: done\n\n## Result\nDiscussed [atrium-completion:example] syntax",
            )
            .unwrap();
        let task = p.task(&key).unwrap().clone();
        let worker = p.stream_from_task(&task, "codex").unwrap();
        let id = worker.id.clone();
        let legacy_text = completion_text(&worker, &task);
        p.file.streams.push(worker);
        p.post(ATRIUM, vec!["lead".into()], &legacy_text).unwrap();
        p.save().unwrap();
        let mut p = Project::open(&root).unwrap();
        assert_eq!(p.report_finished(&id).unwrap(), Some("lead".into()));
        assert_eq!(
            p.notices.len(),
            1,
            "marker mention in legacy Result must not defeat deduplication"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn completion_retry_after_failed_save_and_result_edit_is_idempotent() {
        let root = unique_root("atrium-receipt-edit");
        let mut p = Project::open(&root).unwrap();
        p.file.streams.push(stream("lead", &[]));
        let key = p
            .add_task(
                "lead",
                "job",
                "# Job\nstatus: done\n\n## Result\nFirst result",
            )
            .unwrap();
        let worker = p.stream_from_task(p.task(&key).unwrap(), "codex").unwrap();
        let id = worker.id.clone();
        p.file.streams.push(worker);
        p.save().unwrap();
        let metadata = p.dir().join("project.json");
        let backup = p.dir().join("project.backup");
        fs::rename(&metadata, &backup).unwrap();
        fs::create_dir(&metadata).unwrap();
        assert!(p.report_finished(&id).is_err());
        assert_eq!(p.notices.len(), 1);
        fs::remove_dir(&metadata).unwrap();
        fs::rename(&backup, &metadata).unwrap();
        let task_path = root.join(p.task(&key).unwrap().rel_path());
        fs::write(
            &task_path,
            "# Job\nstatus: done\n\n## Result\nRevised result",
        )
        .unwrap();
        let mut p = Project::open(&root).unwrap();
        assert!(p.report_finished(&id).unwrap().is_some());
        assert_eq!(
            p.notices.len(),
            1,
            "editing Result must not create a second completion receipt"
        );
        assert_eq!(p.pending_report_parents(), vec!["lead"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unread_completion_survives_task_archival_and_stream_rename() {
        let root = unique_root("atrium-receipt-archive");
        let mut p = Project::open(&root).unwrap();
        p.file.streams.push(stream("lead", &[]));
        let key = p
            .add_task("lead", "job", "# Job\nstatus: done\n\n## Result\nFinished")
            .unwrap();
        let worker = p.stream_from_task(p.task(&key).unwrap(), "codex").unwrap();
        let id = worker.id.clone();
        p.file.streams.push(worker);
        p.report_finished(&id).unwrap();
        fs::remove_file(root.join(p.task(&key).unwrap().rel_path())).unwrap();
        p.stream_mut(&id).unwrap().name = "Renamed after completion".into();
        p.save().unwrap();
        let mut p = Project::open(&root).unwrap();
        assert_eq!(
            p.pending_report_parents(),
            vec!["lead"],
            "unread persisted receipt must not depend on its task still existing"
        );
        p.mark_read("lead");
        p.save().unwrap();
        assert!(
            Project::open(&root)
                .unwrap()
                .pending_report_parents()
                .is_empty()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn completion_receipt_survives_reload_and_partial_save() {
        let root = unique_root("atrium-receipt");
        let _ = fs::remove_dir_all(&root);
        let mut project = Project::open(&root).unwrap();
        project.file.streams.push(stream("lead", &[]));
        let key = project
            .add_task("lead", "job", "# Job\nstatus: open\n\nDo it.")
            .unwrap();
        let task = project.task(&key).unwrap().clone();
        let worker = project.stream_from_task(&task, "codex").unwrap();
        let id = worker.id.clone();
        project.file.streams.push(worker);
        project.set_task_status(&key, TaskStatus::Done).unwrap();
        assert_eq!(
            project.report_finished(&id).unwrap().as_deref(),
            Some("lead")
        );
        assert_eq!(project.notices.len(), 1);
        // Simulate a crash after the notice write but before saving reported.
        project.stream_mut(&id).unwrap().reported = false;
        project.save().unwrap();
        let mut project = Project::open(&root).unwrap();
        assert!(project.report_finished(&id).unwrap().is_some());
        assert_eq!(project.notices.len(), 1);
        let mut project = Project::open(&root).unwrap();
        assert!(project.report_finished(&id).unwrap().is_none());
        assert_eq!(project.notices.len(), 1);
        assert_eq!(project.pending_report_parents(), vec!["lead"]);
        project.mark_read("lead");
        project.save().unwrap();
        let project = Project::open(&root).unwrap();
        assert!(project.pending_report_parents().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pool_workers_reuse_slots_after_finishing_tasks() {
        let root = unique_root("atrium-slots");
        let _ = fs::remove_dir_all(&root);
        let mut project = Project::open(&root).unwrap();
        project.file.max_workers = 2;
        project.file.streams.push(stream("lead", &[]));
        for name in ["a", "b", "c"] {
            project.add_task("lead", name, "# Task\n\nWork").unwrap();
        }
        assert!(project.startable().is_empty());
        assert_eq!(project.workers_needed("lead"), 2);
        for task in ["lead/a", "lead/b"] {
            let worker = project.pool_worker("lead", "codex").unwrap();
            let id = worker.id.clone();
            project.file.streams.push(worker);
            assert_eq!(project.claim_next(&id).unwrap().unwrap().key(), task);
        }
        assert_eq!(project.workers_needed("lead"), 0);
        project.finish_task("lead-w1", true, "Done.").unwrap();
        assert_eq!(
            project
                .task_of(project.stream("lead-w1").unwrap())
                .unwrap()
                .key(),
            "lead/c"
        );
        assert_eq!(project.workers_needed("lead"), 0);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn atomic_write_collision_never_removes_someone_elses_file() {
        let root = unique_root("atrium-write-collision");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("document");
        let tmp = root.join("occupied.tmp");
        fs::write(&path, b"original").unwrap();
        fs::write(&tmp, b"belongs to another writer").unwrap();
        assert!(atomic_write_at(&path, b"replacement", &tmp).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(fs::read(&tmp).unwrap(), b"belongs to another writer");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_is_private_for_new_files_and_preserves_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let root = unique_root("atrium-write-mode");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("document");
        atomic_write_at(&path, b"new", &root.join("new.tmp")).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        atomic_write_at(&path, b"replacement", &root.join("replace.tmp")).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_metadata_save_does_not_lose_binding_or_completion_receipt() {
        let root = unique_root("atrium-metadata-failure");
        let mut p = Project::open(&root).unwrap();
        p.file.streams.push(stream("lead", &[]));
        let key = p.add_task("lead", "job", "# Job\n\nDo it").unwrap();
        let worker = p.stream_from_task(p.task(&key).unwrap(), "codex").unwrap();
        let id = worker.id.clone();
        p.file.streams.push(worker);
        p.save().unwrap();
        let path = p.dir().join("project.json");
        let backup = p.dir().join("project.backup");
        fs::rename(&path, &backup).unwrap();
        fs::create_dir(&path).unwrap(); // metadata rename now fails without permissions tricks
        assert!(p.bind_thread(&id, "session").is_err());
        assert!(p.stream(&id).unwrap().thread_id.is_none());
        p.set_task_status(&key, TaskStatus::Done).unwrap();
        assert!(p.report_finished(&id).is_err());
        assert!(!p.stream(&id).unwrap().reported);
        assert_eq!(p.notices.len(), 1);
        assert!(p.report_finished(&id).is_err());
        assert_eq!(p.notices.len(), 1);
        assert!(
            !fs::read_dir(p.dir())
                .unwrap()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().contains("tmp-"))
        );
        fs::remove_dir(&path).unwrap();
        fs::rename(&backup, &path).unwrap();
        let mut p = Project::open(&root).unwrap();
        assert!(p.report_finished(&id).unwrap().is_some());
        assert_eq!(p.notices.len(), 1);
        assert!(p.report_finished(&id).unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unreadable_task_and_notice_never_replace_original_contents() {
        let root = unique_root("atrium-read-failure");
        let mut p = Project::open(&root).unwrap();
        let key = p
            .add_task("lead", "job", "# Job\n\nImportant instructions")
            .unwrap();
        let task_path = root.join(p.task(&key).unwrap().rel_path());
        fs::write(&task_path, [0xff, 0xfe]).unwrap();
        assert!(p.set_task_status(&key, TaskStatus::Running).is_err());
        assert_eq!(fs::read(&task_path).unwrap(), vec![0xff, 0xfe]);
        let notices = p.dir().join("notices.md");
        fs::write(&notices, [0xff, 0xfe]).unwrap();
        assert!(p.post(ATRIUM, vec!["lead".into()], "finished").is_err());
        assert_eq!(fs::read(&notices).unwrap(), vec![0xff, 0xfe]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_failure_wake_survives_restart_and_archival_until_read() {
        let root = unique_root("atrium-start-wake");
        let mut p = Project::open(&root).unwrap();
        p.file.streams.push(stream("lead", &[]));
        let key = p.add_task("lead", "job", "# Job").unwrap();
        let worker = p.stream_from_task(p.task(&key).unwrap(), "codex").unwrap();
        let id = worker.id.clone();
        let task_path = root.join(p.task(&key).unwrap().rel_path());
        p.set_task_status(&key, TaskStatus::Running).unwrap();
        p.file.streams.push(worker);
        p.save().unwrap();
        assert_eq!(
            p.fail_start(&id, "startup refused").unwrap().as_deref(),
            Some("lead")
        );
        assert!(p.stream(&id).is_none());
        fs::remove_file(&task_path).unwrap();
        let mut p = Project::open(&root).unwrap();
        assert!(p.recover_unbound_tasks().unwrap().is_empty());
        assert_eq!(p.pending_report_parents(), ["lead"]);
        assert_eq!(p.unread("lead").len(), 1);
        p.mark_read("lead");
        p.save().unwrap();
        assert!(
            Project::open(&root)
                .unwrap()
                .pending_report_parents()
                .is_empty()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_failure_append_and_metadata_retries_preserve_evidence_and_binding() {
        let root = unique_root("atrium-start-wake-retry");
        let mut p = Project::open(&root).unwrap();
        p.file.streams.push(stream("lead", &[]));
        let key = p.add_task("lead", "job", "# Job").unwrap();
        let worker = p.stream_from_task(p.task(&key).unwrap(), "codex").unwrap();
        let id = worker.id.clone();
        p.set_task_status(&key, TaskStatus::Running).unwrap();
        p.file.streams.push(worker);
        p.save().unwrap();
        let notices = p.dir().join("notices.md");
        let backup = p.dir().join("notices.backup");
        fs::rename(&notices, &backup).unwrap();
        fs::create_dir(&notices).unwrap();
        assert!(p.fail_start(&id, "first attempt").is_err());
        assert!(p.stream(&id).is_some());
        assert_eq!(p.task(&key).unwrap().status, TaskStatus::Running);
        fs::remove_dir(&notices).unwrap();
        fs::rename(&backup, &notices).unwrap();

        // Force final stream-metadata save failure after notice and Failed
        // task are durable, then restart with the earlier metadata.
        let metadata = p.dir().join("project.json");
        let backup = p.dir().join("metadata.backup");
        fs::rename(&metadata, &backup).unwrap();
        fs::create_dir(&metadata).unwrap();
        assert!(p.fail_start(&id, "retry reason").is_err());
        assert!(p.stream(&id).is_some());
        assert_eq!(p.task(&key).unwrap().status, TaskStatus::Failed);
        fs::remove_dir(&metadata).unwrap();
        fs::rename(&backup, &metadata).unwrap();
        let mut p = Project::open(&root).unwrap();
        assert_eq!(p.recover_unbound_tasks().unwrap(), ["lead"]);
        assert!(p.stream(&id).is_none());
        assert_eq!(p.notices.len(), 1);
        assert_eq!(p.pending_report_parents(), ["lead"]);

        let other = p.add_task("lead", "bound", "# Bound").unwrap();
        let bound = p
            .stream_from_task(p.task(&other).unwrap(), "codex")
            .unwrap();
        let bound_id = bound.id.clone();
        p.set_task_status(&other, TaskStatus::Running).unwrap();
        p.file.streams.push(bound);
        p.bind_thread(&bound_id, "live-agent").unwrap();
        assert!(p.fail_start(&bound_id, "stale refusal").unwrap().is_none());
        assert!(p.recover_unbound_tasks().unwrap().is_empty());
        assert_eq!(
            p.stream(&bound_id).unwrap().thread_id.as_deref(),
            Some("live-agent")
        );
        assert_eq!(p.task(&other).unwrap().status, TaskStatus::Running);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reopened_start_failure_gets_a_new_receipt_even_in_the_same_second() {
        let root = unique_root("atrium-start-repeat");
        let mut p = Project::open(&root).unwrap();
        p.file.streams.push(stream("lead", &[]));
        let key = p.add_task("lead", "job", "# Job").unwrap();
        let first = p.stream_from_task(p.task(&key).unwrap(), "codex").unwrap();
        let first_id = first.id.clone();
        let when = first.created_at;
        p.set_task_status(&key, TaskStatus::Running).unwrap();
        p.file.streams.push(first);
        p.save().unwrap();
        assert!(p.fail_start(&first_id, "first refusal").unwrap().is_some());
        p.mark_read("lead");
        p.save().unwrap();
        let mut p = Project::open(&root).unwrap();
        p.set_task_status(&key, TaskStatus::Open).unwrap();
        let mut second = p.stream_from_task(p.task(&key).unwrap(), "codex").unwrap();
        second.created_at = when;
        assert_ne!(second.id, first_id);
        let second_id = second.id.clone();
        p.set_task_status(&key, TaskStatus::Running).unwrap();
        p.file.streams.push(second);
        p.save().unwrap();
        p.fail_start(&second_id, "second refusal").unwrap();
        assert_eq!(p.notices.len(), 2);
        assert_eq!(p.unread("lead").len(), 1);
        assert_eq!(p.pending_report_parents(), ["lead"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ordinary_and_mismatched_notices_do_not_become_startup_wakes() {
        let root = unique_root("atrium-start-wake-foreign");
        let mut p = Project::open(&root).unwrap();
        p.file.streams.push(stream("lead", &[]));
        let marker = start_failure_marker("lead", "lead/job", ("worker", 1));
        p.post(
            YOU,
            vec!["lead".into()],
            &format!("Could not start an agent for lead/job: prose {marker}"),
        )
        .unwrap();
        p.post(
            ATRIUM,
            vec!["lead".into()],
            "Could not start an agent for lead/job: legacy prose",
        )
        .unwrap();
        p.post(
            ATRIUM,
            vec!["lead".into()],
            &format!("Unrelated scheduler prose {marker}"),
        )
        .unwrap();
        p.post(
            ATRIUM,
            vec!["all".into()],
            &format!("Could not start an agent for lead/job: broadcast {marker}"),
        )
        .unwrap();
        assert!(p.pending_report_parents().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_failure_preserves_evidence_until_status_can_be_written() {
        let root = unique_root("atrium-start-failure");
        let mut p = Project::open(&root).unwrap();
        p.file.streams.push(stream("lead", &[]));
        let key = p
            .add_task("lead", "job", "# Job\nstatus: running\n\nDo it")
            .unwrap();
        let worker = p.stream_from_task(p.task(&key).unwrap(), "codex").unwrap();
        let id = worker.id.clone();
        p.file.streams.push(worker);
        p.save().unwrap();
        let task_path = root.join(p.task(&key).unwrap().rel_path());
        let original = fs::read(&task_path).unwrap();
        // First post succeeds, but task status cannot be read. Keep the stream.
        fs::write(&task_path, [0xff]).unwrap();
        assert!(p.fail_start(&id, "backend unavailable").is_err());
        assert!(p.stream(&id).is_some());
        assert_eq!(p.notices.len(), 1);
        fs::write(&task_path, original).unwrap();
        p.stamp = None;
        p.reload();
        assert_eq!(
            p.fail_start(&id, "backend unavailable").unwrap(),
            Some("lead".into())
        );
        assert!(p.stream(&id).is_none());
        assert_eq!(p.task(&key).unwrap().status, TaskStatus::Failed);
        assert_eq!(p.notices.len(), 1);
        let mut p = Project::open(&root).unwrap();
        assert!(p.recover_unbound_tasks().unwrap().is_empty());
        assert!(p.startable().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restart_recovers_an_unbound_persisted_claim_without_relaunch() {
        let root = unique_root("atrium-bound-claim");
        let mut p = Project::open(&root).unwrap();
        p.file.streams.push(stream("lead", &[]));
        let key = p
            .add_task("lead", "job", "# Job\nstatus: running\n\nDo it")
            .unwrap();
        let worker = p.stream_from_task(p.task(&key).unwrap(), "codex").unwrap();
        p.file.streams.push(worker);
        p.save().unwrap();
        let mut p = Project::open(&root).unwrap();
        assert_eq!(p.recover_unbound_tasks().unwrap(), vec!["lead"]);
        assert_eq!(p.task(&key).unwrap().status, TaskStatus::Failed);
        assert!(p.recover_unbound_tasks().unwrap().is_empty());
        assert_eq!(p.notices.len(), 1);
        // A finished task without a session is not a claim to recover. Never
        // overwrite an agent/user's finished outcome during startup repair.
        let done = p
            .add_task("lead", "done", "# Done\nstatus: done\n\nResult")
            .unwrap();
        let worker = p.stream_from_task(p.task(&done).unwrap(), "codex").unwrap();
        p.file.streams.push(worker);
        p.save().unwrap();
        let mut p = Project::open(&root).unwrap();
        assert!(p.recover_unbound_tasks().unwrap().is_empty());
        assert_eq!(p.task(&done).unwrap().status, TaskStatus::Done);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startable_with_filters_controller_tasks_by_eligibility() {
        let mut project = Project::default();
        let mut boss = stream("boss", &[]);
        boss.role = Role::Controller;
        project.file.streams.extend([boss, stream("lead", &[])]);
        project.tasks = vec![
            Task::parse("boss", "a", "# Offline\nagent: claude\n"),
            Task::parse("boss", "b", "# Ready\nagent: codex\n"),
            Task::parse("lead", "c", "# Pool task\nagent: codex\n"),
        ];
        assert_eq!(project.startable().len(), 2);
        let tasks = project.startable_with(|task| task.agent.as_deref() == Some("codex"));
        assert_eq!(
            tasks.iter().map(|t| t.key()).collect::<Vec<_>>(),
            vec!["boss/b"]
        );
    }

    #[test]
    fn restart_leaves_pool_claims_to_the_pool_lifecycle() {
        let root = unique_root("atrium-recover-pool");
        let mut p = Project::open(&root).unwrap();
        p.file.streams.push(stream("lead", &[]));
        let key = p.add_task("lead", "job", "# Job").unwrap();
        let worker = p.pool_worker("lead", "codex").unwrap();
        let id = worker.id.clone();
        p.file.streams.push(worker);
        p.save().unwrap();
        p.claim_next(&id).unwrap().unwrap();

        let mut p = Project::open(&root).unwrap();
        assert!(p.recover_unbound_tasks().unwrap().is_empty());
        assert!(p.stream(&id).is_some());
        let task = p.task(&key).unwrap();
        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(task.claimed_by.as_deref(), Some(id.as_str()));
        assert!(p.notices.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interrupted_unbound_claim_is_reported_not_relaunched() {
        let root = unique_root("atrium-unbound");
        let _ = fs::remove_dir_all(&root);
        let mut project = Project::open(&root).unwrap();
        project.file.streams.push(stream("lead", &[]));
        project.save().unwrap();
        let key = project
            .add_task("lead", "job", "# Job\nstatus: running\n\nDo it.")
            .unwrap();
        assert_eq!(project.recover_unbound_tasks().unwrap(), vec!["lead"]);
        assert_eq!(project.task(&key).unwrap().status, TaskStatus::Failed);
        assert!(project.startable().is_empty());
        let mut project = Project::open(&root).unwrap();
        assert!(project.recover_unbound_tasks().unwrap().is_empty());
        assert_eq!(project.notices.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }
}
