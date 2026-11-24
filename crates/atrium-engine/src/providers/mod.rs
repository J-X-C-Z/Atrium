//! Provider adapters: Codex app-server and ACP first.
//!
//! Ports `packages/provider-core` (the adapter interface, its capability
//! record, selection transitions and the adapter registry) and the Codex
//! adapter from `apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts`
//! over Atrium's existing `codex-client` transport. ACP has its own item.

pub mod adapter;
pub mod claude;
pub mod claude_translate;
pub mod codex;
pub mod codex_elicitation;
pub mod codex_turn;
pub mod registry;

pub use adapter::{
    AdapterEvent, AdapterEventSink, AdapterTurnInput, CODEX_DRIVER_KIND, EnsureThreadInput,
    OpenSessionInput, ProviderAdapter, ProviderAdapterError, ProviderConfig,
    ProviderSessionRuntime, RuntimePolicy, RuntimeRequestResponse, SelectionTransitionPlan,
    SkillGroup, SteerInput, ThreadSnapshot, TurnMessage, acp_selection_transition,
    turn_scoped_selection_transition,
};
pub use claude::{
    ClaudeAdapter, ClaudeAdapterConfig, claude_provider_capabilities,
    core_capabilities as claude_core_capabilities,
};
pub use claude_translate::{CLAUDE_DRIVER_KIND, ClaudeTranslator};
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
