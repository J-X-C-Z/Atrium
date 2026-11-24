// Ported from packages/provider-acp/src/server/adapter.ts,
// packages/provider-acp/src/server/AcpSessionRuntime.ts,
// packages/provider-acp/src/server/sessionConfig.ts,
// packages/provider-acp/src/server/clientPolicy.ts and
// packages/provider-acp-registry/src/server/adapter.ts (T3 Code v0.1.11, MIT).
// The Effect services and streams are replaced by Atrium's synchronous
// `ProviderAdapter` seam over the existing `acp-client`; the capability
// record, option categories, usage rules and error text are kept.
//!
//! The ACP adapter. One agent process per provider session; ACP sessions are
//! provider threads; each prompt is a turn. ACP carries no turn or item ids,
//! so the client's translator makes them. Capabilities are declared once and
//! every control is degraded through them: model and mode switching only when
//! the session advertised an option, steering only when the agent accepted
//! prompts mid-turn, and child work only when the flavor can prove it.
//!
//! The flavor ([`AcpFlavor`]) is what a specific agent does beyond plain ACP.
//! Antigravity's flavor lives in [`super::acp_antigravity`]; it adds sign-in,
//! local skills, its own usage reader and child-work parsing.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use crate::contracts::ChatAttachment;
use acp_client::{AcpClient, AcpConfig, PromptAttachment, PromptDelivery};
use atrium_core::{
    AgentEvent, ApprovalDecision, ApprovalKind, Capabilities, ConfigOption, RequestId,
};
use futures::StreamExt;
use serde::Serialize;
use serde_json::{Value, json};

use super::acp_antigravity::{AntigravityAuthMethod, AntigravityUsageReader};
use super::adapter::{
    AdapterEvent, AdapterEventSink, AdapterTurnInput, EnsureThreadInput, OpenSessionInput,
    ProviderAdapter, ProviderAdapterError, ProviderConfig, ProviderSessionRuntime, RuntimePolicy,
    RuntimeRequestResponse, SelectionTransitionPlan, SkillGroup, SteerInput, ThreadSnapshot,
    TurnMessage,
};
use crate::contracts::{
    ApprovalCapabilities, CheckpointCapabilities, ContextCapabilities, IdentityCapabilities,
    ModelSelection, NativeRefStrength, NodeId, PlanningCapabilities, ProviderApprovalDecision,
    ProviderCapabilities, ProviderDriverKind, ProviderInstanceId, ProviderRef, ProviderRequestKind,
    ProviderSession, ProviderSessionId, ProviderSessionStatus, ProviderThread, ProviderThreadId,
    ProviderThreadStatus, ProviderTurn, ProviderTurnId, ProviderTurnStatus, ResponseCapability,
    RuntimeMode, RuntimePolicyCapabilities, RuntimePolicyEnforcement, RuntimeRequest,
    RuntimeRequestId, RuntimeRequestKind, RuntimeRequestStatus, SessionCapabilities,
    StreamingCapabilities, SubagentCapabilities, TerminalStatusQuality, ThreadCapabilities,
    ThreadId, ToolCapabilities, TurnCapabilities,
};

/// T3's registry driver kind. A plain ACP agent (Claude Code, DeepSeek) is one
/// configured instance of this driver.
pub const ACP_DRIVER_KIND: &str = "acp";
/// T3's native Antigravity driver kind; its flavor is not plain ACP.
pub const ANTIGRAVITY_DRIVER_KIND: &str = "antigravity";
/// Antigravity's "no explicit model" selection id.
pub const ANTIGRAVITY_DEFAULT_MODEL: &str = "antigravity-default";
/// T3's synthetic option id for `modes` when no `mode` config option exists.
pub const ACP_SESSION_MODE_OPTION_ID: &str = "__mode";

/// Quiet period after a prompt reply before the turn closes, so updates the
/// agent queued ahead of the reply are translated first.
const PROMPT_SETTLE: Duration = Duration::from_millis(35);

/// T3's `ACP_DEFERRED_FINALIZE_DEBOUNCE`: a settled prompt whose commands
/// still run waits this long for their updates before closing the turn.
const DEFERRED_FINALIZE_DEBOUNCE: Duration = Duration::from_millis(150);

/// The longest deferral; a command that never reports cannot hold a turn open.
const DEFERRED_FINALIZE_MAX: Duration = Duration::from_secs(5);

/// What an agent does beyond the protocol. The generic flavor is plain ACP.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AcpFlavor {
    #[default]
    Generic,
    Antigravity,
}

impl AcpFlavor {
    /// T3's `reportsPerTurnPromptUsage` and `geminiPromptUsage`.
    pub fn per_turn_usage(&self) -> bool {
        matches!(self, Self::Antigravity)
    }

    /// Antigravity reports `inputTokens` without cache reads and
    /// `outputTokens` without thoughts; the contract folds both in.
    pub fn gemini_usage(&self) -> bool {
        matches!(self, Self::Antigravity)
    }

    pub fn driver(&self) -> &'static str {
        match self {
            Self::Generic => ACP_DRIVER_KIND,
            Self::Antigravity => ANTIGRAVITY_DRIVER_KIND,
        }
    }

    /// Antigravity's sign-in is an ACP `authenticate` call selected by method.
    pub fn auth_methods(&self) -> &'static [AntigravityAuthMethod] {
        match self {
            Self::Generic => &[],
            Self::Antigravity => AntigravityAuthMethod::ALL,
        }
    }
}

/// How to construct an ACP adapter. The transport config is Atrium's.
#[derive(Clone, Debug)]
pub struct AcpAdapterConfig {
    pub acp: AcpConfig,
    pub flavor: AcpFlavor,
    pub display_name: Option<String>,
    /// Antigravity's instance profile (its `GEMINI_HOME`), when one is managed.
    pub profile_directory: Option<PathBuf>,
    /// The user home used to find Antigravity's user skills.
    pub user_home: Option<PathBuf>,
    /// Where the host stores chat attachments. Contract attachment ids are
    /// resolved here (`{id}{extension}`), as T3's `resolveAttachmentPath`
    /// does. The host boundary sets it; `ATRIUM_ATTACHMENTS_DIR` is the
    /// default when present.
    pub attachments_directory: Option<PathBuf>,
    /// Model ids the agent accepts without advertising them (T3's
    /// `customModels`).
    pub custom_models: Vec<String>,
    /// Context window per model, when the host knows one its agent does not
    /// advertise (T3's `contextWindowTokens`).
    pub custom_model_context_windows: Vec<(String, u64)>,
    /// Resend the model even when the session already reports it (T3's
    /// `alwaysSetModel`).
    pub always_set_model: bool,
    /// Values applied for options the selection leaves unset, with T3's
    /// `_t3/empty` mapping to "". Explicit selections win.
    pub unset_option_defaults: Vec<(String, String)>,
    /// Stop a turn that has produced nothing for this long (T3's
    /// `promptInactivityTimeout`).
    pub prompt_inactivity_timeout: Option<Duration>,
    /// Wait for open tools before closing a settled turn (T3's
    /// `deferFinalizeForBackgroundWork`). `None` uses the flavor default.
    pub defer_finalize_for_background_work: Option<bool>,
}

impl Default for AcpAdapterConfig {
    fn default() -> Self {
        Self {
            acp: AcpConfig {
                backend: ACP_DRIVER_KIND.into(),
                program: PathBuf::from("acp"),
                args: Vec::new(),
                env: Vec::new(),
            },
            flavor: AcpFlavor::Generic,
            display_name: None,
            profile_directory: None,
            user_home: std::env::var_os("HOME").map(PathBuf::from),
            attachments_directory: std::env::var_os("ATRIUM_ATTACHMENTS_DIR").map(PathBuf::from),
            custom_models: Vec::new(),
            custom_model_context_windows: Vec::new(),
            always_set_model: false,
            unset_option_defaults: Vec::new(),
            prompt_inactivity_timeout: None,
            defer_finalize_for_background_work: None,
        }
    }
}

impl AcpAdapterConfig {
    /// Whether a settled prompt waits for its open tools before the turn
    /// closes. Antigravity runs commands that outlive the prompt, so it
    /// defers by default; plain ACP agents do not.
    pub fn defers_finalize(&self) -> bool {
        self.defer_finalize_for_background_work
            .unwrap_or(matches!(self.flavor, AcpFlavor::Antigravity))
    }
}

impl AcpAdapterConfig {
    /// A generic ACP agent started by `program args`.
    pub fn for_command(
        backend: impl Into<String>,
        program: impl Into<PathBuf>,
        args: Vec<String>,
    ) -> Self {
        let backend = backend.into();
        let program = program.into();
        let flavor = if AcpFlavor::detect(&backend, &program) {
            AcpFlavor::Antigravity
        } else {
            AcpFlavor::Generic
        };
        Self {
            acp: AcpConfig {
                backend,
                program,
                args,
                env: Vec::new(),
            },
            flavor,
            ..Self::default()
        }
    }

    pub fn antigravity(program: impl Into<PathBuf>, args: Vec<String>) -> Self {
        Self {
            acp: AcpConfig {
                backend: ANTIGRAVITY_DRIVER_KIND.into(),
                program: program.into(),
                args,
                env: Vec::new(),
            },
            flavor: AcpFlavor::Antigravity,
            display_name: Some("Antigravity".into()),
            ..Self::default()
        }
    }
}

impl AcpFlavor {
    /// `agy` (and `agy.exe`/`agy.cmd`) is Antigravity's CLI. A backend named
    /// `antigravity` is too, whatever wrapper runs it.
    pub fn detect(backend: &str, program: &Path) -> bool {
        let name = program
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let base = name
            .strip_suffix(".exe")
            .or_else(|| name.strip_suffix(".cmd"))
            .unwrap_or(&name);
        backend == ANTIGRAVITY_DRIVER_KIND || base == "agy"
    }
}

/// T3's `AcpProviderCapabilitiesV2`.
pub fn acp_provider_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        sessions: SessionCapabilities {
            supports_multiple_provider_threads_per_session: false,
            supports_model_switch_in_session: false,
            supports_provider_switching_via_handoff: true,
            supports_runtime_mode_switch_in_session: false,
            pending_requests_survive_restart: false,
        },
        threads: ThreadCapabilities {
            can_create_empty_thread: true,
            can_read_thread_snapshot: false,
            can_rollback_thread: true,
            can_fork_thread: false,
            can_fork_from_turn: false,
            can_fork_while_source_running: None,
            can_fork_from_subagent_thread: false,
            exposes_native_thread_id: true,
        },
        turns: TurnCapabilities {
            supports_native_goals: None,
            exposes_native_turn_id: false,
            emits_turn_started: true,
            emits_turn_completed: true,
            supports_interrupt: true,
            supports_native_resume: Some(false),
            supports_active_steering: false,
            active_steering_interrupts_tools: None,
            active_steering_accepts_attachments: None,
            active_steering_during_compaction: None,
            supports_steering_by_interrupt_restart: true,
            supports_queued_messages: true,
            terminal_status_quality: TerminalStatusQuality::Strong,
        },
        streaming: StreamingCapabilities {
            streams_assistant_text: true,
            streams_reasoning: true,
            streams_tool_output: true,
            streams_plan_text: false,
            emits_message_completed: true,
        },
        tools: ToolCapabilities {
            exposes_tool_item_ids: true,
            emits_tool_started: true,
            emits_tool_completed: true,
            emits_tool_output: true,
            supports_mcp_tools: false,
            supports_dynamic_tool_callbacks: false,
        },
        approvals: ApprovalCapabilities {
            supports_command_approval: true,
            supports_file_read_approval: true,
            supports_file_change_approval: true,
            supports_apply_patch_approval: false,
            approvals_have_native_request_ids: false,
            approval_callbacks_are_live_only: true,
            approvals_can_originate_from_subagents: false,
        },
        planning: PlanningCapabilities {
            emits_plan_updated: true,
            emits_todo_list: true,
            emits_proposed_plan: false,
            supports_structured_questions: true,
            plan_deltas_have_item_ids: false,
        },
        subagents: SubagentCapabilities {
            supports_subagents: false,
            exposes_subagent_thread_ids: false,
            emits_subagent_lifecycle: false,
            can_wait_for_subagents: false,
            can_close_subagents: false,
            can_fork_subagent_thread: false,
        },
        context: ContextCapabilities {
            accepts_system_context: false,
            accepts_developer_context: false,
            accepts_synthetic_user_context: true,
            can_generate_summaries: true,
            can_consume_handoff_summaries: true,
            supports_delta_handoff: true,
            supports_full_thread_handoff: true,
            max_recommended_handoff_chars: None,
        },
        checkpointing: CheckpointCapabilities {
            app_can_checkpoint_filesystem: true,
            supports_nested_checkpoint_scopes: true,
            // ACP lacks truncation; rollback starts a fresh session and replays.
            provider_can_rollback_conversation: false,
            provider_rollback_returns_snapshot: true,
            provider_can_read_conversation_snapshot: false,
        },
        identity: IdentityCapabilities {
            native_thread_ids: NativeRefStrength::Strong,
            native_turn_ids: NativeRefStrength::Weak,
            native_item_ids: NativeRefStrength::Weak,
            native_request_ids: NativeRefStrength::Weak,
        },
        runtime_policy: RuntimePolicyCapabilities {
            enforcement: RuntimePolicyEnforcement::ClientBoundary,
        },
    }
}

