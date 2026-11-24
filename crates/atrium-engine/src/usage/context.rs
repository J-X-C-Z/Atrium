// Ported from packages/client-runtime/src/state/contextWindow.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/orchestration-v2/IdleCompaction.ts (T3 Code v0.1.11, MIT).
// Ported from packages/shared/src/claudeCompaction.ts (T3 Code v0.1.11, MIT).
// Ported from apps/web/src/components/chat/ThreadDetailsUsage.tsx (T3 Code v0.1.11, MIT).
//! What the context window reports and when a Claude thread compacts.
//!
//! Two decisions share T3's thresholds: whether an idle thread should offer or
//! run a compaction before its prompt cache expires, and whether a send into an
//! idle thread compacts first. Both stop below
//! [`COMPACTION_SUGGESTION_MIN_TOKENS`] (200K) and wait until the thread has
//! been idle long enough that the prompt cache is about to expire.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::contracts::thread::SettledOverride;

use super::limits::parse_ms;

/// T3's `COMPACTION_SUGGESTION_MIN_TOKENS`: the stale-thread Compact chip and
/// idle compaction both stop below this size.
pub const COMPACTION_SUGGESTION_MIN_TOKENS: f64 = 200_000.0;
/// T3's `LEAD_MS`: start this long before the prompt cache expires.
pub const IDLE_COMPACTION_LEAD_MS: i64 = 2 * 60_000;
/// T3's fallback TTL: Claude's 1h cache plus slack.
pub const CLAUDE_RESUME_COMPACTION_FALLBACK_MS: i64 = 70 * 60_000;
/// T3's `IDLE_COMPACTION_TEXT`.
pub const IDLE_COMPACTION_TEXT: &str = "/compact";
/// T3's resume-dialog answers.
pub const CLAUDE_RESUME_COMPACTION_NEVER_ANSWER: &str = "Don't ask again";
pub const CLAUDE_RESUME_COMPACTION_COMPACT_ANSWER: &str = "Compact and continue";

/// The context bar's warning levels from `ThreadDetailsUsage`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextTone {
    Warning,
    Error,
}

/// A context bar is a warning at 80% used and an error at 90%.
pub fn context_tone(used_percentage: Option<f64>) -> Option<ContextTone> {
    let used = used_percentage?;
    if used >= 90.0 {
        Some(ContextTone::Error)
    } else if used >= 80.0 {
        Some(ContextTone::Warning)
    } else {
        None
    }
}

/// `TurnTokenUsage`; fields this module does not read stay optional.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub used_tokens: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_threshold: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_ttl_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Value>,
    pub updated_at: String,
}

/// `ThreadTokenUsageSnapshot`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderContextUsage {
    pub used_tokens: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_processed_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_input_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_cached_input_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_output_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reasoning_output_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_uses: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacts_automatically: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_threshold: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Value>,
}

/// The active provider thread's own usage and last update.
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderContextThread {
    pub id: String,
    pub context_usage: Option<ProviderContextUsage>,
    pub updated_at: String,
}

/// What the context window reports to the UI.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextWindowSnapshot {
    pub used_tokens: f64,
    pub total_processed_tokens: Option<f64>,
    pub max_tokens: Option<f64>,
    pub remaining_tokens: Option<f64>,
    pub used_percentage: Option<f64>,
    pub remaining_percentage: Option<f64>,
    pub input_tokens: Option<f64>,
    pub cached_input_tokens: Option<f64>,
    pub output_tokens: Option<f64>,
    pub reasoning_output_tokens: Option<f64>,
    pub last_used_tokens: Option<f64>,
    pub last_input_tokens: Option<f64>,
    pub last_cached_input_tokens: Option<f64>,
    pub last_output_tokens: Option<f64>,
    pub last_reasoning_output_tokens: Option<f64>,
    pub tool_uses: Option<f64>,
    pub duration_ms: Option<f64>,
    pub compacts_automatically: Option<bool>,
    pub auto_compact_threshold: Option<f64>,
    pub cost: Option<Value>,
    pub updated_at: String,
}

