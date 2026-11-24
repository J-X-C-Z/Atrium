//! The `atrium` MCP server: the calls of [`crate::agent`] as tools, over
//! stdio (newline-delimited JSON-RPC), for one workstream.
//!
//! Atrium registers it in every project session it starts, as
//! `atrium mcp --project <root> --stream <id>`, so the agent is always
//! itself: it cannot act as another workstream. The files stay the source of
//! truth, so an agent without MCP can still do everything by hand.
//!
//! [`handle`] answers one JSON-RPC message and knows no transport: stdio
//! ([`serve`]) and the node's network route (`/v1/mcp`, for agents on other
//! computers) both call it. In a project spread over several computers, a
//! [`Devices`] adds the `device_*` tools.

use std::{
    io::{BufRead, Write},
    path::PathBuf,
};

use anyhow::{Result, anyhow};
use atrium_engine::{
    contracts::ThreadId,
    mcp::{
        EnvironmentReadResult, McpConfig, McpError, McpService, ProjectListResult, ProjectWire,
        SystemClock, tools as engine_tools,
    },
    store::EventStore,
};
use serde_json::{Value, json};

use crate::{Project, Role, agent::NewTask, spread::Spread};

const PROTOCOL: &str = "2025-06-18";

/// How the `device_*` tools reach other computers. Offered only in spread
/// projects, and only where a transport supplies one.
pub trait Devices: Send + Sync {
    /// Runs the device tool `name` as workstream `me`.
    fn call(&self, project: &Project, me: &str, name: &str, args: &Value) -> Result<String>;
}

/// How the server is told who it serves, from its command line.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub root: PathBuf,
    pub stream: String,
}

impl Target {
    /// Reads `--project <root> --stream <id>` (or `ATRIUM_PROJECT` and
    /// `ATRIUM_STREAM`) from `args`, the arguments after `mcp`.
    pub fn from_args(args: &[String]) -> Result<Self> {
        let mut root = std::env::var("ATRIUM_PROJECT").ok();
        let mut stream = std::env::var("ATRIUM_STREAM").ok();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--project" => root = args.next().cloned(),
                "--stream" => stream = args.next().cloned(),
                other => return Err(anyhow!("unknown argument {other}")),
            }
        }
        Ok(Self {
            root: PathBuf::from(root.ok_or_else(|| anyhow!("--project <folder> is required"))?),
            stream: stream.ok_or_else(|| anyhow!("--stream <id> is required"))?,
        })
    }
}

/// Where the orchestration toolkit's threads and events live.
///
/// The merged server offers the orchestration tools only when this is set.
/// The app passes it in the environment of every agent session it starts
/// (`ATRIUM_ENGINE_DB` and `ATRIUM_THREAD`); without it, the project tools
/// behave exactly as before.
#[derive(Clone, Debug, PartialEq)]
pub struct EngineConfig {
    /// The SQLite database the engine stores events and projections in.
    pub database: PathBuf,
    /// The engine thread this session serves, when the agent runs in one.
    pub thread: Option<String>,
    /// The `ProjectId` the project tools report; derived from the root when absent.
    pub project_id: Option<String>,
    pub environment_id: String,
    pub label: String,
}

impl EngineConfig {
    /// Reads the engine settings from the app's environment.
    pub fn from_env() -> Option<Self> {
        let database = std::env::var_os("ATRIUM_ENGINE_DB").map(PathBuf::from)?;
        Some(Self {
            database,
            thread: std::env::var("ATRIUM_THREAD").ok(),
            project_id: std::env::var("ATRIUM_PROJECT_ID").ok(),
            environment_id: std::env::var("ATRIUM_ENVIRONMENT_ID")
                .unwrap_or_else(|_| "atrium".into()),
            label: std::env::var("ATRIUM_ENVIRONMENT_LABEL").unwrap_or_else(|_| "Atrium".into()),
        })
    }
}

/// Answers requests from `input` on `output` until `input` ends.
pub fn serve(target: &Target, input: impl BufRead, output: impl Write) -> Result<()> {
    serve_with(target, None, input, output)
}