/// T3's `AntigravityProviderCapabilitiesV2`: generic ACP plus what agy adds.
pub fn antigravity_provider_capabilities() -> ProviderCapabilities {
    let mut capabilities = acp_provider_capabilities();
    capabilities.sessions.supports_model_switch_in_session = true;
    capabilities
        .sessions
        .supports_runtime_mode_switch_in_session = true;
    capabilities.tools.supports_mcp_tools = true;
    capabilities.subagents.supports_subagents = true;
    capabilities
}

/// The capability record for `flavor`.
pub fn provider_capabilities(flavor: &AcpFlavor) -> ProviderCapabilities {
    match flavor {
        AcpFlavor::Generic => acp_provider_capabilities(),
        AcpFlavor::Antigravity => antigravity_provider_capabilities(),
    }
}

/// T3's `canSwitchModelInSession`: the session's advertised options decide,
/// not the driver's static record.
pub fn can_switch_model_in_session(options: &[ConfigOption]) -> bool {
    option_with_category(options, "model").is_some()
}

/// The option a category names, or the conventional id when no category did.
pub fn option_with_category<'a>(
    options: &'a [ConfigOption],
    category: &str,
) -> Option<&'a ConfigOption> {
    options
        .iter()
        .find(|option| option.category.as_deref() == Some(category))
        .or_else(|| {
            let id = match category {
                "model" => "model",
                "thought_level" => "reasoning_effort",
                "mode" => ACP_SESSION_MODE_OPTION_ID,
                _ => return None,
            };
            options.iter().find(|option| option.id == id)
        })
}

/// The option a model-selection id names. The model selection uses app-side
/// ids such as `effort`/`serviceTier`; the session uses its own categories.
fn option_for_selection<'a>(options: &'a [ConfigOption], id: &str) -> Option<&'a ConfigOption> {
    match id {
        "reasoningEffort" | "effort" => options
            .iter()
            .find(|option| is_thought_level_option(option)),
        "serviceTier" => options
            .iter()
            .find(|option| option.category.as_deref() == Some("service_tier")),
        "mode" => option_with_category(options, "mode"),
        other => options.iter().find(|option| option.id == other),
    }
}

/// T3's `ACP_EMPTY_OPTION_VALUE`: the wire uses this for an unset choice, and
/// writing it back clears the option.
fn acp_option_value(value: &str) -> &str {
    if value == "_t3/empty" { "" } else { value }
}

/// T3's `isAcpThoughtLevelOption`: `thought_level`, or Mistral Vibe's
/// `thinking` select.
pub fn is_thought_level_option(option: &ConfigOption) -> bool {
    option.category.as_deref() == Some("thought_level")
        || (option.category.as_deref() == Some("thinking") && !option.choices.is_empty())
}

/// One prompt's usage as T3's `TurnTokenUsage`. Token deltas are already
/// folded into the transcript's `TokenUsage`; this is the per-turn record.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnTokenUsage {
    pub usage_scope: &'static str,
    pub usage_status: &'static str,
    pub has_subagents: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
}

/// T3's `acpPerTurnPromptUsage`. Input includes cache reads and writes,
/// reasoning is a subset of output; counters that exceed their parent are
/// clamped and unset ones are dropped.
pub fn acp_per_turn_prompt_usage(
    usage: Option<&Value>,
    has_subagents: bool,
    terminal_status: ProviderTurnStatus,
    gemini_usage: bool,
) -> TurnTokenUsage {
    let Some(usage) = usage.filter(|usage| usage.is_object()) else {
        return TurnTokenUsage {
            usage_scope: "main_agent",
            usage_status: "unavailable",
            has_subagents,
            input_tokens: None,
            output_tokens: None,
            cached_input_tokens: None,
            cache_creation_tokens: None,
            reasoning_tokens: None,
        };
    };
    let n = |key: &str| usage[key].as_u64();
    let mut input = n("inputTokens").unwrap_or(0);
    let mut output = n("outputTokens").unwrap_or(0);
    let cached_read = n("cachedReadTokens");
    let cached_write = n("cachedWriteTokens");
    let thought = n("thoughtTokens");
    if gemini_usage {
        input += cached_read.unwrap_or(0);
        output += thought.unwrap_or(0);
    }
    let clamp = |value: Option<u64>, parent: u64| value.map(|value| value.min(parent));
    let cached_input = clamp(cached_read, input);
    let cache_creation = clamp(cached_write, input - cached_input.unwrap_or(0));
    let reasoning = clamp(thought, output);
    TurnTokenUsage {
        usage_scope: "main_agent",
        usage_status: if terminal_status == ProviderTurnStatus::Completed {
            "complete"
        } else {
            "partial"
        },
        has_subagents,
        input_tokens: Some(input),
        output_tokens: Some(output),
        cached_input_tokens: cached_input,
        cache_creation_tokens: cache_creation,
        reasoning_tokens: reasoning,
    }
}

/// T3's `antigravityPermissionMode`: the session mode agy is asked to run.
pub fn antigravity_permission_mode(mode: RuntimeMode) -> &'static str {
    match mode {
        RuntimeMode::FullAccess => "yolo",
        RuntimeMode::AutoAcceptEdits => "auto_edit",
        RuntimeMode::Auto => "auto",
        RuntimeMode::ApprovalRequired => "default",
    }
}

/// The agy CLI rejects `yolo`/`auto_edit`; those stay `default`, where the
/// agent asks and Atrium answers from the live policy.
pub fn antigravity_session_mode(mode: &str, options: &[ConfigOption]) -> String {
    if mode == "auto" {
        let advertises_auto = option_with_category(options, "mode")
            .is_some_and(|option| option.choices.iter().any(|choice| choice.value == "auto"));
        return if advertises_auto { "auto" } else { "default" }.into();
    }
    if mode == "yolo" || mode == "auto_edit" {
        return "default".into();
    }
    mode.to_string()
}

/// The runtime mode an Antigravity session mode grants.
pub fn antigravity_runtime_mode_for_session_mode(mode: &str) -> Option<RuntimeMode> {
    match mode {
        "yolo" => Some(RuntimeMode::FullAccess),
        "auto_edit" => Some(RuntimeMode::AutoAcceptEdits),
        "auto" => Some(RuntimeMode::Auto),
        "default" => Some(RuntimeMode::ApprovalRequired),
        _ => None,
    }
}

/// T3's `acpPermissionDisposition`. Reads never ask (unless a sandbox type
/// is unknown); below full access, mutations that are not auto-accepted ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionDisposition {
    Allow,
    Ask,
    Deny,
}

/// T3's `acpPermissionDisposition` with the tool call's locations, so a
/// `workspaceWrite` sandbox can allow mutations inside its roots.
pub fn permission_disposition_for(
    policy: &RuntimePolicy,
    kind: Option<&str>,
    locations: &[String],
) -> PermissionDisposition {
    let kind = kind.unwrap_or("other");
    if is_acp_read_kind(kind) {
        return read_disposition(policy);
    }
    if acp_policy_requires_approval(policy) {
        return PermissionDisposition::Ask;
    }
    match sandbox_type(policy) {
        Some("readOnly") => PermissionDisposition::Deny,
        Some("workspaceWrite") => {
            if is_acp_mutation_kind(kind) && workspace_write_allows(policy, locations) {
                PermissionDisposition::Allow
            } else {
                PermissionDisposition::Deny
            }
        }
        Some("dangerFullAccess" | "externalSandbox") => PermissionDisposition::Allow,
        Some(_) => PermissionDisposition::Deny,
        None => {
            if policy.runtime_mode == RuntimeMode::ApprovalRequired {
                return PermissionDisposition::Deny;
            }
            if policy.runtime_mode == RuntimeMode::AutoAcceptEdits
                && policy.approval_policy.is_none()
                && !is_acp_mutation_kind(kind)
            {
                return PermissionDisposition::Ask;
            }
            PermissionDisposition::Allow
        }
    }
}

/// [`permission_disposition_for`] without locations, for callers that only
/// know the kind.
pub fn permission_disposition(policy: &RuntimePolicy, kind: Option<&str>) -> PermissionDisposition {
    permission_disposition_for(policy, kind, &[])
}

pub fn is_acp_mutation_kind(kind: &str) -> bool {
    matches!(kind, "edit" | "delete" | "move")
}

fn is_acp_read_kind(kind: &str) -> bool {
    matches!(kind, "read" | "search" | "think")
}

/// T3's `acpPolicyRequiresApproval`.
fn acp_policy_requires_approval(policy: &RuntimePolicy) -> bool {
    match &policy.approval_policy {
        Some(value) => value != "never",
        None => policy.runtime_mode == RuntimeMode::ApprovalRequired,
    }
}

/// The tagged sandbox type T3 switches on (`sandboxPolicy.type`).
fn sandbox_type(policy: &RuntimePolicy) -> Option<&str> {
    policy
        .sandbox_policy
        .as_ref()
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str)
}

/// T3's `acpReadDisposition`: an absent sandbox implies reads are allowed.
fn read_disposition(policy: &RuntimePolicy) -> PermissionDisposition {
    match sandbox_type(policy) {
        None | Some("readOnly" | "workspaceWrite" | "dangerFullAccess" | "externalSandbox") => {
            PermissionDisposition::Allow
        }
        Some(_) => PermissionDisposition::Deny,
    }
}

/// Whether every location is inside the workspace or a writable root. Paths
/// are canonicalized so `..` cannot escape; missing files canonicalize their
/// deepest existing parent, as T3 does.
fn workspace_write_allows(policy: &RuntimePolicy, locations: &[String]) -> bool {
    let cwd = policy.cwd.as_deref().unwrap_or("").trim();
    let mut roots = Vec::new();
    if !cwd.is_empty()
        && let Some(root) = canonical_for_containment(Path::new(cwd))
    {
        roots.push(root);
    }
    if let Some(writable) = policy
        .sandbox_policy
        .as_ref()
        .and_then(|value| value.get("writableRoots"))
        .and_then(Value::as_array)
    {
        for root in writable.iter().filter_map(Value::as_str) {
            let resolved = resolve_permission_path(root, cwd);
            if let Some(canonical) = resolved.as_deref().and_then(canonical_for_containment) {
                roots.push(canonical);
            }
        }
    }
    if roots.is_empty() || locations.is_empty() {
        return false;
    }
    locations.iter().all(|location| {
        resolve_permission_path(location, cwd)
            .as_deref()
            .and_then(canonical_for_containment)
            .is_some_and(|path| roots.iter().any(|root| path.starts_with(root)))
    })
}

fn resolve_permission_path(path: &str, cwd: &str) -> Option<PathBuf> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    let path = Path::new(path);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    (!cwd.trim().is_empty()).then(|| Path::new(cwd).join(path))
}

fn canonical_for_containment(path: &Path) -> Option<PathBuf> {
    let mut candidate = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        if let Ok(canonical) = std::fs::canonicalize(&candidate) {
            let mut result = canonical;
            for part in missing.iter().rev() {
                result.push(part);
            }
            return Some(result);
        }
        let parent = candidate.parent()?.to_path_buf();
        missing.push(candidate.file_name()?.to_os_string());
        candidate = parent;
    }
}

/// T3's `contextWindowTokens` lookup: the selection's own `contextWindow`
/// option, else the session's current value, parsed as `200k`/`1m`.
pub fn context_window_tokens(options: &[ConfigOption], selection: &ModelSelection) -> Option<u64> {
    let option = options
        .iter()
        .find(|option| option.id == "contextWindow")
        .or_else(|| {
            options
                .iter()
                .find(|option| option.category.as_deref() == Some("context_window"))
        })?;
    let selected = selection
        .options
        .as_ref()
        .and_then(|selections| {
            selections
                .0
                .iter()
                .find(|selection| selection.id == option.id)
        })
        .and_then(|selection| selection.value.as_str());
    parse_context_window(selected.unwrap_or(&option.current))
}

