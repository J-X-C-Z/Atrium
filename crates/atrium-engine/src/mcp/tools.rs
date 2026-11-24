//! The orchestration toolkit's tool descriptors.
//!
//! Ported from `apps/server/src/mcp/toolkits/orchestrator/tools.ts`,
//! `toolkits/thread/tools.ts`, `toolkits/project/tools.ts` and
//! `toolkits/environment/tools.ts` (T3 Code v0.1.11, MIT). Names, input
//! properties and the descriptions a model reads for limits are T3's; string
//! prose that named T3 says Atrium here.
//!
//! Tools for scheduled tasks, PR watches, workflows, links and secrets are
//! deliberately absent: those are items 29–31, and the engine has no service
//! behind them yet.

use serde_json::{Value, json};

/// Every toolkit tool, project and environment ones included.
pub const ORCHESTRATION_TOOLS: &[&str] = &[
    "orchestrator_capabilities",
    "delegate_task",
    "task_status",
    "task_cancel",
    "t3_thread_launch",
    "t3_thread_list",
    "t3_thread_read",
    "t3_thread_send",
    "t3_thread_wait",
    "t3_thread_interrupt",
    "t3_thread_configure",
    "t3_pending_request",
    "t3_queue",
];

/// Tools the merged server answers from Atrium's project files.
pub const PROJECT_TOOLS: &[&str] = &["t3_project_list", "t3_project_read", "t3_environment_read"];

pub fn is_orchestration_tool(name: &str) -> bool {
    ORCHESTRATION_TOOLS.contains(&name)
}

pub fn is_merged_tool(name: &str) -> bool {
    ORCHESTRATION_TOOLS.contains(&name) || PROJECT_TOOLS.contains(&name)
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
    })
}

fn string() -> Value {
    json!({ "type": "string" })
}