/// [`serve`] with the device tools.
pub fn serve_with(
    target: &Target,
    devices: Option<&dyn Devices>,
    input: impl BufRead,
    mut output: impl Write,
) -> Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle_with(target, &message, devices),
            Err(error) => Some(json!({
                "jsonrpc": "2.0", "id": null,
                "error": { "code": -32700, "message": format!("parse error: {error}") }
            })),
        };
        if let Some(reply) = reply {
            writeln!(output, "{reply}")?;
            output.flush()?;
        }
    }
    Ok(())
}

/// The one server: Atrium's project tools plus T3's orchestration toolkit.
///
/// The engine store is opened once for the session. Project task files still
/// divide a project's work; `delegate_task` is for ad-hoc help outside them.
pub fn serve_merged(
    target: &Target,
    engine: Option<&EngineConfig>,
    devices: Option<&dyn Devices>,
    input: impl BufRead,
    mut output: impl Write,
) -> Result<()> {
    let mut store = match engine {
        Some(config) => Some(EventStore::open(&config.database)?),
        None => None,
    };
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle_merged_with(target, &message, devices, engine, store.as_mut()),
            Err(error) => Some(json!({
                "jsonrpc": "2.0", "id": null,
                "error": { "code": -32700, "message": format!("parse error: {error}") }
            })),
        };
        if let Some(reply) = reply {
            writeln!(output, "{reply}")?;
            output.flush()?;
        }
    }
    Ok(())
}

/// The reply to one message; none for notifications.
pub fn handle(target: &Target, message: &Value) -> Option<Value> {
    handle_with(target, message, None)
}

/// [`handle`] with the device tools.
pub fn handle_with(
    target: &Target,
    message: &Value,
    devices: Option<&dyn Devices>,
) -> Option<Value> {
    handle_inner(target, message, devices, None, None)
}

/// [`handle`] with the orchestration toolkit; `store` must be the engine
/// database `engine` names.
pub fn handle_merged(
    target: &Target,
    message: &Value,
    engine: Option<&EngineConfig>,
    store: Option<&mut EventStore>,
) -> Option<Value> {
    handle_inner(target, message, None, engine, store)
}

/// [`handle_merged`] with the device tools.
pub fn handle_merged_with(
    target: &Target,
    message: &Value,
    devices: Option<&dyn Devices>,
    engine: Option<&EngineConfig>,
    store: Option<&mut EventStore>,
) -> Option<Value> {
    handle_inner(target, message, devices, engine, store)
}

fn handle_inner(
    target: &Target,
    message: &Value,
    devices: Option<&dyn Devices>,
    engine: Option<&EngineConfig>,
    store: Option<&mut EventStore>,
) -> Option<Value> {
    let id = message.get("id")?.clone();
    let method = message["method"].as_str().unwrap_or("");
    let params = &message["params"];
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or(PROTOCOL),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "atrium", "version": env!("CARGO_PKG_VERSION") },
            "instructions": INSTRUCTIONS,
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({
            "tools": tools_list(target, devices.is_some(), engine.is_some() && store.is_some()),
        })),
        "tools/call" => Ok(call_merged(
            target,
            params["name"].as_str().unwrap_or(""),
            &params["arguments"],
            devices,
            engine,
            store,
        )),
        _ => Err(json!({ "code": -32601, "message": format!("no method {method}") })),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    })
}

/// The reply to one tool call, from whichever half of the merged server owns
/// the name.
fn call_merged(
    target: &Target,
    name: &str,
    args: &Value,
    devices: Option<&dyn Devices>,
    engine: Option<&EngineConfig>,
    store: Option<&mut EventStore>,
) -> Value {
    if engine_tools::is_orchestration_tool(name) {
        return match (engine, store) {
            (Some(config), Some(store)) => engine_call(target, name, args, config, store),
            _ => tool_error(McpError::new(
                "capability_denied",
                format!("No tool {name}."),
            )),
        };
    }
    if engine_tools::PROJECT_TOOLS.contains(&name) {
        return match Project::load(&target.root) {
            Ok(project) => match project_tool(target, &project, name, args, engine) {
                Ok(value) => structured(value),
                Err(error) => text_error(error),
            },
            Err(error) => text_error(error),
        };
    }
    call(target, name, args, devices)
}