pub fn parse_context_window(value: &str) -> Option<u64> {
    let value = value.trim();
    let (digits, factor) = match value.as_bytes().last()?.to_ascii_lowercase() {
        b'k' => (&value[..value.len() - 1], 1_000u64),
        b'm' => (&value[..value.len() - 1], 1_000_000u64),
        _ => return None,
    };
    let count: u64 = digits.parse().ok()?;
    (count > 0).then(|| count * factor)
}

/// T3's `validateModelSelection`. Empty, `default`, `auto`, a custom model
/// and Antigravity's default id are accepted; a concrete model the session
/// did not advertise is refused with T3's message.
pub fn validate_model_selection(
    options: &[ConfigOption],
    custom_models: &[String],
    selection: &ModelSelection,
) -> Result<(), String> {
    let model = selection.model.trim();
    if model.is_empty()
        || model == "default"
        || model == "auto"
        || model == ANTIGRAVITY_DEFAULT_MODEL
        || custom_models.iter().any(|custom| custom == model)
    {
        return Ok(());
    }
    let Some(option) = option_with_category(options, "model") else {
        return Ok(());
    };
    if option.choices.is_empty() {
        return Ok(());
    }
    if option.choices.iter().any(|choice| choice.value == model)
        || legacy_antigravity_family(option, model).is_some()
    {
        return Ok(());
    }
    Err("The requested model is not available in this session.".into())
}

/// T3's `legacyAntigravityFamily`: runtime 1.4 lists `gemini-3.8-flash` where
/// 1.3 listed `gemini-3.8-flash-high`; the agent still accepts the old id.
pub fn legacy_antigravity_family(option: &ConfigOption, model: &str) -> Option<String> {
    option
        .choices
        .iter()
        .map(|choice| choice.value.as_str())
        .filter(|family| model.starts_with(&format!("{family}-")))
        .max_by_key(|family| family.len())
        .map(str::to_owned)
}

/// One configured ACP provider instance.
pub struct AcpAdapter {
    instance_id: ProviderInstanceId,
    config: AcpAdapterConfig,
    capabilities: ProviderCapabilities,
}

impl AcpAdapter {
    pub fn new(instance_id: impl Into<String>, config: AcpAdapterConfig) -> Self {
        Self {
            instance_id: ProviderInstanceId::new(instance_id),
            capabilities: provider_capabilities(&config.flavor),
            config,
        }
    }

    pub fn config(&self) -> &AcpAdapterConfig {
        &self.config
    }

    /// The usage reader a host scans when it reports Antigravity usage.
    /// The reader uses the instance profile when one is managed; otherwise
    /// the caller scans Antigravity's usual roots under the user home.
    pub fn usage_reader(&self) -> Option<AntigravityUsageReader> {
        match self.config.flavor {
            AcpFlavor::Antigravity => Some(AntigravityUsageReader::new(
                self.config.profile_directory.clone(),
            )),
            AcpFlavor::Generic => None,
        }
    }

    /// The user home the usage reader scans when no profile is managed.
    pub fn usage_home(&self) -> Option<PathBuf> {
        self.config
            .profile_directory
            .clone()
            .or_else(|| self.config.user_home.clone())
    }

    /// The context window a model selection asks for, when the session or the
    /// host knows it. ACP reports it as a config option (T3's
    /// `contextWindowTokens`); custom models may carry one in the host.
    pub fn context_window_tokens(
        &self,
        options: &[ConfigOption],
        selection: &ModelSelection,
    ) -> Option<u64> {
        context_window_tokens(options, selection).or_else(|| {
            self.config
                .custom_model_context_windows
                .iter()
                .find(|(model, _)| model == &selection.model)
                .map(|(_, tokens)| *tokens)
        })
    }
}

impl ProviderAdapter for AcpAdapter {
    fn instance_id(&self) -> &ProviderInstanceId {
        &self.instance_id
    }

    fn driver(&self) -> &str {
        self.config.flavor.driver()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }

    fn plan_selection_transition(
        &self,
        _current: &ModelSelection,
        _target: &ModelSelection,
    ) -> SelectionTransitionPlan {
        // ACP applies a selection to a live session through config options;
        // the caller passes the session's advertised capability to
        // `acp_selection_transition` for the refusal case.
        SelectionTransitionPlan::ApplyOnNextTurn
    }

    fn open_session(
        &self,
        input: OpenSessionInput,
        sink: AdapterEventSink,
    ) -> Result<Arc<dyn ProviderSessionRuntime>, ProviderAdapterError> {
        let driver = self.config.flavor.driver();
        let started = futures::executor::block_on(AcpClient::start(self.config.acp.clone()))
            .map_err(|error| {
                log::warn!("ACP agent did not start: {error:#}");
                ProviderAdapterError::OpenSession {
                    driver: driver.into(),
                    provider_session_id: input.provider_session_id.to_string(),
                }
            })?;
        let (client, incoming) = started;
        Ok(Arc::new(AcpSessionRuntime::new(
            self.instance_id.clone(),
            self.capabilities.clone(),
            self.config.clone(),
            client,
            incoming,
            input,
            sink,
        )))
    }
}

struct PromptFinished {
    native_session: String,
    result: Result<String, String>,
    usage: Option<Value>,
    delivery: PromptDelivery,
    limits: Option<atrium_core::RateLimits>,
    provider_turn_id: ProviderTurnId,
    run_ordinal: u32,
    /// A prompt-steered turn spans several prompts; only the last reply
    /// closes it.
    finishes_turn: bool,
}

enum PumpMessage {
    Incoming(agent_rpc::Incoming),
    Finished(Box<PromptFinished>),
    Stop,
}

#[derive(Clone)]
struct ActiveTurn {
    provider_turn: ProviderTurn,
    run_ordinal: u32,
    has_subagents: bool,
    /// Tool, command and file-change items that started and not completed.
    open_tools: usize,
}

struct Inner {
    driver: String,
    instance_id: ProviderInstanceId,
    provider_session_id: ProviderSessionId,
    provider_session: Mutex<ProviderSession>,
    client: AcpClient,
    config: AcpAdapterConfig,
    sink: AdapterEventSink,
    options: Mutex<Vec<ConfigOption>>,
    threads: Mutex<HashMap<String, ProviderThread>>,
    native_session: Mutex<Option<String>>,
    active: Mutex<Option<ActiveTurn>>,
    runtime_policy: Mutex<RuntimePolicy>,
    /// Execute grants remembered for this session and the current turn, as
    /// T3's `makeAcpClientPolicyGrants` does.
    grants: Mutex<PolicyGrants>,
    /// The last usage limits the agent reported, if any. Never synthesized.
    last_limits: Mutex<Option<atrium_core::RateLimits>>,
    /// When the running prompt last showed progress, for the inactivity
    /// watchdog.
    last_progress: Mutex<Option<Instant>>,
    now: Mutex<String>,
    closed: AtomicBool,
    /// Prompts in flight; a steered turn has more than one.
    prompts_in_flight: AtomicU64,
    /// Set when the watchdog stopped the prompt for inactivity; the reply
    /// then settles the turn as failed.
    timed_out: AtomicBool,
    pump: Mutex<Option<mpsc::Sender<PumpMessage>>>,
}

/// T3's `makeAcpClientPolicyGrants`: ACP cannot link commands to terminals,
/// so an approved execute is granted for the session or the turn.
#[derive(Clone, Debug, Default)]
struct PolicyGrants {
    session_execute: bool,
    turn_execute: Option<String>,
}

impl PolicyGrants {
    fn record(&mut self, kind: Option<&str>, session: bool, turn_key: Option<String>) {
        if kind != Some("execute") {
            return;
        }
        if session {
            self.session_execute = true;
        } else if let Some(turn_key) = turn_key {
            self.turn_execute = Some(turn_key);
        }
    }

    fn allows_execute(&self, turn_key: Option<&str>) -> bool {
        self.session_execute || (turn_key.is_some() && self.turn_execute.as_deref() == turn_key)
    }
}

impl Inner {
    fn emit(&self, event: AdapterEvent) {
        (self.sink)(event);
    }

    fn emit_agent(&self, event: AgentEvent) {
        if let AgentEvent::ItemStarted { item, .. } = &event
            && item_is_tool(item)
            && let Some(active) = self.active.lock().expect("acp active").as_mut()
        {
            active.open_tools += 1;
        }
        if let AgentEvent::ItemCompleted { item, .. } = &event
            && item_is_tool(item)
            && let Some(active) = self.active.lock().expect("acp active").as_mut()
        {
            active.open_tools = active.open_tools.saturating_sub(1);
        }
        if let AgentEvent::ItemStarted { item, .. } | AgentEvent::ItemCompleted { item, .. } =
            &event
            && item_is_subagent(item)
            && let Some(active) = self.active.lock().expect("acp active").as_mut()
        {
            active.has_subagents = true;
        }
        self.emit(AdapterEvent::Agent(event));
    }

    fn set_now(&self, now: &str) {
        *self.now.lock().expect("acp now") = now.to_string();
    }

    fn now(&self) -> String {
        self.now.lock().expect("acp now").clone()
    }

    fn set_policy(&self, policy: &RuntimePolicy) {
        *self.runtime_policy.lock().expect("acp policy") = policy.clone();
    }

    fn policy(&self) -> RuntimePolicy {
        self.runtime_policy.lock().expect("acp policy").clone()
    }

    fn thread_for_native(&self, native: &str) -> Option<ProviderThread> {
        self.threads
            .lock()
            .expect("acp threads")
            .get(native)
            .cloned()
    }

    fn permission_request(
        &self,
        event: &AgentEvent,
    ) -> Option<(ProviderRequestKind, RuntimeRequest)> {
        let AgentEvent::ApprovalRequested(request) = event else {
            return None;
        };
        let kind = match &request.kind {
            ApprovalKind::Command { .. } => ProviderRequestKind::Command,
            ApprovalKind::FileChange { .. } => ProviderRequestKind::FileChange,
            ApprovalKind::Tool { .. } => ProviderRequestKind::Permission,
        };
        let request_id = request_id_text(&request.request_id);
        // Antigravity shares `session/request_permission` for native
        // questions; those are answered with an option, not an approval.
        let runtime_kind = if self
            .client
            .permission_params(&request.request_id)
            .is_some_and(|params| is_antigravity_question(&params))
        {
            RuntimeRequestKind::UserInput
        } else {
            match kind {
                ProviderRequestKind::Command => RuntimeRequestKind::Command,
                ProviderRequestKind::FileChange => RuntimeRequestKind::FileChange,
                _ => RuntimeRequestKind::Permission,
            }
        };
        let turn_id = self
            .active
            .lock()
            .expect("acp active")
            .as_ref()
            .map(|active| active.provider_turn.id.clone());
        let now = self.now();
        Some((
            kind,
            RuntimeRequest {
                id: RuntimeRequestId::new(request_id.clone()),
                node_id: NodeId::new(format!("runtime-request-{request_id}")),
                provider_turn_id: turn_id,
                native_request_ref: Some(ProviderRef {
                    driver: ProviderDriverKind::new(&self.driver),
                    native_id: Some(request_id),
                    strength: NativeRefStrength::Weak,
                    fingerprint: None,
                    ordinal: None,
                }),
                kind: runtime_kind,
                status: RuntimeRequestStatus::Pending,
                response_capability: ResponseCapability::Live {
                    provider_session_id: self.provider_session_id.clone(),
                    supports_feedback: Some(false),
                },
                created_at: now,
                resolved_at: None,
                decision: None,
                feedback: None,
                answers: None,
            },
        ))
    }

    /// A permission the policy settles without a person is answered at once.
    fn auto_answer(&self, event: &AgentEvent) -> bool {
        let AgentEvent::ApprovalRequested(request) = event else {
            return false;
        };
        let params = self.client.permission_params(&request.request_id);
        // Native questions are not approvals; a person or the asking client
        // answers them with an option, whatever the runtime policy is.
        if params.as_ref().is_some_and(is_antigravity_question) {
            return false;
        }
        let (kind, locations) = permission_operation(&request.kind, params.as_ref());
        let policy = self.policy();
        let turn_key = self
            .active
            .lock()
            .expect("acp active")
            .as_ref()
            .map(|active| active.provider_turn.id.to_string());
        let disposition = permission_disposition_for(&policy, kind, &locations);
        let disposition = match disposition {
            // A prior allow for this session or turn covers a later execute.
            PermissionDisposition::Ask
                if kind == Some("execute")
                    && self
                        .grants
                        .lock()
                        .expect("acp grants")
                        .allows_execute(turn_key.as_deref()) =>
            {
                PermissionDisposition::Allow
            }
            other => other,
        };
        match disposition {
            PermissionDisposition::Allow => {
                let _ = self
                    .client
                    .answer_permission(request.request_id.clone(), ApprovalDecision::Accept);
                true
            }
            PermissionDisposition::Deny => {
                let _ = self
                    .client
                    .answer_permission(request.request_id.clone(), ApprovalDecision::Decline);
                true
            }
            PermissionDisposition::Ask => false,
        }
    }

