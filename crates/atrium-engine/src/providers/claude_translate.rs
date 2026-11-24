// Ported from apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts
// (T3 Code v0.1.11, MIT): `terminalStatusFromResult`, the tool classification
// table (`CLAUDE_KNOWN_TOOL_CLASSIFICATIONS`), the plan/todo projection, the
// subagent lifecycle projection and the rate-limit mapping. The Agent SDK's
// frame shapes are already typed by `claude-client`; this module turns them
// into `atrium_core::AgentEvent`s without holding a process.
//!
//! Pure translation: no process, no clock. Time-dependent values (`now_secs`
//! for a usage-limit reset) are passed in by the adapter, which takes them
//! from the caller's injected `now`.

use std::collections::{HashMap, HashSet};

use atrium_core::{
    AgentCommand, AgentEvent, ApprovalKind, ApprovalRequest, Cost, Item, ItemStatus, PlanStep,
    RateLimits, RateWindow, ThreadMeta, TokenCounts, TokenUsage, TurnStatus,
};
use claude_client::events::{
    AssistantMessage, BackgroundTask, ClaudeEvent, ContentBlock, Delta, PermissionDenied,
    RateLimitInfo, SessionInit, StreamEvent, TaskNotification, TaskProgress, TaskStarted,
    TurnResult, Usage, UserEvent,
};
use claude_client::{PermissionRequest, SessionInfo};
use serde_json::Value;

/// T3's `CLAUDE_PROVIDER` (`ProviderDriverKind.make("claudeAgent")`).
pub const CLAUDE_DRIVER_KIND: &str = "claudeAgent";

/// What kind of row a Claude tool call becomes, from T3's
/// `CLAUDE_KNOWN_TOOL_CLASSIFICATIONS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolClass {
    Command,
    FileChange,
    FileRead,
    WebSearch,
    Plan,
    SubAgent,
    Other,
}

fn normalized(name: &str) -> String {
    name.to_ascii_lowercase().replace(['_', '-', ' '], "")
}

/// T3's `classifyClaudeNativeTool`, reduced to the classes the item model
/// distinguishes.
pub fn classify_tool(name: &str) -> ToolClass {
    match normalized(name).as_str() {
        "bash" | "shell" | "localshell" | "monitor" | "sendmessage" | "taskstop" => {
            ToolClass::Command
        }
        "edit" | "multiedit" | "write" | "notebookedit" => ToolClass::FileChange,
        "read" | "glob" | "grep" | "ls" => ToolClass::FileRead,
        "webfetch" | "websearch" => ToolClass::WebSearch,
        "todowrite" => ToolClass::Plan,
        "task" | "agent" => ToolClass::SubAgent,
        _ => ToolClass::Other,
    }
}

/// The request kind T3 reports for a tool, `ProviderRequestKind` names.
pub fn request_kind_for_tool(name: &str) -> &'static str {
    match classify_tool(name) {
        ToolClass::Command | ToolClass::SubAgent => "command",
        ToolClass::FileChange => "file-change",
        ToolClass::FileRead => "file-read",
        ToolClass::WebSearch => "command",
        ToolClass::Plan => "command",
        ToolClass::Other if name.to_ascii_lowercase().starts_with("mcp__") => "permission",
        ToolClass::Other => "command",
    }
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                Value::String(text) => Some(text.clone()),
                Value::Object(_) => block.get("text").and_then(Value::as_str).map(str::to_owned),
                _ => None,
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => pretty(other),
    }
}

fn pretty(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

fn command_of(input: &Value) -> String {
    match &input["command"] {
        Value::String(command) => command.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" "),
        _ => input
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    }
}

fn counts(usage: &Usage) -> TokenCounts {
    let prompt =
        usage.input_tokens + usage.cache_creation_input_tokens + usage.cache_read_input_tokens;
    TokenCounts {
        total: prompt + usage.output_tokens,
        input: prompt,
        cached_input: usage.cache_read_input_tokens,
        output: usage.output_tokens,
        reasoning: usage.thinking_tokens.unwrap_or_default(),
    }
}

/// The item id a streamed or completed content block belongs to.
fn block_id(message: &str, index: u64) -> String {
    format!("{message}:block:{index}")
}

#[derive(Clone, Debug)]
struct ToolCall {
    name: String,
    input: Value,
}

struct Turn {
    id: String,
}

/// Stateful translation of one live Claude session.
///
/// One translator per native session. It remembers the open turn, the tool
/// calls whose results have not arrived, and the background tasks the CLI
/// reported, so frames can be folded with the same meaning T3 gives them.
pub struct ClaudeTranslator {
    driver: String,
    thread_id: String,
    backend: String,
    cwd: String,
    model: Option<String>,
    turn: Option<Turn>,
    /// The assistant message currently streaming.
    message: Option<String>,
    /// Content-block index to the item id it streams into.
    streamed_blocks: HashMap<u64, String>,
    tool_calls: HashMap<String, ToolCall>,
    /// TodoWrite calls whose tool result should not create a second row.
    plan_calls: HashSet<String>,
    tasks: HashMap<String, String>,
    usage_limit_seen: bool,
    next_synthetic: u64,
    title: Option<String>,
    announced: bool,
}

impl ClaudeTranslator {
    pub fn new(driver: impl Into<String>, thread_id: impl Into<String>) -> Self {
        Self {
            driver: driver.into(),
            thread_id: thread_id.into(),
            backend: "claude".into(),
            cwd: String::new(),
            model: None,
            turn: None,
            message: None,
            streamed_blocks: HashMap::new(),
            tool_calls: HashMap::new(),
            plan_calls: HashSet::new(),
            tasks: HashMap::new(),
            usage_limit_seen: false,
            next_synthetic: 0,
            title: None,
            announced: false,
        }
    }

    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    pub fn driver(&self) -> &str {
        &self.driver
    }

    pub fn current_turn(&self) -> Option<&str> {
        self.turn.as_ref().map(|turn| turn.id.as_str())
    }

    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub fn set_title(&mut self, title: Option<String>) {
        self.title = title;
    }

