// Ported from apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts (T3 Code v0.1.11, MIT).
// Result classification ports the helpers in that file around
// `terminalStatusFromResult`, `providerFailureFromResult` and
// `claudeUsageLimitResetAt`.

//! Typed translation of the `claude` CLI's stream-JSON frames.
//!
//! The protocol grows fields every release, so every parser reads leniently:
//! unknown fields are ignored, missing ones default, and unknown frame or
//! block types are kept in an `Other`/`Unknown` variant instead of failing the
//! whole line. The CLI emits one JSON object per line on stdout.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::usage::{iso_millis_from_epoch_seconds, iso_millis_from_string};

/// How tool execution is gated for a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    #[default]
    Default,
    AcceptEdits,
    BypassPermissions,
    Plan,
    DontAsk,
    Auto,
}

impl PermissionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::BypassPermissions => "bypassPermissions",
            Self::Plan => "plan",
            Self::DontAsk => "dontAsk",
            Self::Auto => "auto",
        }
    }
}

/// A launch-time effort level. `UltraThink` is not a `--effort` value: T3
/// implements it by prefixing the prompt, which [`crate::args`] and the client
/// do as well.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
    #[serde(rename = "ultrathink")]
    UltraThink,
}

impl Effort {
    /// The value accepted by `--effort`, or `None` for prompt-injected effort.
    pub fn launch_arg(self) -> Option<&'static str> {
        match self {
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::XHigh => Some("xhigh"),
            Self::Max => Some("max"),
            Self::UltraThink => None,
        }
    }
}

/// T3's per-message effort handling
/// (`applyClaudePromptEffortPrefix` in `packages/shared/src/model.ts`):
/// prefix ordinary prompts with `Ultrathink:`, never slash commands.
pub fn apply_claude_prompt_effort_prefix(text: &str, effort: Option<Effort>) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return trimmed.to_owned();
    }
    if effort != Some(Effort::UltraThink) {
        return trimmed.to_owned();
    }
    // T3's `/^\/[^\s/]+(?:\s|$)/u`: a leading `/name` followed by whitespace
    // or the end is a command. A path such as `/home/theo/file.ts` is not.
    if let Some(rest) = trimmed.strip_prefix('/') {
        let name: String = rest
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != '/')
            .collect();
        let after = &rest[name.len()..];
        if !name.is_empty()
            && (after.is_empty() || after.chars().next().is_some_and(char::is_whitespace))
        {
            return trimmed.to_owned();
        }
    }
    if trimmed.starts_with("Ultrathink:") {
        return trimmed.to_owned();
    }
    format!("Ultrathink:\n{trimmed}")
}

/// One translated CLI frame.
#[derive(Debug, Clone)]
pub enum ClaudeEvent {
    /// `system/init`: session metadata and the advertised command/skill lists.
    SessionInit(SessionInit),
    HookStarted(HookStarted),
    HookResponse(HookResponse),
    /// A completed assistant content block (the CLI emits one per block).
    Assistant(AssistantMessage),
    /// A user-role frame: tool results, subagent prompts, and prompt echoes.
    User(UserEvent),
    /// A partial streaming event (`--include-partial-messages`).
    Stream(StreamEvent),
    /// The turn's terminal `result` frame.
    Result(TurnResult),
    /// Subscription rate-limit state.
    RateLimit(RateLimitInfo),
    /// An API retry notice.
    ApiRetry(ApiRetry),
    TaskStarted(TaskStarted),
    TaskProgress(TaskProgress),
    TaskNotification(TaskNotification),
    /// Replace the host's live background-task set.
    BackgroundTasksChanged(Vec<BackgroundTask>),
    ToolProgress(ToolProgress),
    ThinkingTokens {
        estimated_tokens: u64,
        estimated_tokens_delta: i64,
    },
    Status(StatusEvent),
    CompactBoundary(CompactBoundary),
    PermissionDenied(PermissionDenied),
    /// A frame this crate does not model yet.
    Unknown {
        kind: String,
        subtype: Option<String>,
        raw: Value,
    },
}

/// `system/init`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionInit {
    pub session_id: String,
    pub uuid: Option<String>,
    pub model: String,
    pub cwd: String,
    pub permission_mode: PermissionMode,
    pub output_style: String,
    pub claude_code_version: String,
    pub api_key_source: Option<String>,
    pub tools: Vec<String>,
    pub slash_commands: Vec<String>,
    pub skills: Vec<String>,
    pub agents: Vec<String>,
    pub mcp_servers: Vec<McpServerStatus>,
    pub plugins: Vec<PluginInfo>,
    pub fast_mode_state: Option<String>,
}

/// One MCP server in `system/init`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct McpServerStatus {
    pub name: String,
    pub status: String,
    pub source: Option<String>,
}

/// One plugin in `system/init`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PluginInfo {
    pub name: String,
    pub path: String,
    pub version: Option<String>,
}

/// A hook starting.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HookStarted {
    pub session_id: String,
    pub uuid: Option<String>,
    pub hook_id: String,
    pub hook_name: String,
    pub hook_event: String,
}

/// A hook finishing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HookResponse {
    pub session_id: String,
    pub uuid: Option<String>,
    pub hook_id: String,
    pub hook_name: String,
    pub hook_event: String,
    pub output: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i64>,
    pub outcome: String,
}

/// A completed assistant message. While streaming, each frame carries one
/// content block and `stop_reason` stays `None` until the final frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AssistantMessage {
    pub session_id: String,
    pub uuid: Option<String>,
    pub parent_tool_use_id: Option<String>,
    pub message_id: Option<String>,
    pub model: Option<String>,
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<String>,
    pub usage: Option<Usage>,
    pub error: Option<String>,
    pub user_message_uuid: Option<String>,
}

/// A content block in an assistant or user message.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        signature: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
        /// Sometimes `{"type": "direct"}`; kept opaquely.
        caller: Option<Value>,
    },
    ToolResult {
        tool_use_id: String,
        content: Value,
        is_error: bool,
    },
    Image {
        source: Value,
    },
    Other {
        kind: String,
        raw: Value,
    },
}

/// A user-role frame from the CLI.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UserEvent {
    pub session_id: String,
    pub uuid: Option<String>,
    pub parent_tool_use_id: Option<String>,
    pub message_id: Option<String>,
    pub content: Vec<ContentBlock>,
    pub is_synthetic: bool,
    /// Structured tool output (per-tool shape) for a tool result.
    pub tool_use_result: Option<Value>,
}

