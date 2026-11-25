//! `project.*`: a project's `.atrium/` folder as JSON ops, and the app's
//! scheduler as a decision (`project.next`) the worker carries out.
//!
//! Roles are named by [`role_id`]: `controller`, `lead:<id>` and
//! `worker:<lead>/<id>`, where `<id>` is the workstream's id. Times given to
//! the project ops (`now`, quota ends) are Unix seconds, like `team.*`.

use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use atrium_project::{
    ATRIUM, DIR, Project, Role, Task, TaskStatus, Workstream, YOU, autopilot::Wake, pool::TaskLock,
    spread, with_notices,
};
use atrium_team::{Pick, Quotas, Team, schedule};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    store::{rewrite_task, write_atomic},
    team::{TeamView, load as load_team},
};

// ---------- roles ----------

/// `controller`, `lead:<id>`, `worker:<lead>/<id>`.
pub fn role_id(s: &Workstream) -> String {
    match s.role {
        Role::Controller => "controller".into(),
        Role::Lead => format!("lead:{}", s.id),
        Role::Worker => format!("worker:{}/{}", s.parent.as_deref().unwrap_or(""), s.id),
    }
}

fn role_json(s: &Workstream) -> Value {
    match s.role {
        Role::Controller => json!({ "kind": "controller" }),
        Role::Lead => json!({ "kind": "lead", "name": s.id }),
        Role::Worker => {
            json!({ "kind": "worker", "lead": s.parent.as_deref().unwrap_or(""), "name": s.id })
        }
    }
}

/// The workstream a role id names.
fn stream_of<'a>(project: &'a Project, role: &str) -> Option<&'a Workstream> {
    if role == "controller" {
        return project.controller();
    }
    let id = if let Some(id) = role.strip_prefix("lead:") {
        id
    } else if let Some(rest) = role.strip_prefix("worker:") {
        rest.rsplit('/').next().unwrap_or(rest)
    } else {
        role
    };
    project.stream(id)
}

fn need_stream<'a>(project: &'a Project, role: &str) -> Result<&'a Workstream, String> {
    stream_of(project, role).ok_or_else(|| format!("no role {role} in this project"))
}

fn load(root: &Path) -> Result<Project, String> {
    Project::load(root).map_err(|e| format!("{e:#}"))
}

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

// ---------- the project as the UI sees it ----------

fn task_status(project: &Project, task: &Task) -> &'static str {
    match task.status {
        TaskStatus::Running => "in_progress",
        TaskStatus::Done => "done",
        TaskStatus::Failed => "blocked",
        TaskStatus::Open if project.awaits_approval(task) => "review",
        TaskStatus::Open => {
            let key = task.key();
            let startable = project.startable().iter().any(|t| t.key() == key);
            let claimable = project
                .claimable(&task.owner)
                .iter()
                .any(|t| t.key() == key);
            if startable || claimable {
                "ready"
            } else {
                "todo"
            }
        }
    }
}

fn mtime_ms(path: &Path) -> Option<i64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(since.as_millis()).ok()
}

fn task_json(project: &Project, task: &Task) -> Value {
    let key = task.key();
    let owner_stream = project.stream(&task.owner);
    // Whoever works on it, else whoever asked for it.
    let assigned = task
        .claimed_by
        .as_deref()
        .and_then(|id| project.stream(id))
        .or_else(|| project.stream_for_task(&key))
        .or(owner_stream);
    let milestone = owner_stream
        .and_then(|o| o.task.clone())
        .filter(|k| project.task(k).is_some_and(|t| !t.phases.is_empty()));
    let mut v = json!({
        "id": key,
        "title": task.title,
        "status": task_status(project, task),
        "file": task.rel_path(),
        "pool": project.is_pool_task(task),
    });
    if let Some(a) = assigned {
        v["owner"] = json!(role_id(a));
    }
    if let Some(m) = &milestone {
        v["milestone"] = json!(m);
        if let Some(phase) = project
            .task(m)
            .and_then(|t| t.current_phase().map(|i| t.phases[i].title.clone()))
        {
            v["phase"] = json!(phase);
        }
    }
    if let Some(at) = mtime_ms(&project.root.join(task.rel_path())) {
        v["updatedAt"] = json!(at);
    }
    if let Some(agent) = &task.agent {
        v["agent"] = json!(agent);
    }
    if !task.after.is_empty() {
        v["after"] = json!(task.after);
    }
    if let Some((done, total)) = task.progress() {
        v["steps"] = json!({ "done": done, "total": total });
    }
    v
}

fn node_json(project: &Project, team: &Team, s: &Workstream) -> Value {
    let member = team.member_for(&s.backend);
    let task = project.task_of(s);
    let mut v = json!({
        "id": role_id(s),
        "role": role_json(s),
        "title": s.name,
        "agent": s.backend,
        "children": children_json(project, team, &s.id),
    });
    if let Some(m) = member {
        v["member"] = json!(m);
        if let Some(model) = team.members.get(m).and_then(|m| m.model.clone()) {
            v["model"] = json!(model);
        }
    }
    if let Some(t) = task {
        v["currentTask"] = json!(t.title);
    }
    if let Some(thread) = &s.thread_id {
        v["thread"] = json!(thread);
    }
    v
}

fn children_json(project: &Project, team: &Team, parent: &str) -> Vec<Value> {
    project
        .file
        .streams
        .iter()
        .filter(|s| !s.retired && s.parent.as_deref() == Some(parent))
        .map(|s| node_json(project, team, s))
        .collect()
}

