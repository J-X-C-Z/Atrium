//! The app's usage report and continue-after-reset sweep (item 26).
//!
//! `atrium_engine::usage` owns the provider readers, the limit classifier and
//! the recovery decision. This module keeps the app's canonical report per
//! backend (an agent, here or on a device), projects it to the legacy
//! [`RateLimits`] the existing views read, and exposes typed accessors for the
//! UI: the banner, every window of every agent and device, and a thread's
//! context window with its compaction offer.
//!
//! Everything here is GPUI-free. Decisions take `now` as data, and the
//! persisted recovery journal is the engine's
//! [`SqliteRecoveryJournal`] so an armed reset survives a restart.

// The typed read API below serves item 30's UI, which is not in this branch,
// and the write helpers wait for the Codex/Claude adapter seam.
#![allow(dead_code)]

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use atrium_core::{RateLimits, RateWindow};
use atrium_engine::contracts::ProviderFailureClass;
use atrium_engine::usage::limits::UsageLimitUnavailable;
use atrium_engine::usage::{
    self as engine_usage, ClaudeRateLimitEvent, ClaudeScopedLimitNames, ClaudeUsageProbe,
    CodexRateLimitSnapshot, CodexRateLimitWindow, ContextTone, LimitRecovery,
    LimitRecoveryCandidate, LimitRecoveryCommand, LimitRecoverySettings, ProviderUsageLimits,
    RecoveryError, RecoveryThreadStatus, SqliteRecoveryJournal, StoredRecovery, UsageLimitsUpdate,
    UsageWindow, UsageWindowKind, apply_usage_limits_update, claude_rate_limit_event_to_update,
    claude_usage_response_to_limits, codex_rate_limits_to_limits, context_tone,
    derive_latest_context_window_snapshot, resolve_usage_limits_after_probe,
    should_offer_resume_compaction, sweep,
};
use serde_json::Value;

/// A window at or above this share is shown as "near the limit".
pub const NEAR_LIMIT_PERCENT: f64 = 80.0;
/// A window at or above this share is exhausted.
pub const AT_LIMIT_PERCENT: f64 = 100.0;
/// The clone of Claude's `get_usage` driver name the compaction rule expects.
const CLAUDE_DRIVER: &str = "claudeAgent";

// ---------------------------------------------------------------------------
// Small time helpers. The engine keeps its own copies private; the app needs
// the same ISO form for report times and persisted reset times.
// ---------------------------------------------------------------------------

pub fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

pub fn iso_at(ms: i64) -> Option<String> {
    let timestamp = jiff::Timestamp::from_millisecond(ms).ok()?;
    let zoned = timestamp.to_zoned(jiff::tz::TimeZone::UTC);
    Some(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        zoned.year(),
        zoned.month(),
        zoned.day(),
        zoned.hour(),
        zoned.minute(),
        zoned.second(),
        zoned.millisecond()
    ))
}

pub fn parse_ms(value: &str) -> Option<i64> {
    value
        .parse::<jiff::Timestamp>()
        .ok()
        .map(|timestamp| timestamp.as_millisecond())
}

/// The agent part of a backend id, dropping a device suffix.
pub fn base_backend(backend: &str) -> &str {
    backend.split_once('@').map_or(backend, |(agent, _)| agent)
}

pub fn device_of(backend: &str) -> Option<&str> {
    backend.split_once('@').map(|(_, device)| device)
}