/// A partial streaming event.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    MessageStart {
        model: Option<String>,
        message_id: Option<String>,
        usage: Option<Usage>,
    },
    ContentBlockStart {
        index: u64,
        block: ContentBlock,
    },
    ContentBlockDelta {
        index: u64,
        delta: Delta,
    },
    ContentBlockStop {
        index: u64,
    },
    MessageDelta {
        stop_reason: Option<String>,
        stop_sequence: Option<String>,
        usage: Option<Usage>,
    },
    MessageStop,
    Ping,
    Other {
        kind: String,
        raw: Value,
    },
}

/// A partial content delta.
#[derive(Debug, Clone, PartialEq)]
pub enum Delta {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        estimated_tokens: Option<u64>,
    },
    Signature {
        signature: String,
    },
    InputJson {
        partial_json: String,
    },
    Other {
        kind: String,
        raw: Value,
    },
}

/// Token counts from an assistant message or result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub service_tier: Option<String>,
    /// Already counted inside `output_tokens`.
    #[serde(default)]
    pub thinking_tokens: Option<u64>,
}

/// Per-model usage from the result frame.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub web_search_requests: u64,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub context_window: u64,
    #[serde(default)]
    pub max_output_tokens: u64,
    #[serde(default)]
    pub thinking_tokens: Option<u64>,
}

/// A permission denial the CLI recorded without a prompt.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PermissionDenial {
    pub tool_name: String,
    pub tool_use_id: String,
    pub tool_input: Value,
}

/// The turn's terminal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TurnStatus {
    Completed,
    Interrupted,
    Failed,
    Cancelled,
}

/// The `result` frame. `subtype` is `success` or an error subtype such as
/// `error_max_turns`; API failures can still arrive as `success` with
/// `is_error` set, which is why the classifiers below never trust `subtype`
/// alone.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnResult {
    pub subtype: String,
    pub is_error: bool,
    pub session_id: String,
    pub uuid: Option<String>,
    pub result: String,
    pub stop_reason: Option<String>,
    pub terminal_reason: Option<String>,
    pub api_error_status: Option<i64>,
    pub num_turns: u32,
    pub duration_ms: Option<u64>,
    pub duration_api_ms: Option<u64>,
    pub total_cost_usd: Option<f64>,
    pub usage: Option<Usage>,
    pub model_usage: BTreeMap<String, ModelUsage>,
    pub permission_denials: Vec<PermissionDenial>,
    pub errors: Vec<String>,
    pub user_message_uuid: Option<String>,
}

/// How bad a failed turn is for retry purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    UsageLimit,
    ProviderError,
    TransportError,
}

/// A classified turn failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeFailure {
    pub class: FailureClass,
    pub message: String,
    pub code: String,
    pub retryable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<String>,
}

impl TurnResult {
    /// The CLI's user-facing error list, minus internal telemetry lines.
    pub fn user_facing_error(&self) -> Option<&str> {
        if self.subtype == "success" && !self.is_error {
            return None;
        }
        self.errors
            .iter()
            .find(|error| !error.starts_with("[ede_diagnostic]"))
            .map(String::as_str)
    }

    /// `success` with status 529, which T3 treats as a provider failure.
    pub fn is_overloaded(&self) -> bool {
        self.subtype == "success" && self.api_error_status == Some(529)
    }

    /// T3's `terminalStatusFromResult`.
    pub fn terminal_status(&self) -> TurnStatus {
        if matches!(
            self.terminal_reason.as_deref(),
            Some("aborted_tools" | "aborted_streaming")
        ) {
            return TurnStatus::Interrupted;
        }
        if self.subtype == "success" {
            if self.is_overloaded()
                || self.api_error_status == Some(429)
                || terminal_result_error(self.terminal_reason.as_deref(), None).is_some()
                || (self.is_error && self.failure_hint().is_some())
            {
                TurnStatus::Failed
            } else {
                TurnStatus::Completed
            }
        } else {
            let text = self.errors.join("\n").to_lowercase();
            if text.contains("interrupt") {
                TurnStatus::Interrupted
            } else if text.contains("cancel") {
                TurnStatus::Cancelled
            } else {
                TurnStatus::Failed
            }
        }
    }

    /// The text T3 passes to the classifier as `failureHint` for this result.
    fn failure_hint(&self) -> Option<&str> {
        if self.subtype == "success"
            && self.terminal_reason.as_deref() == Some("api_error")
            && self.result.trim().starts_with("API Error:")
        {
            Some(self.result.trim())
        } else {
            None
        }
    }

    /// Classifies a failed turn. `usage_limited` is true when a preceding
    /// `rate_limit_event` or assistant error already proved a usage limit.
    pub fn failure(&self) -> Option<ClaudeFailure> {
        self.failure_with_usage_limit(false)
    }

    /// Classifies a failed turn, with prior rate-limit evidence supplied by
    /// the caller (T3 tracks this while a turn streams).
    pub fn failure_with_usage_limit(&self, usage_limited: bool) -> Option<ClaudeFailure> {
        let now_secs = jiff::Timestamp::now().as_second();
        self.failure_at(usage_limited, now_secs)
    }

