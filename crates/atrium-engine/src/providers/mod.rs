//! Provider adapters: Codex app-server and ACP first.
//!
//! Ports `packages/provider-core` (the adapter interface, its capability
//! record, selection transitions and the adapter registry), the Codex
//! adapter from `apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts`
//! over Atrium's existing `codex-client` transport, and the ACP adapter from
//! `packages/provider-acp` plus Antigravity's flavor over `acp-client`.

pub mod acp;
pub mod acp_antigravity;
pub mod adapter;
pub mod codex;
pub mod codex_elicitation;
pub mod codex_turn;
pub mod registry;

pub use acp::{
    ACP_DRIVER_KIND, ACP_SESSION_MODE_OPTION_ID, ANTIGRAVITY_DEFAULT_MODEL,
    ANTIGRAVITY_DRIVER_KIND, AcpAdapter, AcpAdapterConfig, AcpFlavor, AcpSessionRuntime,
    PermissionDisposition, TurnTokenUsage, acp_core_capabilities, acp_per_turn_prompt_usage,
    acp_provider_capabilities, antigravity_permission_mode, antigravity_provider_capabilities,
    antigravity_runtime_mode_for_session_mode, antigravity_session_mode,
    can_switch_model_in_session, context_window_tokens, is_thought_level_option,
    legacy_antigravity_family, option_with_category, parse_context_window, permission_disposition,
    permission_disposition_for, provider_capabilities, validate_model_selection,
};
pub use acp_antigravity::{
    ANTIGRAVITY_AUTH_BROWSER_MARKER, ANTIGRAVITY_AUTH_STDOUT_PREFIX, ANTIGRAVITY_AUTH_TIMEOUT_MS,
    ANTIGRAVITY_CODE_REDIRECT_URI, ANTIGRAVITY_SIGN_IN_REQUIRED_MESSAGE, AntigravityAuthConfig,
    AntigravityAuthMachine, AntigravityAuthMethod, AntigravityAuthPhase, AntigravityAuthState,
    AntigravityAuthorization, AntigravityAuthorizationKind, AntigravityCallback,
    AntigravityProfile, AntigravityUsageReader, PromptCapabilities, SubagentLaunch, UsageRecord,
    UsageScan, UsageTotals, antigravity_environment, antigravity_login_response,
    antigravity_profile_settings, auth_config_issue, auth_label, auth_uses_browser,
    describe_subagent_launches, discover_skills, is_concrete_model, model_name,
    parse_antigravity_authorization_url, prepare_antigravity_profile, rate_model,
    subagent_launches, user_skill_directories,
};
pub use adapter::{
    AdapterEvent, AdapterEventSink, AdapterTurnInput, CODEX_DRIVER_KIND, EnsureThreadInput,
    OpenSessionInput, ProviderAdapter, ProviderAdapterError, ProviderConfig,
    ProviderSessionRuntime, RuntimePolicy, RuntimeRequestResponse, SelectionTransitionPlan,
    SkillGroup, SteerInput, ThreadSnapshot, TurnMessage, acp_selection_transition,
    turn_scoped_selection_transition,
};
pub use codex::{
    CodexAdapter, CodexAdapterConfig, MIN_CODEX_VERSION, codex_provider_capabilities,
    core_capabilities,
};
pub use codex_elicitation::{
    ElicitationDescription, describe_mcp_elicitation, to_mcp_elicitation_response,
};
pub use codex_turn::{
    TurnStartInput, build_thread_settings_update, build_turn_start_params,
    runtime_mode_turn_defaults, service_tier_value,
};
pub use registry::{
    AdapterMetadata, ProviderAdapterRegistry, ProviderAdapterRegistryError, RegisteredAdapter,
};
