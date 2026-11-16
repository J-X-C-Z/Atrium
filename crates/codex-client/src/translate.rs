//! Codex app-server protocol (v2) to AgentDesk's model.
//!
//! The protocol is marked experimental and grows fields every release, so we
//! read it leniently from `serde_json::Value`: unknown fields are ignored,
//! missing ones default, and unknown item types still show up as
//! [`Item::Other`] instead of failing the whole message.

use agentdesk_core::{
    Account, AgentEvent, ApprovalKind, ApprovalRequest, ChangeKind, Credits, FileChange, Goal,
    GoalStatus, Item, ItemStatus, PlanStep, Question, RateLimits, RateWindow, ThreadMeta,
    TokenUsage, TurnStatus, UserInputRequest,
};
use serde_json::Value;

fn s(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
}

fn opt_s(value: &Value, key: &str) -> Option<String> {
    value[key]
        .as_str()
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// A thread as the sidebar lists it. Ephemeral threads (such as the forks
/// that predict the next message) are not the user's chats: `None`.
pub fn thread_meta(thread: &Value) -> Option<ThreadMeta> {
    if thread["ephemeral"].as_bool() == Some(true) {
        return None;
    }
    let id = thread["id"].as_str()?.to_string();
    Some(ThreadMeta {
        id,
        name: opt_s(thread, "name"),
        preview: s(thread, "preview"),
        cwd: s(thread, "cwd"),
        model: opt_s(thread, "model"),
        created_at: thread["createdAt"].as_i64().unwrap_or_default(),
        updated_at: thread["recencyAt"]
            .as_i64()
            .or_else(|| thread["updatedAt"].as_i64())
            .unwrap_or_default(),
    })
}

pub fn turn_status(value: &Value) -> TurnStatus {
    match value.as_str() {
        Some("completed") => TurnStatus::Completed,
        Some("interrupted") => TurnStatus::Interrupted,
        Some("failed") => TurnStatus::Failed,
        _ => TurnStatus::InProgress,
    }
}

/// A stored turn: `(id, status, items)`.
pub fn turn(turn: &Value) -> Option<(String, TurnStatus, Vec<Item>)> {
    let id = turn["id"].as_str()?.to_string();
    let items = turn["items"]
        .as_array()
        .map(|items| items.iter().filter_map(item).collect())
        .unwrap_or_default();
    Some((id, turn_status(&turn["status"]), items))
}

fn item_status(value: &Value) -> ItemStatus {
    match value.as_str() {
        Some("completed") => ItemStatus::Completed,
        Some("failed") => ItemStatus::Failed,
        Some("declined") => ItemStatus::Declined,
        _ => ItemStatus::InProgress,
    }
}

fn pretty(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// The text a user message carries, and the paths it attached
/// (images and mentioned files).
fn user_content(content: &Value) -> (String, Vec<String>) {
    let Some(parts) = content.as_array() else {
        return (String::new(), Vec::new());
    };
    let mut text = Vec::new();
    let mut attachments = Vec::new();
    for part in parts {
        match part["type"].as_str() {
            Some("text") => text.push(s(part, "text")),
            Some("localImage") | Some("localAudio") | Some("mention") | Some("skill") => {
                attachments.push(s(part, "path"))
            }
            Some("image") | Some("audio") => attachments
                .push(opt_s(part, "url").unwrap_or_else(|| format!("[{}]", s(part, "type")))),
            Some(other) => text.push(format!("[{other}]")),
            None => {}
        }
    }
    text.retain(|part| !part.is_empty());
    (text.join("\n"), attachments)
}

fn joined(value: &Value) -> String {
    value
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default()
}

/// One thread item. `None` only when it has no id.
pub fn item(item: &Value) -> Option<Item> {
    let id = item["id"].as_str()?.to_string();
    let kind = item["type"].as_str().unwrap_or_default();
    Some(match kind {
        "userMessage" => {
            let (text, attachments) = user_content(&item["content"]);
            Item::User {
                id,
                text,
                attachments,
            }
        }
        "agentMessage" => Item::Agent {
            id,
            text: s(item, "text"),
        },
        "plan" => Item::Agent {
            id,
            text: s(item, "text"),
        },
        "reasoning" => {
            let summary = joined(&item["summary"]);
            let text = if summary.is_empty() {
                joined(&item["content"])
            } else {
                summary
            };
            Item::Reasoning { id, text }
        }
        "commandExecution" => Item::Command {
            id,
            command: s(item, "command"),
            cwd: s(item, "cwd"),
            status: item_status(&item["status"]),
            output: s(item, "aggregatedOutput"),
            exit_code: item["exitCode"].as_i64().map(|code| code as i32),
            duration_ms: item["durationMs"].as_u64(),
        },
        "fileChange" => Item::FileChange {
            id,
            changes: item["changes"]
                .as_array()
                .map(|changes| changes.iter().map(file_change).collect())
                .unwrap_or_default(),
            status: item_status(&item["status"]),
        },
        "mcpToolCall" => {
            let result = if let Some(error) = item["error"]["message"].as_str() {
                error.to_string()
            } else {
                tool_text(&item["result"]["content"])
            };
            Item::Tool {
                id,
                name: format!("{}.{}", s(item, "server"), s(item, "tool")),
                status: item_status(&item["status"]),
                arguments: pretty(&item["arguments"]),
                result,
                duration_ms: item["durationMs"].as_u64(),
            }
        }
        "dynamicToolCall" => Item::Tool {
            id,
            name: match opt_s(item, "namespace") {
                Some(ns) => format!("{ns}.{}", s(item, "tool")),
                None => s(item, "tool"),
            },
            status: item_status(&item["status"]),
            arguments: pretty(&item["arguments"]),
            result: tool_text(&item["contentItems"]),
            duration_ms: item["durationMs"].as_u64(),
        },
        "webSearch" => {
            let action = &item["action"];
            let detail = match action["type"].as_str() {
                Some("open_page") => opt_s(action, "url").map(|url| format!("Opened {url}")),
                Some("find_in_page") => {
                    opt_s(action, "pattern").map(|p| format!("Looked for “{p}”"))
                }
                Some("search") => {
                    action["queries"]
                        .as_array()
                        .filter(|q| q.len() > 1)
                        .map(|queries| {
                            queries
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(" · ")
                        })
                }
                _ => None,
            };
            let query = opt_s(item, "query")
                .or_else(|| opt_s(action, "query"))
                .unwrap_or_default();
            Item::WebSearch { id, query, detail }
        }
        "imageGeneration" => Item::Image {
            id,
            status: match item["status"].as_str() {
                Some("completed") | Some("succeeded") => ItemStatus::Completed,
                Some("failed") => ItemStatus::Failed,
                _ if !item["failure"].is_null() => ItemStatus::Failed,
                _ if item["savedPath"].is_string() => ItemStatus::Completed,
                _ => ItemStatus::InProgress,
            },
            path: opt_s(item, "savedPath"),
            prompt: opt_s(item, "revisedPrompt"),
        },
        "collabAgentToolCall" => {
            let tool = s(item, "tool");
            let label = match tool.as_str() {
                "spawnAgent" => "Started a sub-agent",
                "sendInput" | "sendMessage" | "followupTask" => "Messaged a sub-agent",
                "wait" => "Waited for sub-agents",
                "closeAgent" => "Closed a sub-agent",
                "interruptAgent" => "Interrupted a sub-agent",
                "resumeAgent" => "Resumed a sub-agent",
                "listAgents" => "Listed sub-agents",
                _ => "Sub-agent",
            };
            Item::SubAgent {
                id,
                label: label.into(),
                status: match item["status"].as_str() {
                    Some("completed") => ItemStatus::Completed,
                    Some("failed") => ItemStatus::Failed,
                    _ => ItemStatus::InProgress,
                },
                detail: opt_s(item, "prompt"),
            }
        }
        "subAgentActivity" => Item::SubAgent {
            id,
            label: match item["kind"].as_str() {
                Some("started") => "Sub-agent started",
                Some("completed") => "Sub-agent finished",
                Some("interrupted") => "Sub-agent interrupted",
                _ => "Sub-agent working",
            }
            .into(),
            status: match item["kind"].as_str() {
                Some("completed") => ItemStatus::Completed,
                Some("interrupted") => ItemStatus::Failed,
                _ => ItemStatus::InProgress,
            },
            detail: opt_s(item, "agentPath").or_else(|| opt_s(item, "model")),
        },
        "contextCompaction" => Item::Other {
            id,
            label: "Context compacted".into(),
        },
        "enteredReviewMode" => Item::Other {
            id,
            label: "Started a review".into(),
        },
        "exitedReviewMode" => Item::Other {
            id,
            label: "Finished the review".into(),
        },
        "imageView" => Item::Other {
            id,
            label: format!("Viewed {}", s(item, "path")),
        },
        other => Item::Other {
            id,
            label: other.to_string(),
        },
    })
}

fn file_change(change: &Value) -> FileChange {
    let kind = match change["kind"]["type"].as_str() {
        Some("add") => ChangeKind::Add,
        Some("delete") => ChangeKind::Delete,
        _ => ChangeKind::Update {
            move_to: opt_s(&change["kind"], "move_path"),
        },
    };
    FileChange {
        path: s(change, "path"),
        kind,
        diff: s(change, "diff"),
    }
}

/// Text out of MCP or dynamic tool content blocks.
fn tool_text(content: &Value) -> String {
    let Some(parts) = content.as_array() else {
        return pretty(content);
    };
    parts
        .iter()
        .map(|part| match part["type"].as_str() {
            Some("text") | Some("inputText") => s(part, "text"),
            Some(other) => format!("[{other}]"),
            None => pretty(part),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn ids(params: &Value) -> (String, String) {
    (s(params, "threadId"), s(params, "turnId"))
}

/// A notification, translated. Ones the UI has no use for give `None`.
pub fn event(method: &str, params: &Value) -> Option<AgentEvent> {
    let (thread_id, turn_id) = ids(params);
    Some(match method {
        "thread/started" => AgentEvent::ThreadStarted(thread_meta(&params["thread"])?),
        "thread/name/updated" => AgentEvent::ThreadNameUpdated {
            thread_id,
            name: opt_s(params, "threadName"),
        },
        "turn/started" => AgentEvent::TurnStarted {
            thread_id,
            turn_id: s(&params["turn"], "id"),
        },
        "turn/completed" => {
            let turn = &params["turn"];
            AgentEvent::TurnCompleted {
                thread_id,
                turn_id: s(turn, "id"),
                status: turn_status(&turn["status"]),
                error: opt_s(&turn["error"], "message"),
                duration_ms: turn["durationMs"].as_u64(),
            }
        }
        "item/started" => AgentEvent::ItemStarted {
            thread_id,
            turn_id,
            item: item(&params["item"])?,
        },
        "item/completed" => AgentEvent::ItemCompleted {
            thread_id,
            turn_id,
            item: item(&params["item"])?,
        },
        "item/agentMessage/delta" | "item/plan/delta" => AgentEvent::AgentMessageDelta {
            thread_id,
            turn_id,
            item_id: s(params, "itemId"),
            delta: s(params, "delta"),
        },
        "item/reasoning/summaryTextDelta" => AgentEvent::ReasoningDelta {
            thread_id,
            turn_id,
            item_id: s(params, "itemId"),
            delta: s(params, "delta"),
        },
        "item/reasoning/summaryPartAdded" => AgentEvent::ReasoningDelta {
            thread_id,
            turn_id,
            item_id: s(params, "itemId"),
            delta: "\n\n".into(),
        },
        "item/commandExecution/outputDelta" => AgentEvent::CommandOutputDelta {
            thread_id,
            turn_id,
            item_id: s(params, "itemId"),
            delta: s(params, "delta"),
        },
        "turn/plan/updated" => AgentEvent::PlanUpdated {
            thread_id,
            turn_id,
            explanation: opt_s(params, "explanation"),
            steps: params["plan"]
                .as_array()
                .map(|steps| {
                    steps
                        .iter()
                        .map(|step| PlanStep {
                            step: s(step, "step"),
                            done: step["status"] == "completed",
                            active: step["status"] == "inProgress",
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        "thread/tokenUsage/updated" => {
            let usage = &params["tokenUsage"];
            AgentEvent::TokenUsage {
                thread_id,
                usage: TokenUsage {
                    total_tokens: usage["total"]["totalTokens"].as_u64().unwrap_or_default(),
                    last_turn_tokens: usage["last"]["totalTokens"].as_u64().unwrap_or_default(),
                    context_window: usage["modelContextWindow"].as_u64(),
                },
            }
        }
        "serverRequest/resolved" => AgentEvent::ApprovalResolved {
            thread_id,
            request_id: params["requestId"].clone(),
        },
        "error" => AgentEvent::Error {
            thread_id: Some(thread_id).filter(|id| !id.is_empty()),
            message: opt_s(&params["error"], "message")
                .unwrap_or_else(|| "Codex reported an error".into()),
            will_retry: params["willRetry"].as_bool().unwrap_or(false),
        },
        "warning" | "configWarning" | "deprecationNotice" | "guardianWarning" => {
            AgentEvent::Warning(
                opt_s(params, "message")
                    .or_else(|| opt_s(params, "summary"))
                    .unwrap_or_else(|| method.to_string()),
            )
        }
        "thread/prediction/updated" => AgentEvent::PredictionUpdated {
            thread_id,
            text: match params["result"]["type"].as_str() {
                Some("completed") => opt_s(&params["result"], "text").map(|t| t.trim().to_string()),
                _ => None,
            },
        },
        "thread/goal/updated" => AgentEvent::GoalUpdated {
            thread_id,
            goal: goal(&params["goal"]),
        },
        "thread/goal/cleared" => AgentEvent::GoalUpdated {
            thread_id,
            goal: None,
        },
        "account/rateLimits/updated" => {
            AgentEvent::RateLimitsUpdated(rate_limits(&params["rateLimits"]))
        }
        "account/updated" => AgentEvent::AccountChanged,
        "account/login/completed" => AgentEvent::LoginCompleted {
            success: params["success"].as_bool().unwrap_or(false),
            error: opt_s(params, "error"),
        },
        _ => return None,
    })
}

/// A server request the user must answer, translated. Others give `None`
/// and should be declined so Codex does not wait on them.
pub fn request(id: &Value, method: &str, params: &Value) -> Option<AgentEvent> {
    let (thread_id, turn_id) = ids(params);
    if method == "item/tool/requestUserInput" {
        let questions = params["questions"]
            .as_array()
            .map(|questions| {
                questions
                    .iter()
                    .map(|q| Question {
                        id: s(q, "id"),
                        header: s(q, "header"),
                        question: s(q, "question"),
                        options: q["options"]
                            .as_array()
                            .map(|options| {
                                options
                                    .iter()
                                    .map(|o| (s(o, "label"), s(o, "description")))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        allow_other: q["isOther"].as_bool().unwrap_or(false),
                        secret: q["isSecret"].as_bool().unwrap_or(false),
                    })
                    .collect()
            })
            .unwrap_or_default();
        return Some(AgentEvent::UserInputRequested(UserInputRequest {
            request_id: id.clone(),
            thread_id,
            turn_id,
            item_id: s(params, "itemId"),
            questions,
        }));
    }
    let kind = match method {
        "item/commandExecution/requestApproval" => ApprovalKind::Command {
            command: opt_s(params, "command").unwrap_or_else(|| {
                params["commandActions"]
                    .as_array()
                    .map(|actions| {
                        actions
                            .iter()
                            .filter_map(|a| a["command"].as_str())
                            .collect::<Vec<_>>()
                            .join(" && ")
                    })
                    .unwrap_or_default()
            }),
            cwd: s(params, "cwd"),
            reason: opt_s(params, "reason"),
        },
        "item/fileChange/requestApproval" => ApprovalKind::FileChange {
            reason: opt_s(params, "reason"),
            grant_root: opt_s(params, "grantRoot"),
        },
        _ => return None,
    };
    Some(AgentEvent::ApprovalRequested(ApprovalRequest {
        request_id: id.clone(),
        thread_id,
        turn_id,
        item_id: s(params, "itemId"),
        kind,
    }))
}

/// A thread goal, or `None` for none.
pub fn goal(value: &Value) -> Option<Goal> {
    let objective = value["objective"].as_str()?.to_string();
    Some(Goal {
        objective,
        status: value["status"]
            .as_str()
            .and_then(GoalStatus::from_wire)
            .unwrap_or(GoalStatus::Paused),
        token_budget: value["tokenBudget"].as_u64(),
        tokens_used: value["tokensUsed"].as_u64().unwrap_or_default(),
        time_used_secs: value["timeUsedSeconds"].as_u64().unwrap_or_default(),
    })
}

fn rate_window(value: &Value) -> Option<RateWindow> {
    Some(RateWindow {
        used_percent: value["usedPercent"].as_f64()?,
        window_mins: value["windowDurationMins"].as_u64(),
        resets_at: value["resetsAt"].as_i64(),
    })
}

/// A rate-limit snapshot (`RateLimitSnapshot`).
pub fn rate_limits(value: &Value) -> RateLimits {
    RateLimits {
        name: opt_s(value, "limitName"),
        primary: rate_window(&value["primary"]),
        secondary: rate_window(&value["secondary"]),
        credits: value["credits"].is_object().then(|| Credits {
            has_credits: value["credits"]["hasCredits"].as_bool().unwrap_or(false),
            unlimited: value["credits"]["unlimited"].as_bool().unwrap_or(false),
            balance: opt_s(&value["credits"], "balance"),
        }),
        plan: opt_s(value, "planType"),
        reached: !value["rateLimitReachedType"].is_null(),
    }
}

/// The signed-in account from `account/read`, or `None` when signed out.
pub fn account(value: &Value) -> Option<Account> {
    Some(match value["type"].as_str()? {
        "apiKey" => Account::ApiKey,
        "chatgpt" => Account::ChatGpt {
            email: opt_s(value, "email"),
            plan: opt_s(value, "planType").unwrap_or_else(|| "unknown".into()),
            workspace: opt_s(value, "workspaceName"),
        },
        other => Account::Other(other.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_thread_reads_with_its_title_and_recency() {
        let meta = thread_meta(&json!({
            "id": "01a", "preview": "fix the build", "name": null, "cwd": "/repo",
            "model": "gpt-x", "createdAt": 10, "updatedAt": 20, "recencyAt": 30,
            "somethingNew": {"ignored": true}
        }))
        .unwrap();
        assert_eq!(meta.title(), "fix the build");
        assert_eq!(meta.updated_at, 30);
    }

    #[test]
    fn items_of_every_kind_translate() {
        let cmd = item(&json!({
            "type": "commandExecution", "id": "c", "command": "ls", "cwd": "/",
            "status": "completed", "aggregatedOutput": "x", "exitCode": 0, "durationMs": 5,
            "commandActions": [], "source": "agent", "processId": null
        }))
        .unwrap();
        assert!(matches!(
            cmd,
            Item::Command {
                status: ItemStatus::Completed,
                exit_code: Some(0),
                ..
            }
        ));

        let patch = item(&json!({
            "type": "fileChange", "id": "f", "status": "inProgress",
            "changes": [{"path": "a.rs", "kind": {"type": "update", "move_path": null}, "diff": "@@"}]
        }))
        .unwrap();
        assert!(
            matches!(&patch, Item::FileChange { changes, .. } if changes[0].kind == ChangeKind::Update { move_to: None })
        );

        let user = item(&json!({
            "type": "userMessage", "id": "u", "clientId": null,
            "content": [{"type": "text", "text": "hello", "text_elements": []}, {"type": "localImage", "path": "/x.png"}]
        }))
        .unwrap();
        assert_eq!(
            user,
            Item::User {
                id: "u".into(),
                text: "hello".into(),
                attachments: vec!["/x.png".into()],
            }
        );

        let image = item(&json!({"type": "imageGeneration", "id": "g", "status": "completed",
            "revisedPrompt": "a cat", "failure": null, "savedPath": "/tmp/cat.png", "generationId": "x"})).unwrap();
        assert!(matches!(
            image,
            Item::Image {
                status: ItemStatus::Completed,
                path: Some(_),
                ..
            }
        ));
        let search = item(&json!({"type": "webSearch", "id": "w", "query": "gpui",
            "action": {"type": "open_page", "url": "https://gpui.rs"}, "results": null}))
        .unwrap();
        assert!(matches!(search, Item::WebSearch { detail: Some(d), .. } if d.contains("gpui.rs")));

        let future = item(&json!({"type": "somethingFromTheFuture", "id": "z"})).unwrap();
        assert!(matches!(future, Item::Other { .. }));
    }

    #[test]
    fn notifications_translate() {
        let ev = event(
            "item/agentMessage/delta",
            &json!({"threadId": "t", "turnId": "u", "itemId": "a", "delta": "Hi"}),
        )
        .unwrap();
        assert_eq!(
            ev,
            AgentEvent::AgentMessageDelta {
                thread_id: "t".into(),
                turn_id: "u".into(),
                item_id: "a".into(),
                delta: "Hi".into()
            }
        );
        let done = event(
            "turn/completed",
            &json!({"threadId": "t", "turn": {"id": "u", "status": "failed", "items": [], "error": {"message": "boom"}}}),
        )
        .unwrap();
        assert!(matches!(
            done,
            AgentEvent::TurnCompleted {
                status: TurnStatus::Failed,
                error: Some(_),
                ..
            }
        ));
        assert!(event("remoteControl/status/changed", &json!({})).is_none());
    }

    #[test]
    fn pro_notifications_translate() {
        let prediction = event(
            "thread/prediction/updated",
            &json!({"threadId": "t", "sourceTurnId": "u", "result": {"type": "completed", "text": " Run the tests "}}),
        );
        assert_eq!(
            prediction,
            Some(AgentEvent::PredictionUpdated {
                thread_id: "t".into(),
                text: Some("Run the tests".into())
            })
        );
        let limits = event(
            "account/rateLimits/updated",
            &json!({"rateLimits": {"limitId": "codex", "limitName": null, "primary": {"usedPercent": 42.5, "windowDurationMins": 300, "resetsAt": 1700000000},
                "secondary": null, "credits": null, "planType": "pro", "rateLimitReachedType": null}}),
        );
        assert!(matches!(
            limits,
            Some(AgentEvent::RateLimitsUpdated(RateLimits {
                plan: Some(_),
                reached: false,
                ..
            }))
        ));
        assert_eq!(
            account(&json!({"type": "chatgpt", "email": "a@b.c", "planType": "pro", "workspaceName": null}))
                .and_then(|a| a.plan_label()),
            Some("Pro".to_string())
        );
        let asked = request(
            &json!(5),
            "item/tool/requestUserInput",
            &json!({"threadId": "t", "turnId": "u", "itemId": "i",
            "questions": [{"id": "q", "header": "Pick", "question": "Which?", "isOther": true, "isSecret": false,
                "options": [{"label": "A", "description": "first"}]}], "isBlocking": true, "autoResolutionMs": null}),
        );
        assert!(
            matches!(asked, Some(AgentEvent::UserInputRequested(r)) if r.questions[0].options.len() == 1)
        );
        assert!(thread_meta(&json!({"id": "x", "ephemeral": true})).is_none());
    }

    #[test]
    fn approvals_translate_and_others_do_not() {
        let ev = request(
            &json!(0),
            "item/commandExecution/requestApproval",
            &json!({"threadId": "t", "turnId": "u", "itemId": "c", "command": "cargo test", "cwd": "/r", "kind": "command", "startedAtMs": 1, "environmentId": null}),
        )
        .unwrap();
        assert!(matches!(
            ev,
            AgentEvent::ApprovalRequested(ApprovalRequest {
                kind: ApprovalKind::Command { .. },
                ..
            })
        ));
        assert!(request(&json!(1), "account/chatgptAuthTokens/refresh", &json!({})).is_none());
    }
}