    /// Same classification with an injected clock, for tests.
    pub fn failure_at(&self, usage_limited: bool, now_secs: i64) -> Option<ClaudeFailure> {
        let hint = self.failure_hint();
        let gateway_reset = if self.subtype == "success"
            && self.is_error
            && self.terminal_reason.as_deref() == Some("api_error")
            && self.api_error_status == Some(503)
        {
            gateway_cooldown_reset_at(&self.result, now_secs)
        } else {
            None
        };
        if let Some(reset_at) = gateway_reset {
            return Some(ClaudeFailure {
                class: FailureClass::UsageLimit,
                message: "Claude API rate limit reached. Try again later.".into(),
                code: "api_error_429".into(),
                retryable: Some(true),
                reset_at: Some(reset_at),
            });
        }

        let failure_class =
            if (self.subtype == "success" && self.api_error_status == Some(429)) || usage_limited {
                FailureClass::UsageLimit
            } else {
                FailureClass::ProviderError
            };
        let listed_error = self.user_facing_error().map(str::to_owned);
        let api_error_message = hint.map(|text| text.chars().take(500).collect::<String>());
        let structured_error = if self.is_overloaded() {
            Some("Claude API is overloaded (529). Try again shortly.".to_owned())
        } else if self.subtype == "success" && self.api_error_status == Some(429) {
            Some("Claude API rate limit reached. Try again later.".to_owned())
        } else {
            terminal_result_error(
                self.terminal_reason.as_deref(),
                api_error_message.as_deref(),
            )
        };

        if self.subtype != "success" {
            return Some(ClaudeFailure {
                class: failure_class,
                message: listed_error
                    .or(structured_error)
                    .unwrap_or_else(|| self.errors.join("\n")),
                code: self.subtype.clone(),
                retryable: None,
                reset_at: None,
            });
        }
        if !self.is_error && structured_error.is_none() {
            return None;
        }

        let api_status = self.api_error_status;
        // API-key sessions get no rate_limit_info, so a gateway's 429 carries
        // its reset only as text. The CLIProxyAPI branch ignores a nested
        // upstream cooldown and uses the wrapper's own.
        let reset_at = if api_status == Some(429) && self.result.starts_with("API Error:") {
            let text = if self
                .result
                .starts_with("API Error: Request rejected (429) · All credentials for model ")
            {
                // The wrapper's own cooldown only; a nested upstream one is
                // not the reset this session will observe.
                proxy_outer_reset(&self.result)
            } else {
                reset_after_text(&self.result)
            };
            text.and_then(|seconds| {
                let reset = now_secs.saturating_add(seconds as i64);
                (seconds > 0 && reset.saturating_mul(1000) < 8_640_000_000_000_000)
                    .then(|| iso_millis_from_epoch_seconds(reset))
            })
        } else {
            None
        };
        Some(ClaudeFailure {
            class: failure_class,
            message: listed_error
                .or(structured_error)
                .or(api_error_message)
                .unwrap_or_else(|| self.result.clone()),
            code: match api_status {
                Some(status) => format!("api_error_{status}"),
                None => self
                    .terminal_reason
                    .clone()
                    .unwrap_or_else(|| "sdk_result_error".into()),
            },
            retryable: match api_status {
                Some(429 | 529) => Some(true),
                _ => None,
            },
            reset_at,
        })
    }
}

/// T3's `terminalResultError`. Known terminal reasons make even a `success`
/// envelope a failure.
fn terminal_result_error(reason: Option<&str>, failure_hint: Option<&str>) -> Option<String> {
    const CONTEXT_WINDOW_FULL: &str = "Claude stopped: the prompt exceeds the model's context window. Run /compact or start a new thread.";
    let message = match reason {
        Some("api_error") => failure_hint
            .unwrap_or("Claude gave up after repeated API errors.")
            .to_owned(),
        Some("malformed_tool_use_exhausted") => {
            "Claude gave up after repeated malformed tool calls.".to_owned()
        }
        Some("budget_exhausted") => {
            "Claude stopped: the turn's token budget was exhausted.".to_owned()
        }
        Some("structured_output_retry_exhausted") => {
            "Claude could not produce the requested structured output.".to_owned()
        }
        Some("tool_deferred_unavailable") => {
            "Claude could not resume a deferred tool call: the tool is no longer available."
                .to_owned()
        }
        Some("turn_setup_failed") => "Claude could not start the turn.".to_owned(),
        // blocking_limit also means context exhaustion; only rate-limit
        // evidence sets the hint.
        Some("blocking_limit") => failure_hint.unwrap_or(CONTEXT_WINDOW_FULL).to_owned(),
        Some("rapid_refill_breaker") => {
            "Claude stopped: the context refilled too quickly after compaction.".to_owned()
        }
        Some("prompt_too_long") => CONTEXT_WINDOW_FULL.to_owned(),
        Some("image_error") => {
            "Claude stopped: an image in the conversation could not be processed.".to_owned()
        }
        Some("model_error") => "Claude stopped: the model returned an error.".to_owned(),
        _ => return None,
    };
    Some(message)
}

/// Subscription rate-limit state from `rate_limit_event`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RateLimitInfo {
    pub status: String,
    pub resets_at: Option<i64>,
    pub rate_limit_type: Option<String>,
    pub utilization: Option<f64>,
    pub overage_status: Option<String>,
    pub overage_resets_at: Option<i64>,
    pub overage_disabled_reason: Option<String>,
    pub is_using_overage: Option<bool>,
    pub overage_in_use: Option<bool>,
}

impl RateLimitInfo {
    /// True when the plan is blocked and overage does not cover the request.
    pub fn is_blocked(&self) -> bool {
        let overage_allowed = matches!(
            self.overage_status.as_deref(),
            Some("allowed" | "allowed_warning")
        ) || self.is_using_overage == Some(true)
            || self.overage_in_use == Some(true);
        self.status == "rejected" && !overage_allowed
    }

    /// The reset instant as ISO 8601, when the CLI sent one.
    pub fn reset_at_iso(&self) -> Option<String> {
        self.resets_at
            .filter(|value| *value > 0 && *value < 8_640_000_000_000)
            .map(iso_millis_from_epoch_seconds)
    }
}

/// An API retry notice.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ApiRetry {
    pub session_id: String,
    pub uuid: Option<String>,
    pub attempt: u32,
    pub max_retries: u32,
    pub retry_delay_ms: u64,
    pub error_status: Option<i64>,
    pub error: String,
}

/// A background task starting.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskStarted {
    pub session_id: String,
    pub uuid: Option<String>,
    pub task_id: String,
    pub run_id: Option<String>,
    pub tool_use_id: Option<String>,
    pub description: String,
    pub subagent_type: Option<String>,
    pub is_backgrounded: Option<bool>,
    pub spawn_depth: Option<u32>,
    pub parent_task_id: Option<String>,
    pub task_type: Option<String>,
    pub workflow_name: Option<String>,
    pub prompt: Option<String>,
    pub skip_transcript: bool,
}

/// A background task's progress.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskProgress {
    pub session_id: String,
    pub uuid: Option<String>,
    pub task_id: String,
    pub run_id: Option<String>,
    pub tool_use_id: Option<String>,
    pub description: String,
    pub subagent_type: Option<String>,
    pub usage: TaskUsage,
    pub last_tool_name: Option<String>,
    pub summary: Option<String>,
}