/// A tool result: text, flagged as an error when the call failed.
fn call(target: &Target, name: &str, args: &Value, devices: Option<&dyn Devices>) -> Value {
    let outcome = Project::load(&target.root).and_then(|mut project| {
        if crate::devices::is_device_tool(name) {
            return match devices {
                Some(devices) if Spread::of(&project).is_spread() => {
                    devices.call(&project, &target.stream, name, args)
                }
                _ => Err(anyhow!("No tool {name}.")),
            };
        }
        run(&mut project, &target.stream, name, args)
    });
    match outcome {
        Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
        Err(error) => json!({
            "content": [{ "type": "text", "text": format!("{error:#}") }],
            "isError": true,
        }),
    }
}

fn engine_call(
    target: &Target,
    name: &str,
    args: &Value,
    config: &EngineConfig,
    store: &mut EventStore,
) -> Value {
    let mut mcp_config = McpConfig::new(
        project_id(target, Some(config)),
        config.thread.as_deref().map(ThreadId::new),
    );
    mcp_config.environment_id = config.environment_id.clone();
    mcp_config.label = config.label.clone();
    let mut service = McpService::new(&mut *store, SystemClock, mcp_config);
    match service.call(name, args) {
        Ok(value) => structured(value),
        Err(error) => tool_error(error),
    }
}

/// The T3 wire result: the same object in `structuredContent` and as text.
fn structured(value: Value) -> Value {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": value,
    })
}

fn tool_error(error: McpError) -> Value {
    json!({
        "content": [{ "type": "text", "text": format!("{}: {}", error.code, error.message) }],
        "structuredContent": { "code": error.code, "message": error.message },
        "isError": true,
    })
}

fn text_error(error: anyhow::Error) -> Value {
    json!({
        "content": [{ "type": "text", "text": format!("{error:#}") }],
        "isError": true,
    })
}