fn display_name(backend: &str) -> Option<String> {
    match base_backend(backend) {
        "claude" => Some("Claude".into()),
        "codex" => Some("Codex".into()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Reader adapters: the engine owns the shapes; the app only supplies the
// payloads its transports hand over.
// ---------------------------------------------------------------------------

/// `CodexClient`/ACP hand the app core [`RateLimits`]; the ported Codex
/// reader wants its snapshot. This is a lossless-enough bridge for the
/// windows, plan and reached flag (credits stay on the legacy projection).
pub fn report_from_rate_limits(limits: &RateLimits, checked_at: &str) -> ProviderUsageLimits {
    let window = |window: &RateWindow| CodexRateLimitWindow {
        used_percent: window.used_percent,
        resets_at: window.resets_at,
        window_duration_mins: window.window_mins.and_then(|mins| u32::try_from(mins).ok()),
    };
    let snapshot = CodexRateLimitSnapshot {
        limit_id: Some("codex".into()),
        plan_type: limits.plan.clone(),
        rate_limit_reached_type: limits.reached.then(|| "primary".into()),
        primary: limits.primary.as_ref().map(window),
        secondary: limits.secondary.as_ref().map(window),
        individual_limit: None,
    };
    codex_rate_limits_to_limits(checked_at, &snapshot, None, None)
}

/// The legacy projection of a report, used for backends whose only source is
/// the engine report (Claude's OAuth probe, and the engine adapter seam).
pub fn rate_limits_from_report(report: &ProviderUsageLimits, backend: &str) -> RateLimits {
    let mut primary = None;
    let mut secondary = None;
    for window in &report.windows {
        let projected = RateWindow {
            used_percent: window.used_percent,
            window_mins: window.window_duration_mins.map(u64::from),
            resets_at: window
                .resets_at
                .as_deref()
                .and_then(parse_ms)
                .map(|ms| ms.div_euclid(1000)),
        };
        match window.kind {
            UsageWindowKind::Session if primary.is_none() => primary = Some(projected),
            UsageWindowKind::Weekly if secondary.is_none() => secondary = Some(projected),
            UsageWindowKind::Session
            | UsageWindowKind::Weekly
            | UsageWindowKind::Monthly
            | UsageWindowKind::Other => {
                if primary.is_none() {
                    primary = Some(projected);
                } else if secondary.is_none() {
                    secondary = Some(projected);
                }
            }
        }
    }
    RateLimits {
        name: display_name(backend),
        primary,
        secondary,
        credits: None,
        plan: None,
        reached: report
            .windows
            .iter()
            .any(|w| w.used_percent >= AT_LIMIT_PERCENT),
    }
}

/// The Claude OAuth usage body (`five_hour`/`seven_day` with `utilization`
/// and `resets_at`) in the shape the ported `get_usage` reader maps. This is
/// the same payload the local `agent_rpc` reader and a node's endpoint
/// return, so both paths share one parser.
pub fn claude_oauth_probe(body: &Value, checked_at: &str) -> ClaudeUsageProbe {
    let mut rate_limits = serde_json::Map::new();
    for key in ["five_hour", "seven_day"] {
        let window = &body[key];
        if window.is_object() {
            rate_limits.insert(key.to_string(), window.clone());
        }
    }
    let available = !rate_limits.is_empty();
    let response = serde_json::json!({
        "rate_limits_available": available,
        "rate_limits": if available {
            Value::Object(rate_limits)
        } else {
            Value::Null
        },
    });
    claude_usage_response_to_limits(&response, checked_at)
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// What a usage observation changed, for the calls that tell the team.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LimitEffects {
    pub reached: bool,
    pub was_reached: bool,
    /// Unix seconds: when the exhausted windows reset, when known.
    pub resets_at: Option<i64>,
}

/// The app's canonical usage report per backend, plus the legacy projection
/// the existing views read.
#[derive(Default)]
pub struct UsageStore {
    reports: HashMap<String, ProviderUsageLimits>,
    /// Claude's streamed bucket names learned from its probe, so a
    /// `rate_limit_event` lands on the probe's row.
    scoped_names: HashMap<String, ClaudeScopedLimitNames>,
    legacy: HashMap<String, RateLimits>,
    /// Backends whose legacy projection is computed from the report rather
    /// than received whole (Claude today).
    derived_legacy: HashSet<String>,
}

impl UsageStore {
    pub fn limits_for(&self, backend: &str) -> Option<&RateLimits> {
        self.legacy.get(backend)
    }

    pub fn report(&self, backend: &str) -> Option<&ProviderUsageLimits> {
        self.reports.get(backend)
    }

    pub fn remove(&mut self, backend: &str) {
        self.reports.remove(backend);
        self.scoped_names.remove(backend);
        self.legacy.remove(backend);
        self.derived_legacy.remove(backend);
    }

    /// A whole report that arrived whole: replace the legacy projection and
    /// derive the canonical report from it. No team/news effects.
    pub fn set_limits(
        &mut self,
        backend: &str,
        limits: RateLimits,
        checked_at: &str,
    ) -> LimitEffects {
        let was_reached = self
            .legacy
            .get(backend)
            .is_some_and(|limits| limits.reached);
        self.legacy.insert(backend.to_string(), limits);
        self.derived_legacy.remove(backend);
        self.remember_report_from_legacy(backend, checked_at);
        self.effects(backend, was_reached)
    }

    /// Merge a sparse legacy update, then re-derive the canonical report.
    /// Used by the paths that still speak `AgentEvent::RateLimitsUpdated`.
    pub fn remember_limits(
        &mut self,
        backend: &str,
        update: RateLimits,
        checked_at: &str,
    ) -> LimitEffects {
        let was_reached = self
            .legacy
            .get(backend)
            .is_some_and(|limits| limits.reached);
        self.legacy
            .entry(backend.to_string())
            .or_default()
            .merge(update);
        self.derived_legacy.remove(backend);
        self.remember_report_from_legacy(backend, checked_at);
        self.effects(backend, was_reached)
    }

    /// A complete probe from an engine reader. Keeps the last good report
    /// through a failed probe, as T3 does.
    pub fn observe_probe(
        &mut self,
        backend: &str,
        probe: ProviderUsageLimits,
    ) -> Option<LimitEffects> {
        let was_reached = self
            .legacy
            .get(backend)
            .is_some_and(|limits| limits.reached);
        let next = resolve_usage_limits_after_probe(self.reports.get(backend), Some(probe))?;
        self.reports.insert(backend.to_string(), next);
        self.refresh_derived_legacy(backend);
        Some(self.effects(backend, was_reached))
    }

    /// A complete Claude probe, remembering the scoped names its streamed
    /// events will use.
    pub fn observe_claude_probe(
        &mut self,
        backend: &str,
        probe: ClaudeUsageProbe,
    ) -> Option<LimitEffects> {
        self.scoped_names
            .insert(backend.to_string(), probe.names.clone());
        self.observe_probe(backend, probe.limits)
    }

    /// A sparse streamed update. Merged by window id, keeping reset times a
    /// percent-only update omits.
    pub fn observe_update(
        &mut self,
        backend: &str,
        update: &UsageLimitsUpdate,
        checked_at: &str,
    ) -> Option<LimitEffects> {
        let was_reached = self
            .legacy
            .get(backend)
            .is_some_and(|limits| limits.reached);
        let next = apply_usage_limits_update(self.reports.get(backend), update, checked_at)?;
        self.reports.insert(backend.to_string(), next);
        self.refresh_derived_legacy(backend);
        Some(self.effects(backend, was_reached))
    }

    /// A streamed Claude `rate_limit_event`.
    pub fn observe_claude_event(
        &mut self,
        backend: &str,
        event: &ClaudeRateLimitEvent,
        checked_at: &str,
    ) -> Option<LimitEffects> {
        let names = self.scoped_names.get(backend).cloned().unwrap_or_default();
        let update = claude_rate_limit_event_to_update(event, &names)?;
        self.observe_update(backend, &update, checked_at)
    }

    /// Every report's windows, by backend, for a list the UI can draw.
    pub fn agents(&self) -> Vec<AgentUsage> {
        let mut agents: Vec<AgentUsage> = self
            .reports
            .iter()
            .map(|(backend, report)| AgentUsage {
                backend: backend.clone(),
                device: device_of(backend).map(str::to_string),
                checked_at: report.checked_at.clone(),
                unavailable: report.unavailable.clone(),
                windows: report.windows.clone(),
            })
            .collect();
        agents.sort_by(|left, right| left.backend.cmp(&right.backend));
        agents
    }

    /// The banner for one backend, if any window is near or at its limit.
    pub fn banner(&self, backend: &str, now_ms: i64) -> Option<UsageBanner> {
        banner_for(self.reports.get(backend)?, backend, now_ms)
    }

    /// The most pressing banner among every known backend.
    pub fn strongest_banner(&self, now_ms: i64) -> Option<UsageBanner> {
        self.reports
            .iter()
            .filter_map(|(backend, report)| banner_for(report, backend, now_ms))
            .max_by(|left, right| {
                left.level.cmp(&right.level).then_with(|| {
                    left.used_percent
                        .partial_cmp(&right.used_percent)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
            })
    }

    /// One read for the UI: the banner (focus backend first), and every
    /// window of every agent and device.
    pub fn overview(&self, focus: Option<&str>, now_ms: i64) -> UsageOverview {
        let banner = focus
            .and_then(|backend| self.banner(backend, now_ms))
            .or_else(|| self.strongest_banner(now_ms));
        UsageOverview {
            banner,
            agents: self.agents(),
        }
    }

    /// The reset time a usage-limit failure should wait for: the latest
    /// exhausted window's reset, else the latest future reset.
    pub fn usage_limit_reset_at(&self, backend: &str, now_ms: i64) -> Option<String> {
        let report = self.reports.get(backend)?;
        let exhausted = report
            .windows
            .iter()
            .filter(|window| window.used_percent >= AT_LIMIT_PERCENT)
            .filter_map(|window| window.resets_at.as_deref())
            .max_by_key(|reset| parse_ms(reset).unwrap_or(i64::MIN));
        exhausted.map(str::to_string).or_else(|| {
            report
                .windows
                .iter()
                .filter_map(|window| window.resets_at.as_deref())
                .filter(|reset| parse_ms(reset).is_some_and(|reset| reset > now_ms))
                .max_by_key(|reset| parse_ms(reset).unwrap_or(i64::MIN))
                .map(str::to_string)
        })
    }

    fn remember_report_from_legacy(&mut self, backend: &str, checked_at: &str) {
        if let Some(limits) = self.legacy.get(backend) {
            let report = report_from_rate_limits(limits, checked_at);
            self.reports.insert(backend.to_string(), report);
        }
    }

    fn refresh_derived_legacy(&mut self, backend: &str) {
        let Some(report) = self.reports.get(backend) else {
            return;
        };
        let derived = self.derived_legacy.contains(backend) || !self.legacy.contains_key(backend);
        if derived {
            let limits = rate_limits_from_report(report, backend);
            self.legacy.insert(backend.to_string(), limits);
            self.derived_legacy.insert(backend.to_string());
        }
    }

    fn effects(&self, backend: &str, was_reached: bool) -> LimitEffects {
        let Some(limits) = self.legacy.get(backend) else {
            return LimitEffects::default();
        };
        let reached = limits.reached;
        let resets_at = [limits.primary, limits.secondary]
            .into_iter()
            .flatten()
            .filter(|window| window.used_percent >= AT_LIMIT_PERCENT || reached)
            .filter_map(|window| window.resets_at)
            .max();
        LimitEffects {
            reached,
            was_reached,
            resets_at,
        }
    }
}

/// One agent's or device's report as the UI reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentUsage {
    pub backend: String,
    pub device: Option<String>,
    pub checked_at: String,
    pub unavailable: Option<UsageLimitUnavailable>,
    pub windows: Vec<UsageWindow>,
}

/// The banner's severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum UsageBannerLevel {
    Near,
    At,
}

/// A window near or at its limit, and when it resets.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageBanner {
    pub backend: String,
    pub device: Option<String>,
    pub level: UsageBannerLevel,
    pub window_id: String,
    pub label: String,
    pub used_percent: f64,
    pub resets_at: Option<String>,
}

/// Everything the usage banner and details need in one call.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageOverview {
    pub banner: Option<UsageBanner>,
    pub agents: Vec<AgentUsage>,
}

/// The fullest window at or above [`NEAR_LIMIT_PERCENT`].
///
/// An exhausted window whose reset has passed no longer blocks a turn, so it
/// is not shown as "at limit" until the next report confirms it.
pub fn banner_for(report: &ProviderUsageLimits, backend: &str, now_ms: i64) -> Option<UsageBanner> {
    let window = report
        .windows
        .iter()
        .filter(|window| window.used_percent >= NEAR_LIMIT_PERCENT)
        .filter(|window| {
            window.used_percent < AT_LIMIT_PERCENT
                || window
                    .resets_at
                    .as_deref()
                    .and_then(parse_ms)
                    .is_none_or(|reset| reset > now_ms)
        })
        .max_by(|left, right| {
            left.used_percent
                .partial_cmp(&right.used_percent)
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
    Some(UsageBanner {
        backend: backend.to_string(),
        device: device_of(backend).map(str::to_string),
        level: if window.used_percent >= AT_LIMIT_PERCENT {
            UsageBannerLevel::At
        } else {
            UsageBannerLevel::Near
        },
        window_id: window.id.clone(),
        label: window.label.clone(),
        used_percent: window.used_percent,
        resets_at: window.resets_at.clone(),
    })
}

// ---------------------------------------------------------------------------
// Context window
// ---------------------------------------------------------------------------

/// A thread's context window and whether to offer a compaction.
#[derive(Clone, Debug, PartialEq)]
pub struct ContextWindowView {
    pub used_tokens: f64,
    pub total_tokens: Option<f64>,
    pub remaining_tokens: Option<f64>,
    pub used_percentage: Option<f64>,
    pub tone: Option<ContextTone>,
    pub compacts_automatically: Option<bool>,
    /// T3's stale-thread offer: Claude, at least 200K used and idle past the
    /// prompt-cache TTL.
    pub compaction_offered: bool,
}

/// Build the context view from the app's transcript usage.
///
/// This is the one decision that used to live only in the UI: it maps the
/// transcript's `context_used`/`context_window` through the ported
/// `deriveLatestContextWindowSnapshot`, then asks the ported
/// `shouldOfferResumeCompaction` whether a compaction is worth offering.
pub fn context_view(
    provider: &str,
    usage: &atrium_core::TokenUsage,
    updated_at_ms: i64,
    now_ms: i64,
) -> Option<ContextWindowView> {
    let used_tokens = usage.context_used? as f64;
    let live_usage = engine_usage::TokenUsage {
        used_tokens,
        max_tokens: usage.context_window.map(|window| window as f64),
        updated_at: iso_at(updated_at_ms)?,
        ..Default::default()
    };
    let snapshot = derive_latest_context_window_snapshot(&[], Some(&live_usage), None)?;
    let driver = if base_backend(provider) == "claude" {
        CLAUDE_DRIVER
    } else {
        base_backend(provider)
    };
    let now = iso_at(now_ms)?;
    let compaction_offered = should_offer_resume_compaction(
        Some(driver),
        Some(snapshot.used_tokens),
        Some(&snapshot.updated_at),
        None,
        &now,
    );
    Some(ContextWindowView {
        used_tokens: snapshot.used_tokens,
        total_tokens: snapshot.max_tokens,
        remaining_tokens: snapshot.remaining_tokens,
        used_percentage: snapshot.used_percentage,
        tone: context_tone(snapshot.used_percentage),
        compacts_automatically: snapshot.compacts_automatically,
        compaction_offered,
    })
}

// ---------------------------------------------------------------------------
// Continue after reset
// ---------------------------------------------------------------------------

/// The persisted continue-after-reset journal.
///
/// The decision is the engine's [`sweep`]; this wrapper only maps the stored
/// rows to candidates and records what the app did with each command.
pub struct UsageRecovery {
    journal: Option<SqliteRecoveryJournal>,
}

/// One thread whose reset has passed and that should continue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DueContinuation {
    pub command_id: String,
    pub thread_id: String,
    pub continuation_of_run_id: String,
    pub text: String,
}

impl UsageRecovery {
    pub fn disabled() -> Self {
        Self { journal: None }
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, RecoveryError> {
        Ok(Self {
            journal: Some(SqliteRecoveryJournal::open(path)?),
        })
    }

    pub fn in_memory() -> Result<Self, RecoveryError> {
        Ok(Self {
            journal: Some(SqliteRecoveryJournal::open_in_memory()?),
        })
    }

    /// The app's own database, beside the rest of its data. `ATRIUM_DATA_DIR`
    /// names the folder directly, as the sync runtime does.
    pub fn from_env() -> Self {
        let directory = std::env::var_os("ATRIUM_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs::data_dir().map(|base| base.join("Atrium")));
        let Some(directory) = directory else {
            log::warn!("usage recovery: no data directory; continue-after-reset is off");
            return Self::disabled();
        };
        if let Err(error) = std::fs::create_dir_all(&directory) {
            log::warn!(
                "usage recovery: could not create {}: {error}",
                directory.display()
            );
            return Self::disabled();
        }
        match Self::open(directory.join("usage-recovery.sqlite")) {
            Ok(recovery) => recovery,
            Err(error) => {
                log::warn!("usage recovery: could not open its store: {error}");
                Self::disabled()
            }
        }
    }

    pub fn enabled(&self) -> bool {
        self.journal.is_some()
    }

    /// Arm a thread whose turn failed on a usage limit. Returns false when
    /// there is no persisted journal.
    pub fn arm(&mut self, thread_id: &str, run_id: &str, reset_at: &str, armed_at: &str) -> bool {
        let Some(journal) = &self.journal else {
            return false;
        };
        let recovery = LimitRecovery {
            request_id: None,
            run_id: run_id.to_string(),
            reset_at: reset_at.to_string(),
            auto_resume: true,
            snooze: Some(false),
        };
        match journal.record_arm(thread_id, &recovery, armed_at) {
            Ok(()) => true,
            Err(error) => {
                log::warn!("usage recovery: could not arm {thread_id}: {error}");
                false
            }
        }
    }

    /// Every armed thread whose reset has passed, in reset order.
    pub fn due(&self, now_ms: i64) -> Vec<DueContinuation> {
        let Some(journal) = &self.journal else {
            return Vec::new();
        };
        let stored = match journal.pending() {
            Ok(stored) => stored,
            Err(error) => {
                log::warn!("usage recovery: could not read pending threads: {error}");
                return Vec::new();
            }
        };
        let candidates: Vec<LimitRecoveryCandidate> = stored.iter().map(candidate_from).collect();
        sweep(
            &candidates,
            LimitRecoverySettings {
                auto_resume: true,
                snooze: false,
            },
            now_ms,
        )
        .into_iter()
        .filter_map(|command| match command {
            LimitRecoveryCommand::Resume {
                command_id,
                thread_id,
                continuation_of_run_id,
                text,
                ..
            } => Some(DueContinuation {
                command_id,
                thread_id,
                continuation_of_run_id,
                text,
            }),
            LimitRecoveryCommand::Arm { .. } => None,
        })
        .collect()
    }

    /// Mark a thread's continuation as delivered, so a restart does not send
    /// "Continue where you left off." twice.
    pub fn record_resume(&mut self, thread_id: &str, resumed_at: &str) {
        let Some(journal) = &self.journal else {
            return;
        };
        if let Err(error) = journal.record_resume(thread_id, resumed_at) {
            log::warn!("usage recovery: could not record resume for {thread_id}: {error}");
        }
    }

    /// The armed record for a thread, for tests and diagnostics.
    pub fn stored(&self, thread_id: &str) -> Option<StoredRecovery> {
        self.journal
            .as_ref()
            .and_then(|journal| journal.load(thread_id).ok())
            .flatten()
    }
}

fn candidate_from(stored: &StoredRecovery) -> LimitRecoveryCandidate {
    LimitRecoveryCandidate {
        id: stored.thread_id.clone(),
        status: RecoveryThreadStatus::Failed,
        last_error_class: Some(ProviderFailureClass::UsageLimit),
        usage_limit_reset_at: Some(stored.reset_at.clone()),
        latest_run_id: Some(stored.run_id.clone()),
        latest_run_completed_at: Some(stored.armed_at.clone()),
        updated_at: stored.armed_at.clone(),
        archived_at: None,
        settled_override: None,
        pending_runtime_request: false,
        limit_recovery: Some(LimitRecovery {
            request_id: stored.request_id.clone(),
            run_id: stored.run_id.clone(),
            reset_at: stored.reset_at.clone(),
            auto_resume: stored.auto_resume,
            snooze: Some(stored.snooze),
        }),
        snoozed_until: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const AT: &str = "2026-09-03T12:00:00.000Z";
    const NOW_MS: i64 = 1_788_436_800_000; // 2026-09-03T12:00:00.000Z

    fn window(id: &str, kind: UsageWindowKind, used: f64, resets_at: Option<&str>) -> UsageWindow {
        UsageWindow {
            id: id.into(),
            kind,
            label: id.into(),
            model_ids: None,
            provider_ids: None,
            used_percent: used,
            resets_at: resets_at.map(str::to_string),
            window_duration_mins: Some(300),
        }
    }

    fn report(windows: Vec<UsageWindow>) -> ProviderUsageLimits {
        atrium_engine::usage::make_usage_limits(AT, windows)
    }

    fn legacy() -> RateLimits {
        RateLimits {
            name: Some("Codex".into()),
            primary: Some(RateWindow {
                used_percent: 54.0,
                window_mins: Some(300),
                resets_at: Some(NOW_MS / 1000 + 3600),
            }),
            secondary: Some(RateWindow {
                used_percent: 18.0,
                window_mins: Some(10_080),
                resets_at: None,
            }),
            credits: None,
            plan: Some("pro".into()),
            reached: false,
        }
    }

    #[test]
    fn a_codex_snapshot_becomes_the_shared_report_and_back() {
        let report = report_from_rate_limits(&legacy(), AT);
        assert_eq!(report.windows.len(), 2);
        assert_eq!(report.windows[0].id, "primary");
        assert_eq!(report.windows[0].used_percent, 54.0);
        assert_eq!(report.windows[0].kind, UsageWindowKind::Session);
        assert_eq!(report.windows[1].id, "secondary");
        assert_eq!(report.windows[1].kind, UsageWindowKind::Weekly);
        assert_eq!(report.windows[0].window_duration_mins, Some(300));

        let projected = rate_limits_from_report(&report, "codex");
        assert_eq!(projected.primary.unwrap().used_percent, 54.0);
        assert_eq!(projected.secondary.unwrap().used_percent, 18.0);
        assert!(!projected.reached);
    }

    #[test]
    fn claude_oauth_payload_runs_through_the_ported_get_usage_reader() {
        let body = json!({
            "five_hour": {"utilization": 54, "resets_at": "2026-09-03T16:00:00Z"},
            "seven_day": {"utilization": 18.4, "resets_at": null},
        });
        let probe = claude_oauth_probe(&body, AT);
        assert!(probe.limits.unavailable.is_none());
        assert_eq!(probe.limits.windows.len(), 2);
        assert_eq!(probe.limits.windows[0].id, "five_hour");
        assert_eq!(probe.limits.windows[0].used_percent, 54.0);
        assert_eq!(
            probe.limits.windows[0].resets_at.as_deref(),
            Some("2026-09-03T16:00:00.000Z")
        );
        assert_eq!(probe.limits.windows[1].used_percent, 18.4);

        let empty = claude_oauth_probe(&json!({}), AT);
        assert!(empty.limits.unavailable.is_some());
        assert!(empty.limits.windows.is_empty());
    }

    #[test]
    fn a_claude_probe_fills_both_the_report_and_the_legacy_projection() {
        let mut store = UsageStore::default();
        let body = json!({
            "five_hour": {"utilization": 91, "resets_at": null},
            "seven_day": {"utilization": 20, "resets_at": null},
        });
        let effects = store
            .observe_claude_probe("claude@studio", claude_oauth_probe(&body, AT))
            .unwrap();
        assert!(!effects.reached);
        let limits = store.limits_for("claude@studio").unwrap();
        assert_eq!(limits.primary.unwrap().used_percent, 91.0);
        assert_eq!(limits.secondary.unwrap().used_percent, 20.0);
        assert_eq!(limits.name.as_deref(), Some("Claude"));
        assert_eq!(store.agents().len(), 1);
        assert_eq!(store.agents()[0].device.as_deref(), Some("studio"));
    }

    #[test]
    fn a_sparse_update_moves_a_window_and_keeps_the_reset_it_omits() {
        let mut store = UsageStore::default();
        let _ = store.observe_probe(
            "claude",
            report(vec![
                window(
                    "five_hour",
                    UsageWindowKind::Session,
                    40.0,
                    Some("2026-09-03T13:00:00.000Z"),
                ),
                window("seven_day", UsageWindowKind::Weekly, 20.0, None),
            ]),
        );
        let update = UsageLimitsUpdate {
            windows: vec![UsageWindow {
                resets_at: None,
                ..window("five_hour", UsageWindowKind::Session, 88.0, None)
            }],
        };
        store
            .observe_update("claude", &update, "2026-09-03T12:00:05.000Z")
            .unwrap();
        let report = store.report("claude").unwrap();
        assert_eq!(report.windows[0].used_percent, 88.0);
        assert_eq!(
            report.windows[0].resets_at.as_deref(),
            Some("2026-09-03T13:00:00.000Z")
        );
        assert_eq!(
            store
                .limits_for("claude")
                .unwrap()
                .primary
                .unwrap()
                .used_percent,
            88.0
        );
        assert_eq!(
            store.banner("claude", NOW_MS).unwrap().level,
            UsageBannerLevel::Near
        );
    }

    #[test]
    fn a_claude_stream_event_lands_on_the_probe_row_it_named() {
        let mut store = UsageStore::default();
        let body = json!({
            "five_hour": {"utilization": 10, "resets_at": null},
        });
        let _ = store.observe_claude_probe("claude", claude_oauth_probe(&body, AT));
        let effects = store
            .observe_claude_event(
                "claude",
                &ClaudeRateLimitEvent {
                    rate_limit_type: Some("five_hour".into()),
                    utilization: Some(0.95),
                    resets_at: Some((NOW_MS / 1000 + 3600) as f64),
                },
                "2026-09-03T12:00:05.000Z",
            )
            .unwrap();
        assert!(!effects.was_reached);
        let report = store.report("claude").unwrap();
        assert_eq!(report.windows[0].used_percent, 95.0);
        assert_eq!(
            store.banner("claude", NOW_MS).unwrap().level,
            UsageBannerLevel::Near
        );
    }

    #[test]
    fn the_banner_is_near_at_80_and_at_100_until_the_reset_passes() {
        let future = "2026-09-03T13:00:00.000Z";
        let past = "2026-09-03T11:00:00.000Z";
        let near = report(vec![window(
            "five_hour",
            UsageWindowKind::Session,
            85.0,
            Some(future),
        )]);
        assert_eq!(
            banner_for(&near, "claude", NOW_MS).unwrap().level,
            UsageBannerLevel::Near
        );

        let at = report(vec![window(
            "five_hour",
            UsageWindowKind::Session,
            100.0,
            Some(future),
        )]);
        let banner = banner_for(&at, "codex@nas", NOW_MS).unwrap();
        assert_eq!(banner.level, UsageBannerLevel::At);
        assert_eq!(banner.device.as_deref(), Some("nas"));
        assert_eq!(banner.resets_at.as_deref(), Some(future));

        let stale = report(vec![window(
            "five_hour",
            UsageWindowKind::Session,
            100.0,
            Some(past),
        )]);
        assert!(banner_for(&stale, "claude", NOW_MS).is_none());

        let low = report(vec![window(
            "five_hour",
            UsageWindowKind::Session,
            79.9,
            None,
        )]);
        assert!(banner_for(&low, "claude", NOW_MS).is_none());
    }

    #[test]
    fn the_overview_lists_every_agent_and_device_and_prefers_the_focused_banner() {
        let mut store = UsageStore::default();
        store.set_limits("codex", legacy(), AT);
        let _ = store.observe_claude_probe(
            "claude@studio",
            claude_oauth_probe(
                &json!({"five_hour": {"utilization": 100, "resets_at": "2026-09-03T13:00:00Z"}}),
                AT,
            ),
        );
        let overview = store.overview(Some("codex"), NOW_MS);
        assert_eq!(overview.agents.len(), 2);
        assert_eq!(overview.banner.unwrap().backend, "claude@studio");
        let focused = store.overview(Some("claude@studio"), NOW_MS);
        assert_eq!(focused.banner.unwrap().level, UsageBannerLevel::At);
    }

    #[test]
    fn the_reset_to_wait_for_is_the_latest_exhausted_window() {
        let mut store = UsageStore::default();
        let _ = store.observe_probe(
            "claude",
            report(vec![
                window(
                    "five_hour",
                    UsageWindowKind::Session,
                    100.0,
                    Some("2026-09-03T13:00:00.000Z"),
                ),
                window(
                    "seven_day",
                    UsageWindowKind::Weekly,
                    100.0,
                    Some("2026-09-04T13:00:00.000Z"),
                ),
            ]),
        );
        assert_eq!(
            store.usage_limit_reset_at("claude", NOW_MS).as_deref(),
            Some("2026-09-04T13:00:00.000Z")
        );
    }

    #[test]
    fn the_context_view_reports_used_total_and_the_stale_compaction_offer() {
        let usage = atrium_core::TokenUsage {
            context_used: Some(210_000),
            context_window: Some(400_000),
            ..Default::default()
        };
        let fresh = context_view("claude", &usage, NOW_MS - 5 * 60 * 1000, NOW_MS).unwrap();
        assert_eq!(fresh.used_tokens, 210_000.0);
        assert_eq!(fresh.total_tokens, Some(400_000.0));
        assert!(
            (fresh.used_percentage.unwrap() - 52.5).abs() < 1e-9,
            "{:?}",
            fresh.used_percentage
        );
        assert!(!fresh.compaction_offered);

        let stale = context_view("claude", &usage, NOW_MS - 80 * 60 * 1000, NOW_MS).unwrap();
        assert!(stale.compaction_offered);

        let small = context_view(
            "claude",
            &atrium_core::TokenUsage {
                context_used: Some(120_000),
                context_window: Some(400_000),
                ..Default::default()
            },
            NOW_MS - 80 * 60 * 1000,
            NOW_MS,
        )
        .unwrap();
        assert!(!small.compaction_offered);
        assert_eq!(small.tone, None);

        let nearly_full = context_view(
            "codex",
            &atrium_core::TokenUsage {
                context_used: Some(365_000),
                context_window: Some(400_000),
                ..Default::default()
            },
            NOW_MS,
            NOW_MS,
        )
        .unwrap();
        assert_eq!(nearly_full.tone, Some(ContextTone::Error));
    }

    #[test]
    fn an_armed_reset_waits_then_resumes_and_survives_reopening() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage-recovery.sqlite");
        let armed_at = iso_at(NOW_MS).unwrap();
        let reset_at = iso_at(NOW_MS + 60 * 60 * 1000).unwrap();
        {
            let mut recovery = UsageRecovery::open(&path).unwrap();
            assert!(recovery.arm("thread:1", "run:1", &reset_at, &armed_at));
            assert!(recovery.due(NOW_MS).is_empty());
        }
        let recovery = UsageRecovery::open(&path).unwrap();
        let due = recovery.due(NOW_MS + 2 * 60 * 60 * 1000);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].thread_id, "thread:1");
        assert_eq!(due[0].continuation_of_run_id, "run:1");
        assert_eq!(
            due[0].text,
            atrium_engine::usage::recovery::LIMIT_RECOVERY_TEXT
        );

        let mut recovery = recovery;
        recovery.record_resume("thread:1", &iso_at(NOW_MS + 2 * 60 * 60 * 1000).unwrap());
        assert!(recovery.due(NOW_MS + 3 * 60 * 60 * 1000).is_empty());
        assert!(recovery.stored("thread:1").unwrap().resumed_at.is_some());
    }

    #[test]
    fn a_disabled_recovery_store_never_arms_or_resumes() {
        let mut recovery = UsageRecovery::disabled();
        assert!(!recovery.enabled());
        assert!(!recovery.arm("thread:1", "run:1", "2026-09-03T13:00:00.000Z", AT));
        assert!(recovery.due(NOW_MS + 60 * 60 * 1000).is_empty());
    }
}