fn org_json(project: &Project, team: &Team) -> Value {
    if let Some(c) = project.controller() {
        let mut v = node_json(project, team, c);
        // Streams with no parent (a lead added by hand) hang under the controller.
        let orphans: Vec<Value> = project
            .file
            .streams
            .iter()
            .filter(|s| !s.retired && s.role != Role::Controller)
            .filter(|s| {
                s.parent
                    .as_deref()
                    .is_none_or(|p| project.stream(p).is_none())
            })
            .map(|s| node_json(project, team, s))
            .collect();
        if let Some(children) = v["children"].as_array_mut() {
            children.extend(orphans);
        }
        return v;
    }
    let tops: Vec<Value> = project
        .file
        .streams
        .iter()
        .filter(|s| {
            !s.retired
                && s.parent
                    .as_deref()
                    .is_none_or(|p| project.stream(p).is_none())
        })
        .map(|s| node_json(project, team, s))
        .collect();
    json!({ "id": "controller", "role": { "kind": "controller" }, "children": tops })
}

fn notice_ms(at: &str) -> i64 {
    jiff::civil::DateTime::strptime("%Y-%m-%d %H:%M", at)
        .ok()
        .and_then(|d| d.to_zoned(jiff::tz::TimeZone::UTC).ok())
        .map_or(0, |z| z.timestamp().as_millisecond())
}