/// The id the project tools report. Without an app-supplied id, the root
/// path hashes to a stable one, so a client can list and then read it.
fn project_id(target: &Target, engine: Option<&EngineConfig>) -> String {
    if let Some(id) = engine.and_then(|config| config.project_id.clone()) {
        return id;
    }
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(target.root.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("atrium-{hex}")
}

fn project_wire(target: &Target, project: &Project, engine: Option<&EngineConfig>) -> ProjectWire {
    let now = jiff::Timestamp::now().to_string();
    let title = if project.file.name.trim().is_empty() {
        target
            .root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Atrium project".into())
    } else {
        project.file.name.clone()
    };
    ProjectWire {
        id: project_id(target, engine),
        title,
        workspace_root: target.root.to_string_lossy().into_owned(),
        default_model_selection: None,
        scripts: Vec::new(),
        created_at: now.clone(),
        updated_at: now,
        deleted_at: None,
    }
}

fn project_tool(
    target: &Target,
    project: &Project,
    name: &str,
    args: &Value,
    engine: Option<&EngineConfig>,
) -> Result<Value> {
    match name {
        "t3_project_list" => Ok(serde_json::to_value(ProjectListResult {
            projects: vec![project_wire(target, project, engine)],
            next_cursor: None,
        })?),
        "t3_project_read" => {
            let wire = project_wire(target, project, engine);
            let wanted = args["projectId"].as_str().unwrap_or("");
            if wanted != wire.id {
                return Err(anyhow!("Project {wanted} was not found."));
            }
            Ok(serde_json::to_value(wire)?)
        }
        "t3_environment_read" => {
            let environment_id = engine
                .map(|config| config.environment_id.clone())
                .unwrap_or_else(|| "atrium".into());
            let label = engine
                .map(|config| config.label.clone())
                .unwrap_or_else(|| "Atrium".into());
            Ok(serde_json::to_value(EnvironmentReadResult::current(
                environment_id,
                label,
            ))?)
        }
        other => Err(anyhow!("No tool {other}.")),
    }
}

fn run(project: &mut Project, me: &str, name: &str, args: &Value) -> Result<String> {
    let text = |key: &str| args[key].as_str().unwrap_or("").to_string();
    let list = |key: &str| -> Vec<String> {
        match &args[key] {
            Value::Array(items) => items
                .iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect(),
            Value::String(one) => one
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            _ => Vec::new(),
        }
    };
    match name {
        "whoami" => project.whoami(me),
        "list_tasks" => project.list_tasks(me, args["all"].as_bool().unwrap_or(false)),
        "create_task" => project.create_task(me, &new_task(args)),
        "create_tasks" => {
            let tasks: Vec<NewTask> = args["tasks"]
                .as_array()
                .map(|tasks| tasks.iter().map(new_task).collect())
                .unwrap_or_default();
            project.create_tasks(me, &tasks)
        }
        "claim_next" => Ok(match project.claim_from_disk(me)? {
            Some(task) => format!(
                "Claimed for you:\n\n{}\n\nDo it, then `finish_task`; that hands you the next one.",
                crate::agent::task_brief(&task)
            ),
            None if project.pool_session_full(me) => {
                "Session task limit reached. Stop here for a fresh worker.".into()
            }
            None => {
                "The pool is empty: nothing to claim. Stop here; you are woken if new tasks arrive."
                    .into()
            }
        }),
        "pool_status" => {
            let pool = project.pool(me);
            let mut out = format!(
                "{} ready, {} waiting on others, {} in progress, {} done, {} failed.\n",
                pool.ready.len(),
                pool.waiting.len(),
                pool.running.len(),
                pool.done,
                pool.failed
            );
            for (worker, task) in &pool.workers {
                out.push_str(&format!(
                    "- `{worker}`: {}\n",
                    task.as_deref()
                        .map_or("idle".to_string(), |t| format!("on `{t}`"))
                ));
            }
            if !pool.ready.is_empty() {
                out.push_str(&format!("Next up: {}\n", pool.ready.join(", ")));
            }
            Ok(out)
        }
        "check_step" => {
            let ticked =
                project.check_step(me, &text("step"), args["done"].as_bool().unwrap_or(true))?;
            let mut out = format!(
                "{} \"{}\" ({}/{} steps).",
                if ticked.done { "Ticked" } else { "Unticked" },
                ticked.step,
                ticked.progress.0,
                ticked.progress.1
            );
            if let Some(phase) = ticked.finished_phase {
                out.push_str(&format!(
                    " Phase \"{phase}\" is done; your parent has been told."
                ));
            }
            Ok(out)
        }
        "finish_task" => project.finish_task_for(
            me,
            args["task"].as_str(),
            text("status") != "failed",
            &text("result"),
        ),
        "notify" => project.notify(me, &list("to"), &text("text")),
        "read_notices" => match args["group"].as_str().filter(|g| !g.trim().is_empty()) {
            Some(group) => project.group_history(me, group),
            None => project.notices_for(me, args["recent"].as_bool().unwrap_or(false)),
        },
        "create_group" => project.create_group(me, &text("name"), &list("members")),
        "list_groups" => Ok(project.groups_text(me)),
        "write_plan" => project.write_plan(me, &text("text")),
        "write_status" => project.write_status(me, &text("text")),
        _ => Err(anyhow!("No tool {name}.")),
    }
}

/// A task as the tools describe one.
fn new_task(args: &Value) -> NewTask {
    let text = |key: &str| args[key].as_str().unwrap_or("").to_string();
    let list = |value: &Value| -> Vec<String> {
        match value {
            Value::Array(items) => items
                .iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect(),
            Value::String(one) => one
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            _ => Vec::new(),
        }
    };
    let phases = args["phases"]
        .as_array()
        .map(|phases| {
            phases
                .iter()
                .map(|p| {
                    (
                        p["title"].as_str().unwrap_or("").to_string(),
                        list(&p["steps"]),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    NewTask {
        id: text("id"),
        title: text("title"),
        goal: text("goal"),
        agent: text("agent"),
        scope: list(&args["scope"]),
        phases,
        after: list(&args["after"]),
        priority: match text("priority").as_str() {
            "high" => 1,
            "low" => -1,
            _ => 0,
        },
    }
}

/// The tools `target`'s role may use.
fn tools(target: &Target, with_devices: bool) -> Vec<Value> {
    let own = Project::load(&target.root)
        .ok()
        .and_then(|p| p.stream(&target.stream).cloned());
    let role = own.as_ref().map(|s| s.role);
    let has_task = own.as_ref().is_some_and(|s| s.task.is_some());
    let pooled = own
        .as_ref()
        .is_some_and(|s| s.role == Role::Worker && s.task.is_none());
    let mut tools = vec![
        tool(
            "whoami",
            "Who you are in this project: your role, scope, goal, who you report to, your roadmap with the current phase, and the tasks you asked for. Call it first.",
            json!({}),
            &[],
        ),
        tool(
            "list_tasks",
            "The tasks you asked for, with status, progress and result; with all, every task in the project.",
            json!({ "all": { "type": "boolean", "description": "Every task, not only yours." } }),
            &[],
        ),
    ];
    if role.and_then(Role::child).is_some() {
        let lead = role == Some(Role::Controller);
        tools.push(tool(
            "create_task",
            if lead {
                "Ask for a lead: a long-running agent that owns one part of the project, with a phased roadmap. Atrium starts it (after the user's approval when that is on) and tells you when it finishes."
            } else {
                "Add one task to your pool. Prefer `create_tasks` to add a whole batch at once."
            },
            json!({
                "title": { "type": "string", "description": "Short name, such as \"Web app\"." },
                "goal": { "type": "string", "description": "What to do, what done looks like, and what not to touch." },
                "scope": { "type": "array", "items": { "type": "string" }, "description": "Paths it owns, as globs such as web/**." },
                "agent": { "type": "string", "description": "codex, claude, deepseek or antigravity; leave out for your own." },
                "id": { "type": "string", "description": "File name; made from the title when left out." },
                "phases": {
                    "type": "array",
                    "description": "The roadmap, in order: 2–6 phases named by outcome, each a few checkable steps.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" },
                            "steps": { "type": "array", "items": { "type": "string" } }
                        },
                        "required": ["title", "steps"]
                    }
                }
            }),
            if lead {
                &["title", "goal", "phases"]
            } else {
                &["title", "goal"]
            },
        ));
    }
    if role == Some(Role::Lead) {
        tools.push(tool(
            "create_tasks",
            "Fill your task pool with a batch of self-contained tasks at once (5–20 is typical). Sub-agents start on their own, each claims a task, finishes it and claims the next, without you. You are woken with their results when the pool runs low: then review and add the next batch.",
            json!({
                "tasks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" },
                            "goal": { "type": "string", "description": "What to do, what done looks like, how to check it." },
                            "id": { "type": "string", "description": "File name, to refer to it in `after`; made from the title when left out." },
                            "scope": { "type": "array", "items": { "type": "string" }, "description": "Inside yours; yours when left out." },
                            "after": { "type": "array", "items": { "type": "string" }, "description": "Ids of tasks in your pool that must be done first." },
                            "priority": { "type": "string", "enum": ["high", "normal", "low"] },
                            "agent": { "type": "string", "description": "codex, claude, deepseek or antigravity." }
                        },
                        "required": ["title", "goal"]
                    }
                }
            }),
            &["tasks"],
        ));
        tools.push(tool(
            "pool_status",
            "Your task pool: how many tasks are ready, waiting, in progress, done and failed, and what each sub-agent is on.",
            json!({}),
            &[],
        ));
    }
    if pooled || role.is_none() {
        tools.push(tool(
            "claim_next",
            "Claim the next task from your lead's pool. If you already hold one you get it back. Then do it and `finish_task`, which hands you the next.",
            json!({}),
            &[],
        ));
    }
    if has_task || pooled || role.is_none() {
        tools.push(tool(
            "check_step",
            "Tick a step of your roadmap as soon as it is done: the user follows your progress through it. Finishing a phase tells your parent.",
            json!({
                "step": { "type": "string", "description": "The step's number such as 2.1, its text, or a unique part of it." },
                "done": { "type": "boolean", "description": "false to untick. Default true." }
            }),
            &["step"],
        ));
        tools.push(tool(
            "finish_task",
            if pooled {
                "End the task you hold with a report. It claims your next task and returns it: keep going until it says the pool is empty."
            } else {
                "End your task with a report. Your parent is told at once; stop working afterwards."
            },
            json!({
                "task": { "type": "string", "description": "The owner/id from your claim. Reuse it if retrying this result." },
                "status": { "type": "string", "enum": ["done", "failed"] },
                "result": { "type": "string", "description": "What you changed, how you checked it, and anything left open (or why it failed)." }
            }),
            if pooled { &["task", "status", "result"] } else { &["status", "result"] },
        ));
    }
    tools.push(tool(
        "notify",
        "Post a short notice to other workstreams: a milestone, a changed interface, a question across scopes. It reaches them with their next message; a notice to the controller wakes it. Groups: `@leads` (the controller and the leads), `@<lead-id>` (that lead and its workers) and any group the controller made reach every member but you, without waking idle ones; mention `@<stream-id>` in the text to wake one.",
        json!({
            "to": { "type": "array", "items": { "type": "string" }, "description": "Workstream ids, groups such as \"@leads\" or \"@<lead-id>\", \"all\", or \"you\" for the user." },
            "text": { "type": "string" }
        }),
        &["to", "text"],
    ));
    tools.push(tool(
        "read_notices",
        "Notices for you not handed to you yet (they also arrive with your next message), or with recent the last ones to and from you.",
        json!({
            "recent": { "type": "boolean" },
            "group": { "type": "string", "description": "A group of yours, such as \"@leads\": its whole history, to catch up." }
        }),
        &[],
    ));
    tools.push(tool(
        "list_groups",
        "The groups you are in (`@leads`, your lead's group, custom ones) with their members.",
        json!({}),
        &[],
    ));
    tools.push(tool(
        "write_status",
        "Replace your status file (.atrium/status/<you>.md) with Focus, Done, Next, Touching and Questions.",
        json!({ "text": { "type": "string", "description": "Markdown." } }),
        &["text"],
    ));
    if role == Some(Role::Controller) {
        tools.push(tool(
            "create_group",
            "Make (or replace) a group of workstreams for notices, referred to as `@<name>`. Leads and each lead's workers already have automatic groups.",
            json!({
                "name": { "type": "string", "description": "Letters, digits, - or _; not an existing workstream id, `leads` or `all`." },
                "members": { "type": "array", "items": { "type": "string" }, "description": "Workstream ids." }
            }),
            &["name", "members"],
        ));
        tools.push(tool(
            "write_plan",
            "Replace .atrium/plan.md: the goal, the milestones in order, which lead owns each, and the decisions that cut across leads.",
            json!({ "text": { "type": "string", "description": "Markdown." } }),
            &["text"],
        ));
    }
    if with_devices && Project::load(&target.root).is_ok_and(|p| Spread::of(&p).is_spread()) {
        tools.extend(device_tools());
    }
    // The project half of the merged server. It reads `.atrium/` and needs no
    // engine, so it is offered even when only the project tools are in use.
    tools.extend(engine_tools::project_tool_specs());
    tools
}