/// A background task ending.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskNotification {
    pub session_id: String,
    pub uuid: Option<String>,
    pub task_id: String,
    pub run_id: Option<String>,
    pub tool_use_id: Option<String>,
    /// `completed`, `failed` or `stopped`.
    pub status: String,
    pub reason: Option<String>,
    pub output_file: String,
    pub summary: String,
    pub usage: Option<TaskUsage>,
    pub skip_transcript: bool,
    pub ambient: bool,
}

/// A live background task in `background_tasks_changed`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BackgroundTask {
    pub task_id: String,
    pub run_id: Option<String>,
    pub task_type: String,
    pub subagent_type: Option<String>,
    pub description: String,
    pub parent_task_id: Option<String>,
    pub ambient: bool,
}

/// Token/tool/duration counts for a task.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TaskUsage {
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub tool_uses: u64,
    #[serde(default)]
    pub duration_ms: u64,
}

/// Progress for a running tool.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolProgress {
    pub session_id: String,
    pub uuid: Option<String>,
    pub tool_use_id: String,
    pub tool_name: String,
    pub parent_tool_use_id: Option<String>,
    pub elapsed_time_seconds: f64,
    pub task_id: Option<String>,
    pub heartbeat: bool,
    pub subagent_type: Option<String>,
}

/// A `system/status` frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatusEvent {
    pub session_id: String,
    pub uuid: Option<String>,
    pub status: Option<String>,
    pub permission_mode: Option<PermissionMode>,
    pub compact_result: Option<String>,
    pub compact_error: Option<String>,
}

/// A compaction boundary.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompactBoundary {
    pub session_id: String,
    pub uuid: Option<String>,
    pub trigger: String,
    pub pre_tokens: u64,
    pub post_tokens: Option<u64>,
    pub duration_ms: Option<u64>,
}

