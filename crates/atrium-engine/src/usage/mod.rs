//! Usage windows, limit classification, recovery and context budgets.
//!
//! Ports item 26 of T3 Code v0.1.11: the per-provider usage readers, the
//! usage-limit classification both adapters share, the recovery worker that
//! continues a thread after its reset, and the context-window / idle
//! auto-compaction decisions. The module is GPUI-free and every decision
//! takes `now` as an argument so a scheduler can drive it from a test.
//!
//! Persistence for the recovery worker lives in
//! [`recovery::SqliteRecoveryJournal`]; it owns its table and does not change
//! the orchestration store's schema.

pub mod classify;
pub mod claude;
pub mod codex;
pub mod context;
pub mod limits;
pub mod recovery;

pub use classify::{
    ClaudeApiRetry, ClaudeFailureInput, CodexErrorInfo, CodexManagedFailure,
    classify_codex_managed_error, claude_failure_from_api_retry, claude_failure_from_result,
    codex_error_needs_evidence, codex_terminal_failure_class, codex_usage_limit_error,
    with_codex_managed_failure,
};
pub use claude::{
    ClaudeRateLimitEvent, ClaudeScopedLimitNames, ClaudeUsageProbe,
    claude_rate_limit_event_to_update, claude_usage_response_to_limits, window_limits_model,
};
pub use codex::{
    CodexIndividualLimit, CodexRateLimitPatch, CodexRateLimitSnapshot, CodexRateLimitWindow,
    CodexResetCredit, CodexResetCreditsSummary, CodexUsageReadError, codex_message_reset_at,
    codex_rate_limits_failure_message, codex_rate_limits_to_limits, codex_rate_limits_to_update,
    codex_time_zone, codex_usage_limit_reset_at, merge_codex_rate_limits,
};
pub use context::{
    CLAUDE_RESUME_COMPACTION_COMPACT_ANSWER, CLAUDE_RESUME_COMPACTION_FALLBACK_MS,
    CLAUDE_RESUME_COMPACTION_NEVER_ANSWER, COMPACTION_SUGGESTION_MIN_TOKENS,
    CompactBeforeSendInput, CompactProjection, CompactionItem, ContextEntry, ContextItem,
    ContextProvider, ContextTone, ContextWindowSnapshot, IDLE_COMPACTION_TEXT,
    IdleCompactionContextTransfer, IdleCompactionMessage, IdleCompactionMode,
    IdleCompactionProviderThread, IdleCompactionProviderTurn, IdleCompactionRecords,
    IdleCompactionRun, IdleCompactionSettings, IdleCompactionThread, IdleCompactionWindow,
    ProviderContextThread, ProviderContextUsage, TokenUsage, TurnUsageRef, VisibleItem,
    compact_before_send_tokens, context_tone, derive_latest_context_window_snapshot,
    format_claude_resume_compaction_question, global_idle_compaction,
    has_dismissed_resume_compaction, idle_compaction_window, is_claude_resume_compaction_question,
    latest_provider_turn_token_usage, resolve_idle_compaction, should_offer_resume_compaction,
};
pub use limits::{
    ExternalUsage, ProviderResetCreditOption, ProviderResetCredits, ProviderUsageLimits,
    UsageLimitUnavailableReason, UsageLimitsUpdate, UsageWindow, UsageWindowKind,
    apply_usage_limits_update, clamp_percent, make_unavailable_usage_limits, make_usage_limits,
    resolve_usage_limits_after_probe,
};
pub use recovery::{
    LimitRecovery, LimitRecoveryCandidate, LimitRecoveryCommand, LimitRecoverySettings,
    RecoveryError, RecoveryThreadStatus, SqliteRecoveryJournal, StoredRecovery,
    limit_recovery_command, sweep,
};