    /// Opens a turn and returns its `TurnStarted`.
    pub fn begin_turn(&mut self, turn_id: impl Into<String>, model: Option<String>) -> AgentEvent {
        let turn_id = turn_id.into();
        if let Some(model) = &model {
            self.model = Some(model.clone());
        }
        self.turn = Some(Turn {
            id: turn_id.clone(),
        });
        self.message = None;
        self.streamed_blocks.clear();
        self.tool_calls.clear();
        self.plan_calls.clear();
        // Rate-limit evidence is per turn, as T3 tracks it while a turn
        // streams.
        self.usage_limit_seen = false;
        AgentEvent::TurnStarted {
            thread_id: self.thread_id.clone(),
            turn_id,
        }
    }

    /// The user's message, shown as soon as the adapter sends it. Claude does
    /// not echo the prompt, so the adapter supplies it.
    pub fn user_item(
        &self,
        turn_id: &str,
        message_id: &str,
        text: &str,
        attachments: Vec<String>,
    ) -> AgentEvent {
        AgentEvent::ItemCompleted {
            thread_id: self.thread_id.clone(),
            turn_id: turn_id.to_owned(),
            item: Item::User {
                id: format!("user:{message_id}"),
                text: text.to_owned(),
                attachments,
            },
        }
    }

    pub fn thread_started(&self, unix_seconds: i64) -> AgentEvent {
        AgentEvent::ThreadStarted(ThreadMeta {
            id: self.thread_id.clone(),
            backend: self.backend.clone(),
            name: self.title.clone(),
            preview: self.title.clone().unwrap_or_default(),
            cwd: self.cwd.clone(),
            model: self.model.clone(),
            created_at: unix_seconds,
            updated_at: unix_seconds,
        })
    }

