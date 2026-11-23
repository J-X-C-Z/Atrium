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
        "tools/list" => Ok(json!({ "tools": tools(target, devices.is_some()) })),
        "tools/call" => Ok(call(
            target,
            params["name"].as_str().unwrap_or(""),
            &params["arguments"],
            devices,
        )),
        _ => Err(json!({ "code": -32601, "message": format!("no method {method}") })),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    })
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
    tools
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

const INSTRUCTIONS: &str = "Atrium runs this project as a controller, leads and sub-agents working side by side; these tools are how you take part. Call `whoami` first. Leads: tick each roadmap step with `check_step` as soon as it is done (the user watches progress there), hand self-contained work to sub-agents with `create_task`, and end with `finish_task` when your goal is done. Leads fill a task pool with `create_tasks` and are woken when it runs low. Sub-agents: `claim_next`, do the task, `finish_task` (which hands you the next), and keep going until the pool is empty. The controller: `write_plan`, then `create_task` once per lead with a phased roadmap, then stop until woken. Use `notify` for anything another workstream must know. Everything is also in plain files under .atrium/, which you may read.";

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
}