fn view(project: &Project, team: &Team) -> Value {
    let done = project
        .tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Done)
        .count();
    let needs = project
        .tasks
        .iter()
        .filter(|t| project.awaits_approval(t))
        .count();
    let milestones: Vec<Value> = project
        .file
        .streams
        .iter()
        .filter_map(|s| {
            let t = project.task_of(s)?;
            (!t.phases.is_empty()).then(|| {
                json!({
                    "id": t.key(),
                    "title": t.title,
                    "phases": t.phases.iter().enumerate().map(|(i, p)| {
                        let (done, total) = p.progress();
                        json!({ "id": format!("{}#{}", t.key(), i + 1), "title": p.title, "done": done, "total": total })
                    }).collect::<Vec<_>>(),
                })
            })
        })
        .collect();
    let members: Vec<Value> = TeamView::of(team)
        .members
        .iter()
        .map(|m| {
            let mut v = json!({ "name": m.name, "agent": m.agent, "notes": m.notes, "standIns": m.stand_ins });
            if let Some(x) = &m.model {
                v["model"] = json!(x);
            }
            if let Some(x) = &m.effort {
                v["effort"] = json!(x);
            }
            v
        })
        .collect();
    let notices: Vec<Value> = project
        .notices
        .iter()
        .map(|n| json!({ "at": notice_ms(&n.at), "from": n.from, "to": n.to, "text": n.text }))
        .collect();
    let roles: Vec<Value> = project
        .file
        .streams
        .iter()
        .filter(|s| !s.retired)
        .map(|s| json!({ "id": role_id(s) }))
        .collect();
    json!({
        "root": project.root,
        "name": project.file.name,
        "autopilot": project.file.autopilot.on,
        "progress": { "done": done, "total": project.tasks.len() },
        "roles": roles,
        "needs": needs,
        "brief": project.brief,
        "plan": project.plan,
        "milestones": milestones,
        "tasks": project.tasks.iter().map(|t| task_json(project, t)).collect::<Vec<_>>(),
        "org": org_json(project, team),
        "team": members,
        "notices": notices,
        "config": {
            "maxWorkers": project.file.max_workers,
            "approveLeads": project.file.approve_leads,
            "worktrees": project.file.worktrees,
            "autopilot": project.file.autopilot,
        },
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootArgs {
    root: PathBuf,
    /// The app-wide team file, used when the project has none.
    #[serde(default)]
    team_fallback: Option<PathBuf>,
}

/// `project.open {root}` → `ProjectView`. Reads only.
pub fn open(args: RootArgs) -> Result<Value, String> {
    let project = load(&args.root)?;
    let (team, _) = load_team(Some(&args.root), args.team_fallback.as_deref());
    Ok(view(&project, &team))
}

#[derive(Deserialize)]
pub struct InitArgs {
    root: PathBuf,
    #[serde(default)]
    brief: Option<String>,
}

/// `project.init {root, brief?}` → `{root, name}`. Makes the folder an Atrium
/// project the way the app does; a brief replaces the empty template only.
pub fn init(args: InitArgs) -> Result<Value, String> {
    let fresh = !crate::store::is_project(&args.root);
    let project = Project::open(&args.root).map_err(err)?;
    if let Some(brief) = args.brief.filter(|b| !b.trim().is_empty()) {
        let path = project.dir().join("brief.md");
        let current = fs::read_to_string(&path).unwrap_or_default();
        let template = current.trim().is_empty()
            || current.contains("Shared conventions and decisions every workstream follows");
        if fresh || template {
            write_atomic(&path, format!("{}\n", brief.trim()).as_bytes()).map_err(err)?;
        }
    }
    Ok(json!({ "root": project.root, "name": project.file.name }))
}

// ---------- tasks ----------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskCreateArgs {
    root: PathBuf,
    title: String,
    /// A role id; default: the lead of `milestone`, else the first lead
    /// (the controller with `pool: false`).
    #[serde(default)]
    owner: Option<String>,
    /// A milestone id (the key of a lead's task).
    #[serde(default)]
    milestone: Option<String>,
    #[serde(default)]
    pool: Option<bool>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    scope: Vec<String>,
    #[serde(default)]
    roadmap: Vec<String>,
}

fn pick_owner(
    project: &Project,
    owner: Option<&str>,
    milestone: Option<&str>,
    pool: Option<bool>,
) -> Result<String, String> {
    if let Some(role) = owner.filter(|o| !o.is_empty()) {
        return Ok(need_stream(project, role)?.id.clone());
    }
    if let Some(m) = milestone
        && let Some(lead) = project.stream_for_task(m)
    {
        return Ok(lead.id.clone());
    }
    let lead = project
        .file
        .streams
        .iter()
        .find(|s| s.role == Role::Lead && !s.retired);
    let controller = project.controller();
    let chosen = if pool == Some(false) {
        controller.or(lead)
    } else {
        lead.or(controller)
    };
    chosen
        .map(|s| s.id.clone())
        .ok_or_else(|| "the project has no lead or controller to own a task".to_string())
}

/// `project.task.create {root, title, owner?, milestone?, pool?, body?, agent?, scope?, roadmap?}` → `Task`.
pub fn task_create(args: TaskCreateArgs) -> Result<Value, String> {
    let mut project = load(&args.root)?;
    if args.title.trim().is_empty() {
        return Err("a task needs a title".into());
    }
    let owner = pick_owner(
        &project,
        args.owner.as_deref(),
        args.milestone.as_deref(),
        args.pool,
    )?;
    let text = Task::render(
        &args.title,
        args.agent.as_deref().unwrap_or(""),
        &args.scope,
        &args.body,
        &args.roadmap,
    );
    let key = project.add_task(&owner, &args.title, &text).map_err(err)?;
    let task = project
        .task(&key)
        .ok_or("the task file did not read back")?;
    Ok(task_json(&project, task))
}

#[derive(Deserialize)]
pub struct TaskPatch {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    milestone: Option<String>,
    #[serde(default)]
    pool: Option<bool>,
}

#[derive(Deserialize)]
pub struct TaskUpdateArgs {
    root: PathBuf,
    /// The task's key, `owner/id`.
    id: String,
    patch: TaskPatch,
}

/// Replaces the title line of a task file.
fn with_title(text: &str, title: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let head = format!("# {}", title.trim());
    match lines.iter().position(|l| l.trim().starts_with("# ")) {
        Some(i) => lines[i] = head,
        None => lines.insert(0, head),
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// `project.task.update {root, id, patch: {title?, owner?, milestone?, pool?}}` → `Task`.
/// A new owner moves the task file (only while the task is open).
pub fn task_update(args: TaskUpdateArgs) -> Result<Value, String> {
    let mut project = load(&args.root)?;
    let mut key = args.id.clone();
    let task = project
        .task(&key)
        .ok_or_else(|| format!("no task {key}"))?
        .clone();
    if let Some(title) = args.patch.title.as_deref().filter(|t| !t.trim().is_empty()) {
        rewrite_task(&mut project, &key, |t| with_title(t, title)).map_err(err)?;
    }
    let moves =
        args.patch.owner.is_some() || args.patch.milestone.is_some() || args.patch.pool.is_some();
    if moves {
        let owner = pick_owner(
            &project,
            args.patch.owner.as_deref(),
            args.patch.milestone.as_deref(),
            args.patch.pool,
        )?;
        if owner != task.owner {
            if task.status != TaskStatus::Open {
                return Err("only an open task can change owner".into());
            }
            let _lock = TaskLock::take(&project).map_err(err)?;
            let from = project.root.join(task.rel_path());
            let dir = project.dir().join("tasks").join(&owner);
            fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let mut id = task.id.clone();
            let mut n = 2;
            while dir.join(format!("{id}.md")).exists() {
                id = format!("{}-{n}", task.id);
                n += 1;
            }
            fs::rename(&from, dir.join(format!("{id}.md"))).map_err(|e| e.to_string())?;
            key = format!("{owner}/{id}");
            project.stamp = None;
            project.reload();
        }
    }
    let task = project
        .task(&key)
        .ok_or("the task file did not read back")?;
    Ok(task_json(&project, task))
}

#[derive(Deserialize)]
pub struct TaskMoveArgs {
    root: PathBuf,
    id: String,
    status: String,
}

/// `project.task.move {root, id, status}` → `Task`. Statuses are the UI's:
/// `todo`/`ready` open it (a claim is released), `review` also takes back
/// the user's go-ahead, `in_progress` marks it running, `done`, `blocked`.
pub fn task_move(args: TaskMoveArgs) -> Result<Value, String> {
    let mut project = load(&args.root)?;
    let key = args.id;
    let task = project
        .task(&key)
        .ok_or_else(|| format!("no task {key}"))?
        .clone();
    match args.status.as_str() {
        "todo" | "ready" | "review" => {
            if task.status == TaskStatus::Running {
                project.release(&key).map_err(err)?;
            } else if task.status != TaskStatus::Open {
                project
                    .set_task_status(&key, TaskStatus::Open)
                    .map_err(err)?;
            }
            if args.status == "review" && project.file.approved.contains(&key) {
                project.file.approved.retain(|k| *k != key);
                project.save().map_err(err)?;
            }
        }
        "in_progress" => project
            .set_task_status(&key, TaskStatus::Running)
            .map_err(err)?,
        "done" => project
            .set_task_status(&key, TaskStatus::Done)
            .map_err(err)?,
        "blocked" => project
            .set_task_status(&key, TaskStatus::Failed)
            .map_err(err)?,
        other => return Err(format!("unknown status {other}")),
    }
    let task = project
        .task(&key)
        .ok_or("the task file did not read back")?;
    Ok(task_json(&project, task))
}

#[derive(Deserialize)]
pub struct TaskApproveArgs {
    root: PathBuf,
    id: String,
    /// `false` turns the task down (it fails and its asker is told).
    #[serde(default = "yes")]
    approve: bool,
}

fn yes() -> bool {
    true
}

/// `project.task.approve {root, id, approve?}`: the user's go-ahead for a
/// lead the controller asked for, or a no.
pub fn task_approve(args: TaskApproveArgs) -> Result<Value, String> {
    let mut project = load(&args.root)?;
    let key = args.id;
    let task = project
        .task(&key)
        .ok_or_else(|| format!("no task {key}"))?
        .clone();
    if args.approve {
        if !project.file.approved.contains(&key) {
            project.file.approved.push(key);
            project.save().map_err(err)?;
        }
    } else {
        project
            .set_task_status(&key, TaskStatus::Failed)
            .map_err(err)?;
        project
            .post(
                YOU,
                vec![task.owner.clone()],
                &format!(
                    "I turned down \"{}\" (`{}`). Rethink that part of the plan.",
                    task.title,
                    task.key()
                ),
            )
            .map_err(err)?;
    }
    Ok(Value::Null)
}

#[derive(Deserialize)]
pub struct NoticeArgs {
    root: PathBuf,
    text: String,
    /// Role ids, stream ids or groups (`@leads`); default everyone.
    #[serde(default)]
    to: Vec<String>,
}

/// `project.notice.add {root, text, to?}`: a notice from the user.
pub fn notice_add(args: NoticeArgs) -> Result<Value, String> {
    let mut project = load(&args.root)?;
    if args.text.trim().is_empty() {
        return Err("a notice needs text".into());
    }
    let to: Vec<String> = if args.to.is_empty() {
        vec!["all".into()]
    } else {
        args.to
            .iter()
            .map(|t| stream_of(&project, t).map_or_else(|| t.clone(), |s| s.id.clone()))
            .collect()
    };
    project.post(YOU, to, &args.text).map_err(err)?;
    Ok(Value::Null)
}

#[derive(Deserialize)]
pub struct AutopilotArgs {
    root: PathBuf,
    on: bool,
}

/// `project.autopilot.set {root, on}`.
pub fn autopilot_set(args: AutopilotArgs) -> Result<Value, String> {
    let mut project = load(&args.root)?;
    project.file.autopilot.on = args.on;
    project.save().map_err(err)?;
    Ok(Value::Null)
}

// ---------- starting roles ----------

/// What a placement needs: the team, who is spent, what can start.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Placing {
    #[serde(default)]
    team_fallback: Option<PathBuf>,
    /// Spent members: name → Unix seconds when usable again.
    #[serde(default)]
    quotas: BTreeMap<String, i64>,
    /// Backends that can start now; left out, every backend can.
    #[serde(default)]
    ready: Option<Vec<String>>,
    /// Unix seconds.
    #[serde(default)]
    now: Option<i64>,
    /// This computer's name, for projects spread over several.
    #[serde(default)]
    device: Option<String>,
}

struct Ctx {
    team: Team,
    quotas: Quotas,
    ready: Option<Vec<String>>,
    now: i64,
    device: String,
}

impl Ctx {
    fn of(root: &Path, p: &Placing) -> Self {
        Self {
            team: load_team(Some(root), p.team_fallback.as_deref()).0,
            quotas: Quotas {
                spent: p.quotas.clone(),
            },
            ready: p.ready.clone(),
            now: p.now.unwrap_or_else(|| jiff::Timestamp::now().as_second()),
            device: p.device.clone().unwrap_or_else(|| "this computer".into()),
        }
    }

    fn is_ready(&self, backend: &str) -> bool {
        crate::team::is_ready(&self.ready, backend)
    }

    fn place(&self, wanted: &str) -> Result<Pick, atrium_team::Wait> {
        schedule::place(
            &self.team,
            wanted,
            &self.quotas,
            |b| self.is_ready(b),
            self.now,
        )
    }

    /// The kickoff the app sends: the project's, the team's, the place's.
    fn kickoff(&self, project: &Project, stream: &Workstream, base: String) -> String {
        let extras = schedule::kickoff_extras(
            &self.team,
            &self.quotas,
            &stream.backend,
            stream.role != Role::Worker,
            |b| self.is_ready(b),
            self.now,
        );
        let text = format!("{base}{extras}");
        let place = spread::briefing(project, stream, &self.device);
        if place.is_empty() {
            text
        } else {
            format!("{text}\n\n{place}")
        }
    }
}

fn clock(ts: i64) -> String {
    jiff::Timestamp::from_second(ts)
        .map(|t| t.strftime("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_default()
}

/// A `launch` action for a started workstream.
fn launch(project: &Project, stream: &Workstream, placed: Option<&Pick>, prompt: String) -> Value {
    let worktree = project.file.worktrees && stream.role != Role::Controller;
    let mut v = json!({
        "kind": "launch",
        "role": role_id(stream),
        "stream": stream.id,
        "name": stream.name,
        "agent": placed.map_or(stream.backend.as_str(), |p| p.agent.as_str()),
        "prompt": prompt,
        "cwd": project.work_dir(&stream.id),
        "worktree": worktree,
    });
    if worktree {
        v["branch"] = json!(Project::branch_of(&stream.id));
    }
    if let Some(task) = &stream.task {
        v["task"] = json!(task);
    }
    if let Some(p) = placed {
        v["member"] = json!(p.member);
        if let Some(m) = &p.model {
            v["model"] = json!(m);
        }
        if let Some(e) = &p.effort {
            v["effort"] = json!(e);
        }
        if let Some((member, _)) = &p.instead_of {
            v["standingInFor"] = json!(member);
        }
    }
    v
}

#[derive(Deserialize)]
pub struct KickoffArgs {
    root: PathBuf,
    role: String,
    /// A task key, to name the prompt of a worker not started yet.
    #[serde(default)]
    task: Option<String>,
    #[serde(flatten)]
    placing: Placing,
}

/// `project.kickoff {root, role, task?}` → `{prompt, role, cwd, worktree}`:
/// what the app would send that role when it starts.
pub fn kickoff(args: KickoffArgs) -> Result<Value, String> {
    let project = load(&args.root)?;
    let ctx = Ctx::of(&args.root, &args.placing);
    let stream = match stream_of(&project, &args.role) {
        Some(s) => s.clone(),
        None => {
            let key = args
                .task
                .as_deref()
                .ok_or_else(|| format!("no role {}", args.role))?;
            let task = project.task(key).ok_or_else(|| format!("no task {key}"))?;
            let backend = task
                .agent
                .clone()
                .or_else(|| project.stream(&task.owner).map(|o| o.backend.clone()))
                .unwrap_or_default();
            project
                .stream_from_task(task, &backend)
                .ok_or_else(|| format!("{key} cannot start a role"))?
        }
    };
    let prompt = ctx.kickoff(&project, &stream, project.kickoff(&stream));
    let mut v = launch(&project, &stream, None, prompt);
    v["kind"] = json!("kickoff");
    Ok(v)
}

#[derive(Deserialize)]
pub struct RoleStartArgs {
    root: PathBuf,
    role: String,
    /// The backend, for a role that does not exist yet.
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    goal: Option<String>,
    #[serde(default)]
    scope: Vec<String>,
    #[serde(flatten)]
    placing: Placing,
}

/// `project.role.start {root, role, agent?, name?, goal?, scope?}` →
/// `{launch}`. An existing role gets its kickoff again; `controller` or
/// `lead:<name>` that does not exist yet is created (and saved).
pub fn role_start(args: RoleStartArgs) -> Result<Value, String> {
    let mut project = load(&args.root)?;
    let ctx = Ctx::of(&args.root, &args.placing);
    let stream = match stream_of(&project, &args.role).cloned() {
        Some(s) => s,
        None => {
            let (role, name) = match args.role.as_str() {
                "controller" => (Role::Controller, "Controller".to_string()),
                r if r.starts_with("lead:") => (
                    Role::Lead,
                    args.name.clone().unwrap_or_else(|| r[5..].to_string()),
                ),
                other => {
                    return Err(format!(
                        "cannot create role {other}: only the controller and leads can be added"
                    ));
                }
            };
            if role == Role::Controller && project.controller().is_some() {
                return Err("the project already has a controller".into());
            }
            let wanted = args.agent.clone().unwrap_or_else(|| "claude".into());
            let placed = ctx
                .place(&wanted)
                .map_err(|_| "everyone who could take this is spent right now".to_string())?;
            let stream = Workstream {
                id: project.new_id(args.name.as_deref().unwrap_or(&name)),
                name: args.name.clone().unwrap_or(name),
                backend: placed.agent,
                goal: args.goal.clone().unwrap_or_default().trim().to_string(),
                scope: args.scope.clone(),
                notices_seen: project.notices.len(),
                created_at: jiff::Timestamp::now().as_second(),
                role,
                parent: match role {
                    Role::Lead => project.controller().map(|c| c.id.clone()),
                    _ => None,
                },
                ..Workstream::default()
            };
            project.file.streams.push(stream.clone());
            project.save().map_err(err)?;
            stream
        }
    };
    let placed = ctx
        .place(args.agent.as_deref().unwrap_or(&stream.backend))
        .ok();
    let prompt = ctx.kickoff(&project, &stream, project.kickoff(&stream));
    Ok(json!({ "launch": launch(&project, &stream, placed.as_ref(), prompt) }))
}

#[derive(Deserialize)]
pub struct BindArgs {
    root: PathBuf,
    role: String,
    /// The agent's session or terminal.
    thread: String,
}

/// `project.bind {root, role, thread}`: remembers which session runs a role.
pub fn bind(args: BindArgs) -> Result<Value, String> {
    let mut project = load(&args.root)?;
    let id = need_stream(&project, &args.role)?.id.clone();
    project.bind_thread(&id, &args.thread).map_err(err)?;
    Ok(Value::Null)
}

#[derive(Deserialize)]
pub struct FailedArgs {
    root: PathBuf,
    role: String,
    reason: String,
}

/// `project.stream.failed {root, role, reason}` → `{parent}`: the agent could
/// not start; its task fails (or the pool worker retires) and its parent is told.
pub fn stream_failed(args: FailedArgs) -> Result<Value, String> {
    let mut project = load(&args.root)?;
    let id = need_stream(&project, &args.role)?.id.clone();
    let parent = project.fail_start(&id, &args.reason).map_err(err)?;
    Ok(json!({ "parent": parent.and_then(|p| project.stream(&p).map(role_id)) }))
}

fn wake_of(why: &str, attempt: Option<u32>) -> Result<Wake, String> {
    Ok(match why {
        "turn_ended" => Wake::TurnEnded,
        "heartbeat" => Wake::Heartbeat,
        "stalled" => Wake::Stalled,
        "retry" => Wake::Retry(attempt.unwrap_or(1)),
        "restarted" => Wake::Restarted,
        other => return Err(format!("unknown wake {other}")),
    })
}

#[derive(Deserialize)]
pub struct ContinuationArgs {
    root: PathBuf,
    role: String,
    /// `turn_ended`, `heartbeat`, `stalled`, `retry` or `restarted`.
    why: String,
    #[serde(default)]
    attempt: Option<u32>,
}

/// `project.continuation {root, role, why, attempt?}` → `{text}`: what to tell
/// an agent when waking it to carry on.
pub fn continuation(args: ContinuationArgs) -> Result<Value, String> {
    let project = load(&args.root)?;
    let stream = need_stream(&project, &args.role)?;
    let text = project.continuation(stream, wake_of(&args.why, args.attempt)?);
    Ok(json!({ "text": text, "hasWork": project.has_work(stream) }))
}

// ---------- the scheduler ----------

#[derive(Deserialize)]
pub struct Wakeup {
    role: String,
    why: String,
    #[serde(default)]
    attempt: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NextArgs {
    root: PathBuf,
    /// Roles whose agent is running now.
    #[serde(default)]
    live: Vec<String>,
    /// Of those, the ones in the middle of a turn.
    #[serde(default)]
    busy: Vec<String>,
    /// Autopilot wakes that are due (the worker keeps the clocks).
    #[serde(default)]
    wakes: Vec<Wakeup>,
    /// Agents running at once, at most.
    #[serde(default = "twelve")]
    max_running: usize,
    #[serde(flatten)]
    placing: Placing,
}

fn twelve() -> usize {
    12
}

/// `project.next {root, live, busy, wakes, maxRunning, quotas, ready, now}` →
/// `{actions}`: one pass of the app's scheduler. It changes the project's
/// files the way the app does (a task starts running, a worker is added and
/// claims a task, finished work is reported to its parent, notices are
/// marked as handed over) and returns what to do with agents:
///
/// - `launch {role, stream, agent, member?, model?, effort?, prompt, cwd, worktree, task?, standingInFor?}`
/// - `send {role, text}` to a running agent (notices, a pool claim, a wake)
/// - `stop {role}` for a pool worker that was retired
/// - `need {title, detail, role?, needKind}` for the user
///
/// Call it again after launching (and `project.bind`); it is safe to repeat.
pub fn next(args: NextArgs) -> Result<Value, String> {
    let root = args.root.as_path();
    let mut project = load(root)?;
    let ctx = Ctx::of(root, &args.placing);
    let live: HashSet<String> = args
        .live
        .iter()
        .filter_map(|r| stream_of(&project, r).map(|s| s.id.clone()))
        .collect();
    let busy: HashSet<String> = args
        .busy
        .iter()
        .filter_map(|r| stream_of(&project, r).map(|s| s.id.clone()))
        .collect();
    let mut actions: Vec<Value> = Vec::new();
    let mut running = live.len();

    // Claims whose worker is gone go back to the pool.
    let orphans: Vec<(String, String)> = project
        .tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Running)
        .filter_map(|t| Some((t.key(), t.claimed_by.clone()?)))
        .filter(|(_, owner)| project.stream(owner).is_none_or(|w| w.retired))
        .collect();
    for (key, owner) in orphans {
        let _ = project.release_if_owner(&key, Some(&owner));
    }

    // The user's go-ahead is needed for some leads.
    for task in project.tasks.iter().filter(|t| project.awaits_approval(t)) {
        actions.push(json!({
            "kind": "need",
            "needKind": "brief",
            "title": task.title,
            "detail": first_words(&task.body),
            "role": project.stream(&task.owner).map(role_id),
            "task": task.key(),
        }));
    }

    // Tasks of the controller start leads.
    let startable: Vec<Task> = project
        .startable_with(|task| {
            let Some(owner) = project.stream(&task.owner) else {
                return false;
            };
            ctx.place(task.agent.as_deref().unwrap_or(&owner.backend))
                .is_ok()
        })
        .into_iter()
        .cloned()
        .collect();
    for task in startable {
        if running >= args.max_running {
            break;
        }
        let Some(owner) = project.stream(&task.owner) else {
            continue;
        };
        let Ok(placed) = ctx.place(task.agent.as_deref().unwrap_or(&owner.backend)) else {
            continue;
        };
        let Some(stream) = project.stream_from_task(&task, &placed.agent) else {
            continue;
        };
        project
            .set_task_status(&task.key(), TaskStatus::Running)
            .map_err(err)?;
        project.file.streams.push(stream.clone());
        project.save().map_err(err)?;
        let prompt = ctx.kickoff(&project, &stream, project.kickoff(&stream));
        actions.push(launch(&project, &stream, Some(&placed), prompt));
        if let Some(text) = schedule::stand_in_notice(&task.id, &placed, clock) {
            project
                .post(ATRIUM, vec![task.owner.clone()], &text)
                .map_err(err)?;
        }
        running += 1;
    }

    // Each lead's pool: workers for its ready tasks, and live idle ones claim.
    let leads: Vec<Workstream> = project
        .file
        .streams
        .iter()
        .filter(|s| s.role == Role::Lead && !s.retired)
        .cloned()
        .collect();
    let workers: Vec<Workstream> = project
        .file
        .streams
        .iter()
        .filter(|s| s.role == Role::Worker && s.task.is_none() && s.parent.is_some() && !s.retired)
        .cloned()
        .collect();
    for worker in workers {
        if !live.contains(&worker.id) || busy.contains(&worker.id) {
            continue;
        }
        let held = project.task_of(&worker).map(Task::key);
        if held.is_none() && project.pool_session_full(&worker.id) {
            if project.retire_pool_worker(&worker.id, None).map_err(err)? {
                actions.push(json!({ "kind": "stop", "role": role_id(&worker) }));
            }
            continue;
        }
        if held.is_none()
            && let Some(lead) = worker.parent.as_deref()
            && !project.claimable(lead).is_empty()
            && project.claim_next(&worker.id).map_err(err)?.is_some()
        {
            let text = project.continuation(&worker, Wake::TurnEnded);
            actions.push(json!({ "kind": "send", "role": role_id(&worker), "text": text }));
        }
    }
    for lead in &leads {
        let ready = project.claimable(&lead.id);
        let wanted = match ready.first().and_then(|t| t.agent.as_deref()) {
            Some(b) if ready.iter().all(|t| t.agent.as_deref() == Some(b)) => b.to_string(),
            _ => lead.backend.clone(),
        };
        for _ in 0..project.workers_needed(&lead.id) {
            if running >= args.max_running {
                break;
            }
            let Ok(placed) = ctx.place(&wanted) else {
                break;
            };
            if !ctx.is_ready(&placed.agent) || project.workers_needed(&lead.id) == 0 {
                break;
            }
            let Some(worker) = project.pool_worker(&lead.id, &placed.agent) else {
                break;
            };
            match project.prepare_pool_start(&worker).map_err(err)? {
                Some(base) => {
                    let prompt = ctx.kickoff(&project, &worker, base);
                    actions.push(launch(&project, &worker, Some(&placed), prompt));
                    if let Some(text) = schedule::stand_in_notice(&worker.id, &placed, clock) {
                        project
                            .post(ATRIUM, vec![lead.id.clone()], &text)
                            .map_err(err)?;
                    }
                    running += 1;
                }
                None => {
                    // Another worker took the last task first.
                    let _ = project.retire_pool_worker(&worker.id, None);
                    break;
                }
            }
        }
        // Tell the lead when its pool runs low.
        project.queue_pool_report(&lead.id).map_err(err)?;
    }

    // Finished work is told to its parent (as a notice).
    let finished: Vec<String> = project
        .unreported()
        .into_iter()
        .filter(|(s, _)| !busy.contains(&s.id))
        .map(|(s, _)| s.id.clone())
        .collect();
    for id in finished {
        project.report_finished(&id).map_err(err)?;
    }

    // Notices that address a running, idle agent go to it now.
    let streams: Vec<Workstream> = project
        .file
        .streams
        .iter()
        .filter(|s| !s.retired)
        .cloned()
        .collect();
    let mut woken: HashSet<String> = HashSet::new();
    for s in &streams {
        if !live.contains(&s.id) || busy.contains(&s.id) || project.notice_delivery_blocked(&s.id) {
            continue;
        }
        let unread = project.unread(&s.id);
        if unread.is_empty() || !unread.iter().any(|n| project.notice_wakes(n, &s.id)) {
            continue;
        }
        let text = with_notices(
            &unread,
            "Read them, act on what concerns you, then carry on.",
        );
        project.mark_read(&s.id);
        project.save().map_err(err)?;
        woken.insert(s.id.clone());
        actions.push(json!({ "kind": "send", "role": role_id(s), "text": text }));
    }

    // Autopilot: wakes the worker's clocks say are due.
    if project.file.autopilot.on {
        for wake in &args.wakes {
            let Some(stream) = stream_of(&project, &wake.role).cloned() else {
                continue;
            };
            if !live.contains(&stream.id)
                || busy.contains(&stream.id)
                || woken.contains(&stream.id)
                || !project.has_work(&stream)
            {
                continue;
            }
            let text = project.continuation(&stream, wake_of(&wake.why, wake.attempt)?);
            let unread = project.unread(&stream.id);
            let text = with_notices(&unread, &text);
            project.mark_read(&stream.id);
            project.save().map_err(err)?;
            actions.push(
                json!({ "kind": "send", "role": role_id(&stream), "text": text, "why": wake.why }),
            );
        }
    }
    Ok(json!({ "actions": actions }))
}

/// The first line of `text`, shortened.
fn first_words(text: &str) -> String {
    let line = atrium_project::first_line(text);
    line.chars().take(200).collect()
}

/// The `.atrium` folder name, for tests and messages.
pub const FOLDER: &str = DIR;

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn init_with_leads(root: &Path) {
        init(InitArgs {
            root: root.to_path_buf(),
            brief: Some("Build a calculator.".into()),
        })
        .unwrap();
        let mut p = load(root).unwrap();
        p.file.streams.push(Workstream {
            id: "boss".into(),
            name: "Boss".into(),
            backend: "claude".into(),
            role: Role::Controller,
            ..Default::default()
        });
        p.file.streams.push(Workstream {
            id: "web".into(),
            name: "Web".into(),
            backend: "codex".into(),
            role: Role::Lead,
            parent: Some("boss".into()),
            ..Default::default()
        });
        p.save().unwrap();
    }

    fn root_args(root: &Path) -> RootArgs {
        RootArgs {
            root: root.to_path_buf(),
            team_fallback: None,
        }
    }

    #[test]
    fn a_project_is_opened_as_a_view_with_an_org_tree() {
        let d = dir();
        init_with_leads(d.path());
        let mut p = load(d.path()).unwrap();
        p.file.streams.push(Workstream {
            id: "web-w1".into(),
            name: "w".into(),
            backend: "codex".into(),
            role: Role::Worker,
            parent: Some("web".into()),
            ..Default::default()
        });
        p.save().unwrap();
        let v = open(root_args(d.path())).unwrap();
        assert_eq!(v["brief"].as_str().unwrap().trim(), "Build a calculator.");
        assert_eq!(v["org"]["id"], "controller");
        assert_eq!(v["org"]["children"][0]["id"], "lead:web");
        assert_eq!(
            v["org"]["children"][0]["children"][0]["id"],
            "worker:web/web-w1"
        );
        assert_eq!(
            v["org"]["children"][0]["children"][0]["role"]["lead"],
            "web"
        );
        assert_eq!(v["autopilot"], false);
        assert!(open(root_args(&d.path().join("none"))).is_err());
    }

    #[test]
    fn tasks_are_created_moved_and_updated() {
        let d = dir();
        init_with_leads(d.path());
        let root = d.path().to_path_buf();
        let t = task_create(TaskCreateArgs {
            root: root.clone(),
            title: "Parse input".into(),
            owner: Some("lead:web".into()),
            milestone: None,
            pool: None,
            body: "Do it.".into(),
            agent: None,
            scope: vec![],
            roadmap: vec![],
        })
        .unwrap();
        assert_eq!(t["id"], "web/parse-input");
        assert_eq!(t["owner"], "lead:web");
        assert_eq!(t["pool"], true);
        assert_eq!(t["status"], "ready");
        let moved = task_move(TaskMoveArgs {
            root: root.clone(),
            id: "web/parse-input".into(),
            status: "in_progress".into(),
        })
        .unwrap();
        assert_eq!(moved["status"], "in_progress");
        let back = task_move(TaskMoveArgs {
            root: root.clone(),
            id: "web/parse-input".into(),
            status: "todo".into(),
        })
        .unwrap();
        assert_eq!(back["status"], "ready");
        let done = task_move(TaskMoveArgs {
            root: root.clone(),
            id: "web/parse-input".into(),
            status: "done".into(),
        })
        .unwrap();
        assert_eq!(done["status"], "done");
        let renamed = task_update(TaskUpdateArgs {
            root: root.clone(),
            id: "web/parse-input".into(),
            patch: TaskPatch {
                title: Some("Parse text".into()),
                owner: None,
                milestone: None,
                pool: None,
            },
        })
        .unwrap();
        assert_eq!(renamed["title"], "Parse text");
        let t2 = task_create(TaskCreateArgs {
            root: root.clone(),
            title: "Plan".into(),
            owner: None,
            milestone: None,
            pool: Some(false),
            body: String::new(),
            agent: None,
            scope: vec![],
            roadmap: vec![],
        })
        .unwrap();
        assert_eq!(t2["owner"], "controller");
        let moved = task_update(TaskUpdateArgs {
            root: root.clone(),
            id: t2["id"].as_str().unwrap().into(),
            patch: TaskPatch {
                title: None,
                owner: Some("lead:web".into()),
                milestone: None,
                pool: None,
            },
        })
        .unwrap();
        assert_eq!(moved["id"], "web/plan");
        let v = open(root_args(d.path())).unwrap();
        assert_eq!(v["tasks"].as_array().unwrap().len(), 2);
        assert_eq!(v["progress"]["done"], 1);
    }

    #[test]
    fn notices_and_autopilot_are_written() {
        let d = dir();
        init_with_leads(d.path());
        notice_add(NoticeArgs {
            root: d.path().into(),
            text: "Use Rust.".into(),
            to: vec!["lead:web".into()],
        })
        .unwrap();
        autopilot_set(AutopilotArgs {
            root: d.path().into(),
            on: true,
        })
        .unwrap();
        let v = open(root_args(d.path())).unwrap();
        assert_eq!(v["autopilot"], true);
        assert_eq!(v["notices"][0]["text"], "Use Rust.");
        assert_eq!(v["notices"][0]["from"], "you");
        assert_eq!(v["notices"][0]["to"][0], "web");
    }

    #[test]
    fn the_scheduler_starts_leads_workers_and_hands_over_notices() {
        let d = dir();
        init_with_leads(d.path());
        let root = d.path().to_path_buf();
        // The controller asks for a lead.
        let mut p = load(&root).unwrap();
        p.add_task(
            "boss",
            "API",
            "# API\nstatus: open\nagent: codex\n\nBuild the API.\n",
        )
        .unwrap();
        let next_args = |live: &[&str]| NextArgs {
            root: root.clone(),
            live: live.iter().map(|s| s.to_string()).collect(),
            busy: vec![],
            wakes: vec![],
            max_running: 12,
            placing: Placing {
                now: Some(1000),
                ..Placing::default()
            },
        };
        let out = next(next_args(&[])).unwrap();
        let actions = out["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 1, "{out}");
        assert_eq!(actions[0]["kind"], "launch");
        assert_eq!(actions[0]["role"], "lead:api");
        assert_eq!(actions[0]["agent"], "codex");
        assert!(
            actions[0]["prompt"]
                .as_str()
                .unwrap()
                .contains("Build the API.")
        );
        let p = load(&root).unwrap();
        assert_eq!(p.task("boss/api").unwrap().status, TaskStatus::Running);
        assert_eq!(p.stream("api").unwrap().parent.as_deref(), Some("boss"));
        // Nothing more to start; repeating is safe.
        assert!(
            next(next_args(&["lead:api"])).unwrap()["actions"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        // The lead fills its pool: a worker starts and claims one task.
        let mut p = load(&root).unwrap();
        p.add_task("api", "Parse", "# Parse\nstatus: open\n\nParse it.\n")
            .unwrap();
        p.add_task(
            "api",
            "Render",
            "# Render\nstatus: open\nafter: parse\n\nRender it.\n",
        )
        .unwrap();
        let out = next(next_args(&["lead:api"])).unwrap();
        let actions = out["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 1, "{out}");
        assert_eq!(actions[0]["role"], "worker:api/api-w1");
        assert!(actions[0]["prompt"].as_str().unwrap().contains("Parse"));
        let p = load(&root).unwrap();
        assert_eq!(
            p.task("api/parse").unwrap().claimed_by.as_deref(),
            Some("api-w1")
        );

        // The worker finishes; the lead hears about it, the notice reaches it.
        let mut p = load(&root).unwrap();
        p.set_task_status("api/parse", TaskStatus::Done).unwrap();
        let out = next(next_args(&["lead:api", "worker:api/api-w1"])).unwrap();
        let sends: Vec<_> = out["actions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a["kind"] == "send")
            .collect();
        assert!(sends.iter().any(|a| a["role"] == "lead:api"), "{out}");
        // The worker claims the next ready task.
        assert!(
            sends.iter().any(|a| a["role"] == "worker:api/api-w1"),
            "{out}"
        );
    }

    #[test]
    fn roles_start_and_kickoffs_are_the_apps() {
        let d = dir();
        init(InitArgs {
            root: d.path().into(),
            brief: None,
        })
        .unwrap();
        let out = role_start(RoleStartArgs {
            root: d.path().into(),
            role: "controller".into(),
            agent: Some("claude".into()),
            name: None,
            goal: Some("Ship it".into()),
            scope: vec![],
            placing: Placing::default(),
        })
        .unwrap();
        assert_eq!(out["launch"]["role"], "controller");
        assert!(
            out["launch"]["prompt"]
                .as_str()
                .unwrap()
                .contains("Ship it")
        );
        let p = load(d.path()).unwrap();
        let expected = p.kickoff(p.controller().unwrap());
        assert!(
            out["launch"]["prompt"]
                .as_str()
                .unwrap()
                .starts_with(expected.as_str())
        );
        let again = kickoff(KickoffArgs {
            root: d.path().into(),
            role: "controller".into(),
            task: None,
            placing: Placing::default(),
        })
        .unwrap();
        assert_eq!(again["role"], "controller");
        assert!(
            role_start(RoleStartArgs {
                root: d.path().into(),
                role: "controller".into(),
                agent: None,
                name: None,
                goal: None,
                scope: vec![],
                placing: Placing::default(),
            })
            .is_ok()
        );
        bind(BindArgs {
            root: d.path().into(),
            role: "controller".into(),
            thread: "t1".into(),
        })
        .unwrap();
        assert_eq!(
            load(d.path())
                .unwrap()
                .controller()
                .unwrap()
                .thread_id
                .as_deref(),
            Some("t1")
        );
    }
}