/// A completed compaction's two counts.
#[derive(Clone, Debug, PartialEq)]
pub struct CompactionItem {
    pub provider_thread_id: Option<String>,
    pub status: String,
    pub before_token_count: Option<f64>,
    pub after_token_count: Option<f64>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ContextItem {
    Compaction(CompactionItem),
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContextEntry {
    pub item: ContextItem,
}

/// One provider turn's live usage.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnUsageRef {
    pub provider_thread_id: Option<String>,
    pub token_usage: Option<TokenUsage>,
}

fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

fn percentage(used: f64, max: Option<f64>) -> Option<f64> {
    let max = max.filter(|max| *max > 0.0)?;
    Some((used / max * 100.0).clamp(0.0, 100.0))
}

fn remaining(used: f64, max: Option<f64>) -> Option<f64> {
    max.map(|max| (max - used).max(0.0).round())
}

/// T3's `latestProviderTurnTokenUsage`.
///
/// The newest report by timestamp among the active provider thread's turns,
/// since a provider switch leaves the previous provider's report behind. A
/// fork that has not reported its own usage keeps its source's report.
pub fn latest_provider_turn_token_usage(
    turns: &[TurnUsageRef],
    provider_thread: Option<&ProviderContextThread>,
    fork_source_usage: Option<&TokenUsage>,
) -> Option<TokenUsage> {
    let mut latest: Option<TokenUsage> = None;
    for turn in turns {
        let Some(token_usage) = turn.token_usage.as_ref() else {
            continue;
        };
        if let Some(provider_thread) = provider_thread
            && turn.provider_thread_id.as_deref() != Some(provider_thread.id.as_str())
        {
            continue;
        }
        // Ties go to the later entry, since live upserts append.
        let later_or_equal = match &latest {
            None => true,
            Some(latest) => parse_ms(&token_usage.updated_at)
                .zip(parse_ms(&latest.updated_at))
                .is_some_and(|(next, previous)| next >= previous),
        };
        if later_or_equal {
            latest = Some(token_usage.clone());
        }
    }
    if latest.is_some() || provider_thread.is_some_and(|thread| thread.context_usage.is_some()) {
        return latest;
    }
    fork_source_usage.cloned()
}

fn is_retired_provider_thread_item(
    item: &CompactionItem,
    provider_thread: Option<&ProviderContextThread>,
) -> bool {
    provider_thread.is_some_and(|provider_thread| {
        item.provider_thread_id
            .as_deref()
            .is_some_and(|id| id != provider_thread.id)
    })
}

fn compaction_snapshot(
    item: &CompactionItem,
    max_tokens: Option<f64>,
    updated_at: String,
) -> Option<ContextWindowSnapshot> {
    let used_tokens = finite(item.after_token_count?)?;
    if used_tokens < 0.0 {
        return None;
    }
    let used_percentage = percentage(used_tokens, max_tokens);
    Some(ContextWindowSnapshot {
        used_tokens,
        total_processed_tokens: item.before_token_count.and_then(finite),
        max_tokens,
        remaining_tokens: remaining(used_tokens, max_tokens),
        used_percentage,
        remaining_percentage: used_percentage.map(|used| (100.0 - used).max(0.0)),
        input_tokens: None,
        cached_input_tokens: None,
        output_tokens: None,
        reasoning_output_tokens: None,
        last_used_tokens: None,
        last_input_tokens: None,
        last_cached_input_tokens: None,
        last_output_tokens: None,
        last_reasoning_output_tokens: None,
        tool_uses: None,
        duration_ms: None,
        compacts_automatically: Some(true),
        auto_compact_threshold: None,
        cost: None,
        updated_at,
    })
}

/// T3's `deriveLatestContextWindowSnapshot`.
pub fn derive_latest_context_window_snapshot(
    entries: &[ContextEntry],
    live_usage: Option<&TokenUsage>,
    provider_thread: Option<&ProviderContextThread>,
) -> Option<ContextWindowSnapshot> {
    let latest_completed_compaction = entries.iter().rev().find_map(|entry| match &entry.item {
        ContextItem::Compaction(item)
            if item.status == "completed"
                && !is_retired_provider_thread_item(item, provider_thread) =>
        {
            Some(item)
        }
        _ => None,
    });
    let usage_updated_at = live_usage
        .map(|usage| usage.updated_at.clone())
        .or_else(|| {
            provider_thread
                .filter(|thread| thread.context_usage.is_some())
                .map(|thread| thread.updated_at.clone())
        });
    if let Some(compaction) = latest_completed_compaction {
        let compacted_at = compaction
            .completed_at
            .clone()
            .unwrap_or_else(|| compaction.updated_at.clone());
        let before_or_at = usage_updated_at
            .as_deref()
            .and_then(parse_ms)
            .zip(parse_ms(&compacted_at))
            .is_some_and(|(usage, compacted)| compacted > usage);
        if usage_updated_at.is_none() || before_or_at {
            let max_tokens = live_usage.and_then(|usage| usage.max_tokens).or_else(|| {
                provider_thread
                    .and_then(|thread| thread.context_usage.as_ref())
                    .and_then(|usage| usage.max_tokens)
            });
            return compaction_snapshot(compaction, max_tokens, compacted_at);
        }
    }

    if let Some(live_usage) = live_usage {
        let used_tokens = live_usage.used_tokens.max(0.0);
        let max_tokens = live_usage.max_tokens;
        let used_percentage = percentage(used_tokens, max_tokens);
        return Some(ContextWindowSnapshot {
            used_tokens,
            total_processed_tokens: None,
            max_tokens,
            remaining_tokens: remaining(used_tokens, max_tokens),
            used_percentage,
            remaining_percentage: used_percentage.map(|used| (100.0 - used).max(0.0)),
            input_tokens: live_usage.input_tokens,
            cached_input_tokens: live_usage.cached_input_tokens,
            output_tokens: live_usage.output_tokens,
            reasoning_output_tokens: live_usage.reasoning_output_tokens,
            last_used_tokens: None,
            last_input_tokens: None,
            last_cached_input_tokens: None,
            last_output_tokens: None,
            last_reasoning_output_tokens: None,
            tool_uses: None,
            duration_ms: None,
            compacts_automatically: Some(true),
            auto_compact_threshold: live_usage.auto_compact_threshold,
            cost: None,
            updated_at: live_usage.updated_at.clone(),
        });
    }

    if let Some(provider_thread) = provider_thread
        && let Some(usage) = provider_thread.context_usage.as_ref()
    {
        let max_tokens = finite(usage.max_tokens.unwrap_or(f64::NAN));
        let used_tokens = usage.used_tokens;
        let used_percentage = percentage(used_tokens, max_tokens);
        return Some(ContextWindowSnapshot {
            used_tokens,
            total_processed_tokens: usage.total_processed_tokens.and_then(finite),
            max_tokens,
            remaining_tokens: remaining(used_tokens, max_tokens),
            used_percentage,
            remaining_percentage: used_percentage.map(|used| (100.0 - used).max(0.0)),
            input_tokens: usage.input_tokens.and_then(finite),
            cached_input_tokens: usage.cached_input_tokens.and_then(finite),
            output_tokens: usage.output_tokens.and_then(finite),
            reasoning_output_tokens: usage.reasoning_output_tokens.and_then(finite),
            last_used_tokens: usage.last_used_tokens.and_then(finite),
            last_input_tokens: usage.last_input_tokens.and_then(finite),
            last_cached_input_tokens: usage.last_cached_input_tokens.and_then(finite),
            last_output_tokens: usage.last_output_tokens.and_then(finite),
            last_reasoning_output_tokens: usage.last_reasoning_output_tokens.and_then(finite),
            tool_uses: usage.tool_uses.and_then(finite),
            duration_ms: usage.duration_ms.and_then(finite),
            compacts_automatically: usage.compacts_automatically,
            auto_compact_threshold: usage.auto_compact_threshold,
            cost: usage.cost.clone(),
            updated_at: provider_thread.updated_at.clone(),
        });
    }

    for entry in entries.iter().rev() {
        let ContextItem::Compaction(item) = &entry.item else {
            continue;
        };
        if is_retired_provider_thread_item(item, provider_thread) {
            continue;
        }
        let updated_at = item
            .started_at
            .clone()
            .unwrap_or_else(|| item.updated_at.clone());
        if let Some(snapshot) = compaction_snapshot(item, None, updated_at) {
            return Some(snapshot);
        }
    }
    None
}

fn retired_compaction_question(question: &str) -> bool {
    if !question.starts_with("This session is ") {
        return false;
    }
    let Some(rest) = question.strip_prefix("This session is ") else {
        return false;
    };
    let Some((age, rest)) = rest.split_once(" old and uses ") else {
        return false;
    };
    let valid_age = if let Some((hours, minutes)) = age.split_once("h ") {
        hours.parse::<u32>().is_ok()
            && minutes
                .strip_suffix('m')
                .is_some_and(|minutes| minutes.parse::<u32>().is_ok())
    } else {
        age.strip_suffix('m')
            .is_some_and(|minutes| minutes.parse::<u32>().is_ok())
    };
    if !valid_age {
        return false;
    }
    let Some((tokens, tail)) = rest.split_once(" tokens. Compact it before continuing?") else {
        return false;
    };
    if !tail.is_empty() || tokens.is_empty() {
        return false;
    }
    let mut groups = tokens.split(',');
    let Some(first) = groups.next() else {
        return false;
    };
    if first.is_empty() || first.len() > 3 || !first.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    groups.all(|group| group.len() == 3 && group.bytes().all(|byte| byte.is_ascii_digit()))
}

/// T3's `isClaudeResumeCompactionQuestion`.
pub fn is_claude_resume_compaction_question(question: &str) -> bool {
    retired_compaction_question(question.trim())
}

fn comma_tokens(tokens: u64) -> String {
    let digits = tokens.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// T3's `formatClaudeResumeCompactionQuestion`.
pub fn format_claude_resume_compaction_question(age_minutes: u64, estimated_tokens: u64) -> String {
    let age = if age_minutes >= 60 {
        format!("{}h {}m", age_minutes / 60, age_minutes % 60)
    } else {
        format!("{age_minutes}m")
    };
    format!(
        "This session is {age} old and uses {} tokens. Compact it before continuing?",
        comma_tokens(estimated_tokens)
    )
}

/// T3's `hasDismissedResumeCompaction`.
pub fn has_dismissed_resume_compaction(
    activities: &[(String, Option<BTreeMap<String, Value>>)],
) -> bool {
    activities.iter().any(|(kind, answers)| {
        if kind != "user-input.resolved" {
            return false;
        }
        let Some(answers) = answers else {
            return false;
        };
        answers.iter().any(|(question, answer)| {
            is_claude_resume_compaction_question(question)
                && match answer {
                    Value::String(value) => value == CLAUDE_RESUME_COMPACTION_NEVER_ANSWER,
                    Value::Array(values) => values
                        .iter()
                        .any(|value| value.as_str() == Some(CLAUDE_RESUME_COMPACTION_NEVER_ANSWER)),
                    _ => false,
                }
        })
    })
}

/// T3's `shouldOfferResumeCompaction`.
pub fn should_offer_resume_compaction(
    provider: Option<&str>,
    used_tokens: Option<f64>,
    updated_at: Option<&str>,
    prompt_cache_ttl_ms: Option<i64>,
    now: &str,
) -> bool {
    if provider != Some("claudeAgent")
        || used_tokens.unwrap_or(0.0) < COMPACTION_SUGGESTION_MIN_TOKENS
    {
        return false;
    }
    let Some(updated_ms) = updated_at.and_then(parse_ms) else {
        return false;
    };
    let Some(now_ms) = parse_ms(now) else {
        return false;
    };
    now_ms - updated_ms >= prompt_cache_ttl_ms.unwrap_or(CLAUDE_RESUME_COMPACTION_FALLBACK_MS)
}

/// `IdleCompactionMode` from `settings.ts`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdleCompactionMode {
    Off,
    OnReturn,
    BeforeCacheExpires,
}

/// The parts of `ServerSettings` the two compaction decisions read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IdleCompactionSettings {
    pub idle_compaction: Option<IdleCompactionMode>,
    /// The legacy boolean still shipped to older clients.
    pub compact_idle_threads: Option<bool>,
    pub instance_overrides: BTreeMap<String, IdleCompactionMode>,
}