    /// The commands and skills the CLI advertised at `initialize`, as T3
    /// reports them on a provider thread.
    pub fn commands(&self, info: Option<&SessionInfo>, init: &SessionInit) -> AgentEvent {
        let commands = info
            .map(|info| {
                info.commands
                    .iter()
                    .map(|command| AgentCommand {
                        name: command.name.clone(),
                        description: command.description.clone(),
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|commands: &Vec<AgentCommand>| !commands.is_empty())
            .unwrap_or_else(|| {
                init.slash_commands
                    .iter()
                    .map(|name| AgentCommand {
                        name: name.clone(),
                        description: String::new(),
                    })
                    .collect()
            });
        AgentEvent::CommandsUpdated {
            thread_id: self.thread_id.clone(),
            commands,
        }
    }

    /// One translated CLI frame.
    pub fn on_event(&mut self, event: &ClaudeEvent, now_secs: i64) -> Vec<AgentEvent> {
        match event {
            ClaudeEvent::SessionInit(init) => self.session_init(init, now_secs),
            ClaudeEvent::Stream(stream) => self.stream(stream),
            ClaudeEvent::Assistant(message) => self.assistant(message),
            ClaudeEvent::User(message) => self.user(message),
            ClaudeEvent::Result(result) => self.result(result, now_secs),
            ClaudeEvent::RateLimit(info) => self.rate_limit(info),
            ClaudeEvent::ApiRetry(retry) => vec![AgentEvent::Error {
                thread_id: Some(self.thread_id.clone()),
                message: format!(
                    "Claude is retrying the request (attempt {}/{}){}",
                    retry.attempt,
                    retry.max_retries,
                    if retry.error.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", retry.error)
                    }
                ),
                will_retry: true,
            }],
            ClaudeEvent::TaskStarted(task) => vec![self.task_started(task)],
            ClaudeEvent::TaskProgress(task) => vec![self.task_progress(task)],
            ClaudeEvent::TaskNotification(task) => vec![self.task_notification(task)],
            ClaudeEvent::BackgroundTasksChanged(tasks) => self.background_tasks(tasks),
            ClaudeEvent::ThinkingTokens {
                estimated_tokens, ..
            } => vec![AgentEvent::TokenUsage {
                thread_id: self.thread_id.clone(),
                usage: TokenUsage {
                    last: TokenCounts {
                        reasoning: *estimated_tokens,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            }],
            ClaudeEvent::CompactBoundary(boundary) => {
                self.next_synthetic += 1;
                let id = boundary
                    .uuid
                    .clone()
                    .unwrap_or_else(|| format!("compact-{}", self.next_synthetic));
                vec![AgentEvent::ItemCompleted {
                    thread_id: self.thread_id.clone(),
                    turn_id: self.current_turn().unwrap_or_default().to_owned(),
                    item: Item::Other {
                        id,
                        label: "Context compacted".into(),
                    },
                }]
            }
            ClaudeEvent::PermissionDenied(denied) => {
                vec![AgentEvent::Warning(self.permission_denied_text(denied))]
            }
            ClaudeEvent::Status(status) => {
                let mut events = Vec::new();
                if let Some(error) = &status.compact_error {
                    events.push(AgentEvent::Warning(format!("Compaction failed: {error}")));
                } else if let Some(result) = &status.compact_result
                    && !result.is_empty()
                {
                    events.push(AgentEvent::Warning(format!("Context compacted: {result}")));
                }
                events
            }
            ClaudeEvent::HookStarted(_)
            | ClaudeEvent::HookResponse(_)
            | ClaudeEvent::ToolProgress(_)
            | ClaudeEvent::Unknown { .. } => Vec::new(),
        }
    }

    fn session_init(&mut self, init: &SessionInit, now_secs: i64) -> Vec<AgentEvent> {
        if !init.cwd.is_empty() {
            self.cwd = init.cwd.clone();
        }
        if !init.model.is_empty() {
            self.model = Some(init.model.clone());
        }
        let mut events = Vec::new();
        if !self.announced {
            // The CLI's init frame is the first proof the session exists; a
            // ThreadStarted lets the app register the session before items.
            self.announced = true;
            events.push(self.thread_started(now_secs));
        }
        events.push(AgentEvent::CommandsUpdated {
            thread_id: self.thread_id.clone(),
            commands: init
                .slash_commands
                .iter()
                .map(|name| AgentCommand {
                    name: name.clone(),
                    description: String::new(),
                })
                .collect(),
        });
        events
    }

    fn stream(&mut self, stream: &StreamEvent) -> Vec<AgentEvent> {
        let turn = self.current_turn().map(str::to_owned).unwrap_or_default();
        match stream {
            StreamEvent::MessageStart {
                model,
                message_id,
                usage,
            } => {
                self.next_synthetic += 1;
                let message = message_id
                    .clone()
                    .unwrap_or_else(|| format!("message-{}", self.next_synthetic));
                self.message = Some(message);
                self.streamed_blocks.clear();
                if let Some(model) = model {
                    self.model = Some(model.clone());
                }
                let mut events = Vec::new();
                if let Some(usage) = usage {
                    events.push(AgentEvent::TokenUsage {
                        thread_id: self.thread_id.clone(),
                        usage: TokenUsage {
                            last: counts(usage),
                            ..Default::default()
                        },
                    });
                }
                events.extend(self.turn.is_none().then(|| AgentEvent::TurnStarted {
                    thread_id: self.thread_id.clone(),
                    turn_id: turn,
                }));
                events
            }
            StreamEvent::ContentBlockStart { index, block } => {
                let message = self
                    .message
                    .clone()
                    .unwrap_or_else(|| format!("message-{}", self.next_synthetic));
                let id = block_id(&message, *index);
                self.streamed_blocks.insert(*index, id.clone());
                match block {
                    ContentBlock::Text { .. } => vec![AgentEvent::ItemStarted {
                        thread_id: self.thread_id.clone(),
                        turn_id: turn,
                        item: Item::Agent {
                            id,
                            text: String::new(),
                        },
                    }],
                    ContentBlock::Thinking { .. } => vec![AgentEvent::ItemStarted {
                        thread_id: self.thread_id.clone(),
                        turn_id: turn,
                        item: Item::Reasoning {
                            id,
                            text: String::new(),
                        },
                    }],
                    ContentBlock::ToolUse {
                        id: tool_id,
                        name,
                        input,
                        ..
                    } => {
                        if classify_tool(name) == ToolClass::Plan {
                            return self.plan_event(input, &turn);
                        }
                        self.tool_calls.insert(
                            tool_id.clone(),
                            ToolCall {
                                name: name.clone(),
                                input: input.clone(),
                            },
                        );
                        vec![AgentEvent::ItemStarted {
                            thread_id: self.thread_id.clone(),
                            turn_id: turn,
                            item: tool_item(tool_id, name, input, ItemStatus::InProgress, ""),
                        }]
                    }
                    _ => Vec::new(),
                }
            }
            StreamEvent::ContentBlockDelta { index, delta } => {
                let Some(id) = self.streamed_blocks.get(index).cloned() else {
                    return Vec::new();
                };
                match delta {
                    Delta::Text { text } => vec![AgentEvent::AgentMessageDelta {
                        thread_id: self.thread_id.clone(),
                        turn_id: turn,
                        item_id: id,
                        delta: text.clone(),
                    }],
                    Delta::Thinking { thinking, .. } => vec![AgentEvent::ReasoningDelta {
                        thread_id: self.thread_id.clone(),
                        turn_id: turn,
                        item_id: id,
                        delta: thinking.clone(),
                    }],
                    Delta::Signature { .. } | Delta::InputJson { .. } | Delta::Other { .. } => {
                        Vec::new()
                    }
                }
            }
            StreamEvent::MessageDelta { usage, .. } => usage
                .as_ref()
                .map(|usage| {
                    vec![AgentEvent::TokenUsage {
                        thread_id: self.thread_id.clone(),
                        usage: TokenUsage {
                            last: counts(usage),
                            ..Default::default()
                        },
                    }]
                })
                .unwrap_or_default(),
            StreamEvent::ContentBlockStop { .. }
            | StreamEvent::MessageStop
            | StreamEvent::Ping
            | StreamEvent::Other { .. } => Vec::new(),
        }
    }

    fn assistant(&mut self, message: &AssistantMessage) -> Vec<AgentEvent> {
        let turn = self.current_turn().map(str::to_owned).unwrap_or_default();
        let key = message
            .message_id
            .clone()
            .or_else(|| message.uuid.clone())
            .unwrap_or_else(|| {
                self.next_synthetic += 1;
                format!("message-{}", self.next_synthetic)
            });
        let same_message = self.message.as_deref() == Some(key.as_str());
        if !same_message {
            self.message = Some(key.clone());
            self.streamed_blocks.clear();
        }
        let mut events = Vec::new();
        if let Some(model) = &message.model {
            self.model = Some(model.clone());
        }
        if let Some(error) = &message.error {
            events.push(AgentEvent::Error {
                thread_id: Some(self.thread_id.clone()),
                message: error.clone(),
                will_retry: false,
            });
        }
        for (index, block) in message.content.iter().enumerate() {
            let index = index as u64;
            let streamed = if same_message {
                self.streamed_blocks.get(&index).cloned()
            } else {
                None
            };
            let id = streamed.clone().unwrap_or_else(|| {
                message
                    .uuid
                    .as_ref()
                    .map(|uuid| format!("{key}:{uuid}:{index}"))
                    .unwrap_or_else(|| block_id(&key, index))
            });
            match block {
                ContentBlock::Text { text } => events.push(AgentEvent::ItemCompleted {
                    thread_id: self.thread_id.clone(),
                    turn_id: turn.clone(),
                    item: Item::Agent {
                        id,
                        text: text.clone(),
                    },
                }),
                ContentBlock::Thinking { thinking, .. } => events.push(AgentEvent::ItemCompleted {
                    thread_id: self.thread_id.clone(),
                    turn_id: turn.clone(),
                    item: Item::Reasoning {
                        id,
                        text: thinking.clone(),
                    },
                }),
                ContentBlock::ToolUse {
                    id: tool_id,
                    name,
                    input,
                    ..
                } => {
                    if classify_tool(name) == ToolClass::Plan {
                        self.plan_calls.insert(tool_id.clone());
                        events.extend(self.plan_event(input, &turn));
                        continue;
                    }
                    self.tool_calls.insert(
                        tool_id.clone(),
                        ToolCall {
                            name: name.clone(),
                            input: input.clone(),
                        },
                    );
                    events.push(AgentEvent::ItemStarted {
                        thread_id: self.thread_id.clone(),
                        turn_id: turn.clone(),
                        item: tool_item(tool_id, name, input, ItemStatus::InProgress, ""),
                    });
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => events.extend(self.tool_result(
                    tool_use_id,
                    content,
                    *is_error,
                    message.user_message_uuid.as_deref(),
                    &turn,
                )),
                ContentBlock::Image { .. } => events.push(AgentEvent::ItemCompleted {
                    thread_id: self.thread_id.clone(),
                    turn_id: turn.clone(),
                    item: Item::Other {
                        id,
                        label: "Image".into(),
                    },
                }),
                ContentBlock::Other { kind, .. } => events.push(AgentEvent::ItemCompleted {
                    thread_id: self.thread_id.clone(),
                    turn_id: turn.clone(),
                    item: Item::Other {
                        id,
                        label: kind.clone(),
                    },
                }),
            }
        }
        events
    }

    fn user(&mut self, message: &UserEvent) -> Vec<AgentEvent> {
        let turn = self.current_turn().map(str::to_owned).unwrap_or_default();
        let mut events = Vec::new();
        for block in &message.content {
            if let ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } = block
            {
                events.extend(self.tool_result(
                    tool_use_id,
                    content,
                    *is_error,
                    message.uuid.as_deref(),
                    &turn,
                ));
            }
        }
        events
    }

    fn tool_result(
        &mut self,
        tool_use_id: &str,
        content: &Value,
        is_error: bool,
        uuid: Option<&str>,
        turn: &str,
    ) -> Vec<AgentEvent> {
        if self.plan_calls.remove(tool_use_id) {
            return Vec::new();
        }
        let status = if is_error {
            ItemStatus::Failed
        } else {
            ItemStatus::Completed
        };
        let output = text_of(content);
        let Some(call) = self.tool_calls.remove(tool_use_id) else {
            return vec![AgentEvent::ItemCompleted {
                thread_id: self.thread_id.clone(),
                turn_id: turn.to_owned(),
                item: Item::Tool {
                    id: uuid
                        .map(|uuid| format!("tool:{uuid}"))
                        .unwrap_or_else(|| format!("tool:{tool_use_id}")),
                    name: "Tool".into(),
                    status,
                    arguments: String::new(),
                    result: output,
                    duration_ms: None,
                },
            }];
        };
        let item = match classify_tool(&call.name) {
            ToolClass::Command => Item::Command {
                id: tool_use_id.to_owned(),
                command: command_of(&call.input),
                cwd: call
                    .input
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or(&self.cwd)
                    .to_owned(),
                status,
                output,
                exit_code: None,
                duration_ms: None,
            },
            ToolClass::WebSearch => Item::WebSearch {
                id: tool_use_id.to_owned(),
                query: call
                    .input
                    .get("query")
                    .or_else(|| call.input.get("url"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                detail: Some(output).filter(|detail| !detail.is_empty()),
            },
            ToolClass::SubAgent => Item::SubAgent {
                id: tool_use_id.to_owned(),
                label: call.name.clone(),
                status,
                detail: Some(output).filter(|detail| !detail.is_empty()),
            },
            ToolClass::Plan => Item::Tool {
                id: tool_use_id.to_owned(),
                name: call.name.clone(),
                status,
                arguments: pretty(&call.input),
                result: output,
                duration_ms: None,
            },
            ToolClass::FileChange | ToolClass::FileRead | ToolClass::Other => Item::Tool {
                id: tool_use_id.to_owned(),
                name: call.name.clone(),
                status,
                arguments: pretty(&call.input),
                result: output,
                duration_ms: None,
            },
        };
        vec![AgentEvent::ItemCompleted {
            thread_id: self.thread_id.clone(),
            turn_id: turn.to_owned(),
            item,
        }]
    }

    fn plan_event(&mut self, input: &Value, turn: &str) -> Vec<AgentEvent> {
        let steps = plan_steps(input);
        if steps.is_empty() {
            return Vec::new();
        }
        vec![AgentEvent::PlanUpdated {
            thread_id: self.thread_id.clone(),
            turn_id: turn.to_owned(),
            explanation: None,
            steps,
        }]
    }

    fn result(&mut self, result: &TurnResult, now_secs: i64) -> Vec<AgentEvent> {
        let turn = self.current_turn().map(str::to_owned).unwrap_or_default();
        let mut events = Vec::new();
        if let Some(usage) = result.usage.as_ref() {
            let context_window = result
                .model_usage
                .values()
                .map(|usage| usage.context_window)
                .find(|window| *window > 0);
            events.push(AgentEvent::TokenUsage {
                thread_id: self.thread_id.clone(),
                usage: TokenUsage {
                    total: counts(usage),
                    last: counts(usage),
                    context_used: Some(
                        usage.input_tokens
                            + usage.cache_creation_input_tokens
                            + usage.cache_read_input_tokens,
                    ),
                    context_window,
                    cost: result.total_cost_usd.map(|amount| Cost {
                        amount,
                        currency: "USD".into(),
                    }),
                },
            });
        }
        let status = core_turn_status(result.terminal_status());
        let error = if status == TurnStatus::Failed {
            result
                .failure_at(self.usage_limit_seen, now_secs)
                .map(|failure| failure.message)
                .or_else(|| result.user_facing_error().map(str::to_owned))
                .or_else(|| Some("Claude stopped the turn.".into()))
        } else {
            None
        };
        events.push(AgentEvent::TurnCompleted {
            thread_id: self.thread_id.clone(),
            turn_id: turn,
            status,
            error,
            duration_ms: result.duration_ms,
        });
        self.turn = None;
        self.message = None;
        self.streamed_blocks.clear();
        self.tool_calls.clear();
        self.plan_calls.clear();
        events
    }

    fn rate_limit(&mut self, info: &RateLimitInfo) -> Vec<AgentEvent> {
        if info.is_blocked() {
            self.usage_limit_seen = true;
        }
        let percent = info.utilization.map(|utilization| utilization * 100.0);
        let window = |mins: Option<u64>| RateWindow {
            used_percent: percent.unwrap_or_default().clamp(0.0, 100.0),
            window_mins: mins,
            resets_at: info.resets_at,
        };
        let mut limits = RateLimits {
            name: info.rate_limit_type.clone(),
            reached: info.is_blocked(),
            ..Default::default()
        };
        match info.rate_limit_type.as_deref() {
            Some("five_hour") => limits.primary = Some(window(Some(300))),
            Some("seven_day") => limits.secondary = Some(window(Some(10_080))),
            Some(_) => limits.primary = Some(window(None)),
            None => {}
        }
        vec![AgentEvent::RateLimitsUpdated(limits)]
    }

    fn task_started(&mut self, task: &TaskStarted) -> AgentEvent {
        let id = format!("task:{}", task.task_id);
        self.tasks.insert(task.task_id.clone(), id.clone());
        AgentEvent::ItemStarted {
            thread_id: self.thread_id.clone(),
            turn_id: self.current_turn().unwrap_or_default().to_owned(),
            item: Item::SubAgent {
                id,
                label: task_label(&task.description, task.subagent_type.as_deref()),
                status: ItemStatus::InProgress,
                detail: task
                    .subagent_type
                    .clone()
                    .or_else(|| task.task_type.clone())
                    .or_else(|| task.prompt.clone()),
            },
        }
    }

    fn task_progress(&mut self, task: &TaskProgress) -> AgentEvent {
        let id = self
            .tasks
            .get(&task.task_id)
            .cloned()
            .unwrap_or_else(|| format!("task:{}", task.task_id));
        AgentEvent::ItemStarted {
            thread_id: self.thread_id.clone(),
            turn_id: self.current_turn().unwrap_or_default().to_owned(),
            item: Item::SubAgent {
                id,
                label: task_label(&task.description, task.subagent_type.as_deref()),
                status: ItemStatus::InProgress,
                detail: task
                    .summary
                    .clone()
                    .or_else(|| task.last_tool_name.clone())
                    .or_else(|| task.subagent_type.clone()),
            },
        }
    }

    fn task_notification(&mut self, task: &TaskNotification) -> AgentEvent {
        let id = self
            .tasks
            .remove(&task.task_id)
            .unwrap_or_else(|| format!("task:{}", task.task_id));
        AgentEvent::ItemCompleted {
            thread_id: self.thread_id.clone(),
            turn_id: self.current_turn().unwrap_or_default().to_owned(),
            item: Item::SubAgent {
                id,
                label: "Sub-agent".into(),
                status: match task.status.as_str() {
                    "completed" => ItemStatus::Completed,
                    "failed" => ItemStatus::Failed,
                    _ => ItemStatus::Completed,
                },
                detail: Some(task.summary.clone()).filter(|summary| !summary.is_empty()),
            },
        }
    }

    fn background_tasks(&mut self, tasks: &[BackgroundTask]) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        for task in tasks {
            if self.tasks.contains_key(&task.task_id) {
                continue;
            }
            let id = format!("task:{}", task.task_id);
            self.tasks.insert(task.task_id.clone(), id.clone());
            events.push(AgentEvent::ItemStarted {
                thread_id: self.thread_id.clone(),
                turn_id: self.current_turn().unwrap_or_default().to_owned(),
                item: Item::SubAgent {
                    id,
                    label: task_label(&task.description, task.subagent_type.as_deref()),
                    status: ItemStatus::InProgress,
                    detail: Some(task.task_type.clone()),
                },
            });
        }
        events
    }

    fn permission_denied_text(&self, denied: &PermissionDenied) -> String {
        if denied.message.is_empty() {
            format!("Claude did not run {}.", denied.tool_name)
        } else {
            denied.message.clone()
        }
    }

    /// A `can_use_tool` prompt as an approval, with T3's request kind.
    pub fn permission(&mut self, request: &PermissionRequest) -> (AgentEvent, &'static str) {
        let class = classify_tool(&request.tool_name);
        let title = request
            .display_name
            .clone()
            .or_else(|| request.title.clone())
            .unwrap_or_else(|| request.tool_name.clone());
        let reason = request
            .decision_reason
            .clone()
            .or_else(|| request.description.clone());
        let kind = match class {
            ToolClass::Command => ApprovalKind::Command {
                command: command_of(&request.input),
                cwd: request
                    .input
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                reason,
            },
            ToolClass::FileChange => ApprovalKind::FileChange {
                reason,
                grant_root: request.blocked_path.clone(),
            },
            ToolClass::FileRead => ApprovalKind::Tool {
                title: format!("Read {}", request.tool_name),
                detail: Some(pretty(&request.input)),
            },
            ToolClass::WebSearch => ApprovalKind::Tool {
                title: format!("Search with {}", request.tool_name),
                detail: Some(pretty(&request.input)),
            },
            ToolClass::Plan | ToolClass::SubAgent | ToolClass::Other => ApprovalKind::Tool {
                title,
                detail: Some(pretty(&request.input)).filter(|detail| !detail.is_empty()),
            },
        };
        (
            AgentEvent::ApprovalRequested(ApprovalRequest {
                request_id: Value::String(request.request_id.clone()),
                thread_id: self.thread_id.clone(),
                turn_id: self.current_turn().unwrap_or_default().to_owned(),
                item_id: request
                    .tool_use_id
                    .clone()
                    .unwrap_or_else(|| request.request_id.clone()),
                kind,
            }),
            request_kind_for_tool(&request.tool_name),
        )
    }
}

fn task_label(description: &str, subagent_type: Option<&str>) -> String {
    if !description.is_empty() {
        description.to_owned()
    } else if let Some(subagent_type) = subagent_type {
        format!("{subagent_type} sub-agent")
    } else {
        "Sub-agent".into()
    }
}

fn plan_steps(input: &Value) -> Vec<PlanStep> {
    let todos = input
        .get("todos")
        .or_else(|| input.get("items"))
        .and_then(Value::as_array);
    todos
        .map(|todos| {
            todos
                .iter()
                .filter_map(|todo| {
                    let step = todo
                        .get("content")
                        .or_else(|| todo.get("step"))
                        .or_else(|| todo.get("activeForm"))
                        .and_then(Value::as_str)?;
                    let status = todo
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    Some(PlanStep {
                        step: step.to_owned(),
                        done: status == "completed",
                        active: status == "in_progress" || status == "inProgress",
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn tool_item(id: &str, name: &str, input: &Value, status: ItemStatus, result: &str) -> Item {
    match classify_tool(name) {
        ToolClass::Command => Item::Command {
            id: id.to_owned(),
            command: command_of(input),
            cwd: input
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            status,
            output: result.to_owned(),
            exit_code: None,
            duration_ms: None,
        },
        ToolClass::WebSearch => Item::WebSearch {
            id: id.to_owned(),
            query: input
                .get("query")
                .or_else(|| input.get("url"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            detail: None,
        },
        ToolClass::SubAgent => Item::SubAgent {
            id: id.to_owned(),
            label: name.to_owned(),
            status,
            detail: input
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        ToolClass::Plan | ToolClass::FileChange | ToolClass::FileRead | ToolClass::Other => {
            Item::Tool {
                id: id.to_owned(),
                name: name.to_owned(),
                status,
                arguments: pretty(input),
                result: result.to_owned(),
                duration_ms: None,
            }
        }
    }
}

/// T3's `terminalStatusFromResult` mapped onto the app's four states.
pub fn core_turn_status(status: claude_client::events::TurnStatus) -> TurnStatus {
    match status {
        claude_client::events::TurnStatus::Completed => TurnStatus::Completed,
        claude_client::events::TurnStatus::Interrupted
        | claude_client::events::TurnStatus::Cancelled => TurnStatus::Interrupted,
        claude_client::events::TurnStatus::Failed => TurnStatus::Failed,
    }
}

/// The wire name used by [`crate::providers::adapter::AdapterEvent::TurnTerminal`].
pub fn terminal_status_wire(status: claude_client::events::TurnStatus) -> &'static str {
    match status {
        claude_client::events::TurnStatus::Completed => "completed",
        claude_client::events::TurnStatus::Interrupted => "interrupted",
        claude_client::events::TurnStatus::Failed => "failed",
        claude_client::events::TurnStatus::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atrium_core::{EntryKind, Transcript};
    use claude_client::events::{CompactBoundary, TaskUsage, TurnStatus as ClaudeTurnStatus};
    use serde_json::json;

    fn translator() -> ClaudeTranslator {
        ClaudeTranslator::new(CLAUDE_DRIVER_KIND, "native-session")
    }

    fn fold(transcript: &mut Transcript, events: Vec<AgentEvent>) {
        for event in &events {
            transcript.apply(event);
        }
    }

    fn text(value: &str) -> ContentBlock {
        ContentBlock::Text {
            text: value.to_owned(),
        }
    }

    // Ported from ClaudeAdapterV2.test.ts's assistant/streaming cases (T3
    // Code v0.1.11, MIT): partial text becomes one row, and the completed
    // block keeps what streamed.
    #[test]
    fn partial_text_and_a_completed_assistant_block_are_one_row() {
        let mut t = translator();
        let mut transcript = Transcript::new();
        let started = t.begin_turn("turn-1", Some("claude-sonnet-4-6".into()));
        fold(&mut transcript, vec![started]);

        fold(
            &mut transcript,
            t.on_event(
                &ClaudeEvent::Stream(StreamEvent::MessageStart {
                    model: Some("claude-sonnet-4-6".into()),
                    message_id: Some("msg-1".into()),
                    usage: None,
                }),
                0,
            ),
        );
        fold(
            &mut transcript,
            t.on_event(
                &ClaudeEvent::Stream(StreamEvent::ContentBlockStart {
                    index: 0,
                    block: text(""),
                }),
                0,
            ),
        );
        for delta in ["Let me ", "look."] {
            fold(
                &mut transcript,
                t.on_event(
                    &ClaudeEvent::Stream(StreamEvent::ContentBlockDelta {
                        index: 0,
                        delta: Delta::Text { text: delta.into() },
                    }),
                    0,
                ),
            );
        }
        fold(
            &mut transcript,
            t.on_event(
                &ClaudeEvent::Assistant(AssistantMessage {
                    message_id: Some("msg-1".into()),
                    model: Some("claude-sonnet-4-6".into()),
                    content: vec![text("Let me look.")],
                    ..Default::default()
                }),
                0,
            ),
        );
        let agents: Vec<&str> = transcript
            .entries()
            .iter()
            .filter_map(|entry| match &entry.kind {
                EntryKind::Item(Item::Agent { text, .. }) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(agents, ["Let me look."]);
    }

    #[test]
    fn thinking_streams_as_reasoning() {
        let mut t = translator();
        t.begin_turn("turn-1", None);
        let events = t.on_event(
            &ClaudeEvent::Stream(StreamEvent::MessageStart {
                model: None,
                message_id: Some("msg-1".into()),
                usage: None,
            }),
            0,
        );
        assert!(events.is_empty());
        t.on_event(
            &ClaudeEvent::Stream(StreamEvent::ContentBlockStart {
                index: 0,
                block: ContentBlock::Thinking {
                    thinking: String::new(),
                    signature: None,
                },
            }),
            0,
        );
        let events = t.on_event(
            &ClaudeEvent::Stream(StreamEvent::ContentBlockDelta {
                index: 0,
                delta: Delta::Thinking {
                    thinking: "weighing options".into(),
                    estimated_tokens: None,
                },
            }),
            0,
        );
        assert!(matches!(
            &events[0],
            AgentEvent::ReasoningDelta { delta, .. } if delta == "weighing options"
        ));
    }

    // Ported from ClaudeAdapterV2.test.ts's Bash approval cases (T3 Code
    // v0.1.11, MIT): a Bash call is a command item and a command approval.
    #[test]
    fn a_bash_call_and_its_result_are_command_items() {
        let mut t = translator();
        t.begin_turn("turn-1", None);
        t.on_event(
            &ClaudeEvent::Assistant(AssistantMessage {
                message_id: Some("msg-1".into()),
                uuid: Some("u1".into()),
                content: vec![ContentBlock::ToolUse {
                    id: "toolu_1".into(),
                    name: "Bash".into(),
                    input: json!({"command": "echo mock", "cwd": "/work"}),
                    caller: None,
                }],
                ..Default::default()
            }),
            0,
        );
        let events = t.on_event(
            &ClaudeEvent::User(UserEvent {
                uuid: Some("u2".into()),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "toolu_1".into(),
                    content: json!("mock output"),
                    is_error: false,
                }],
                ..Default::default()
            }),
            0,
        );
        let AgentEvent::ItemCompleted {
            item:
                Item::Command {
                    command,
                    cwd,
                    output,
                    status,
                    ..
                },
            ..
        } = &events[0]
        else {
            panic!("unexpected {:?}", events[0]);
        };
        assert_eq!(command, "echo mock");
        assert_eq!(cwd, "/work");
        assert_eq!(output, "mock output");
        assert_eq!(*status, ItemStatus::Completed);

        let (approval, kind) = t.permission(&PermissionRequest {
            request_id: "req-1".into(),
            tool_name: "Bash".into(),
            input: json!({"command": "echo mock"}),
            tool_use_id: Some("toolu_1".into()),
            permission_suggestions: Vec::new(),
            blocked_path: None,
            decision_reason: Some("Bash is not pre-approved".into()),
            decision_reason_type: None,
            agent_id: None,
            mcp_server: None,
            title: None,
            display_name: None,
            description: None,
            suppress_always_allow_rule: None,
        });
        assert_eq!(kind, "command");
        match approval {
            AgentEvent::ApprovalRequested(request) => {
                assert_eq!(request.turn_id, "turn-1");
                assert!(matches!(request.kind, ApprovalKind::Command { .. }));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_file_edit_is_a_file_change_approval() {
        let mut t = translator();
        t.begin_turn("turn-1", None);
        let (approval, kind) = t.permission(&PermissionRequest {
            request_id: "req-2".into(),
            tool_name: "Edit".into(),
            input: json!({"file_path": "/work/a.rs", "old_string": "a", "new_string": "b"}),
            tool_use_id: Some("toolu_2".into()),
            permission_suggestions: Vec::new(),
            blocked_path: Some("/work".into()),
            decision_reason: None,
            decision_reason_type: None,
            agent_id: None,
            mcp_server: None,
            title: None,
            display_name: None,
            description: None,
            suppress_always_allow_rule: None,
        });
        assert_eq!(kind, "file-change");
        assert!(matches!(
            approval,
            AgentEvent::ApprovalRequested(ApprovalRequest {
                kind: ApprovalKind::FileChange { .. },
                ..
            })
        ));
    }

    // Ported from ClaudeAdapterV2.test.ts's TodoWrite/plan cases (T3 Code
    // v0.1.11, MIT).
    #[test]
    fn todowrite_becomes_a_plan_without_a_tool_row() {
        let mut t = translator();
        t.begin_turn("turn-1", None);
        let events = t.on_event(
            &ClaudeEvent::Assistant(AssistantMessage {
                message_id: Some("msg-1".into()),
                uuid: Some("u1".into()),
                content: vec![ContentBlock::ToolUse {
                    id: "toolu_plan".into(),
                    name: "TodoWrite".into(),
                    input: json!({"todos": [
                        {"content": "Read the code", "status": "completed"},
                        {"content": "Write the fix", "status": "in_progress"},
                        {"content": "Run tests", "status": "pending"}
                    ]}),
                    caller: None,
                }],
                ..Default::default()
            }),
            0,
        );
        let Some(AgentEvent::PlanUpdated { steps, .. }) = events.first() else {
            panic!("unexpected {events:?}");
        };
        assert_eq!(steps.len(), 3);
        assert!(steps[0].done);
        assert!(steps[1].active);
        assert!(!steps[2].done && !steps[2].active);

        // The tool result for the plan call adds no second row.
        let events = t.on_event(
            &ClaudeEvent::User(UserEvent {
                uuid: Some("u2".into()),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "toolu_plan".into(),
                    content: json!("Todos updated"),
                    is_error: false,
                }],
                ..Default::default()
            }),
            0,
        );
        assert!(events.is_empty());
    }

    // Ported from ClaudeAdapterV2.test.ts's subagent lifecycle cases (T3
    // Code v0.1.11, MIT).
    #[test]
    fn a_subagent_lifecycle_is_a_child_item() {
        let mut t = translator();
        t.begin_turn("turn-1", None);
        t.tasks.insert("task-1".into(), "task:task-1".into());
        let started = t.on_event(
            &ClaudeEvent::TaskStarted(TaskStarted {
                task_id: "task-1".into(),
                description: "Read package.json".into(),
                subagent_type: Some("Explore".into()),
                ..Default::default()
            }),
            0,
        );
        assert!(matches!(
            &started[0],
            AgentEvent::ItemStarted {
                item: Item::SubAgent { id, label, status, .. },
                ..
            } if id == "task:task-1" && label == "Read package.json"
                && *status == ItemStatus::InProgress
        ));
        let done = t.on_event(
            &ClaudeEvent::TaskNotification(TaskNotification {
                task_id: "task-1".into(),
                status: "completed".into(),
                summary: "12 tools".into(),
                usage: Some(TaskUsage {
                    total_tokens: 10_750,
                    tool_uses: 1,
                    duration_ms: 5_183,
                }),
                ..Default::default()
            }),
            0,
        );
        assert!(matches!(
            &done[0],
            AgentEvent::ItemCompleted {
                item: Item::SubAgent { id, status: ItemStatus::Completed, .. },
                ..
            } if id == "task:task-1"
        ));
    }

    #[test]
    fn background_task_changes_add_child_items_once() {
        let mut t = translator();
        t.begin_turn("turn-1", None);
        let tasks = vec![BackgroundTask {
            task_id: "bg-1".into(),
            task_type: "local_bash".into(),
            description: "sleep".into(),
            ..Default::default()
        }];
        let first = t.on_event(&ClaudeEvent::BackgroundTasksChanged(tasks.clone()), 0);
        assert_eq!(first.len(), 1);
        let again = t.on_event(&ClaudeEvent::BackgroundTasksChanged(tasks), 0);
        assert!(again.is_empty());
    }

    // Ported from ClaudeAdapterV2.test.ts's result classification cases (T3
    // Code v0.1.11, MIT), through `claude-client`'s ported classifiers.
    #[test]
    fn result_statuses_and_usage_map_onto_agent_events() {
        let mut t = translator();
        t.begin_turn("turn-1", None);
        let events = t.on_event(
            &ClaudeEvent::Result(TurnResult {
                subtype: "success".into(),
                session_id: "s1".into(),
                result: "done".into(),
                terminal_reason: Some("completed".into()),
                duration_ms: Some(1_291),
                total_cost_usd: Some(0.01),
                usage: Some(Usage {
                    input_tokens: 3,
                    output_tokens: 6,
                    cache_creation_input_tokens: 2_338,
                    cache_read_input_tokens: 9_455,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            0,
        );
        assert!(matches!(
            &events[0],
            AgentEvent::TokenUsage { usage, .. }
                if usage.total.output == 6 && usage.context_window.is_none()
        ));
        assert!(matches!(
            &events[1],
            AgentEvent::TurnCompleted {
                status: TurnStatus::Completed,
                duration_ms: Some(1_291),
                ..
            }
        ));

        let mut t = translator();
        t.begin_turn("turn-1", None);
        let events = t.on_event(
            &ClaudeEvent::Result(TurnResult {
                subtype: "error_max_turns".into(),
                session_id: "s1".into(),
                errors: vec!["Max turns reached".into()],
                terminal_reason: Some("aborted_streaming".into()),
                ..Default::default()
            }),
            0,
        );
        assert!(matches!(
            &events.last().unwrap(),
            AgentEvent::TurnCompleted {
                status: TurnStatus::Interrupted,
                ..
            }
        ));
    }

    #[test]
    fn a_blocked_rate_limit_becomes_a_usage_window() {
        let mut t = translator();
        let events = t.on_event(
            &ClaudeEvent::RateLimit(RateLimitInfo {
                status: "rejected".into(),
                resets_at: Some(1_790_298_600),
                rate_limit_type: Some("five_hour".into()),
                utilization: Some(1.0),
                ..Default::default()
            }),
            0,
        );
        let AgentEvent::RateLimitsUpdated(limits) = &events[0] else {
            panic!("unexpected {events:?}");
        };
        assert!(limits.reached);
        assert_eq!(limits.primary.unwrap().used_percent, 100.0);
        assert_eq!(limits.primary.unwrap().window_mins, Some(300));
        assert!(t.usage_limit_seen);
    }

    #[test]
    fn a_compaction_boundary_is_an_other_item() {
        let mut t = translator();
        t.begin_turn("turn-1", None);
        let events = t.on_event(
            &ClaudeEvent::CompactBoundary(CompactBoundary {
                trigger: "auto".into(),
                pre_tokens: 190_000,
                ..Default::default()
            }),
            0,
        );
        assert!(matches!(
            &events[0],
            AgentEvent::ItemCompleted {
                item: Item::Other { label, .. },
                ..
            } if label == "Context compacted"
        ));
    }

    #[test]
    fn init_carries_slash_commands() {
        let mut t = translator();
        let events = t.on_event(
            &ClaudeEvent::SessionInit(SessionInit {
                session_id: "s1".into(),
                model: "claude-sonnet-4-6".into(),
                cwd: "/work".into(),
                slash_commands: vec!["review".into(), "compact".into()],
                ..Default::default()
            }),
            0,
        );
        assert!(matches!(
            events.last().unwrap(),
            AgentEvent::CommandsUpdated { commands, .. }
                if commands.len() == 2 && commands[0].name == "review"
        ));
        assert_eq!(t.cwd(), "/work");
    }

    #[test]
    fn cancelled_results_keep_t3s_wire_name() {
        assert_eq!(
            terminal_status_wire(ClaudeTurnStatus::Cancelled),
            "cancelled"
        );
        assert_eq!(
            core_turn_status(ClaudeTurnStatus::Cancelled),
            TurnStatus::Interrupted
        );
    }

    #[test]
    fn tool_classification_follows_t3s_table() {
        for (name, class) in [
            ("Bash", ToolClass::Command),
            ("Read", ToolClass::FileRead),
            ("Edit", ToolClass::FileChange),
            ("Write", ToolClass::FileChange),
            ("WebFetch", ToolClass::WebSearch),
            ("TodoWrite", ToolClass::Plan),
            ("Task", ToolClass::SubAgent),
            ("mcp__atrium__device_list", ToolClass::Other),
        ] {
            assert_eq!(classify_tool(name), class, "{name}");
        }
        assert_eq!(request_kind_for_tool("Read"), "file-read");
        assert_eq!(
            request_kind_for_tool("mcp__atrium__device_list"),
            "permission"
        );
    }
}