fn optional_string(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn boolean(description: &str) -> Value {
    json!({ "type": "boolean", "description": description })
}

fn runtime_mode() -> Value {
    json!({
        "type": "string",
        "enum": ["inherit", "approval-required", "auto-accept-edits", "auto", "full-access"],
    })
}

fn interaction_mode() -> Value {
    json!({ "type": "string", "enum": ["inherit", "default", "plan"] })
}

fn environment_id() -> Value {
    optional_string(
        "Linked environment to act in. Omit for this one. Ids are then that environment's.",
    )
}

fn thread_id() -> Value {
    string()
}

fn client_request_id() -> Value {
    optional_string("Reuse it to retry the same call. A new call gets a new key.")
}

/// `t3_thread_read`'s page-size limits live only in the descriptions: Codex
/// drops `maximum`, and an oversized value clamps on decode.
pub fn thread_update_input_schema() -> Value {
    object(
        json!({
            "environmentId": environment_id(),
            "action": { "type": "string", "enum": ["rename", "regenerate_title"] },
            "title": { "type": "string" },
        }),
        &["action"],
    )
}

/// The project and environment descriptors: the merged server answers these
/// from Atrium's files, so they are offered even when no engine is attached.
pub fn project_tool_specs() -> Vec<Value> {
    tool_specs()
        .into_iter()
        .filter(|spec| {
            spec["name"]
                .as_str()
                .is_some_and(|name| PROJECT_TOOLS.contains(&name))
        })
        .collect()
}

/// Every tool descriptor the merged server offers.
pub fn tool_specs() -> Vec<Value> {
    vec![
        tool(
            "orchestrator_capabilities",
            "List providers, models, options, and runtime modes for `delegate_task`. Short list. `search` finds others. Any catalog model works.",
            object(
                json!({
                    "environmentId": environment_id(),
                    "search": optional_string("Match model ids and labels. Replaces the short list."),
                }),
                &[],
            ),
        ),
        tool(
            "delegate_task",
            "Start one child agent with only `task`. No parent history. Keep `taskId` for `task_status` and `task_cancel`. No secrets in `task`. For Atrium project work, the task files and `create_task` are the way the project divides work; use this for ad-hoc help only.",
            object(
                json!({
                    "task": { "type": "string", "description": "What the child must do. No parent history is sent." },
                    "target": { "type": ["object", "string"], "description": "Provider, model, options, environment and project to run in." },
                    "title": { "type": "string" },
                    "role": { "type": "string", "enum": ["implementation", "research", "review", "design", "test", "general"] },
                    "isolation": { "type": "string", "enum": ["shared", "worktree"], "description": "'shared' (default): parent checkout. 'worktree': new prepared checkout from the parent commit." },
                    "mode": { "type": "string", "enum": ["async", "wait"], "description": "Default 'async'. 'wait' only if this turn needs the result. A question or approval ends the wait." },
                    "timeoutMs": { "type": "number", "description": "Mode 'wait' only. At most 45 s. A timeout does not cancel the child." },
                    "clientRequestId": client_request_id(),
                    "runtimeMode": runtime_mode(),
                    "interactionMode": interaction_mode(),
                }),
                &["task"],
            ),
        ),
        tool(
            "task_status",
            "Read one delegated task you started. Do not call it on a running task before its completion wakes you. `workState`: working, waiting_for_children, result_available. A finished turn with live child work is not done. `summary`: final result. Reading it marks it delivered.",
            object(json!({ "taskId": thread_id() }), &["taskId"]),
        ),
        tool(
            "task_cancel",
            "Stop one delegated task by `taskId`. Stops its running and queued turns and its delegates. A finished task keeps its result.",
            object(
                json!({
                    "taskId": thread_id(),
                    "reason": { "type": "string" },
                    "clientRequestId": client_request_id(),
                }),
                &["taskId"],
            ),
        ),
        tool(
            "t3_thread_launch",
            "Create one new top-level thread, bound to its workspace before the agent starts. Task in `message`. Omit `projectId`, model and modes to inherit yours; modes cannot exceed yours. After an error or lost reply, retry with the same `clientRequestId`. Follow it with `t3_thread_read` or `t3_thread_wait`. Mention it with its `link`.",
            object(
                json!({
                    "environmentId": environment_id(),
                    "projectId": optional_string("Project to create the thread in. Default: this project."),
                    "scratch": boolean("No project: a new Scratch folder. Not with `projectId` or `workspaceStrategy`."),
                    "title": string(),
                    "modelSelection": { "type": "object", "description": "Provider instance, model and options. Default: yours." },
                    "runtimeMode": runtime_mode(),
                    "interactionMode": interaction_mode(),
                    "workspaceStrategy": {
                        "type": "object",
                        "description": "'worktree': new checkout from `baseRef`. 'existing_worktree': bind `worktreePath`. 'root': project checkout, the default.",
                    },
                    "message": { "type": "string", "description": "First prompt. Omit it and `attachments` for idle." },
                    "attachments": { "type": "array", "items": { "type": "object" } },
                    "clientRequestId": client_request_id(),
                }),
                &["title"],
            ),
        ),
        tool(
            "t3_thread_list",
            "List threads newest first, in one project or with `allProjects`. Page with `cursor`.",
            object(
                json!({
                    "environmentId": environment_id(),
                    "projectId": optional_string("Project to use. Default: yours. Required outside a thread."),
                    "allProjects": { "type": "boolean" },
                    "statuses": { "type": "array", "items": { "type": "string" } },
                    "titleContains": { "type": "string" },
                    "settled": { "type": "boolean" },
                    "snoozed": { "type": "boolean" },
                    "includeSubagents": { "type": "boolean" },
                    "cursor": { "type": "integer", "minimum": 0 },
                    "limit": { "type": "integer", "minimum": 1, "description": "Max 100." },
                }),
                &[],
            ),
        ),
        tool(
            "t3_thread_read",
            "Read any thread by id. View `messages` (default) or `activity` (all items). Newest first; older: `beforePosition=earlierPosition`; from the start: `afterPosition=-1`, then `nextPosition`. Cut-off text: `itemId`, `textOffset=nextTextOffset`. In your reply, name it as its `link` verbatim.",
            object(
                json!({
                    "environmentId": environment_id(),
                    "threadId": thread_id(),
                    "itemId": optional_string("Read one item's text instead of the timeline."),
                    "textOffset": { "type": "integer", "minimum": 0 },
                    "view": { "type": "string", "enum": ["messages", "activity"] },
                    "afterPosition": { "type": "integer", "minimum": -1 },
                    "beforePosition": { "type": "integer", "minimum": 0 },
                    "limit": { "type": "integer", "minimum": 1, "description": "Max 100." },
                    "runLimit": { "type": "integer", "minimum": 1, "description": "Max 50." },
                    "maxCharsPerItem": { "type": "integer", "minimum": 1, "description": "Max 20000." },
                }),
                &["threadId"],
            ),
        ),
        tool(
            "t3_thread_send",
            "Send a message to any thread. The target cannot run with broader modes than yours. `mode:'auto'` starts an idle thread, steers a running turn, or queues. `queue`: new turn. `steer`: mid-turn update. `restart`: interrupt and restart. In your reply, name it as its `link` verbatim.",
            object(
                json!({
                    "environmentId": environment_id(),
                    "threadId": thread_id(),
                    "message": { "type": "string" },
                    "mode": { "type": "string", "enum": ["auto", "queue", "steer", "restart"] },
                    "freshContext": boolean("Start a new provider session without the thread's earlier context, like /clear. Never steers."),
                    "clientRequestId": client_request_id(),
                }),
                &["threadId", "message"],
            ),
        ),
        tool(
            "t3_thread_wait",
            "Wait for a thread's run to finish. No `runId`: latest. Idle: at once. On timeout, with `next` end your turn; without, call again.",
            object(
                json!({
                    "environmentId": environment_id(),
                    "threadId": thread_id(),
                    "runId": optional_string("The run to wait for. Default: the latest."),
                    "timeoutMs": { "type": "number", "description": "At most 45 s. Default 30 s." },
                    "untilDelegatedWorkSettled": boolean("Also wait for its delegated and background tasks and their completions to settle."),
                }),
                &["threadId"],
            ),
        ),
        tool(
            "t3_thread_interrupt",
            "Interrupt a top-level thread's running turn. No `runId`: newest running turn. No effect when idle. For a delegate, use `task_cancel`.",
            object(
                json!({
                    "environmentId": environment_id(),
                    "threadId": thread_id(),
                    "runId": optional_string("The run to interrupt. Default: the newest running turn."),
                    "reason": { "type": "string" },
                    "stop": boolean("Stop the thread as its Stop button does instead: interrupt the running turn, hold queued messages and stop every task it delegated."),
                    "clientRequestId": client_request_id(),
                }),
                &["threadId"],
            ),
        ),
        tool(
            "t3_thread_configure",
            "Read a thread's provider, model, and modes, or set its provider, model, and options with `modelSelection`. Omit `threadId` for this thread. Permission modes stay. Values from `orchestrator_capabilities`.",
            object(
                json!({
                    "environmentId": environment_id(),
                    "threadId": optional_string("The thread. Default: this one."),
                    "modelSelection": { "type": "object", "description": "Provider instance, model and options." },
                }),
                &[],
            ),
        ),
        tool(
            "t3_pending_request",
            "Pending user questions in a thread. Omit `threadId` for this thread. list: their ids; approvals are not listed. read: one by `requestId`. respond: answer one with `answers`; this cannot approve a permission request.",
            object(
                json!({
                    "action": { "type": "string", "enum": ["list", "read", "respond"] },
                    "environmentId": environment_id(),
                    "threadId": optional_string("The thread. Default: this one."),
                    "requestId": optional_string("The request, for read or respond."),
                    "answers": { "type": "object", "additionalProperties": true },
                }),
                &["action"],
            ),
        ),
        tool(
            "t3_queue",
            "Manage a thread's queued messages. Omit `threadId` for this thread. list: send order, paged by `cursor` and `limit`. read: up to 16,000 chars of one. edit: replace its `text`; attachments stay. cancel. reorder: before `beforeRunId`, or to the end with null. promote_to_steer: send it now as steering to active `targetRunId`. Writes fail once the run is no longer queued.",
            object(
                json!({
                    "action": { "type": "string", "enum": ["list", "read", "edit", "cancel", "reorder", "promote_to_steer"] },
                    "environmentId": environment_id(),
                    "threadId": optional_string("The thread. Default: this one."),
                    "queuedRunId": optional_string("The queued run the action changes."),
                    "text": { "type": "string" },
                    "beforeRunId": { "type": ["string", "null"] },
                    "targetRunId": optional_string("The active run to steer, for promote_to_steer."),
                    "cursor": { "type": "integer", "minimum": 0 },
                    "limit": { "type": "integer", "minimum": 1, "description": "Max 100." },
                }),
                &["action"],
            ),
        ),
        // Project and environment tools: the merged server answers these from
        // Atrium's files, with T3's wire shape.
        tool(
            "t3_project_list",
            "List projects. Pages can shift.",
            object(
                json!({
                    "environmentId": environment_id(),
                    "cursor": { "type": "integer", "minimum": 0 },
                    "limit": { "type": "integer", "minimum": 1, "description": "Max 100." },
                }),
                &[],
            ),
        ),
        tool(
            "t3_project_read",
            "Read one project: workspace and saved scripts.",
            object(
                json!({
                    "environmentId": environment_id(),
                    "projectId": string(),
                }),
                &["projectId"],
            ),
        ),
        tool(
            "t3_environment_read",
            "Read this server's identity and preferences. Providers and models: `orchestrator_capabilities`.",
            object(json!({ "environmentId": environment_id() }), &[]),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_are_unique_and_listed_once() {
        let specs = tool_specs();
        let names: Vec<&str> = specs
            .iter()
            .map(|spec| spec["name"].as_str().unwrap())
            .collect();
        for name in ORCHESTRATION_TOOLS.iter().chain(PROJECT_TOOLS) {
            assert_eq!(
                names.iter().filter(|candidate| *candidate == name).count(),
                1
            );
        }
    }

    // Ported from apps/server/src/mcp/toolkits/orchestrator/tools.test.ts.
    #[test]
    fn states_thread_read_page_size_limits_in_the_parameter_descriptions() {
        let specs = tool_specs();
        let read = specs
            .iter()
            .find(|spec| spec["name"] == "t3_thread_read")
            .expect("t3_thread_read");
        for (name, maximum) in [
            ("limit", "100"),
            ("runLimit", "50"),
            ("maxCharsPerItem", "20000"),
        ] {
            let description = read["inputSchema"]["properties"][name]["description"]
                .as_str()
                .unwrap_or_default();
            assert!(
                description.contains(maximum),
                "{name} lacks {maximum}: {description}"
            );
        }
    }

    #[test]
    fn project_and_environment_tools_are_distinct_from_orchestration() {
        for name in PROJECT_TOOLS {
            assert!(!is_orchestration_tool(name));
        }
        assert!(is_orchestration_tool("t3_thread_read"));
        assert!(!is_orchestration_tool("whoami"));
    }
}