/// The tool list for one session: Atrium's project tools, and T3's
/// orchestration toolkit when an engine is attached.
fn tools_list(target: &Target, with_devices: bool, with_engine: bool) -> Vec<Value> {
    let mut list = tools(target, with_devices);
    if with_engine {
        list.extend(engine_tools::tool_specs().into_iter().filter(|spec| {
            spec["name"]
                .as_str()
                .is_some_and(engine_tools::is_orchestration_tool)
        }));
    }
    list
}

/// The tools for reading other computers' files, in a spread project.
fn device_tools() -> Vec<Value> {
    let device = json!({ "type": "string", "description": "A computer of this project: its device id, or the scheduler's name for the computer running the project." });
    let path = |what: &str| json!({ "type": "string", "description": format!("{what}, relative to the project, such as perf/results.") });
    vec![
        tool(
            "device_list",
            "List a folder of another computer's copy of this project.",
            json!({ "device": device, "path": path("The folder; empty for the project folder") }),
            &["device"],
        ),
        tool(
            "device_read",
            "Read a file of another computer's copy of this project: its text, or a note for a binary or very large file.",
            json!({ "device": device, "path": path("The file") }),
            &["device", "path"],
        ),
        tool(
            "device_copy",
            "Copy a file or folder from another computer into your own scope, on your computer (at most 200 MB). Use it to bring data you need to work on; to change what another computer owns, ask its owner with `notify`.",
            json!({
                "device": device,
                "path": path("What to copy"),
                "to": path("Where it goes, inside your scope; the same path when left out"),
            }),
            &["device", "path"],
        ),
    ]
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required },
    })
}