/// T3's `globalIdleCompaction`.
pub fn global_idle_compaction(settings: &IdleCompactionSettings) -> IdleCompactionMode {
    settings
        .idle_compaction
        .unwrap_or(if settings.compact_idle_threads.unwrap_or(false) {
            IdleCompactionMode::BeforeCacheExpires
        } else {
            IdleCompactionMode::OnReturn
        })
}

/// T3's `resolveIdleCompaction`: an instance's own mode wins, else the global.
pub fn resolve_idle_compaction(
    settings: &IdleCompactionSettings,
    instance_id: &str,
) -> IdleCompactionMode {
    settings
        .instance_overrides
        .get(instance_id)
        .copied()
        .unwrap_or_else(|| global_idle_compaction(settings))
}

/// The records `idleCompactionWindow` reads.
#[derive(Clone, Debug, PartialEq)]
pub struct IdleCompactionThread {
    pub id: String,
    pub provider_instance_id: String,
    pub active_provider_thread_id: Option<String>,
    pub archived_at: Option<String>,
    pub deleted_at: Option<String>,
    pub settled_at: Option<String>,
    pub settled_override: Option<SettledOverride>,
    pub snoozed_until: Option<String>,
    /// `isListedThreadLineage`: not a subagent and not a side chat.
    pub lineage_listed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IdleCompactionRun {
    pub id: String,
    pub ordinal: u32,
    pub status: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub user_message_id: String,
    pub provider_thread_id: Option<String>,
    pub provider_instance_id: String,
    pub queue_held: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IdleCompactionMessage {
    pub id: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IdleCompactionProviderThread {
    pub id: String,
    pub driver: String,
    pub provider_instance_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IdleCompactionProviderTurn {
    pub provider_thread_id: String,
    pub token_usage: Option<TokenUsage>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IdleCompactionRuntimeRequest {
    pub status: String,
    pub answers: Option<BTreeMap<String, Value>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IdleCompactionContextTransfer {
    pub kind: String,
    pub target_thread_id: Option<String>,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IdleCompactionRecords {
    pub thread: IdleCompactionThread,
    pub runs: Vec<IdleCompactionRun>,
    pub messages: Vec<IdleCompactionMessage>,
    pub provider_threads: Vec<IdleCompactionProviderThread>,
    pub provider_turns: Vec<IdleCompactionProviderTurn>,
    pub runtime_requests: Vec<IdleCompactionRuntimeRequest>,
    pub context_transfers: Vec<IdleCompactionContextTransfer>,
    /// `derivePendingBackgroundWork(...).length > 0`; the caller derives it.
    pub pending_background_work: bool,
}

/// When an idle Claude thread should compact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdleCompactionWindow {
    pub run_id: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// T3's `idleCompactionWindow`: the window ends when the prompt cache of the
/// last request expires.
pub fn idle_compaction_window(
    records: &IdleCompactionRecords,
    now_ms: i64,
) -> Option<IdleCompactionWindow> {
    let thread = &records.thread;
    if thread.deleted_at.is_some() || thread.archived_at.is_some() {
        return None;
    }
    if thread.settled_at.is_some() || thread.settled_override == Some(SettledOverride::Settled) {
        return None;
    }
    if thread
        .snoozed_until
        .as_deref()
        .and_then(parse_ms)
        .is_some_and(|until| until > now_ms)
    {
        return None;
    }
    if !thread.lineage_listed {
        return None;
    }
    let latest_run = records
        .runs
        .iter()
        .filter(|run| run.status != "cancelled" || run.started_at.is_some())
        .max_by_key(|run| run.ordinal)?;
    if latest_run.status != "completed" || latest_run.completed_at.is_none() {
        return None;
    }
    let message = records
        .messages
        .iter()
        .find(|message| message.id == latest_run.user_message_id)?;
    // Never compact a compaction, so one idle spell compacts at most once.
    if message.text.trim().to_lowercase() == IDLE_COMPACTION_TEXT {
        return None;
    }
    if records
        .runtime_requests
        .iter()
        .any(|request| request.status == "pending")
        || records.context_transfers.iter().any(|transfer| {
            transfer.kind == "merge_back"
                && transfer.target_thread_id.as_deref() == Some(thread.id.as_str())
                && transfer.status == "pending"
        })
        || records.pending_background_work
    {
        return None;
    }
    let active_provider_thread_id = thread.active_provider_thread_id.as_deref();
    let provider_thread = records
        .provider_threads
        .iter()
        .find(|provider| Some(provider.id.as_str()) == active_provider_thread_id)?;
    // The thread's current selection receives the /compact, so it must still be
    // this Claude session.
    if provider_thread.driver != "claudeAgent"
        || latest_run.provider_thread_id.as_deref() != Some(provider_thread.id.as_str())
        || provider_thread.provider_instance_id != thread.provider_instance_id
        || latest_run.provider_instance_id != thread.provider_instance_id
    {
        return None;
    }
    let mut usages: Vec<&TokenUsage> = records
        .provider_turns
        .iter()
        .filter(|turn| turn.provider_thread_id == provider_thread.id)
        .filter_map(|turn| turn.token_usage.as_ref())
        .collect();
    usages.sort_by(|left, right| {
        parse_ms(&right.updated_at)
            .unwrap_or(i64::MIN)
            .cmp(&parse_ms(&left.updated_at).unwrap_or(i64::MIN))
    });
    if usages.first().map_or(0.0, |usage| usage.used_tokens) < COMPACTION_SUGGESTION_MIN_TOKENS {
        return None;
    }
    // A request served fully from cache creates no cache entry and reports no
    // TTL, but it still refreshes the cache. Treat it as unknown and skip.
    let end_ms = usages
        .first()
        .and_then(|usage| usage.prompt_cache_expires_at.as_deref())
        .and_then(parse_ms)?;
    if now_ms >= end_ms {
        return None;
    }
    Some(IdleCompactionWindow {
        run_id: latest_run.id.clone(),
        start_ms: end_ms - IDLE_COMPACTION_LEAD_MS,
        end_ms,
    })
}

fn is_compaction_command(text: &str) -> bool {
    let lower = text.to_lowercase();
    let Some(rest) = lower.strip_prefix(IDLE_COMPACTION_TEXT) else {
        return false;
    };
    rest.is_empty() || rest.starts_with(char::is_whitespace)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisibleItemKind {
    UserMessage,
    AssistantMessage,
    Compaction,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VisibleItem {
    pub kind: VisibleItemKind,
    pub text: String,
    pub attachments: usize,
    pub run_id: Option<String>,
    pub compaction: Option<CompactionItem>,
}

/// The provider facts the send decision reads.
#[derive(Clone, Debug, PartialEq)]
pub struct ContextProvider {
    pub instance_id: String,
    pub driver: String,
    pub continuation_group_key: Option<String>,
    pub slash_commands: Vec<String>,
}

/// The thread projection the send decision reads.
#[derive(Clone, Debug, PartialEq)]
pub struct CompactProjection {
    pub thread_provider_instance_id: String,
    pub active_provider_thread_id: Option<String>,
    pub provider_threads: Vec<IdleCompactionProviderThread>,
    pub provider_turns: Vec<IdleCompactionProviderTurn>,
    pub runtime_requests: Vec<IdleCompactionRuntimeRequest>,
    pub runs: Vec<IdleCompactionRun>,
    pub visible_items: Vec<VisibleItem>,
    pub context_transfers: Vec<IdleCompactionContextTransfer>,
    /// Usage inherited by an unstarted native fork, when it matches.
    pub fork_source_usage: Option<TokenUsage>,
}

/// One plain send into an idle thread.
#[derive(Clone, Debug)]
pub struct CompactBeforeSendInput<'a> {
    pub projection: &'a CompactProjection,
    pub selection_instance: &'a str,
    pub providers: &'a [ContextProvider],
    pub settings: &'a IdleCompactionSettings,
    pub text: &'a str,
    pub thread_idle: bool,
    pub now: &'a str,
}

fn pending_handoff(input: &CompactBeforeSendInput<'_>) -> bool {
    if input.projection.visible_items.is_empty() {
        return false;
    }
    let from = input
        .projection
        .runs
        .iter()
        .rev()
        .find(|run| run.status != "rolled_back" && run.started_at.is_some())
        .map(|run| run.provider_instance_id.as_str());
    let Some(from) = from else {
        return false;
    };
    if from == input.selection_instance {
        return false;
    }
    let Some(source) = input
        .providers
        .iter()
        .find(|provider| provider.instance_id == from)
    else {
        return true;
    };
    let Some(target) = input
        .providers
        .iter()
        .find(|provider| provider.instance_id == input.selection_instance)
    else {
        return true;
    };
    source.continuation_group_key.is_none()
        || source.driver != target.driver
        || source.continuation_group_key != target.continuation_group_key
}

/// T3's `compactBeforeSendTokens`: the tokens a plain send compacts first, or
/// `None` when it sends as is.
pub fn compact_before_send_tokens(input: &CompactBeforeSendInput<'_>) -> Option<f64> {
    let provider = input
        .providers
        .iter()
        .find(|provider| provider.instance_id == input.selection_instance)?;
    if !input.thread_idle
        || is_compaction_command(input.text)
        || resolve_idle_compaction(input.settings, &provider.instance_id) == IdleCompactionMode::Off
        || !provider
            .slash_commands
            .iter()
            .any(|command| command == "compact")
        || pending_handoff(input)
        || input
            .projection
            .runtime_requests
            .iter()
            .any(|request| request.status == "pending")
        || input
            .projection
            .runs
            .iter()
            .any(|run| run.status == "queued" && run.queue_held == Some(true))
    {
        return None;
    }
    let real_user_message = input.projection.visible_items.iter().any(|item| {
        item.kind == VisibleItemKind::UserMessage
            && (item.text.trim().to_lowercase() != IDLE_COMPACTION_TEXT || item.attachments > 0)
    });
    let started_run_without_message = input.projection.runs.iter().any(|run| {
        run.status != "rolled_back"
            && run.started_at.is_some()
            && !input.projection.visible_items.iter().any(|item| {
                item.kind == VisibleItemKind::UserMessage
                    && item.run_id.as_deref() == Some(run.id.as_str())
            })
    });
    if !real_user_message && !started_run_without_message {
        return None;
    }
    let activities: Vec<(String, Option<BTreeMap<String, Value>>)> = input
        .projection
        .runtime_requests
        .iter()
        .filter(|request| request.status == "resolved")
        .map(|request| ("user-input.resolved".to_owned(), request.answers.clone()))
        .collect();
    if has_dismissed_resume_compaction(&activities) {
        return None;
    }

    let provider_thread = input.projection.provider_threads.iter().find(|thread| {
        Some(thread.id.as_str()) == input.projection.active_provider_thread_id.as_deref()
    });
    let turns: Vec<TurnUsageRef> = if input.projection.active_provider_thread_id.is_some() {
        input
            .projection
            .provider_turns
            .iter()
            .map(|turn| TurnUsageRef {
                provider_thread_id: Some(turn.provider_thread_id.clone()),
                token_usage: turn.token_usage.clone(),
            })
            .collect()
    } else {
        Vec::new()
    };
    let fork_source = (input.selection_instance == input.projection.thread_provider_instance_id)
        .then_some(input.projection.fork_source_usage.as_ref())
        .flatten();
    let provider_thread_ref = provider_thread.map(|thread| ProviderContextThread {
        id: thread.id.clone(),
        context_usage: None,
        updated_at: String::new(),
    });
    let live_usage =
        latest_provider_turn_token_usage(&turns, provider_thread_ref.as_ref(), fork_source);
    let entries: Vec<ContextEntry> = input
        .projection
        .visible_items
        .iter()
        .filter_map(|item| {
            item.compaction.clone().map(|compaction| ContextEntry {
                item: ContextItem::Compaction(compaction),
            })
        })
        .collect();
    let context_window = derive_latest_context_window_snapshot(
        &entries,
        live_usage.as_ref(),
        provider_thread_ref.as_ref(),
    )?;
    should_offer_resume_compaction(
        Some(&provider.driver),
        Some(context_window.used_tokens),
        Some(&context_window.updated_at),
        live_usage.and_then(|usage| usage.prompt_cache_ttl_ms),
        input.now,
    )
    .then_some(context_window.used_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW_MS: i64 = 1_792_411_200_000;
    const MINUTE_MS: i64 = 60_000;

    fn iso(offset_ms: i64) -> String {
        super::super::limits::format_iso_millis(NOW_MS + offset_ms).unwrap()
    }

    fn compaction_entry(provider_thread_id: Option<&str>, updated_at: &str) -> ContextEntry {
        ContextEntry {
            item: ContextItem::Compaction(CompactionItem {
                provider_thread_id: provider_thread_id.map(str::to_owned),
                status: "completed".into(),
                before_token_count: Some(10_000.0),
                after_token_count: Some(2_000.0),
                started_at: None,
                completed_at: None,
                updated_at: updated_at.into(),
            }),
        }
    }

    #[test]
    fn uses_retained_compaction_token_data_when_available() {
        let snapshot = derive_latest_context_window_snapshot(
            &[compaction_entry(None, "2026-06-20T00:00:00.000Z")],
            None,
            None,
        )
        .unwrap();
        assert_eq!(snapshot.used_tokens, 2_000.0);
        assert_eq!(snapshot.total_processed_tokens, Some(10_000.0));
    }

    #[test]
    fn ignores_a_retired_provider_threads_compaction_after_a_provider_switch() {
        let active = ProviderContextThread {
            id: "acp-thread".into(),
            context_usage: None,
            updated_at: "2026-10-19T12:10:00.000Z".into(),
        };
        let retired = compaction_entry(Some("pi-thread"), "2026-06-20T00:00:00.000Z");
        let current = compaction_entry(Some("acp-thread"), "2026-06-20T00:00:00.000Z");
        assert_eq!(
            derive_latest_context_window_snapshot(&[retired], None, Some(&active)),
            None
        );
        assert_eq!(
            derive_latest_context_window_snapshot(&[current], None, Some(&active))
                .unwrap()
                .used_tokens,
            2_000.0
        );
        assert_eq!(
            derive_latest_context_window_snapshot(
                &[compaction_entry(None, "2026-06-20T00:00:00.000Z")],
                None,
                Some(&active)
            )
            .unwrap()
            .used_tokens,
            2_000.0
        );
    }

    #[test]
    fn prefers_a_completed_compaction_over_usage_reported_before_it() {
        let usage_before = TokenUsage {
            used_tokens: 985_000.0,
            max_tokens: Some(1_000_000.0),
            updated_at: "2026-06-19T23:55:00.000Z".into(),
            ..Default::default()
        };
        let snapshot = derive_latest_context_window_snapshot(
            &[compaction_entry(None, "2026-06-20T00:00:00.000Z")],
            Some(&usage_before),
            None,
        )
        .unwrap();
        assert_eq!(snapshot.used_tokens, 2_000.0);
        assert_eq!(snapshot.max_tokens, Some(1_000_000.0));

        // A report in the compaction's own millisecond is the provider's
        // post-compaction usage.
        let reported_with = TokenUsage {
            used_tokens: 2_000.0,
            max_tokens: Some(1_000_000.0),
            auto_compact_threshold: Some(900_000.0),
            updated_at: "2026-06-20T00:00:00.000Z".into(),
            ..Default::default()
        };
        assert_eq!(
            derive_latest_context_window_snapshot(
                &[compaction_entry(None, "2026-06-20T00:00:00.000Z")],
                Some(&reported_with),
                None
            )
            .unwrap()
            .auto_compact_threshold,
            Some(900_000.0)
        );

        let mut without_count = compaction_entry(None, "2026-06-20T00:00:00.000Z");
        if let ContextItem::Compaction(item) = &mut without_count.item {
            item.after_token_count = None;
        }
        assert_eq!(
            derive_latest_context_window_snapshot(&[without_count], Some(&usage_before), None),
            None
        );
        let older = compaction_entry(None, "2026-06-19T00:00:00.000Z");
        let mut without_count = compaction_entry(None, "2026-06-20T00:00:00.000Z");
        if let ContextItem::Compaction(item) = &mut without_count.item {
            item.after_token_count = None;
        }
        let newer_thread = ProviderContextThread {
            id: "thread".into(),
            context_usage: None,
            updated_at: "2026-06-21T00:00:00.000Z".into(),
        };
        assert_eq!(
            derive_latest_context_window_snapshot(
                &[older, without_count],
                None,
                Some(&newer_thread)
            ),
            None
        );
    }

    #[test]
    fn prefers_the_providers_live_report_over_compaction_items() {
        let usage = TokenUsage {
            used_tokens: 42_000.0,
            max_tokens: Some(200_000.0),
            input_tokens: Some(40_000.0),
            output_tokens: Some(2_000.0),
            auto_compact_threshold: Some(150_000.0),
            updated_at: "2026-08-27T00:00:00.000Z".into(),
            ..Default::default()
        };
        let snapshot = derive_latest_context_window_snapshot(&[], Some(&usage), None).unwrap();
        assert_eq!(snapshot.used_tokens, 42_000.0);
        assert_eq!(snapshot.max_tokens, Some(200_000.0));
        assert_eq!(snapshot.remaining_tokens, Some(158_000.0));
        assert_eq!(snapshot.used_percentage, Some(21.0));
        assert_eq!(snapshot.auto_compact_threshold, Some(150_000.0));
    }

    #[test]
    fn picks_the_newest_provider_turn_report_not_the_last_in_snapshot_order() {
        let provider_thread = ProviderContextThread {
            id: "provider-thread".into(),
            context_usage: None,
            updated_at: "2026-08-27T00:00:00.000Z".into(),
        };
        let turns = vec![
            TurnUsageRef {
                provider_thread_id: Some("provider-thread".into()),
                token_usage: Some(TokenUsage {
                    used_tokens: 300_000.0,
                    updated_at: "2026-06-20T12:00:00.000Z".into(),
                    ..Default::default()
                }),
            },
            TurnUsageRef {
                provider_thread_id: Some("provider-thread".into()),
                token_usage: None,
            },
            TurnUsageRef {
                provider_thread_id: Some("provider-thread".into()),
                token_usage: Some(TokenUsage {
                    used_tokens: 90_000.0,
                    updated_at: "2026-06-20T09:00:00.000Z".into(),
                    ..Default::default()
                }),
            },
        ];
        assert_eq!(
            latest_provider_turn_token_usage(&turns, Some(&provider_thread), None)
                .unwrap()
                .used_tokens,
            300_000.0
        );
    }

    #[test]
    fn prefers_the_active_provider_threads_older_report_over_a_newer_retired_one() {
        let turns = vec![
            TurnUsageRef {
                provider_thread_id: Some("pi-thread".into()),
                token_usage: Some(TokenUsage {
                    used_tokens: 180_000.0,
                    updated_at: "2026-10-19T12:10:00.000Z".into(),
                    ..Default::default()
                }),
            },
            TurnUsageRef {
                provider_thread_id: Some("acp-thread".into()),
                token_usage: Some(TokenUsage {
                    used_tokens: 12_000.0,
                    updated_at: "2026-10-19T11:00:00.000Z".into(),
                    ..Default::default()
                }),
            },
        ];
        let provider_thread = ProviderContextThread {
            id: "acp-thread".into(),
            context_usage: None,
            updated_at: "2026-10-19T12:00:00.000Z".into(),
        };
        assert_eq!(
            latest_provider_turn_token_usage(&turns, Some(&provider_thread), None)
                .unwrap()
                .used_tokens,
            12_000.0
        );
    }

    #[test]
    fn keeps_fork_point_context_and_its_cache_expiry_before_the_fork_reports_usage() {
        let source = TokenUsage {
            used_tokens: 513_000.0,
            max_tokens: Some(1_000_000.0),
            prompt_cache_ttl_ms: Some(5 * MINUTE_MS),
            updated_at: "2026-10-19T12:00:00.000Z".into(),
            ..Default::default()
        };
        let usage = latest_provider_turn_token_usage(&[], None, Some(&source)).unwrap();
        let snapshot = derive_latest_context_window_snapshot(&[], Some(&usage), None).unwrap();
        assert_eq!(snapshot.used_tokens, 513_000.0);
        assert!(should_offer_resume_compaction(
            Some("claudeAgent"),
            Some(snapshot.used_tokens),
            Some(&snapshot.updated_at),
            usage.prompt_cache_ttl_ms,
            "2026-10-19T12:06:00.000Z"
        ));
    }

    #[test]
    fn prefers_target_provider_thread_usage_over_inherited_usage() {
        for used_tokens in [0.0, 42_000.0] {
            let provider_thread = ProviderContextThread {
                id: "provider-thread".into(),
                context_usage: Some(ProviderContextUsage {
                    used_tokens,
                    max_tokens: Some(200_000.0),
                    cost: Some(json!({ "amount": 0.42, "currency": "USD" })),
                    ..Default::default()
                }),
                updated_at: "2026-10-19T12:10:00.000Z".into(),
            };
            let snapshot =
                derive_latest_context_window_snapshot(&[], None, Some(&provider_thread)).unwrap();
            assert_eq!(snapshot.used_tokens, used_tokens);
            assert_eq!(snapshot.max_tokens, Some(200_000.0));
            assert_eq!(
                snapshot.cost,
                Some(json!({ "amount": 0.42, "currency": "USD" }))
            );
            assert_eq!(snapshot.updated_at, "2026-10-19T12:10:00.000Z");
        }
    }

    #[test]
    fn resume_compaction_requires_claude_enough_tokens_and_a_stale_cache() {
        let now = "2026-10-30T23:03:21.000Z";
        assert!(should_offer_resume_compaction(
            Some("claudeAgent"),
            Some(293_611.0),
            Some("2026-10-30T14:23:02.000Z"),
            None,
            now
        ));
        assert!(!should_offer_resume_compaction(
            Some("claudeAgent"),
            Some(150_000.0),
            Some("2026-10-30T14:23:02.000Z"),
            None,
            now
        ));
        assert!(!should_offer_resume_compaction(
            Some("codex"),
            Some(293_611.0),
            Some("2026-10-30T14:23:02.000Z"),
            None,
            now
        ));
        assert!(!should_offer_resume_compaction(
            Some("claudeAgent"),
            Some(293_611.0),
            Some("2026-10-30T22:30:00.000Z"),
            None,
            now
        ));
    }

    #[test]
    fn the_resume_question_round_trips_and_its_dismissal_is_recognized() {
        let question = format_claude_resume_compaction_question(90, 250_000);
        assert_eq!(
            question,
            "This session is 1h 30m old and uses 250,000 tokens. Compact it before continuing?"
        );
        assert!(is_claude_resume_compaction_question(&question));
        let mut answers = BTreeMap::new();
        answers.insert(question, json!(CLAUDE_RESUME_COMPACTION_NEVER_ANSWER));
        assert!(has_dismissed_resume_compaction(&[(
            "user-input.resolved".into(),
            Some(answers)
        )]));
        let mut unrelated = BTreeMap::new();
        unrelated.insert(
            "The build cache is large. Compact it before continuing?".to_owned(),
            json!(CLAUDE_RESUME_COMPACTION_NEVER_ANSWER),
        );
        assert!(!has_dismissed_resume_compaction(&[(
            "user-input.resolved".into(),
            Some(unrelated)
        )]));
    }

    #[test]
    fn context_tone_warns_at_80_and_errors_at_90() {
        assert_eq!(context_tone(Some(79.9)), None);
        assert_eq!(context_tone(Some(80.0)), Some(ContextTone::Warning));
        assert_eq!(context_tone(Some(90.0)), Some(ContextTone::Error));
        assert_eq!(context_tone(None), None);
    }

    fn idle_records(
        used_tokens: Option<f64>,
        cache_expires_in_ms: Option<i64>,
        text: &str,
    ) -> IdleCompactionRecords {
        let run_id = "run-1";
        let provider_thread_id = "provider-thread";
        IdleCompactionRecords {
            thread: IdleCompactionThread {
                id: "idle-thread".into(),
                provider_instance_id: "claudeAgent".into(),
                active_provider_thread_id: Some(provider_thread_id.into()),
                archived_at: None,
                deleted_at: None,
                settled_at: None,
                settled_override: None,
                snoozed_until: None,
                lineage_listed: true,
            },
            runs: vec![IdleCompactionRun {
                id: run_id.into(),
                ordinal: 1,
                status: "completed".into(),
                started_at: Some(iso(-MINUTE_MS)),
                completed_at: Some(iso(0)),
                user_message_id: "message-1".into(),
                provider_thread_id: Some(provider_thread_id.into()),
                provider_instance_id: "claudeAgent".into(),
                queue_held: None,
            }],
            messages: vec![IdleCompactionMessage {
                id: "message-1".into(),
                text: text.into(),
            }],
            provider_threads: vec![IdleCompactionProviderThread {
                id: provider_thread_id.into(),
                driver: "claudeAgent".into(),
                provider_instance_id: "claudeAgent".into(),
            }],
            provider_turns: vec![IdleCompactionProviderTurn {
                provider_thread_id: provider_thread_id.into(),
                token_usage: Some(TokenUsage {
                    used_tokens: used_tokens.unwrap_or(250_000.0),
                    prompt_cache_expires_at: cache_expires_in_ms.map(iso),
                    updated_at: iso(0),
                    ..Default::default()
                }),
            }],
            runtime_requests: Vec::new(),
            context_transfers: Vec::new(),
            pending_background_work: false,
        }
    }

    #[test]
    fn idle_compaction_skips_small_contexts_unknown_expiry_other_providers_compactions_and_side_chats()
     {
        let cache = Some(i64::from(60) * MINUTE_MS);
        assert_eq!(
            idle_compaction_window(
                &idle_records(Some(199_999.0), cache, "Explain this code"),
                NOW_MS
            ),
            None
        );
        assert_eq!(
            idle_compaction_window(
                &idle_records(Some(250_000.0), None, "Explain this code"),
                NOW_MS
            ),
            None
        );
        let mut codex = idle_records(Some(250_000.0), cache, "Explain this code");
        codex.provider_threads[0].driver = "codex".into();
        assert_eq!(idle_compaction_window(&codex, NOW_MS), None);
        let mut switched = idle_records(Some(250_000.0), cache, "Explain this code");
        switched.thread.provider_instance_id = "codex".into();
        assert_eq!(idle_compaction_window(&switched, NOW_MS), None);
        assert_eq!(
            idle_compaction_window(&idle_records(Some(250_000.0), cache, "/compact"), NOW_MS),
            None
        );
        let mut steered = idle_records(Some(250_000.0), cache, "Explain this code");
        steered.runs.push(IdleCompactionRun {
            id: "run-2".into(),
            ordinal: 2,
            status: "cancelled".into(),
            started_at: None,
            completed_at: None,
            user_message_id: "message-2".into(),
            provider_thread_id: Some("provider-thread".into()),
            provider_instance_id: "claudeAgent".into(),
            queue_held: None,
        });
        assert_eq!(
            idle_compaction_window(&steered, NOW_MS).unwrap().run_id,
            "run-1"
        );
        // A turn Claude itself reported as cancelled did run, so it stays the
        // latest.
        let mut steered = idle_records(Some(250_000.0), cache, "Explain this code");
        steered.runs.push(IdleCompactionRun {
            id: "run-2".into(),
            ordinal: 2,
            status: "cancelled".into(),
            started_at: Some(iso(-MINUTE_MS)),
            completed_at: None,
            user_message_id: "message-2".into(),
            provider_thread_id: Some("provider-thread".into()),
            provider_instance_id: "claudeAgent".into(),
            queue_held: None,
        });
        assert_eq!(idle_compaction_window(&steered, NOW_MS), None);
        let mut archived = idle_records(Some(250_000.0), cache, "Explain this code");
        archived.thread.archived_at = Some(iso(0));
        assert_eq!(idle_compaction_window(&archived, NOW_MS), None);
        let mut side_chat = idle_records(Some(250_000.0), cache, "Explain this code");
        assert!(idle_compaction_window(&side_chat, NOW_MS).is_some());
        side_chat.thread.lineage_listed = false;
        assert_eq!(idle_compaction_window(&side_chat, NOW_MS), None);
    }

    #[test]
    fn idle_compaction_window_starts_two_minutes_before_the_cache_expires() {
        let window = idle_compaction_window(
            &idle_records(
                Some(250_000.0),
                Some(i64::from(10) * MINUTE_MS),
                "Explain this code",
            ),
            NOW_MS,
        )
        .unwrap();
        assert_eq!(window.start_ms, NOW_MS + 8 * MINUTE_MS);
        assert_eq!(window.end_ms, NOW_MS + 10 * MINUTE_MS);
    }

    const LAST_TURN_AT: &str = "2026-10-30T14:23:02.000Z";
    const RETURNED_AT: &str = "2026-10-30T23:03:21.000Z";

    fn providers(slash_commands: &[&str]) -> Vec<ContextProvider> {
        [
            ("claudeAgent", "claudeAgent", "claude:~/.claude"),
            ("claude_work", "claudeAgent", "claude:~/.claude"),
            ("claude_other_home", "claudeAgent", "claude:~/.claude-other"),
            ("codex", "codex", "codex:~/.codex"),
        ]
        .into_iter()
        .map(|(instance, driver, group)| ContextProvider {
            instance_id: instance.into(),
            driver: driver.into(),
            continuation_group_key: Some(group.into()),
            slash_commands: slash_commands
                .iter()
                .map(|command| (*command).to_owned())
                .collect(),
        })
        .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn send_input(
        used_tokens: Option<f64>,
        prompt_cache_ttl_ms: Option<i64>,
        _now: &str,
        source: &str,
        selected: &str,
        only_earlier_usage: bool,
        unstarted_fork: bool,
        slash_commands: &[&str],
        dismissed: bool,
        held_queue: bool,
        pending_request: bool,
        only_compact_messages: bool,
        prompt_outside_window: bool,
        _text: &str,
        _thread_idle: bool,
    ) -> (CompactProjection, Vec<ContextProvider>) {
        let provider_threads = vec![
            IdleCompactionProviderThread {
                id: "provider-thread-current".into(),
                driver: if selected == "codex" {
                    "codex"
                } else {
                    "claudeAgent"
                }
                .into(),
                provider_instance_id: selected.into(),
            },
            IdleCompactionProviderThread {
                id: "provider-thread-earlier".into(),
                driver: "claudeAgent".into(),
                provider_instance_id: "claudeAgent".into(),
            },
        ];
        let usage = Some(TokenUsage {
            used_tokens: used_tokens.unwrap_or(293_611.0),
            prompt_cache_ttl_ms,
            updated_at: LAST_TURN_AT.into(),
            ..Default::default()
        });
        let visible_items = if prompt_outside_window {
            vec![VisibleItem {
                kind: VisibleItemKind::AssistantMessage,
                text: "Build fixed".into(),
                attachments: 0,
                run_id: None,
                compaction: None,
            }]
        } else {
            vec![VisibleItem {
                kind: VisibleItemKind::UserMessage,
                text: if only_compact_messages {
                    "/compact".into()
                } else {
                    "Fix the build".into()
                },
                attachments: 0,
                run_id: Some("run-1".into()),
                compaction: None,
            }]
        };
        let mut answers = None;
        if dismissed {
            let mut map = BTreeMap::new();
            map.insert(
                format_claude_resume_compaction_question(90, 250_000),
                json!(CLAUDE_RESUME_COMPACTION_NEVER_ANSWER),
            );
            answers = Some(map);
        }
        let mut projection = CompactProjection {
            thread_provider_instance_id: source.into(),
            active_provider_thread_id: Some("provider-thread-current".into()),
            provider_threads,
            provider_turns: vec![IdleCompactionProviderTurn {
                provider_thread_id: if only_earlier_usage {
                    "provider-thread-earlier".into()
                } else {
                    "provider-thread-current".into()
                },
                token_usage: usage.clone(),
            }],
            runtime_requests: vec![IdleCompactionRuntimeRequest {
                status: if dismissed {
                    "resolved".into()
                } else if pending_request {
                    "pending".into()
                } else {
                    "resolved".into()
                },
                answers,
            }],
            runs: vec![
                IdleCompactionRun {
                    id: "run-1".into(),
                    ordinal: 1,
                    status: "completed".into(),
                    started_at: Some("2026-10-30T14:20:00.000Z".into()),
                    completed_at: Some(LAST_TURN_AT.into()),
                    user_message_id: "message-1".into(),
                    provider_thread_id: Some("provider-thread-current".into()),
                    provider_instance_id: source.into(),
                    queue_held: None,
                },
                IdleCompactionRun {
                    id: "queued-run".into(),
                    ordinal: 2,
                    status: "queued".into(),
                    started_at: None,
                    completed_at: None,
                    user_message_id: "message-2".into(),
                    provider_thread_id: None,
                    provider_instance_id: source.into(),
                    queue_held: Some(held_queue),
                },
            ],
            visible_items,
            context_transfers: Vec::new(),
            fork_source_usage: None,
        };
        if unstarted_fork {
            projection.thread_provider_instance_id = source.into();
            projection.active_provider_thread_id = None;
            projection.runs.clear();
            projection.provider_threads.clear();
            projection.provider_turns.clear();
            projection.fork_source_usage = Some(TokenUsage {
                used_tokens: 293_611.0,
                updated_at: LAST_TURN_AT.into(),
                ..Default::default()
            });
        }
        (projection, providers(slash_commands))
    }

    #[allow(clippy::too_many_arguments)]
    fn compact(
        used_tokens: Option<f64>,
        prompt_cache_ttl_ms: Option<i64>,
        now: &str,
        settings: IdleCompactionSettings,
        source: &str,
        selected: &str,
        only_earlier_usage: bool,
        unstarted_fork: bool,
        slash_commands: &[&str],
        dismissed: bool,
        held_queue: bool,
        pending_request: bool,
        only_compact_messages: bool,
        prompt_outside_window: bool,
        text: &str,
        thread_idle: bool,
    ) -> Option<f64> {
        let (projection, providers) = send_input(
            used_tokens,
            prompt_cache_ttl_ms,
            now,
            source,
            selected,
            only_earlier_usage,
            unstarted_fork,
            slash_commands,
            dismissed,
            held_queue,
            pending_request,
            only_compact_messages,
            prompt_outside_window,
            text,
            thread_idle,
        );
        compact_before_send_tokens(&CompactBeforeSendInput {
            projection: &projection,
            selection_instance: selected,
            providers: &providers,
            settings: &settings,
            text,
            thread_idle,
            now,
        })
    }

    fn default_settings() -> IdleCompactionSettings {
        IdleCompactionSettings::default()
    }

    #[test]
    fn compact_before_send_covers_t3s_thresholds_and_gates() {
        let cases: Vec<(&str, Option<f64>)> = vec![
            (
                "a stale Claude thread over 200K",
                compact(
                    None,
                    None,
                    RETURNED_AT,
                    default_settings(),
                    "claudeAgent",
                    "claudeAgent",
                    false,
                    false,
                    &["compact"],
                    false,
                    false,
                    false,
                    false,
                    false,
                    "back again",
                    true,
                ),
            ),
            (
                "when the original prompt is outside the loaded window",
                compact(
                    None,
                    None,
                    RETURNED_AT,
                    default_settings(),
                    "claudeAgent",
                    "claudeAgent",
                    false,
                    false,
                    &["compact"],
                    false,
                    false,
                    false,
                    false,
                    true,
                    "back again",
                    true,
                ),
            ),
            (
                "when the environment compacts before the cache expires",
                compact(
                    None,
                    None,
                    RETURNED_AT,
                    IdleCompactionSettings {
                        idle_compaction: Some(IdleCompactionMode::BeforeCacheExpires),
                        ..Default::default()
                    },
                    "claudeAgent",
                    "claudeAgent",
                    false,
                    false,
                    &["compact"],
                    false,
                    false,
                    false,
                    false,
                    false,
                    "back again",
                    true,
                ),
            ),
            (
                "past a reported TTL shorter than the fallback",
                compact(
                    None,
                    Some(5 * MINUTE_MS),
                    "2026-10-30T14:30:00.000Z",
                    default_settings(),
                    "claudeAgent",
                    "claudeAgent",
                    false,
                    false,
                    &["compact"],
                    false,
                    false,
                    false,
                    false,
                    false,
                    "back again",
                    true,
                ),
            ),
            (
                "within the fallback TTL",
                compact(
                    None,
                    None,
                    "2026-10-30T15:23:02.000Z",
                    default_settings(),
                    "claudeAgent",
                    "claudeAgent",
                    false,
                    false,
                    &["compact"],
                    false,
                    false,
                    false,
                    false,
                    false,
                    "back again",
                    true,
                ),
            ),
            (
                "under 200K",
                compact(
                    Some(150_000.0),
                    None,
                    RETURNED_AT,
                    default_settings(),
                    "claudeAgent",
                    "claudeAgent",
                    false,
                    false,
                    &["compact"],
                    false,
                    false,
                    false,
                    false,
                    false,
                    "back again",
                    true,
                ),
            ),
            (
                "with idle compaction off",
                compact(
                    None,
                    None,
                    RETURNED_AT,
                    IdleCompactionSettings {
                        idle_compaction: Some(IdleCompactionMode::Off),
                        ..Default::default()
                    },
                    "claudeAgent",
                    "claudeAgent",
                    false,
                    false,
                    &["compact"],
                    false,
                    false,
                    false,
                    false,
                    false,
                    "back again",
                    true,
                ),
            ),
            (
                "with the Claude instance overriding idle compaction to off",
                compact(
                    None,
                    None,
                    RETURNED_AT,
                    IdleCompactionSettings {
                        idle_compaction: Some(IdleCompactionMode::OnReturn),
                        instance_overrides: BTreeMap::from([(
                            "claudeAgent".to_owned(),
                            IdleCompactionMode::Off,
                        )]),
                        ..Default::default()
                    },
                    "claudeAgent",
                    "claudeAgent",
                    false,
                    false,
                    &["compact"],
                    false,
                    false,
                    false,
                    false,
                    false,
                    "back again",
                    true,
                ),
            ),
            (
                "on a Codex thread",
                compact(
                    None,
                    None,
                    RETURNED_AT,
                    default_settings(),
                    "codex",
                    "codex",
                    false,
                    false,
                    &["compact"],
                    false,
                    false,
                    false,
                    false,
                    false,
                    "back again",
                    true,
                ),
            ),
            (
                "after switching a stale Codex thread to Claude",
                compact(
                    None,
                    None,
                    RETURNED_AT,
                    default_settings(),
                    "codex",
                    "claudeAgent",
                    false,
                    false,
                    &["compact"],
                    false,
                    false,
                    false,
                    false,
                    false,
                    "back again",
                    true,
                ),
            ),
            (
                "after switching to a Claude instance that continues the conversation",
                compact(
                    None,
                    None,
                    RETURNED_AT,
                    default_settings(),
                    "claudeAgent",
                    "claude_work",
                    false,
                    false,
                    &["compact"],
                    false,
                    false,
                    false,
                    false,
                    false,
                    "back again",
                    true,
                ),
            ),
            (
                "after switching to a Claude instance with another home",
                compact(
                    None,
                    None,
                    RETURNED_AT,
                    default_settings(),
                    "claudeAgent",
                    "claude_other_home",
                    false,
                    false,
                    &["compact"],
                    false,
                    false,
                    false,
                    false,
                    false,
                    "back again",
                    true,
                ),
            ),
            (
                "when only an earlier conversation reported usage",
                compact(
                    None,
                    None,
                    RETURNED_AT,
                    default_settings(),
                    "claudeAgent",
                    "claudeAgent",
                    true,
                    false,
                    &["compact"],
                    false,
                    false,
                    false,
                    false,
                    false,
                    "back again",
                    true,
                ),
            ),
            (
                "an unstarted fork resuming its source instance",
                compact(
                    None,
                    None,
                    RETURNED_AT,
                    default_settings(),
                    "claudeAgent",
                    "claudeAgent",
                    false,
                    true,
                    &["compact"],
                    false,
                    false,
                    false,
                    false,
                    false,
                    "back again",
                    true,
                ),
            ),
            (
                "an unstarted fork selecting another Claude home",
                compact(
                    None,
                    None,
                    RETURNED_AT,
                    default_settings(),
                    "claudeAgent",
                    "claude_other_home",
                    false,
                    true,
                    &["compact"],
                    false,
                    false,
                    false,
                    false,
                    false,
                    "back again",
                    true,
                ),
            ),
            (
                "for a provider without /compact",
                compact(
                    None,
                    None,
                    RETURNED_AT,
                    default_settings(),
                    "claudeAgent",
                    "claudeAgent",
                    false,
                    false,
                    &[],
                    false,
                    false,
                    false,
                    false,
                    false,
                    "back again",
                    true,
                ),
            ),
            (
                "after Claude's resume dialog was answered Don't ask again",
                compact(
                    None,
                    None,
                    RETURNED_AT,
                    default_settings(),
                    "claudeAgent",
                    "claudeAgent",
                    false,
                    false,
                    &["compact"],
                    true,
                    false,
                    false,
                    false,
                    false,
                    "back again",
                    true,
                ),
            ),
            (
                "while the queue is held",
                compact(
                    None,
                    None,
                    RETURNED_AT,
                    default_settings(),
                    "claudeAgent",
                    "claudeAgent",
                    false,
                    false,
                    &["compact"],
                    false,
                    true,
                    false,
                    false,
                    false,
                    "back again",
                    true,
                ),
            ),
            (
                "while a request waits for an answer",
                compact(
                    None,
                    None,
                    RETURNED_AT,
                    default_settings(),
                    "claudeAgent",
                    "claudeAgent",
                    false,
                    false,
                    &["compact"],
                    false,
                    false,
                    true,
                    false,
                    false,
                    "back again",
                    true,
                ),
            ),
            (
                "while the thread is working",
                compact(
                    None,
                    None,
                    RETURNED_AT,
                    default_settings(),
                    "claudeAgent",
                    "claudeAgent",
                    false,
                    false,
                    &["compact"],
                    false,
                    false,
                    false,
                    false,
                    false,
                    "back again",
                    false,
                ),
            ),
            (
                "with no conversation but /compact",
                compact(
                    None,
                    None,
                    RETURNED_AT,
                    default_settings(),
                    "claudeAgent",
                    "claudeAgent",
                    false,
                    false,
                    &["compact"],
                    false,
                    false,
                    false,
                    true,
                    false,
                    "back again",
                    true,
                ),
            ),
            (
                "for a typed /compact",
                compact(
                    None,
                    None,
                    RETURNED_AT,
                    default_settings(),
                    "claudeAgent",
                    "claudeAgent",
                    false,
                    false,
                    &["compact"],
                    false,
                    false,
                    false,
                    false,
                    false,
                    "/compact keep the plan",
                    true,
                ),
            ),
        ];
        let expected = [
            true, true, true, true, false, false, false, false, false, false, true, false, false,
            true, false, false, false, false, false, false, false, false,
        ];
        for ((name, got), want) in cases.into_iter().zip(expected) {
            assert_eq!(
                got,
                want.then_some(293_611.0),
                "compactBeforeSendTokens case: {name}"
            );
        }
    }
}