    fn merge_limits(&self, limits: atrium_core::RateLimits) {
        let mut last = self.last_limits.lock().expect("acp limits");
        match last.as_mut() {
            Some(current) => current.merge(limits),
            None => *last = Some(limits),
        }
    }

    fn last_limits(&self) -> Option<atrium_core::RateLimits> {
        self.last_limits.lock().expect("acp limits").clone()
    }

    /// The prompt blocks a turn sends. Contract ids resolve through the host's
    /// attachments directory; seam-supplied paths are already resolved. The
    /// flavor decides whether files are path notes (generic ACP) or native
    /// resource/image/audio blocks (Antigravity).
    fn turn_prompt_blocks(
        &self,
        text: &str,
        message: &TurnMessage,
        extra_paths: &[PathBuf],
    ) -> Result<Vec<Value>, ProviderAdapterError> {
        let mut resolved = resolve_contract_attachments(
            self.config.attachments_directory.as_deref(),
            &message.attachments,
        );
        resolved.extend(extra_paths.iter().map(|path| resolved_from_path(path)));
        match self.config.flavor {
            AcpFlavor::Generic => generic_prompt_blocks(&self.client, text, &resolved)
                .map_err(|error| ProviderAdapterError::protocol(&self.driver, error.to_string())),
            AcpFlavor::Antigravity => super::acp_antigravity::build_prompt_blocks(
                text,
                &resolved,
                super::acp_antigravity::PromptCapabilities {
                    images: self.client.info.images,
                    audio: self.client.info.audio,
                    embedded_context: self.client.info.embedded_context,
                },
            )
            .map_err(|detail| ProviderAdapterError::protocol(&self.driver, detail)),
        }
    }
}