const INSTRUCTIONS: &str = "Atrium runs this project as a controller, leads and sub-agents working side by side; these tools are how you take part. Call `whoami` first. Leads: tick each roadmap step with `check_step` as soon as it is done (the user watches progress there), hand self-contained work to sub-agents with `create_task`, and end with `finish_task` when your goal is done. Leads fill a task pool with `create_tasks` and are woken when it runs low. Sub-agents: `claim_next`, do the task, `finish_task` (which hands you the next), and keep going until the pool is empty. The controller: `write_plan`, then `create_task` once per lead with a phased roadmap, then stop until woken. Use `notify` for anything another workstream must know. Everything is also in plain files under .atrium/, which you may read. The project task files are how this project divides its work; keep using `create_task`/`create_tasks` for that. The orchestration tools (`t3_thread_*`, `delegate_task`, `task_status`, `task_cancel`, `orchestrator_capabilities`) are for ad-hoc help outside the project plan: a quick side thread, another agent on a separate task, or reading a conversation. Do not use `delegate_task` to stand in for a project task that belongs in a task file, and do not use a project task for a one-off question a child thread can answer.";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Workstream;
    use std::fs;

    #[test]
    fn a_session_lists_role_tools_and_calls_them() {
        let root = std::env::temp_dir().join(format!("atrium-mcp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let mut project = Project::open(&root).unwrap();
        project.file.streams.push(Workstream {
            id: "boss".into(),
            role: Role::Controller,
            ..Default::default()
        });
        project.save().unwrap();
        let target = Target {
            root: root.clone(),
            stream: "boss".into(),
        };
        let input = [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"create_task","arguments":{
                "title":"Web","goal":"Build it","scope":["web/**"],
                "phases":[{"title":"Skeleton","steps":["Routes"]}]}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"finish_task","arguments":{"status":"done","result":"x"}}}),
        ]
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join("\n");
        let mut out = Vec::new();
        serve(&target, input.as_bytes(), &mut out).unwrap();
        let replies: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(replies.len(), 4, "the notification gets no reply");
        assert_eq!(replies[0]["result"]["protocolVersion"], "2025-03-26");
        let names: Vec<&str> = replies[1]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"create_task") && names.contains(&"write_plan"));
        assert!(
            !names.contains(&"check_step"),
            "a controller has no roadmap"
        );
        assert!(replies[2]["result"]["isError"].is_null(), "{}", replies[2]);
        assert!(root.join(".atrium/tasks/boss/web.md").exists());
        assert_eq!(replies[3]["result"]["isError"], true);
        let _ = fs::remove_dir_all(&root);
    }

    struct Echo;
    impl Devices for Echo {
        fn call(&self, _: &Project, me: &str, name: &str, args: &Value) -> Result<String> {
            Ok(format!("{me} ran {name} on {}", args["device"]))
        }
    }

    #[test]
    fn device_tools_exist_only_in_spread_projects() {
        let root = std::env::temp_dir().join(format!("atrium-mcp-dev-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let mut project = Project::open(&root).unwrap();
        project.file.streams.push(Workstream {
            id: "boss".into(),
            role: Role::Controller,
            ..Default::default()
        });
        project.save().unwrap();
        let target = Target {
            root: root.clone(),
            stream: "boss".into(),
        };
        let list = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
        let call = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
            "name":"device_read","arguments":{"device":"win","path":"a"}}});
        let names = |reply: &Value| -> Vec<String> {
            reply["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_string())
                .collect()
        };
        // Not spread: no device tools, with or without a transport for them.
        let reply = handle_with(&target, &list, Some(&Echo)).unwrap();
        assert!(!names(&reply).contains(&"device_read".to_string()));
        assert_eq!(
            handle_with(&target, &call, Some(&Echo)).unwrap()["result"]["isError"],
            true
        );
        project.file.streams.push(Workstream {
            id: "perf".into(),
            role: Role::Lead,
            backend: "codex@win".into(),
            ..Default::default()
        });
        project.save().unwrap();
        let reply = handle_with(&target, &list, Some(&Echo)).unwrap();
        let spread = names(&reply);
        for tool in ["device_list", "device_read", "device_copy"] {
            assert!(spread.contains(&tool.to_string()), "{tool}");
        }
        // Without a transport for them (no node to ask) they are not offered.
        assert!(!names(&handle(&target, &list).unwrap()).contains(&"device_read".to_string()));
        let reply = handle_with(&target, &call, Some(&Echo)).unwrap();
        assert_eq!(
            reply["result"]["content"][0]["text"],
            "boss ran device_read on \"win\""
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn one_server_offers_project_tools_and_the_orchestration_toolkit() {
        let root = std::env::temp_dir().join(format!("atrium-mcp-merged-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let mut project = Project::open(&root).unwrap();
        project.file.name = "Merged".into();
        project.file.streams.push(Workstream {
            id: "boss".into(),
            role: Role::Controller,
            ..Default::default()
        });
        project.save().unwrap();
        let target = Target {
            root: root.clone(),
            stream: "boss".into(),
        };
        let engine = EngineConfig {
            database: PathBuf::from(":memory:"),
            thread: None,
            project_id: Some("project:merged".into()),
            environment_id: "atrium".into(),
            label: "Atrium".into(),
        };
        let mut store = EventStore::in_memory().unwrap();
        let list = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
        let reply = handle_merged(&target, &list, Some(&engine), Some(&mut store)).unwrap();
        let names: Vec<&str> = reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"whoami"));
        assert!(names.contains(&"t3_project_list"));
        assert!(names.contains(&"t3_environment_read"));
        assert!(names.contains(&"t3_thread_read"));
        assert!(names.contains(&"delegate_task"));

        // Without an engine, the project tools stay and the orchestration
        // toolkit is not offered: there is nothing behind it.
        let reply = handle_merged(&target, &list, None, None).unwrap();
        let names: Vec<&str> = reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"t3_project_list"));
        assert!(!names.contains(&"t3_thread_read"));

        let call = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
            "name":"t3_project_list","arguments":{}}});
        let reply = handle_merged(&target, &call, Some(&engine), Some(&mut store)).unwrap();
        assert!(reply["result"]["isError"].is_null(), "{reply}");
        let projects = &reply["result"]["structuredContent"]["projects"];
        assert_eq!(projects[0]["id"], "project:merged");
        assert_eq!(projects[0]["title"], "Merged");
        assert_eq!(
            projects[0]["workspaceRoot"],
            root.to_string_lossy().as_ref()
        );
        let _ = fs::remove_dir_all(&root);
    }
}