/// An automatic denial the CLI recorded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PermissionDenied {
    pub session_id: String,
    pub uuid: Option<String>,
    pub tool_name: String,
    pub tool_use_id: String,
    pub agent_id: Option<String>,
    pub decision_reason_type: Option<String>,
    pub decision_reason: Option<String>,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn opt_text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn opt_bool(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

fn u64_field(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or_default()
}

fn opt_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

fn opt_f64(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Parses one CLI frame. Returns `None` for a documented frame kind this crate
/// ignores entirely; unknown kinds come back as [`ClaudeEvent::Unknown`].
pub fn event(value: &Value) -> Option<ClaudeEvent> {
    let kind = value.get("type").and_then(Value::as_str)?;
    Some(match kind {
        "assistant" => ClaudeEvent::Assistant(assistant(value)),
        "user" => ClaudeEvent::User(user(value)),
        "stream_event" => ClaudeEvent::Stream(stream_event(value)),
        "result" => ClaudeEvent::Result(result(value)),
        "rate_limit_event" => ClaudeEvent::RateLimit(rate_limit(value)),
        "tool_progress" => ClaudeEvent::ToolProgress(tool_progress(value)),
        "system" => system(value)?,
        other => ClaudeEvent::Unknown {
            kind: other.to_owned(),
            subtype: opt_text(value, "subtype"),
            raw: value.clone(),
        },
    })
}

fn system(value: &Value) -> Option<ClaudeEvent> {
    let subtype = value.get("subtype").and_then(Value::as_str)?;
    Some(match subtype {
        "init" => ClaudeEvent::SessionInit(session_init(value)),
        "hook_started" => ClaudeEvent::HookStarted(HookStarted {
            session_id: text(value, "session_id"),
            uuid: opt_text(value, "uuid"),
            hook_id: text(value, "hook_id"),
            hook_name: text(value, "hook_name"),
            hook_event: text(value, "hook_event"),
        }),
        "hook_response" => ClaudeEvent::HookResponse(HookResponse {
            session_id: text(value, "session_id"),
            uuid: opt_text(value, "uuid"),
            hook_id: text(value, "hook_id"),
            hook_name: text(value, "hook_name"),
            hook_event: text(value, "hook_event"),
            output: text(value, "output"),
            stdout: text(value, "stdout"),
            stderr: text(value, "stderr"),
            exit_code: opt_i64(value, "exit_code"),
            outcome: text(value, "outcome"),
        }),
        "task_started" => ClaudeEvent::TaskStarted(TaskStarted {
            session_id: text(value, "session_id"),
            uuid: opt_text(value, "uuid"),
            task_id: text(value, "task_id"),
            run_id: opt_text(value, "run_id"),
            tool_use_id: opt_text(value, "tool_use_id"),
            description: text(value, "description"),
            subagent_type: opt_text(value, "subagent_type"),
            is_backgrounded: opt_bool(value, "is_backgrounded"),
            spawn_depth: value
                .get("spawn_depth")
                .and_then(Value::as_u64)
                .map(|depth| depth as u32),
            parent_task_id: opt_text(value, "parent_task_id"),
            task_type: opt_text(value, "task_type"),
            workflow_name: opt_text(value, "workflow_name"),
            prompt: opt_text(value, "prompt"),
            skip_transcript: opt_bool(value, "skip_transcript").unwrap_or(false),
        }),
        "task_progress" => ClaudeEvent::TaskProgress(TaskProgress {
            session_id: text(value, "session_id"),
            uuid: opt_text(value, "uuid"),
            task_id: text(value, "task_id"),
            run_id: opt_text(value, "run_id"),
            tool_use_id: opt_text(value, "tool_use_id"),
            description: text(value, "description"),
            subagent_type: opt_text(value, "subagent_type"),
            usage: serde_json::from_value(value.get("usage").cloned().unwrap_or_default())
                .unwrap_or_default(),
            last_tool_name: opt_text(value, "last_tool_name"),
            summary: opt_text(value, "summary"),
        }),
        "task_notification" => ClaudeEvent::TaskNotification(TaskNotification {
            session_id: text(value, "session_id"),
            uuid: opt_text(value, "uuid"),
            task_id: text(value, "task_id"),
            run_id: opt_text(value, "run_id"),
            tool_use_id: opt_text(value, "tool_use_id"),
            status: text(value, "status"),
            reason: opt_text(value, "reason"),
            output_file: text(value, "output_file"),
            summary: text(value, "summary"),
            usage: value
                .get("usage")
                .and_then(|usage| serde_json::from_value(usage.clone()).ok()),
            skip_transcript: opt_bool(value, "skip_transcript").unwrap_or(false),
            ambient: opt_bool(value, "ambient").unwrap_or(false),
        }),
        "background_tasks_changed" => ClaudeEvent::BackgroundTasksChanged(
            value
                .get("tasks")
                .and_then(Value::as_array)
                .map(|tasks| {
                    tasks
                        .iter()
                        .map(|task| BackgroundTask {
                            task_id: text(task, "task_id"),
                            run_id: opt_text(task, "run_id"),
                            task_type: text(task, "task_type"),
                            subagent_type: opt_text(task, "subagent_type"),
                            description: text(task, "description"),
                            parent_task_id: opt_text(task, "parent_task_id"),
                            ambient: opt_bool(task, "ambient").unwrap_or(false),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ),
        "thinking_tokens" => ClaudeEvent::ThinkingTokens {
            estimated_tokens: u64_field(value, "estimated_tokens"),
            estimated_tokens_delta: opt_i64(value, "estimated_tokens_delta").unwrap_or_default(),
        },
        "status" => ClaudeEvent::Status(StatusEvent {
            session_id: text(value, "session_id"),
            uuid: opt_text(value, "uuid"),
            status: opt_text(value, "status"),
            permission_mode: serde_json::from_value(
                value.get("permissionMode").cloned().unwrap_or_default(),
            )
            .ok(),
            compact_result: opt_text(value, "compact_result"),
            compact_error: opt_text(value, "compact_error"),
        }),
        "compact_boundary" => {
            let metadata = value.get("compact_metadata").cloned().unwrap_or_default();
            ClaudeEvent::CompactBoundary(CompactBoundary {
                session_id: text(value, "session_id"),
                uuid: opt_text(value, "uuid"),
                trigger: text(&metadata, "trigger"),
                pre_tokens: u64_field(&metadata, "pre_tokens"),
                post_tokens: metadata.get("post_tokens").and_then(Value::as_u64),
                duration_ms: metadata.get("duration_ms").and_then(Value::as_u64),
            })
        }
        "permission_denied" => ClaudeEvent::PermissionDenied(PermissionDenied {
            session_id: text(value, "session_id"),
            uuid: opt_text(value, "uuid"),
            tool_name: text(value, "tool_name"),
            tool_use_id: text(value, "tool_use_id"),
            agent_id: opt_text(value, "agent_id"),
            decision_reason_type: opt_text(value, "decision_reason_type"),
            decision_reason: opt_text(value, "decision_reason"),
            message: text(value, "message"),
        }),
        "api_retry" => ClaudeEvent::ApiRetry(ApiRetry {
            session_id: text(value, "session_id"),
            uuid: opt_text(value, "uuid"),
            attempt: u64_field(value, "attempt") as u32,
            max_retries: u64_field(value, "max_retries") as u32,
            retry_delay_ms: u64_field(value, "retry_delay_ms"),
            error_status: opt_i64(value, "error_status"),
            error: text(value, "error"),
        }),
        // Hook progress, informational banners, local command output,
        // session-state changes and other additive system frames.
        _ => ClaudeEvent::Unknown {
            kind: "system".into(),
            subtype: Some(subtype.to_owned()),
            raw: value.clone(),
        },
    })
}

fn session_init(value: &Value) -> SessionInit {
    SessionInit {
        session_id: text(value, "session_id"),
        uuid: opt_text(value, "uuid"),
        model: text(value, "model"),
        cwd: text(value, "cwd"),
        permission_mode: serde_json::from_value(
            value.get("permissionMode").cloned().unwrap_or_default(),
        )
        .unwrap_or_default(),
        output_style: text(value, "output_style"),
        claude_code_version: text(value, "claude_code_version"),
        api_key_source: opt_text(value, "apiKeySource"),
        tools: strings(value.get("tools")),
        slash_commands: strings(value.get("slash_commands")),
        skills: strings(value.get("skills")),
        agents: strings(value.get("agents")),
        mcp_servers: value
            .get("mcp_servers")
            .and_then(Value::as_array)
            .map(|servers| {
                servers
                    .iter()
                    .map(|server| McpServerStatus {
                        name: text(server, "name"),
                        status: text(server, "status"),
                        source: opt_text(server, "source"),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        plugins: value
            .get("plugins")
            .and_then(Value::as_array)
            .map(|plugins| {
                plugins
                    .iter()
                    .map(|plugin| PluginInfo {
                        name: text(plugin, "name"),
                        path: text(plugin, "path"),
                        version: opt_text(plugin, "version"),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        fast_mode_state: opt_text(value, "fast_mode_state"),
    }
}

fn usage(value: Option<&Value>) -> Option<Usage> {
    let value = value?;
    if !value.is_object() {
        return None;
    }
    Some(Usage {
        input_tokens: u64_field(value, "input_tokens"),
        output_tokens: u64_field(value, "output_tokens"),
        cache_creation_input_tokens: u64_field(value, "cache_creation_input_tokens"),
        cache_read_input_tokens: u64_field(value, "cache_read_input_tokens"),
        service_tier: opt_text(value, "service_tier"),
        thinking_tokens: value
            .get("output_tokens_details")
            .and_then(|details| details.get("thinking_tokens"))
            .and_then(Value::as_u64),
    })
}

fn assistant(value: &Value) -> AssistantMessage {
    let message = value.get("message").cloned().unwrap_or_default();
    AssistantMessage {
        session_id: text(value, "session_id"),
        uuid: opt_text(value, "uuid"),
        parent_tool_use_id: opt_text(value, "parent_tool_use_id"),
        message_id: opt_text(&message, "id"),
        model: opt_text(&message, "model"),
        content: content_blocks(message.get("content")),
        stop_reason: opt_text(&message, "stop_reason"),
        usage: usage(message.get("usage")),
        error: opt_text(value, "error"),
        user_message_uuid: opt_text(value, "user_message_uuid"),
    }
}

fn user(value: &Value) -> UserEvent {
    let message = value.get("message").cloned().unwrap_or_default();
    UserEvent {
        session_id: text(value, "session_id"),
        uuid: opt_text(value, "uuid"),
        parent_tool_use_id: opt_text(value, "parent_tool_use_id"),
        message_id: opt_text(&message, "id"),
        content: content_blocks(message.get("content")),
        is_synthetic: opt_bool(value, "isSynthetic").unwrap_or(false),
        tool_use_result: value.get("tool_use_result").cloned(),
    }
}

fn content_blocks(value: Option<&Value>) -> Vec<ContentBlock> {
    match value {
        Some(Value::String(text)) => vec![ContentBlock::Text { text: text.clone() }],
        Some(Value::Array(blocks)) => blocks.iter().map(content_block).collect(),
        _ => Vec::new(),
    }
}

fn content_block(value: &Value) -> ContentBlock {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match kind {
        "text" => ContentBlock::Text {
            text: text(value, "text"),
        },
        "thinking" => ContentBlock::Thinking {
            thinking: text(value, "thinking"),
            signature: opt_text(value, "signature"),
        },
        "tool_use" => ContentBlock::ToolUse {
            id: text(value, "id"),
            name: text(value, "name"),
            input: value.get("input").cloned().unwrap_or(Value::Null),
            caller: value.get("caller").cloned(),
        },
        "tool_result" => ContentBlock::ToolResult {
            tool_use_id: text(value, "tool_use_id"),
            content: value.get("content").cloned().unwrap_or(Value::Null),
            is_error: opt_bool(value, "is_error").unwrap_or(false),
        },
        "image" => ContentBlock::Image {
            source: value.get("source").cloned().unwrap_or(Value::Null),
        },
        other => ContentBlock::Other {
            kind: other.to_owned(),
            raw: value.clone(),
        },
    }
}

fn stream_event(value: &Value) -> StreamEvent {
    let event = value.get("event").cloned().unwrap_or_default();
    let kind = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match kind {
        "message_start" => {
            let message = event.get("message").cloned().unwrap_or_default();
            StreamEvent::MessageStart {
                model: opt_text(&message, "model"),
                message_id: opt_text(&message, "id"),
                usage: usage(message.get("usage")),
            }
        }
        "content_block_start" => StreamEvent::ContentBlockStart {
            index: u64_field(&event, "index"),
            block: event
                .get("content_block")
                .map(content_block)
                .unwrap_or(ContentBlock::Other {
                    kind: String::new(),
                    raw: Value::Null,
                }),
        },
        "content_block_delta" => StreamEvent::ContentBlockDelta {
            index: u64_field(&event, "index"),
            delta: delta(event.get("delta").unwrap_or(&Value::Null)),
        },
        "content_block_stop" => StreamEvent::ContentBlockStop {
            index: u64_field(&event, "index"),
        },
        "message_delta" => {
            let delta = event.get("delta").cloned().unwrap_or_default();
            StreamEvent::MessageDelta {
                stop_reason: opt_text(&delta, "stop_reason"),
                stop_sequence: opt_text(&delta, "stop_sequence"),
                usage: usage(event.get("usage")),
            }
        }
        "message_stop" => StreamEvent::MessageStop,
        "ping" => StreamEvent::Ping,
        other => StreamEvent::Other {
            kind: other.to_owned(),
            raw: event,
        },
    }
}

fn delta(value: &Value) -> Delta {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match kind {
        "text_delta" => Delta::Text {
            text: text(value, "text"),
        },
        "thinking_delta" => Delta::Thinking {
            thinking: text(value, "thinking"),
            estimated_tokens: value.get("estimated_tokens").and_then(Value::as_u64),
        },
        "signature_delta" => Delta::Signature {
            signature: text(value, "signature"),
        },
        "input_json_delta" => Delta::InputJson {
            partial_json: text(value, "partial_json"),
        },
        other => Delta::Other {
            kind: other.to_owned(),
            raw: value.clone(),
        },
    }
}

fn result(value: &Value) -> TurnResult {
    TurnResult {
        subtype: text(value, "subtype"),
        is_error: opt_bool(value, "is_error").unwrap_or(false),
        session_id: text(value, "session_id"),
        uuid: opt_text(value, "uuid"),
        result: text(value, "result"),
        stop_reason: opt_text(value, "stop_reason"),
        terminal_reason: opt_text(value, "terminal_reason"),
        api_error_status: opt_i64(value, "api_error_status"),
        num_turns: u64_field(value, "num_turns") as u32,
        duration_ms: value.get("duration_ms").and_then(Value::as_u64),
        duration_api_ms: value.get("duration_api_ms").and_then(Value::as_u64),
        total_cost_usd: opt_f64(value, "total_cost_usd"),
        usage: usage(value.get("usage")),
        model_usage: value
            .get("modelUsage")
            .and_then(Value::as_object)
            .map(|models| {
                models
                    .iter()
                    .filter_map(|(name, usage)| {
                        serde_json::from_value(usage.clone())
                            .ok()
                            .map(|usage| (name.clone(), usage))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        permission_denials: value
            .get("permission_denials")
            .and_then(Value::as_array)
            .map(|denials| {
                denials
                    .iter()
                    .map(|denial| PermissionDenial {
                        tool_name: text(denial, "tool_name"),
                        tool_use_id: text(denial, "tool_use_id"),
                        tool_input: denial.get("tool_input").cloned().unwrap_or(Value::Null),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        errors: strings(value.get("errors")),
        user_message_uuid: opt_text(value, "user_message_uuid"),
    }
}

fn rate_limit(value: &Value) -> RateLimitInfo {
    let info = value.get("rate_limit_info").cloned().unwrap_or_default();
    RateLimitInfo {
        status: text(&info, "status"),
        resets_at: opt_i64(&info, "resetsAt"),
        rate_limit_type: opt_text(&info, "rateLimitType"),
        utilization: opt_f64(&info, "utilization"),
        overage_status: opt_text(&info, "overageStatus"),
        overage_resets_at: opt_i64(&info, "overageResetsAt"),
        overage_disabled_reason: opt_text(&info, "overageDisabledReason"),
        is_using_overage: opt_bool(&info, "isUsingOverage"),
        overage_in_use: opt_bool(&info, "overageInUse"),
    }
}

fn tool_progress(value: &Value) -> ToolProgress {
    ToolProgress {
        session_id: text(value, "session_id"),
        uuid: opt_text(value, "uuid"),
        tool_use_id: text(value, "tool_use_id"),
        tool_name: text(value, "tool_name"),
        parent_tool_use_id: opt_text(value, "parent_tool_use_id"),
        elapsed_time_seconds: opt_f64(value, "elapsed_time_seconds").unwrap_or_default(),
        task_id: opt_text(value, "task_id"),
        heartbeat: opt_bool(value, "heartbeat").unwrap_or(false),
        subagent_type: opt_text(value, "subagent_type"),
    }
}

// ---------------------------------------------------------------------------
// Reset-time parsing shared with the usage port
// ---------------------------------------------------------------------------

/// Parses `(reset after 1h 2m 3s)`-style durations into seconds.
pub(crate) fn reset_after_text(text: &str) -> Option<u64> {
    let start = text.find("reset after ")? + "reset after ".len();
    parse_duration(&text[start..])
}

/// For Claude Code's CLIProxyAPI wrapper: use the wrapper's own outer cooldown,
/// not a nested upstream one.
fn proxy_outer_reset(text: &str) -> Option<u64> {
    const PREFIX: &str = "API Error: Request rejected (429) · All credentials for model ";
    if !text.starts_with(PREFIX) {
        return None;
    }
    let rest = &text[PREFIX.len()..];
    // Skip the model id up to " are cooling down".
    let rest = rest.split_once(" are cooling down")?.1.trim_start();
    // The wrapper's own cooldown comes first, or after a nested `(last error:
    // ...)` group. A nested-only cooldown is not the reset to report.
    if let Some(after) = rest.strip_prefix("(reset after ") {
        return parse_duration(after);
    }
    let inner = rest.strip_prefix("(last error: ")?;
    // Find the closing `)` of the nested group, then look for an outer one.
    let mut depth = 1usize;
    let mut index = inner.len();
    for (offset, ch) in inner.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    index = offset + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    reset_after_text(&inner[index..])
}

fn parse_duration(mut text: &str) -> Option<u64> {
    let mut seconds = 0u64;
    let mut parsed_any = false;
    loop {
        text = text.trim_start();
        let digits = text.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            break;
        }
        let number: u64 = text[..digits].parse().ok()?;
        let unit = text[digits..].chars().next()?;
        let multiplier = match unit {
            'h' => 3_600,
            'm' => 60,
            's' => 1,
            _ => break,
        };
        seconds = seconds.checked_add(number.checked_mul(multiplier)?)?;
        parsed_any = true;
        text = &text[digits + unit.len_utf8()..];
    }
    parsed_any.then_some(seconds)
}

fn gateway_cooldown_reset_at(text: &str, now_secs: i64) -> Option<String> {
    // T3's regex only treats a 503 as a rate limit when the body is the
    // gateway's `rate_limit_error` envelope; an `overloaded_error` with a
    // cooldown is a plain provider error.
    if !text.starts_with("API Error: 503 [")
        || !text.contains("] [429]: {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\"")
    {
        return None;
    }
    let seconds = reset_after_text(text)?;
    let reset_ms = now_secs
        .saturating_mul(1000)
        .saturating_add((seconds as i64).saturating_mul(1000));
    (seconds > 0 && reset_ms < 8_640_000_000_000_000)
        .then(|| iso_millis_from_epoch_seconds(reset_ms / 1000))
}

/// ISO 8601 for a reset supplied as epoch seconds, as T3's
/// `claudeUsageLimitResetAt` does.
pub fn usage_limit_reset_at(resets_at: Option<i64>) -> Option<String> {
    resets_at
        .filter(|value| *value > 0 && *value < 8_640_000_000_000)
        .map(iso_millis_from_epoch_seconds)
}

/// Normalizes an ISO timestamp string from a usage response.
pub fn normalize_iso(value: &str) -> Option<String> {
    iso_millis_from_string(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// One recorded `result` frame from T3's `simple` fixture.
    const SIMPLE_RESULT: &str = r#"{"type":"result","subtype":"success","is_error":false,"api_error_status":null,"duration_ms":1291,"duration_api_ms":1039,"num_turns":1,"result":"fixture simple ok","stop_reason":"end_turn","session_id":"77171d01-fb4f-4dff-a961-d9dd334be93d","total_cost_usd":0.011703,"usage":{"input_tokens":3,"cache_creation_input_tokens":2338,"cache_read_input_tokens":9455,"output_tokens":6,"service_tier":"standard"},"modelUsage":{"claude-sonnet-4-6":{"inputTokens":3,"outputTokens":6,"cacheReadInputTokens":9455,"cacheCreationInputTokens":2338,"webSearchRequests":0,"costUSD":0.011703,"contextWindow":200000,"maxOutputTokens":32000}},"permission_denials":[],"terminal_reason":"completed","fast_mode_state":"off","uuid":"6b6fcbd9-d2dd-4b4f-9345-d6c7a7d7eb4e"}"#;

    #[test]
    fn the_simple_fixture_result_parses_and_completes() {
        let value: Value = serde_json::from_str(SIMPLE_RESULT).unwrap();
        let Some(ClaudeEvent::Result(result)) = event(&value) else {
            panic!("result frame");
        };
        assert_eq!(result.subtype, "success");
        assert_eq!(result.terminal_status(), TurnStatus::Completed);
        assert_eq!(result.usage.as_ref().unwrap().output_tokens, 6);
        assert_eq!(
            result.model_usage["claude-sonnet-4-6"].context_window,
            200_000
        );
        assert!(result.failure().is_none());
    }

    #[test]
    fn a_thinking_block_and_tool_use_parse_from_an_assistant_frame() {
        let value = json!({
            "type": "assistant",
            "message": {
                "id": "msg_1",
                "model": "claude-sonnet-4-6",
                "content": [
                    {"type": "thinking", "thinking": "Let me look.", "signature": "sig"},
                    {"type": "tool_use", "id": "toolu_1", "name": "Read",
                     "input": {"file_path": "/tmp/a"}, "caller": {"type": "direct"}}
                ],
                "usage": {"input_tokens": 3, "output_tokens": 2}
            },
            "parent_tool_use_id": null,
            "session_id": "s1",
            "uuid": "u1"
        });
        let Some(ClaudeEvent::Assistant(message)) = event(&value) else {
            panic!("assistant frame");
        };
        assert_eq!(message.message_id.as_deref(), Some("msg_1"));
        assert_eq!(message.parent_tool_use_id, None);
        assert!(
            matches!(&message.content[0], ContentBlock::Thinking { thinking, .. } if thinking == "Let me look.")
        );
        assert!(
            matches!(&message.content[1], ContentBlock::ToolUse { name, .. } if name == "Read")
        );
        assert_eq!(message.usage.unwrap().input_tokens, 3);
    }

    #[test]
    fn partial_stream_events_cover_text_thinking_and_tool_input() {
        let text_delta = json!({
            "type": "stream_event",
            "event": {"type": "content_block_delta", "index": 0,
                      "delta": {"type": "text_delta", "text": "hello"}},
            "session_id": "s1", "parent_tool_use_id": null, "uuid": "u1"
        });
        assert!(matches!(
            event(&text_delta),
            Some(ClaudeEvent::Stream(StreamEvent::ContentBlockDelta {
                delta: Delta::Text { text }, ..
            })) if text == "hello"
        ));
        let thinking_delta = json!({
            "type": "stream_event",
            "event": {"type": "content_block_delta", "index": 0,
                      "delta": {"type": "thinking_delta", "thinking": "hm",
                                "estimated_tokens": null}},
            "session_id": "s1", "parent_tool_use_id": null, "uuid": "u2"
        });
        assert!(matches!(
            event(&thinking_delta),
            Some(ClaudeEvent::Stream(StreamEvent::ContentBlockDelta {
                delta: Delta::Thinking { thinking, .. }, ..
            })) if thinking == "hm"
        ));
        let input_delta = json!({
            "type": "stream_event",
            "event": {"type": "content_block_delta", "index": 1,
                      "delta": {"type": "input_json_delta", "partial_json": "{\"a\":"}},
            "session_id": "s1", "parent_tool_use_id": null, "uuid": "u3"
        });
        assert!(matches!(
            event(&input_delta),
            Some(ClaudeEvent::Stream(StreamEvent::ContentBlockDelta {
                delta: Delta::InputJson { partial_json }, ..
            })) if partial_json == "{\"a\":"
        ));
    }

    #[test]
    fn init_carries_commands_skills_and_agents() {
        let value = json!({
            "type": "system",
            "subtype": "init",
            "agents": ["Explore"],
            "apiKeySource": "none",
            "claude_code_version": "2.1.111",
            "cwd": "/repo",
            "tools": ["Read"],
            "mcp_servers": [{"name": "atrium", "status": "connected"}],
            "model": "claude-sonnet-4-6",
            "permissionMode": "default",
            "slash_commands": ["review", "compact"],
            "output_style": "default",
            "skills": ["review", "deploy"],
            "plugins": [{"name": "p", "path": "/p"}],
            "fast_mode_state": "off",
            "uuid": "u1",
            "session_id": "s1"
        });
        let Some(ClaudeEvent::SessionInit(init)) = event(&value) else {
            panic!("init frame");
        };
        assert_eq!(init.slash_commands, ["review", "compact"]);
        assert_eq!(init.skills, ["review", "deploy"]);
        assert_eq!(init.agents, ["Explore"]);
        assert_eq!(init.permission_mode, PermissionMode::Default);
        assert_eq!(init.mcp_servers[0].name, "atrium");
    }

    #[test]
    fn subagent_and_background_task_frames_parse() {
        let started = json!({
            "type": "system", "subtype": "task_started", "task_id": "t1",
            "tool_use_id": "toolu_1", "description": "Read package.json",
            "task_type": "local_agent", "prompt": "Read it", "uuid": "u1",
            "session_id": "s1"
        });
        assert!(matches!(
            event(&started),
            Some(ClaudeEvent::TaskStarted(task))
                if task.task_id == "t1" && task.task_type.as_deref() == Some("local_agent")
        ));
        let notification = json!({
            "type": "system", "subtype": "task_notification", "task_id": "t1",
            "tool_use_id": "toolu_1", "status": "completed", "output_file": "/tmp/t1.output",
            "summary": "done", "usage": {"total_tokens": 10750, "tool_uses": 1, "duration_ms": 5183},
            "uuid": "u2", "session_id": "s1"
        });
        let Some(ClaudeEvent::TaskNotification(task)) = event(&notification) else {
            panic!("task notification");
        };
        assert_eq!(task.status, "completed");
        assert_eq!(task.usage.unwrap().total_tokens, 10_750);
        let changed = json!({
            "type": "system", "subtype": "background_tasks_changed",
            "tasks": [{"task_id": "t2", "task_type": "local_bash", "description": "sleep"}],
            "uuid": "u3", "session_id": "s1"
        });
        assert!(matches!(
            event(&changed),
            Some(ClaudeEvent::BackgroundTasksChanged(tasks))
                if tasks.len() == 1 && tasks[0].task_type == "local_bash"
        ));
    }

    #[test]
    fn usage_limit_events_and_reset_times_parse() {
        let value = json!({
            "type": "rate_limit_event",
            "rate_limit_info": {
                "status": "rejected", "rateLimitType": "five_hour",
                "resetsAt": 1_790_298_600, "utilization": 1.0,
                "overageStatus": "rejected", "overageDisabledReason": "org_level_disabled",
                "isUsingOverage": false
            },
            "uuid": "u1", "session_id": "s1"
        });
        let Some(ClaudeEvent::RateLimit(info)) = event(&value) else {
            panic!("rate limit frame");
        };
        assert!(info.is_blocked());
        assert_eq!(
            info.reset_at_iso().as_deref(),
            Some("2026-09-25T01:10:00.000Z")
        );
        assert_eq!(
            usage_limit_reset_at(Some(1_790_298_600)).as_deref(),
            Some("2026-09-25T01:10:00.000Z")
        );
        assert_eq!(usage_limit_reset_at(Some(0)), None);
    }

    #[test]
    fn effort_prefix_matches_t3() {
        for (text, expected) in [
            (
                "Investigate this failure",
                "Ultrathink:\nInvestigate this failure",
            ),
            (" /review src/model.ts ", "/review src/model.ts"),
            ("/deploy.prod to staging", "/deploy.prod to staging"),
            ("Ultrathink:\nalready", "Ultrathink:\nalready"),
            (
                "/home/theo/app.ts crashed on load",
                "Ultrathink:\n/home/theo/app.ts crashed on load",
            ),
        ] {
            assert_eq!(
                apply_claude_prompt_effort_prefix(text, Some(Effort::UltraThink)),
                expected,
                "{text:?}"
            );
        }
        assert_eq!(
            apply_claude_prompt_effort_prefix("Investigate this failure", Some(Effort::High)),
            "Investigate this failure"
        );
    }
}