/// The tool kind and locations a permission request names, preferring the
/// raw ACP params over the translated approval kind.
fn permission_operation<'a>(
    kind: &'a ApprovalKind,
    params: Option<&'a Value>,
) -> (Option<&'a str>, Vec<String>) {
    if let Some(params) = params {
        let tool = &params["toolCall"];
        let fallback = match kind {
            ApprovalKind::Command { .. } => Some("execute"),
            ApprovalKind::FileChange { .. } => Some("edit"),
            ApprovalKind::Tool { .. } => None,
        };
        let kind = tool["kind"].as_str().or(fallback);
        let locations = tool["locations"]
            .as_array()
            .map(|locations| {
                locations
                    .iter()
                    .filter_map(|location| location["path"].as_str())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        return (kind, locations);
    }
    let kind = match kind {
        ApprovalKind::Command { .. } => Some("execute"),
        ApprovalKind::FileChange { .. } => Some("edit"),
        ApprovalKind::Tool { .. } => None,
    };
    (kind, Vec::new())
}

impl AcpSessionRuntime {
    /// Signs in with one of the agent's advertised methods (Antigravity's
    /// `oauth-personal`, `oauth-business`, ...). The native process owns the
    /// token exchange and storage; nothing here reads a credential.
    pub fn sign_in(&self, method_id: &str) -> Result<(), ProviderAdapterError> {
        futures::executor::block_on(self.inner.client.authenticate(method_id))
            .map_err(|error| ProviderAdapterError::protocol(&self.inner.driver, error.to_string()))
    }

    /// Sign-in URLs the agent announced since the last call. The native
    /// process owns the token exchange; Atrium only forwards the link.
    pub fn take_auth_urls(&self) -> Vec<String> {
        self.inner.client.take_auth_urls()
    }

    /// Clears the agent's stored credentials (ACP `logout`).
    pub fn sign_out(&self) -> Result<(), ProviderAdapterError> {
        futures::executor::block_on(self.inner.client.logout())
            .map_err(|error| ProviderAdapterError::protocol(&self.inner.driver, error.to_string()))
    }

    /// The last usage limits the agent actually reported, if any.
    pub fn last_limits(&self) -> Option<atrium_core::RateLimits> {
        self.inner.last_limits()
    }

    /// The context window for `selection` from the session's options and the
    /// host's custom-model table.
    pub fn context_window_tokens(&self, selection: &ModelSelection) -> Option<u64> {
        context_window_tokens(&self.options(), selection).or_else(|| {
            self.inner
                .config
                .custom_model_context_windows
                .iter()
                .find(|(model, _)| model == &selection.model)
                .map(|(_, tokens)| *tokens)
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        instance_id: ProviderInstanceId,
        capabilities: ProviderCapabilities,
        config: AcpAdapterConfig,
        client: AcpClient,
        incoming: futures::channel::mpsc::UnboundedReceiver<agent_rpc::Incoming>,
        input: OpenSessionInput,
        sink: AdapterEventSink,
    ) -> Self {
        let driver = config.flavor.driver().to_string();
        let cwd = input.runtime_policy.cwd.clone().unwrap_or_else(current_dir);
        let provider_session = ProviderSession {
            id: input.provider_session_id.clone(),
            driver: ProviderDriverKind::new(&driver),
            provider_instance_id: instance_id.clone(),
            status: ProviderSessionStatus::Ready,
            cwd,
            model: Some(input.model_selection.model.clone()),
            capabilities,
            extension_statuses: Vec::new(),
            created_at: input.now.clone(),
            updated_at: input.now.clone(),
            last_error: None,
        };
        let inner = Arc::new(Inner {
            driver: driver.clone(),
            instance_id,
            provider_session_id: input.provider_session_id.clone(),
            provider_session: Mutex::new(provider_session.clone()),
            client: client.clone(),
            config,
            sink: sink.clone(),
            options: Mutex::new(Vec::new()),
            threads: Mutex::new(HashMap::new()),
            native_session: Mutex::new(input.initial_native_thread_id.clone()),
            active: Mutex::new(None),
            runtime_policy: Mutex::new(input.runtime_policy.clone()),
            grants: Mutex::new(PolicyGrants::default()),
            last_limits: Mutex::new(None),
            last_progress: Mutex::new(None),
            now: Mutex::new(input.now.clone()),
            closed: AtomicBool::new(false),
            prompts_in_flight: AtomicU64::new(0),
            timed_out: AtomicBool::new(false),
            pump: Mutex::new(None),
        });
        sink(AdapterEvent::ProviderSessionUpdated {
            driver,
            provider_session,
        });
        Self::start_pump(inner.clone(), incoming);
        Self { inner }
    }

    fn start_pump(
        inner: Arc<Inner>,
        mut incoming: futures::channel::mpsc::UnboundedReceiver<agent_rpc::Incoming>,
    ) {
        let (tx, rx) = mpsc::channel();
        if let Ok(mut pump) = inner.pump.lock() {
            *pump = Some(tx.clone());
        }
        let forwarder = tx.clone();
        let _ = thread::Builder::new()
            .name(format!("{}-acp-in", inner.driver))
            .spawn(move || {
                futures::executor::block_on(async move {
                    while let Some(message) = incoming.next().await {
                        if forwarder.send(PumpMessage::Incoming(message)).is_err() {
                            return;
                        }
                    }
                    let _ = forwarder.send(PumpMessage::Stop);
                });
            });
        let _ = thread::Builder::new()
            .name(format!("{}-acp-translate", inner.driver))
            .spawn(move || pump_loop(inner, rx));
    }

    fn set_pump_stopped(&self) {
        if let Ok(mut pump) = self.inner.pump.lock()
            && let Some(tx) = pump.take()
        {
            let _ = tx.send(PumpMessage::Stop);
        }
    }

    fn native_session(&self) -> Result<String, ProviderAdapterError> {
        self.inner
            .native_session
            .lock()
            .expect("acp native session")
            .clone()
            .ok_or_else(|| {
                ProviderAdapterError::protocol(
                    &self.inner.driver,
                    "Provider thread is missing a native ACP session id",
                )
            })
    }

    fn new_provider_thread(
        &self,
        app_thread: ThreadId,
        native: String,
        now: &str,
        status: ProviderThreadStatus,
    ) -> ProviderThread {
        ProviderThread {
            id: ProviderThreadId::new(format!("provider-thread-{native}")),
            driver: ProviderDriverKind::new(&self.inner.driver),
            provider_instance_id: self.inner.instance_id.clone(),
            provider_session_id: Some(self.inner.provider_session_id.clone()),
            app_thread_id: Some(app_thread),
            owner_node_id: None,
            native_thread_ref: Some(ProviderRef {
                driver: ProviderDriverKind::new(&self.inner.driver),
                native_id: Some(native),
                strength: NativeRefStrength::Strong,
                fingerprint: None,
                ordinal: None,
            }),
            native_conversation_head_ref: None,
            status,
            first_run_ordinal: None,
            last_run_ordinal: None,
            handoff_ids: Vec::new(),
            forked_from: None,
            pending_background_tasks: Vec::new(),
            context_usage: None,
            native_metadata: None,
            goal: None,
            created_at: now.to_string(),
            updated_at: now.to_string(),
        }
    }

    fn update_thread(
        &self,
        thread: ProviderThread,
        pending_background_work: Option<bool>,
    ) -> ProviderThread {
        if let Some(native) = thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.clone())
        {
            self.inner
                .threads
                .lock()
                .expect("acp threads")
                .insert(native, thread.clone());
        }
        self.inner.emit(AdapterEvent::ProviderThreadUpdated {
            driver: self.inner.driver.clone(),
            provider_thread: thread.clone(),
            pending_background_work,
        });
        thread
    }

    fn set_options(&self, options: Vec<ConfigOption>) {
        *self.inner.options.lock().expect("acp options") = options;
    }

    fn options(&self) -> Vec<ConfigOption> {
        self.inner.options.lock().expect("acp options").clone()
    }

    fn update_provider_session(&self, status: ProviderSessionStatus, model: Option<String>) {
        {
            let mut session = self.inner.provider_session.lock().expect("acp session");
            session.status = status;
            if model.is_some() {
                session.model = model;
            }
            session.updated_at = self.inner.now();
        }
        let session = self
            .inner
            .provider_session
            .lock()
            .expect("acp session")
            .clone();
        self.inner.emit(AdapterEvent::ProviderSessionUpdated {
            driver: self.inner.driver.clone(),
            provider_session: session,
        });
    }

    /// Apply a selection through the session's advertised options, in T3's
    /// order: model, then the other categories, then the mode from policy.
    fn apply_selection(
        &self,
        selection: &ModelSelection,
        policy: Option<&RuntimePolicy>,
    ) -> Result<(), ProviderAdapterError> {
        let options = self.options();
        let native = self.native_session()?;
        // T3 refuses a concrete model the session did not advertise; custom
        // models and Antigravity's older family ids are allowed through.
        validate_model_selection(&options, &self.inner.config.custom_models, selection)
            .map_err(|detail| ProviderAdapterError::protocol(&self.inner.driver, detail))?;
        let apply = |id: &str, value: &str| -> Result<(), ProviderAdapterError> {
            futures::executor::block_on(self.inner.client.set_option(&native, id, value))
                .map(|_| ())
                .map_err(|error| {
                    ProviderAdapterError::protocol(&self.inner.driver, error.to_string())
                })
        };
        if let Some(model_option) = option_with_category(&options, "model") {
            // Antigravity's "no explicit model" selection keeps the current one.
            let requested = if selection.model == ANTIGRAVITY_DEFAULT_MODEL {
                model_option.current.clone()
            } else {
                selection.model.clone()
            };
            let custom = self
                .inner
                .config
                .custom_models
                .iter()
                .any(|model| model == &requested);
            let known = model_option
                .choices
                .iter()
                .any(|choice| choice.value == requested);
            let legacy = legacy_antigravity_family(model_option, &requested).is_some();
            let can_set = model_option.choices.is_empty() || known || custom || legacy;
            let resend = self.inner.config.always_set_model;
            if can_set && !requested.is_empty() && (requested != model_option.current || resend) {
                apply(&model_option.id, &requested)?;
            }
            // Any selection shifts the model, so refresh before mapping the
            // other options' current values.
            let updated = self.inner.client.session_options(&native);
            if !updated.is_empty() {
                self.set_options(updated);
            }
        }
        let options = self.options();
        let explicit: Vec<String> = selection
            .options
            .as_ref()
            .map(|selections| {
                selections
                    .0
                    .iter()
                    .map(|selection| selection.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        if let Some(selections) = &selection.options {
            for selected in &selections.0 {
                let Some(value) = selected.value.as_str() else {
                    continue;
                };
                let value = acp_option_value(value);
                if let Some(option) = option_for_selection(&options, &selected.id)
                    && option.current != value
                {
                    apply(&option.id, value)?;
                }
            }
        }
        // T3's `unsetOptionDefaults`: apply the flavor/host defaults for
        // options the selection left unset; an explicit selection wins.
        for (id, value) in &self.inner.config.unset_option_defaults {
            if explicit.iter().any(|selected| selected == id) {
                continue;
            }
            let value = acp_option_value(value);
            if let Some(option) = option_for_selection(&options, id)
                && option.current != value
            {
                apply(&option.id, value)?;
            }
        }
        let options = self.options();
        if let Some(policy) = policy
            && let Some(mode_option) = option_with_category(&options, "mode")
        {
            let mode = match self.inner.config.flavor {
                AcpFlavor::Antigravity => antigravity_session_mode(
                    antigravity_permission_mode(policy.runtime_mode),
                    &options,
                ),
                AcpFlavor::Generic => match policy.interaction_mode {
                    crate::contracts::ProviderInteractionMode::Plan => "plan".to_string(),
                    crate::contracts::ProviderInteractionMode::Default => String::new(),
                },
            };
            if !mode.is_empty() && mode_option.current != mode {
                futures::executor::block_on(self.inner.client.set_option(
                    &native,
                    &mode_option.id,
                    &mode,
                ))
                .map_err(|error| {
                    ProviderAdapterError::protocol(&self.inner.driver, error.to_string())
                })?;
            }
        }
        // `set_option` returns the agent's updated options; keep ours in step
        // so `config` and the next selection decision see them.
        let updated = self.inner.client.session_options(&native);
        if !updated.is_empty() {
            self.set_options(updated);
        }
        Ok(())
    }

    fn run_prompt(
        &self,
        native: String,
        blocks: Vec<Value>,
        provider_turn_id: ProviderTurnId,
        run_ordinal: u32,
    ) -> Result<(), ProviderAdapterError> {
        let client = self.inner.client.clone();
        let pump = self
            .inner
            .pump
            .lock()
            .expect("acp pump")
            .clone()
            .ok_or_else(|| {
                ProviderAdapterError::protocol(&self.inner.driver, "ACP pump stopped")
            })?;
        let in_flight = self.inner.prompts_in_flight.fetch_add(1, Ordering::Relaxed) + 1;
        thread::Builder::new()
            .name(format!("{}-acp-prompt", self.inner.driver))
            .spawn(move || {
                let parts = futures::executor::block_on(client.prompt_with_blocks(&native, blocks));
                let _ = pump.send(PumpMessage::Finished(Box::new(PromptFinished {
                    native_session: native,
                    result: parts.result,
                    usage: parts.usage,
                    delivery: parts.delivery,
                    limits: parts.limits,
                    provider_turn_id,
                    run_ordinal,
                    finishes_turn: in_flight == 1,
                })));
            })
            .map_err(ProviderAdapterError::failed)?;
        Ok(())
    }
}

pub struct AcpSessionRuntime {
    inner: Arc<Inner>,
}

impl ProviderSessionRuntime for AcpSessionRuntime {
    fn provider_session(&self) -> ProviderSession {
        self.inner
            .provider_session
            .lock()
            .expect("acp session")
            .clone()
    }

    fn driver(&self) -> &str {
        &self.inner.driver
    }

    fn ensure_thread(
        &self,
        input: EnsureThreadInput,
    ) -> Result<ProviderThread, ProviderAdapterError> {
        self.inner.set_now(&input.now);
        self.inner.set_policy(&input.runtime_policy);
        if !input.force_new_native_thread
            && let Some(existing) = input.existing_provider_thread.clone()
            && existing.native_thread_ref.is_some()
        {
            return self.resume_thread(
                &existing,
                Some(&input.model_selection),
                Some(&input.runtime_policy),
                &input.now,
            );
        }
        let cwd = input.runtime_policy.cwd.clone().unwrap_or_else(current_dir);
        let start =
            futures::executor::block_on(self.inner.client.new_session_with(Path::new(&cwd), &[]))
                .map_err(|error| {
                if is_sign_in_required(&error) {
                    ProviderAdapterError::failed(sign_in_message(&self.inner.config.flavor))
                } else {
                    ProviderAdapterError::EnsureThread {
                        driver: self.inner.driver.clone(),
                        thread_id: input.thread_id.to_string(),
                    }
                }
            })?;
        *self
            .inner
            .native_session
            .lock()
            .expect("acp native session") = Some(start.session_id.clone());
        self.set_options(start.options);
        self.apply_selection(&input.model_selection, Some(&input.runtime_policy))?;
        self.update_provider_session(
            ProviderSessionStatus::Ready,
            Some(input.model_selection.model.clone()),
        );
        let thread = self.new_provider_thread(
            input.thread_id,
            start.session_id,
            &input.now,
            ProviderThreadStatus::Active,
        );
        Ok(self.update_thread(thread, None))
    }

    fn resume_thread(
        &self,
        provider_thread: &ProviderThread,
        model_selection: Option<&ModelSelection>,
        runtime_policy: Option<&RuntimePolicy>,
        now: &str,
    ) -> Result<ProviderThread, ProviderAdapterError> {
        self.inner.set_now(now);
        if let Some(policy) = runtime_policy {
            self.inner.set_policy(policy);
        }
        let native = provider_thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.clone())
            .ok_or_else(|| {
                ProviderAdapterError::protocol(
                    &self.inner.driver,
                    "Provider thread is missing a native ACP session id",
                )
            })?;
        let cwd = runtime_policy
            .and_then(|policy| policy.cwd.clone())
            .unwrap_or_else(current_dir);
        let options =
            futures::executor::block_on(self.inner.client.open_session_with(&native, &cwd, &[]))
                .map_err(|_error| ProviderAdapterError::ResumeThread {
                    driver: self.inner.driver.clone(),
                    provider_thread_id: provider_thread.id.to_string(),
                })?;
        *self
            .inner
            .native_session
            .lock()
            .expect("acp native session") = Some(native);
        self.set_options(options);
        if let Some(selection) = model_selection {
            self.apply_selection(selection, runtime_policy)?;
        }
        let mut thread = provider_thread.clone();
        thread.status = ProviderThreadStatus::Active;
        thread.provider_session_id = Some(self.inner.provider_session_id.clone());
        thread.updated_at = now.to_string();
        Ok(self.update_thread(thread, None))
    }

    fn start_turn(&self, input: AdapterTurnInput) -> Result<ProviderTurn, ProviderAdapterError> {
        self.inner.set_now(&input.now);
        self.inner.set_policy(&input.runtime_policy);
        let native = self.native_session()?;
        self.apply_selection(&input.model_selection, Some(&input.runtime_policy))?;
        // Build the prompt before announcing a turn: an unreadable attachment
        // fails the start, as T3's adapter does.
        let blocks = self.inner.turn_prompt_blocks(
            &input.message.text,
            &input.message,
            &input.attachment_paths,
        )?;
        let (turn_id, events) = self.inner.client.begin_turn(&native);
        for event in events {
            self.inner.emit_agent(event);
        }
        let ordinal = input.provider_turn_ordinal;
        let provider_turn = ProviderTurn {
            id: ProviderTurnId::new(turn_id),
            provider_thread_id: input.provider_thread.id.clone(),
            node_id: input.root_node_id.clone(),
            run_attempt_id: Some(input.attempt_id.clone()),
            native_turn_ref: Some(ProviderRef {
                driver: ProviderDriverKind::new(&self.inner.driver),
                native_id: None,
                strength: NativeRefStrength::Weak,
                fingerprint: None,
                ordinal: Some(ordinal),
            }),
            native_thread_ref: input.provider_thread.native_thread_ref.clone(),
            ordinal,
            status: ProviderTurnStatus::Running,
            started_at: Some(input.now.clone()),
            completed_at: None,
            token_usage: None,
            turn_token_usage: None,
            model: Some(input.model_selection.model.clone()),
        };
        *self.inner.active.lock().expect("acp active") = Some(ActiveTurn {
            provider_turn: provider_turn.clone(),
            run_ordinal: input.run_ordinal,
            has_subagents: false,
            open_tools: 0,
        });
        self.inner.timed_out.store(false, Ordering::Relaxed);
        *self.inner.last_progress.lock().expect("acp progress") = Some(Instant::now());
        self.inner.emit(AdapterEvent::ProviderTurnUpdated {
            driver: self.inner.driver.clone(),
            thread_id: Some(input.thread_id.clone()),
            provider_turn: provider_turn.clone(),
        });
        let mut thread = input.provider_thread.clone();
        thread.status = ProviderThreadStatus::Active;
        thread.updated_at = input.now;
        self.update_thread(thread, None);
        self.update_provider_session(ProviderSessionStatus::Running, None);
        self.run_prompt(native, blocks, provider_turn.id.clone(), input.run_ordinal)?;
        Ok(provider_turn)
    }

    fn steer_turn(&self, input: SteerInput) -> Result<(), ProviderAdapterError> {
        let native = self.native_session()?;
        if !self.inner.client.info.steering {
            return Err(ProviderAdapterError::SteerRunUnsupported {
                driver: self.inner.driver.clone(),
                provider_thread_id: input.provider_thread.id.to_string(),
            });
        }
        let active = self.inner.active.lock().expect("acp active").clone();
        let Some(active) = active else {
            return Err(ProviderAdapterError::SteerRunUnsupported {
                driver: self.inner.driver.clone(),
                provider_thread_id: input.provider_thread.id.to_string(),
            });
        };
        let blocks = self.inner.turn_prompt_blocks(
            &input.message.text,
            &input.message,
            &input.attachment_paths,
        )?;
        self.run_prompt(native, blocks, active.provider_turn.id, active.run_ordinal)
            .map_err(|_| ProviderAdapterError::SteerRun {
                driver: self.inner.driver.clone(),
                provider_thread_id: input.provider_thread.id.to_string(),
                provider_turn_id: input.provider_turn_id.to_string(),
            })
    }

    fn interrupt_turn(
        &self,
        provider_thread: &ProviderThread,
        provider_turn_id: &ProviderTurnId,
    ) -> Result<(), ProviderAdapterError> {
        let native = self.native_session()?;
        self.inner
            .client
            .cancel(&native)
            .map_err(|_| ProviderAdapterError::Interrupt {
                driver: self.inner.driver.clone(),
                provider_thread_id: provider_thread.id.to_string(),
                provider_turn_id: provider_turn_id.to_string(),
            })
    }

    fn apply_model_selection(
        &self,
        _provider_thread: &ProviderThread,
        model_selection: &ModelSelection,
        runtime_policy: Option<&RuntimePolicy>,
    ) -> Result<(), ProviderAdapterError> {
        if let Some(policy) = runtime_policy {
            self.inner.set_policy(policy);
        }
        self.apply_selection(model_selection, runtime_policy)?;
        let session = self.provider_session();
        self.inner.emit(AdapterEvent::ProviderSessionUpdated {
            driver: self.inner.driver.clone(),
            provider_session: ProviderSession {
                model: Some(model_selection.model.clone()),
                updated_at: self.inner.now(),
                ..session
            },
        });
        Ok(())
    }

    fn apply_thread_title(
        &self,
        _provider_thread: &ProviderThread,
        _title: &str,
    ) -> Result<(), ProviderAdapterError> {
        // ACP has no rename request; titles arrive through `session_info_update`.
        Ok(())
    }

    fn respond_to_runtime_request(
        &self,
        input: RuntimeRequestResponse,
    ) -> Result<(), ProviderAdapterError> {
        let request: RequestId = Value::String(input.request_id.clone());
        if let Some(answers) = input.answers {
            // Antigravity native questions answer with one of their options.
            let answer = answers.values().find_map(|value| match value {
                Value::String(value) => Some(value.clone()),
                Value::Array(values) => values.first().and_then(Value::as_str).map(str::to_owned),
                _ => None,
            });
            if let Some(option) = answer {
                return self
                    .inner
                    .client
                    .answer_permission_option(request, &option)
                    .map_err(|_| ProviderAdapterError::RuntimeRequestResponse {
                        driver: self.inner.driver.clone(),
                        request_id: input.request_id,
                    });
            }
        }
        let decision = match input.decision.unwrap_or(ProviderApprovalDecision::Cancel) {
            ProviderApprovalDecision::Accept | ProviderApprovalDecision::AcceptAlways => {
                ApprovalDecision::Accept
            }
            ProviderApprovalDecision::AcceptForSession => ApprovalDecision::AcceptForSession,
            ProviderApprovalDecision::Decline => ApprovalDecision::Decline,
            ProviderApprovalDecision::Cancel => ApprovalDecision::Cancel,
        };
        // T3 remembers an approved execute for the turn, or for the session
        // when the agent actually offered `allow_always`. That memory is what
        // lets a later execute skip the ask through the normal policy path.
        let kind = self
            .inner
            .client
            .permission_params(&request)
            .and_then(|params| params["toolCall"]["kind"].as_str().map(str::to_owned));
        let can_grant_session = self
            .inner
            .client
            .permission_options(&request)
            .iter()
            .any(|(_, kind)| kind == "allow_always");
        if matches!(
            decision,
            ApprovalDecision::Accept | ApprovalDecision::AcceptForSession
        ) {
            let turn_key = self
                .inner
                .active
                .lock()
                .expect("acp active")
                .as_ref()
                .map(|active| active.provider_turn.id.to_string());
            self.inner.grants.lock().expect("acp grants").record(
                kind.as_deref(),
                decision == ApprovalDecision::AcceptForSession && can_grant_session,
                turn_key,
            );
        }
        self.inner
            .client
            .answer_permission(request, decision)
            .map_err(|_| ProviderAdapterError::RuntimeRequestResponse {
                driver: self.inner.driver.clone(),
                request_id: input.request_id,
            })
    }

    fn read_thread_snapshot(
        &self,
        provider_thread: &ProviderThread,
    ) -> Result<ThreadSnapshot, ProviderAdapterError> {
        Err(ProviderAdapterError::Unsupported(format!(
            "ACP cannot read thread {} without loading it",
            provider_thread.id
        )))
    }

    fn skills(&self, cwds: &[String]) -> Result<Vec<SkillGroup>, ProviderAdapterError> {
        match self.inner.config.flavor {
            AcpFlavor::Generic => Ok(cwds
                .iter()
                .map(|cwd| SkillGroup {
                    cwd: cwd.clone(),
                    ..SkillGroup::default()
                })
                .collect()),
            AcpFlavor::Antigravity => Ok(cwds
                .iter()
                .map(|cwd| {
                    super::acp_antigravity::discover_skills(
                        cwd,
                        self.inner
                            .config
                            .user_home
                            .as_deref()
                            .unwrap_or_else(|| Path::new("")),
                        self.inner.config.profile_directory.as_deref(),
                    )
                })
                .collect()),
        }
    }

    fn config(&self, cwd: Option<&str>) -> Result<ProviderConfig, ProviderAdapterError> {
        let options = self.options();
        Ok(ProviderConfig {
            config: json!({
                "cwd": cwd,
                "configOptions": options.iter().map(|option| json!({
                    "id": option.id,
                    "name": option.name,
                    "category": option.category,
                    "currentValue": option.current,
                    "options": option.choices.iter().map(|choice| json!({
                        "value": choice.value,
                        "name": choice.name,
                        "description": choice.description,
                        "group": choice.group,
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            }),
            origins: json!({}),
        })
    }

    fn unload_thread(&self, provider_thread: &ProviderThread) -> Result<(), ProviderAdapterError> {
        let mut thread = provider_thread.clone();
        thread.status = ProviderThreadStatus::Idle;
        thread.updated_at = self.inner.now();
        self.update_thread(thread, None);
        Ok(())
    }

    fn close(&self) {
        if !self.inner.closed.swap(true, Ordering::Relaxed) {
            if let Some(native) = self
                .inner
                .native_session
                .lock()
                .ok()
                .and_then(|s| s.clone())
            {
                let _ = futures::executor::block_on(self.inner.client.close_session(&native));
            }
            self.set_pump_stopped();
            self.inner.client.stop();
        }
    }
}

/// The pump owns the agent's event stream. It translates notifications,
/// answers policy-settled permissions, publishes the rest as runtime
/// requests, and closes a turn only after the stream goes quiet following the
/// prompt reply.
fn pump_loop(inner: Arc<Inner>, rx: mpsc::Receiver<PumpMessage>) {
    let mut pending: Option<PromptFinished> = None;
    let mut quiet_at: Option<Instant> = None;
    loop {
        let mut wait = if pending.is_some() {
            quiet_at
                .map(|quiet_at| {
                    PROMPT_SETTLE
                        .checked_sub(quiet_at.elapsed())
                        .unwrap_or_default()
                })
                .unwrap_or(PROMPT_SETTLE)
                .max(Duration::from_millis(1))
        } else {
            Duration::from_secs(3600)
        };
        if pending.is_none()
            && let Some(deadline) = inactivity_deadline(&inner)
        {
            wait = wait.min(
                deadline
                    .saturating_duration_since(Instant::now())
                    .max(Duration::from_millis(1)),
            );
        }
        match rx.recv_timeout(wait) {
            Ok(PumpMessage::Incoming(incoming)) => {
                quiet_at = pending.as_ref().map(|_| Instant::now());
                *inner.last_progress.lock().expect("acp progress") = Some(Instant::now());
                handle_incoming(&inner, incoming);
            }
            Ok(PumpMessage::Finished(finished)) => {
                pending = Some(*finished);
                quiet_at = Some(Instant::now());
            }
            Ok(PumpMessage::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if pending.is_some() {
                    let quiet = quiet_at
                        .map(|quiet_at| quiet_at.elapsed())
                        .unwrap_or(DEFERRED_FINALIZE_MAX);
                    let defer = inner.config.defers_finalize();
                    let open_tools = active_open_tools(&inner);
                    // T3's deferred finalize: a settled prompt whose command
                    // still runs waits for its updates, but not forever.
                    let debounce = if defer {
                        DEFERRED_FINALIZE_DEBOUNCE
                    } else {
                        PROMPT_SETTLE
                    };
                    if settled_ready(quiet, debounce, defer, open_tools) {
                        let finished = pending.take().expect("pending prompt");
                        finish_prompt(&inner, finished);
                        quiet_at = None;
                    }
                } else {
                    maybe_retire_inactive(&inner);
                }
            }
        }
    }
}

/// The instant the inactivity watchdog should wake, when a prompt is in
/// flight and a timeout is configured.
fn inactivity_deadline(inner: &Inner) -> Option<Instant> {
    inner.config.prompt_inactivity_timeout?;
    if inner.prompts_in_flight.load(Ordering::Relaxed) == 0 {
        return None;
    }
    inner
        .last_progress
        .lock()
        .expect("acp progress")
        .map(|last| last + inner.config.prompt_inactivity_timeout.expect("timeout"))
}

fn active_open_tools(inner: &Inner) -> usize {
    inner
        .active
        .lock()
        .expect("acp active")
        .as_ref()
        .map_or(0, |active| active.open_tools)
}

/// Whether a settled prompt may close. Without deferral the quiet period is
/// enough; with it, open tools extend it up to the maximum.
fn settled_ready(quiet: Duration, debounce: Duration, defer: bool, open_tools: usize) -> bool {
    if quiet < debounce {
        return false;
    }
    !defer || open_tools == 0 || quiet >= DEFERRED_FINALIZE_MAX
}

/// T3's inactivity rule: a prompt that has sent nothing for the timeout is
/// stopped unless a person is being asked, a tool is open, or the prompt has
/// already settled.
fn should_retire_inactive(
    elapsed: Duration,
    timeout: Duration,
    pending_permissions: usize,
    open_tools: usize,
) -> bool {
    elapsed >= timeout && pending_permissions == 0 && open_tools == 0
}

fn maybe_retire_inactive(inner: &Arc<Inner>) {
    let Some(timeout) = inner.config.prompt_inactivity_timeout else {
        return;
    };
    if inner.prompts_in_flight.load(Ordering::Relaxed) == 0 {
        return;
    }
    let last = match *inner.last_progress.lock().expect("acp progress") {
        Some(last) => last,
        None => return,
    };
    let pending_permissions = inner.client.pending_permission_count();
    let open_tools = active_open_tools(inner);
    if !should_retire_inactive(last.elapsed(), timeout, pending_permissions, open_tools) {
        return;
    }
    inner.timed_out.store(true, Ordering::Relaxed);
    *inner.last_progress.lock().expect("acp progress") = Some(Instant::now());
    let minutes = (timeout.as_secs().div_ceil(60)).max(1);
    let native = inner
        .native_session
        .lock()
        .expect("acp native session")
        .clone();
    inner.emit_agent(AgentEvent::Error {
        thread_id: native.clone(),
        message: format!(
            "{} sent nothing for {minutes} minute{}, so Atrium stopped the turn. Send a message to continue.",
            inner.driver,
            if minutes == 1 { "" } else { "s" }
        ),
        will_retry: false,
    });
    if let Some(native) = native {
        let _ = inner.client.cancel(&native);
    }
}

fn handle_incoming(inner: &Arc<Inner>, incoming: agent_rpc::Incoming) {
    let events = inner.client.translate(incoming);
    for event in events {
        if let AgentEvent::RateLimitsUpdated(limits) = &event {
            inner.merge_limits(limits.clone());
        }
        if inner.auto_answer(&event) {
            continue;
        }
        if let Some((_kind, request)) = inner.permission_request(&event) {
            let thread_id = event.thread_id().map(ThreadId::new);
            inner.emit(AdapterEvent::RuntimeRequestUpdated {
                driver: inner.driver.clone(),
                thread_id,
                runtime_request: request,
            });
        }
        inner.emit_agent(event);
    }
}

fn finish_prompt(inner: &Arc<Inner>, finished: PromptFinished) {
    if let Some(limits) = finished.limits.clone() {
        inner.merge_limits(limits.clone());
        inner.emit_agent(AgentEvent::RateLimitsUpdated(limits));
    }
    if let PromptDelivery::Uncertain { error } = &finished.delivery {
        inner.emit_agent(AgentEvent::Error {
            thread_id: Some(finished.native_session.clone()),
            message: error.to_string(),
            will_retry: false,
        });
    }
    inner.prompts_in_flight.fetch_sub(1, Ordering::Relaxed);
    if !finished.finishes_turn {
        // A steered turn's earlier prompt reply does not end the turn.
        return;
    }
    let timed_out = inner.timed_out.swap(false, Ordering::Relaxed);
    let status = if timed_out {
        // The watchdog already emitted the reason; the cancelled reply must
        // still settle the turn as failed, as T3 does.
        ProviderTurnStatus::Failed
    } else {
        match finished.result.as_deref() {
            Ok("end_turn") | Ok("") => ProviderTurnStatus::Completed,
            Ok("cancelled") => ProviderTurnStatus::Interrupted,
            Ok(_) => ProviderTurnStatus::Failed,
            Err(_) => ProviderTurnStatus::Failed,
        }
    };
    let status_wire = match status {
        ProviderTurnStatus::Completed => "completed",
        ProviderTurnStatus::Interrupted => "interrupted",
        ProviderTurnStatus::Failed => "failed",
        _ => "cancelled",
    };
    let now = inner.now();
    let active = inner.active.lock().expect("acp active").take();
    let (provider_turn, has_subagents) = match active {
        Some(active) => (Some(active.provider_turn), active.has_subagents),
        None => (None, false),
    };
    if let Some(mut turn) = provider_turn {
        turn.status = status;
        turn.completed_at = Some(now.clone());
        if inner.config.flavor.per_turn_usage() {
            let usage = acp_per_turn_prompt_usage(
                finished.usage.as_ref(),
                has_subagents,
                status,
                inner.config.flavor.gemini_usage(),
            );
            turn.turn_token_usage = serde_json::to_value(usage).ok();
        }
        let thread_id = turn
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.clone())
            .map(ThreadId::new);
        inner.emit(AdapterEvent::ProviderTurnUpdated {
            driver: inner.driver.clone(),
            thread_id,
            provider_turn: turn.clone(),
        });
        inner.emit(AdapterEvent::TurnTerminal {
            driver: inner.driver.clone(),
            provider_thread_id: turn.provider_thread_id.to_string(),
            provider_turn_id: turn.id.to_string(),
            run_ordinal: finished.run_ordinal,
            status: status_wire.to_string(),
        });
    }
    let events = inner.client.finish_prompt(
        &finished.native_session,
        finished.result.clone(),
        finished.usage.as_ref(),
    );
    for event in events {
        inner.emit_agent(event);
    }
    let thread = inner.thread_for_native(&finished.native_session);
    if let Some(mut thread) = thread {
        thread.status = ProviderThreadStatus::Idle;
        thread.updated_at = now.clone();
        inner.emit(AdapterEvent::ProviderThreadUpdated {
            driver: inner.driver.clone(),
            provider_thread: thread,
            pending_background_work: Some(false),
        });
    }
    {
        let mut session = inner.provider_session.lock().expect("acp session");
        session.status = ProviderSessionStatus::Ready;
        session.updated_at = now;
    }
    let session = inner.provider_session.lock().expect("acp session").clone();
    inner.emit(AdapterEvent::ProviderSessionUpdated {
        driver: inner.driver.clone(),
        provider_session: session,
    });
    let _ = finished.provider_turn_id;
}

/// The contract's attachments are ids, not paths; resolving them against the
/// attachment store belongs to the host boundary (a gap in this item).
/// A contract attachment after the host resolved its id to a path. The
/// flavor builders use the same record for the seam's already-resolved paths.
#[derive(Clone, Debug)]
pub(crate) struct ResolvedAttachment {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub image: bool,
    /// `pasted-text` files are described by path, not embedded.
    pub pasted_text: bool,
    pub path: Option<PathBuf>,
}

/// T3's `resolveAttachmentPath`: the store keeps `<id><extension>` keyed by
/// the attachment's own name/mime. An unresolved id stays `None` so the
/// flavor can report it the way T3 does.
pub(crate) fn resolve_contract_attachments(
    directory: Option<&Path>,
    attachments: &[ChatAttachment],
) -> Vec<ResolvedAttachment> {
    attachments
        .iter()
        .map(|attachment| {
            let (id, name, mime_type, image, pasted_text) = match attachment {
                ChatAttachment::Image(image) => (
                    image.id.clone(),
                    image.name.clone(),
                    image.mime_type.clone(),
                    true,
                    false,
                ),
                ChatAttachment::File(file) => (
                    file.id.clone(),
                    file.name.clone(),
                    file.mime_type.clone(),
                    false,
                    file.source
                        .as_ref()
                        .is_some_and(|source| source.kind == "pasted-text"),
                ),
                ChatAttachment::Unknown(unknown) => (
                    unknown.id.clone(),
                    unknown.name.clone(),
                    unknown.mime_type.clone(),
                    false,
                    false,
                ),
            };
            let path = directory.and_then(|directory| {
                let relative = attachment_relative_path(&id, &name, &mime_type, image)?;
                let resolved = directory.join(relative);
                resolved.is_file().then_some(resolved)
            });
            ResolvedAttachment {
                id,
                name,
                mime_type,
                image,
                pasted_text,
                path,
            }
        })
        .collect()
}

fn attachment_relative_path(id: &str, name: &str, mime_type: &str, image: bool) -> Option<String> {
    let extension = if image {
        image_extension(name, mime_type)
    } else {
        Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .filter(|extension| {
                !extension.is_empty() && extension.chars().all(|c| c.is_ascii_alphanumeric())
            })
    };
    let suffix = extension
        .map(|extension| format!(".{extension}"))
        .unwrap_or_default();
    Some(format!("{id}{suffix}"))
}

fn image_extension(name: &str, mime_type: &str) -> Option<String> {
    let named = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|extension| {
            matches!(
                extension.as_str(),
                "bmp" | "gif" | "jpeg" | "jpg" | "png" | "webp"
            )
        });
    named.or_else(|| {
        Some(
            match mime_type.to_ascii_lowercase().as_str() {
                "image/bmp" => "bmp",
                "image/gif" => "gif",
                "image/jpeg" => "jpg",
                "image/webp" => "webp",
                _ => "png",
            }
            .to_string(),
        )
    })
}

/// A seam-supplied path, with the type its extension implies.
pub(crate) fn resolved_from_path(path: &Path) -> ResolvedAttachment {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    let mime_type = mime_for_extension(
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default(),
    );
    ResolvedAttachment {
        id: name.clone(),
        name,
        mime_type: mime_type.to_string(),
        image: mime_type.starts_with("image/"),
        pasted_text: false,
        path: Some(path.to_path_buf()),
    }
}

pub(crate) fn mime_for_extension(extension: &str) -> &'static str {
    match extension.to_ascii_lowercase().as_str() {
        "bmp" => "image/bmp",
        "gif" => "image/gif",
        "jpeg" | "jpg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "m4a" => "audio/m4a",
        "mp3" => "audio/mpeg",
        "mp4" => "audio/mp4",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "webm" => "audio/webm",
        "json" => "application/json",
        "yml" | "yaml" => "application/yaml",
        "sh" => "application/x-sh",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "txt" | "md" | "log" => "text/plain",
        "rs" | "py" | "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "go" | "java" | "kt"
        | "swift" | "cc" | "cpp" | "hpp" | "cs" | "rb" | "php" | "sql" | "html" | "css"
        | "scss" | "less" | "svelte" | "vue" | "csv" | "tsv" | "toml" | "ini" | "conf" | "diff"
        | "patch" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// The generic ACP prompt: a path note for every attachment and image blocks
/// for supported images, as T3's `providerMessageTextWithAttachmentPaths`
/// plus `imageAttachmentBlocks` do. Unresolved images fail the turn; an
/// unresolved file is reported in the text and the turn continues.
fn generic_prompt_blocks(
    client: &AcpClient,
    text: &str,
    resolved: &[ResolvedAttachment],
) -> Result<Vec<Value>, String> {
    let mut text = text.to_string();
    let mut attachments = Vec::new();
    for attachment in resolved {
        let Some(path) = &attachment.path else {
            if attachment.image {
                return Err(format!("Invalid attachment id '{}'", attachment.id));
            }
            let note = format!(
                "[Attached file \"{}\" is unavailable: could not resolve a saved path for attachment '{}'.]",
                attachment.name, attachment.id
            );
            if text.is_empty() {
                text = note;
            } else {
                text.push_str("\n\n");
                text.push_str(&note);
            }
            continue;
        };
        attachments.push(PromptAttachment {
            path: path.clone(),
            name: attachment.name.clone(),
            mime_type: attachment.mime_type.clone(),
            image: attachment.image,
        });
    }
    client
        .prompt_blocks_for(&text, &attachments)
        .map_err(|error| error.to_string())
}

fn item_is_tool(item: &atrium_core::Item) -> bool {
    matches!(
        item,
        atrium_core::Item::Tool { .. }
            | atrium_core::Item::Command { .. }
            | atrium_core::Item::FileChange { .. }
    )
}

fn item_is_subagent(item: &atrium_core::Item) -> bool {
    match item {
        atrium_core::Item::Tool { name, .. } => {
            let lower = name.to_ascii_lowercase();
            lower.contains("invoke_subagent") || lower.contains("subagent")
        }
        _ => false,
    }
}

fn is_antigravity_question(params: &Value) -> bool {
    params["toolCall"]["toolCallId"]
        .as_str()
        .is_some_and(|id| id.starts_with("interaction_"))
}

fn request_id_text(request: &RequestId) -> String {
    match request {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn current_dir() -> String {
    std::env::current_dir()
        .unwrap_or_default()
        .display()
        .to_string()
}

fn is_sign_in_required(error: &dyn std::fmt::Display) -> bool {
    let text = error.to_string();
    text.contains("needs you to sign in") || text.contains("authenticated")
}

fn sign_in_message(flavor: &AcpFlavor) -> String {
    match flavor {
        AcpFlavor::Antigravity => "Sign in to Antigravity in Settings before you continue.".into(),
        AcpFlavor::Generic => "The agent needs you to sign in before you continue.".into(),
    }
}

/// Core capabilities for an ACP agent: the controls its handshake and session
/// options actually expose.
pub fn acp_core_capabilities(
    info: &acp_client::AgentInfo,
    options: &[ConfigOption],
) -> Capabilities {
    let mut capabilities = Capabilities::acp();
    capabilities.images = info.images;
    capabilities.steer = info.steering;
    capabilities.queue = true;
    capabilities.interrupt = true;
    capabilities.model_choice = can_switch_model_in_session(options);
    capabilities.effort_choice = options.iter().any(is_thought_level_option);
    capabilities.slash_commands = true;
    capabilities
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{
        ProviderOptionSelection, ProviderOptionSelections, ProviderOptionValue,
    };

    fn option(id: &str, category: &str, current: &str, choices: &[&str]) -> ConfigOption {
        ConfigOption {
            id: id.into(),
            name: id.into(),
            category: Some(category.into()),
            current: current.into(),
            choices: choices
                .iter()
                .map(|value| atrium_core::ConfigChoice {
                    value: (*value).into(),
                    name: (*value).into(),
                    description: None,
                    group: None,
                })
                .collect(),
        }
    }

    // Ported from packages/provider-acp/src/server/adapter.ts,
    // `AcpProviderCapabilitiesV2` (T3 Code v0.1.11, MIT).
    #[test]
    fn acp_capabilities_match_t3s_record() {
        let capabilities = acp_provider_capabilities();
        assert!(
            !capabilities
                .sessions
                .supports_multiple_provider_threads_per_session
        );
        assert!(!capabilities.sessions.supports_model_switch_in_session);
        assert!(capabilities.threads.can_create_empty_thread);
        assert!(!capabilities.threads.can_read_thread_snapshot);
        assert!(!capabilities.turns.exposes_native_turn_id);
        assert!(capabilities.turns.supports_interrupt);
        assert!(capabilities.turns.supports_steering_by_interrupt_restart);
        assert!(capabilities.turns.supports_queued_messages);
        assert_eq!(
            capabilities.turns.terminal_status_quality,
            TerminalStatusQuality::Strong
        );
        assert!(!capabilities.tools.supports_mcp_tools);
        assert!(!capabilities.approvals.approvals_have_native_request_ids);
        assert!(!capabilities.subagents.supports_subagents);
        assert!(
            !capabilities
                .checkpointing
                .provider_can_rollback_conversation
        );
        assert_eq!(
            capabilities.identity.native_thread_ids,
            NativeRefStrength::Strong
        );
        assert_eq!(
            capabilities.identity.native_turn_ids,
            NativeRefStrength::Weak
        );
        assert_eq!(
            capabilities.runtime_policy.enforcement,
            RuntimePolicyEnforcement::ClientBoundary
        );
        let value = serde_json::to_value(&capabilities).unwrap();
        assert_eq!(
            value["sessions"]["supportsMultipleProviderThreadsPerSession"],
            false
        );
        assert_eq!(value["turns"]["exposesNativeTurnId"], false);
        assert_eq!(value["runtimePolicy"]["enforcement"], "client-boundary");
    }

    // Ported from apps/server/src/orchestration-v2/Adapters/AntigravityAdapterV2.ts,
    // `AntigravityProviderCapabilitiesV2` (T3 Code v0.1.11, MIT).
    #[test]
    fn antigravity_adds_switching_mcp_and_subagents() {
        let capabilities = antigravity_provider_capabilities();
        assert!(capabilities.sessions.supports_model_switch_in_session);
        assert!(
            capabilities
                .sessions
                .supports_runtime_mode_switch_in_session
        );
        assert!(capabilities.tools.supports_mcp_tools);
        assert!(capabilities.subagents.supports_subagents);
        // Everything else stays ACP's record.
        assert!(!capabilities.threads.can_fork_thread);
        assert!(
            !capabilities
                .checkpointing
                .provider_can_rollback_conversation
        );
    }

    // Ported from packages/provider-acp/src/server/adapter.ts `acpPerTurnPromptUsage`
    // (T3 Code v0.1.11, MIT).
    #[test]
    fn per_turn_usage_clamps_subsets_and_marks_missing_samples() {
        let missing = acp_per_turn_prompt_usage(None, false, ProviderTurnStatus::Completed, false);
        assert_eq!(missing.usage_status, "unavailable");
        assert_eq!(missing.input_tokens, None);

        let usage = json!({
            "inputTokens": 100,
            "cachedReadTokens": 30,
            "cachedWriteTokens": 90,
            "outputTokens": 50,
            "thoughtTokens": 80
        });
        let plain =
            acp_per_turn_prompt_usage(Some(&usage), false, ProviderTurnStatus::Completed, false);
        assert_eq!(plain.input_tokens, Some(100));
        assert_eq!(plain.cached_input_tokens, Some(30));
        // Cache writes cannot exceed what reads leave of input.
        assert_eq!(plain.cache_creation_tokens, Some(70));
        assert_eq!(plain.reasoning_tokens, Some(50));

        let gemini =
            acp_per_turn_prompt_usage(Some(&usage), false, ProviderTurnStatus::Completed, true);
        assert_eq!(gemini.input_tokens, Some(130));
        assert_eq!(gemini.output_tokens, Some(130));
        assert_eq!(gemini.reasoning_tokens, Some(80));

        let failed =
            acp_per_turn_prompt_usage(Some(&usage), true, ProviderTurnStatus::Failed, false);
        assert_eq!(failed.usage_status, "partial");
        assert!(failed.has_subagents);
        assert_eq!(
            serde_json::to_value(&failed).unwrap()["usageScope"],
            "main_agent"
        );
    }

    // Ported from packages/provider-acp/src/server/adapter.ts
    // `canSwitchModelInSession` (T3 Code v0.1.11, MIT).
    #[test]
    fn a_session_advertises_model_switching_through_its_options() {
        assert!(!can_switch_model_in_session(&[]));
        let options = [option("model", "model", "a", &["a", "b"])];
        assert!(can_switch_model_in_session(&options));
        // A mode option alone is not a model switch.
        assert!(!can_switch_model_in_session(&[option(
            "mode",
            "mode",
            "plan",
            &["plan"]
        )]));
    }

    // Ported from apps/server/src/provider/acp/AntigravityAcpSupport.ts
    // (T3 Code v0.1.11, MIT).
    #[test]
    fn antigravity_session_modes_stay_default_when_the_cli_rejects_them() {
        assert_eq!(antigravity_permission_mode(RuntimeMode::FullAccess), "yolo");
        assert_eq!(
            antigravity_permission_mode(RuntimeMode::AutoAcceptEdits),
            "auto_edit"
        );
        assert_eq!(antigravity_session_mode("yolo", &[]), "default");
        assert_eq!(antigravity_session_mode("auto_edit", &[]), "default");
        // Auto only when the session advertises it.
        assert_eq!(antigravity_session_mode("auto", &[]), "default");
        let modes = [option("mode", "mode", "default", &["default", "auto"])];
        assert_eq!(antigravity_session_mode("auto", &modes), "auto");
        assert_eq!(
            antigravity_runtime_mode_for_session_mode("yolo"),
            Some(RuntimeMode::FullAccess)
        );
        assert_eq!(
            antigravity_runtime_mode_for_session_mode("default"),
            Some(RuntimeMode::ApprovalRequired)
        );
        assert_eq!(antigravity_runtime_mode_for_session_mode("plan"), None);
    }

    // Ported from packages/provider-acp/src/server/clientPolicy.ts
    // (T3 Code v0.1.11, MIT), reduced to the modes the app sets.
    #[test]
    fn permission_disposition_follows_the_runtime_policy() {
        let policy = |mode| RuntimePolicy {
            runtime_mode: mode,
            ..RuntimePolicy::default()
        };
        assert_eq!(
            permission_disposition(&policy(RuntimeMode::ApprovalRequired), Some("execute")),
            PermissionDisposition::Ask
        );
        assert_eq!(
            permission_disposition(&policy(RuntimeMode::FullAccess), Some("execute")),
            PermissionDisposition::Allow
        );
        assert_eq!(
            permission_disposition(&policy(RuntimeMode::AutoAcceptEdits), Some("edit")),
            PermissionDisposition::Allow
        );
        assert_eq!(
            permission_disposition(&policy(RuntimeMode::AutoAcceptEdits), Some("execute")),
            PermissionDisposition::Ask
        );
        // Reads never ask, even below full access.
        assert_eq!(
            permission_disposition(&policy(RuntimeMode::ApprovalRequired), Some("read")),
            PermissionDisposition::Allow
        );
    }

    #[test]
    fn flavor_detection_names_agy_and_the_antigravity_backend() {
        assert!(AcpFlavor::detect("claude", Path::new("/opt/agy")));
        assert!(AcpFlavor::detect(
            "antigravity",
            Path::new("C:\\Program Files\\agy.exe")
        ));
        assert!(AcpFlavor::detect(
            "antigravity",
            Path::new("/opt/agy-wrapper")
        ));
        assert!(!AcpFlavor::detect(
            "claude",
            Path::new("/opt/claude-agent-acp")
        ));
        assert!(!AcpFlavor::detect("deepseek", Path::new("/opt/dsh")));
    }

    fn policy(mode: RuntimeMode) -> RuntimePolicy {
        RuntimePolicy {
            runtime_mode: mode,
            cwd: Some("/tmp/work".into()),
            ..RuntimePolicy::default()
        }
    }

    // Ported from packages/provider-acp/src/server/clientPolicy.ts
    // `acpPermissionDisposition` (T3 Code v0.1.11, MIT).
    #[test]
    fn permission_dispositions_match_t3s_client_policy() {
        assert_eq!(
            permission_disposition(&policy(RuntimeMode::ApprovalRequired), Some("execute")),
            PermissionDisposition::Ask
        );
        assert_eq!(
            permission_disposition(&policy(RuntimeMode::FullAccess), Some("execute")),
            PermissionDisposition::Allow
        );
        assert_eq!(
            permission_disposition(&policy(RuntimeMode::AutoAcceptEdits), Some("edit")),
            PermissionDisposition::Allow
        );
        assert_eq!(
            permission_disposition(&policy(RuntimeMode::AutoAcceptEdits), Some("execute")),
            PermissionDisposition::Ask
        );
        assert_eq!(
            permission_disposition(&policy(RuntimeMode::ApprovalRequired), Some("read")),
            PermissionDisposition::Allow
        );

        let sandbox = |kind: &str, mode: RuntimeMode| RuntimePolicy {
            sandbox_policy: Some(json!({ "type": kind })),
            ..policy(mode)
        };
        assert_eq!(
            permission_disposition(
                &sandbox("readOnly", RuntimeMode::FullAccess),
                Some("execute")
            ),
            PermissionDisposition::Deny
        );
        assert_eq!(
            permission_disposition(&sandbox("readOnly", RuntimeMode::FullAccess), Some("read")),
            PermissionDisposition::Allow
        );
        assert_eq!(
            permission_disposition(
                &sandbox("dangerFullAccess", RuntimeMode::ApprovalRequired),
                Some("execute")
            ),
            PermissionDisposition::Ask
        );
        assert_eq!(
            permission_disposition(
                &sandbox("dangerFullAccess", RuntimeMode::FullAccess),
                Some("execute")
            ),
            PermissionDisposition::Allow
        );

        // `workspaceWrite` allows a mutation only inside the workspace.
        let dir = tempfile::tempdir().unwrap();
        let inside = dir.path().join("src/lib.rs");
        std::fs::create_dir_all(inside.parent().unwrap()).unwrap();
        std::fs::write(&inside, "x").unwrap();
        let outside = tempfile::tempdir().unwrap();
        let workspace = RuntimePolicy {
            sandbox_policy: Some(json!({
                "type": "workspaceWrite",
                "writableRoots": [dir.path().display().to_string()],
            })),
            cwd: Some(dir.path().display().to_string()),
            ..policy(RuntimeMode::FullAccess)
        };
        assert_eq!(
            permission_disposition_for(&workspace, Some("edit"), &[inside.display().to_string()]),
            PermissionDisposition::Allow
        );
        assert_eq!(
            permission_disposition_for(
                &workspace,
                Some("edit"),
                &[outside.path().join("escape.rs").display().to_string()]
            ),
            PermissionDisposition::Deny
        );
        // A missing file below the workspace still resolves through its parent.
        assert_eq!(
            permission_disposition_for(
                &workspace,
                Some("edit"),
                &[dir.path().join("new.rs").display().to_string()]
            ),
            PermissionDisposition::Allow
        );

        // `approvalPolicy: never` with approval-required and no sandbox denies.
        let never = RuntimePolicy {
            approval_policy: Some(json!("never")),
            ..policy(RuntimeMode::ApprovalRequired)
        };
        assert_eq!(
            permission_disposition(&never, Some("execute")),
            PermissionDisposition::Deny
        );
    }

    // Ported from packages/provider-acp/src/server/adapter.ts
    // `validateModelSelection` and `configureSession` (T3 Code v0.1.11, MIT).
    #[test]
    fn model_selections_are_refused_unless_known_or_custom() {
        let options = [option("model", "model", "a", &["a", "b"])];
        let selection = |model: &str| ModelSelection {
            instance_id: "acp".into(),
            model: model.into(),
            options: None,
        };
        assert!(validate_model_selection(&options, &[], &selection("a")).is_ok());
        assert!(validate_model_selection(&options, &[], &selection("default")).is_ok());
        assert!(validate_model_selection(&options, &[], &selection("auto")).is_ok());
        assert!(
            validate_model_selection(&options, &[], &selection(ANTIGRAVITY_DEFAULT_MODEL)).is_ok()
        );
        assert_eq!(
            validate_model_selection(&options, &[], &selection("c")).unwrap_err(),
            "The requested model is not available in this session."
        );
        assert!(validate_model_selection(&options, &["c".into()], &selection("c")).is_ok());
        // Antigravity's older family ids are accepted by prefix.
        assert!(validate_model_selection(&options, &[], &selection("b-high")).is_ok());
    }

    // Ported from packages/client-runtime/src/state/contextWindow.ts usage of
    // T3's `contextWindowTokens` (T3 Code v0.1.11, MIT).
    #[test]
    fn context_window_lookup_parses_k_and_m_values() {
        assert_eq!(parse_context_window("200k"), Some(200_000));
        assert_eq!(parse_context_window("1M"), Some(1_000_000));
        assert_eq!(parse_context_window("1024"), None);
        let mut window = option("contextWindow", "context_window", "200k", &["200k", "1m"]);
        window.category = None;
        let options = [window];
        let mut selection = ModelSelection {
            instance_id: "acp".into(),
            model: "a".into(),
            options: None,
        };
        assert_eq!(context_window_tokens(&options, &selection), Some(200_000));
        selection.options = Some(ProviderOptionSelections(vec![ProviderOptionSelection {
            id: "contextWindow".into(),
            value: ProviderOptionValue::String("1m".into()),
        }]));
        assert_eq!(context_window_tokens(&options, &selection), Some(1_000_000));
    }

    #[test]
    fn unset_option_values_round_trip_to_empty() {
        assert_eq!(acp_option_value("_t3/empty"), "");
        assert_eq!(acp_option_value("high"), "high");
    }

    #[test]
    fn contract_attachment_ids_resolve_to_host_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("thread-1_abc.png"), b"png").unwrap();
        let image = ChatAttachment::Image(crate::contracts::ChatImageAttachment {
            id: "thread-1_abc".into(),
            name: "shot.png".into(),
            mime_type: "image/png".into(),
            size_bytes: 3,
            source: None,
        });
        let missing_file = ChatAttachment::File(crate::contracts::ChatFileAttachment {
            id: "thread-1_missing".into(),
            name: "notes.txt".into(),
            mime_type: "text/plain".into(),
            size_bytes: 1,
            source: None,
        });
        let resolved = resolve_contract_attachments(Some(dir.path()), &[image, missing_file]);
        assert_eq!(resolved.len(), 2);
        assert!(
            resolved[0]
                .path
                .as_ref()
                .unwrap()
                .ends_with("thread-1_abc.png")
        );
        assert!(resolved[0].image);
        assert_eq!(resolved[1].path, None);
        assert!(!resolved[1].image);
    }

    #[test]
    fn settled_and_inactive_decisions_follow_the_deferral() {
        assert!(!settled_ready(
            Duration::from_millis(10),
            PROMPT_SETTLE,
            false,
            0
        ));
        assert!(settled_ready(PROMPT_SETTLE, PROMPT_SETTLE, false, 0));
        // Open tools extend a settled turn, but not past the maximum.
        assert!(!settled_ready(
            DEFERRED_FINALIZE_DEBOUNCE,
            DEFERRED_FINALIZE_DEBOUNCE,
            true,
            1
        ));
        assert!(settled_ready(
            DEFERRED_FINALIZE_MAX,
            DEFERRED_FINALIZE_DEBOUNCE,
            true,
            1
        ));

        let timeout = Duration::from_secs(60);
        assert!(should_retire_inactive(timeout, timeout, 0, 0));
        assert!(!should_retire_inactive(timeout, timeout, 1, 0));
        assert!(!should_retire_inactive(timeout, timeout, 0, 1));
        assert!(!should_retire_inactive(
            Duration::from_secs(59),
            timeout,
            0,
            0
        ));
    }

    #[test]
    fn grants_remember_execute_for_the_turn_or_session() {
        let mut grants = PolicyGrants::default();
        assert!(!grants.allows_execute(Some("turn-1")));
        grants.record(Some("edit"), false, Some("turn-1".into()));
        assert!(!grants.allows_execute(Some("turn-1")));
        grants.record(Some("execute"), false, Some("turn-1".into()));
        assert!(grants.allows_execute(Some("turn-1")));
        assert!(!grants.allows_execute(Some("turn-2")));
        grants.record(Some("execute"), true, None);
        assert!(grants.allows_execute(Some("turn-2")));
    }
}
