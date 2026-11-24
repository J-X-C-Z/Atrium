// Ported from apps/server/src/provider/codexUsageLimits.ts (T3 Code v0.1.11, MIT).
//! Codex's rate-limit snapshot, reset credits and printed reset times.
//!
//! Codex reports `primary` and `secondary` by position, not by duration. A
//! paid plan puts 5-hour/weekly windows there; Free and Go use monthly ones.
//! The snapshot is also partial on token ticks, so a merge keeps fields the
//! update does not name and lets an explicit `null` clear one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::limits::{
    ProviderResetCredits, ProviderUsageLimits, UsageLimitsUpdate, UsageWindow, UsageWindowKind,
    clamp_percent, format_iso_millis, iso_from_epoch_seconds, make_usage_limits, parse_ms,
};

const SESSION_MINS: u32 = 5 * 60;
const WEEK_MINS: u32 = 7 * 24 * 60;
const MONTH_MINS: u32 = 30 * 24 * 60;

/// One positional rate-limit window in Codex's snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexRateLimitWindow {
    pub used_percent: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_duration_mins: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexIndividualLimit {
    pub remaining_percent: f64,
    pub resets_at: i64,
}

/// T3's `CodexRateLimitSnapshot`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexRateLimitSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_reached_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<CodexRateLimitWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary: Option<CodexRateLimitWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub individual_limit: Option<CodexIndividualLimit>,
}

/// A partial notification. `Some(None)` is an explicit `null` that clears a
/// field; `None` leaves the previous value alone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CodexRateLimitPatch {
    pub limit_id: Option<Option<String>>,
    pub plan_type: Option<Option<String>>,
    pub rate_limit_reached_type: Option<Option<String>>,
    pub primary: Option<Option<CodexRateLimitWindow>>,
    pub secondary: Option<Option<CodexRateLimitWindow>>,
    pub individual_limit: Option<Option<CodexIndividualLimit>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexResetCredit {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexResetCreditsSummary {
    pub available_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<Vec<CodexResetCredit>>,
}

/// Why a usage read failed. T3 maps each `CodexAppServerError` tag to one of
/// these outcomes without forwarding the provider's message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodexUsageReadError {
    Request { code: i32 },
    Spawn,
    ProcessExited,
    Other,
}

/// T3's `codexRateLimitsFailureMessage`.
pub fn codex_rate_limits_failure_message(error: &CodexUsageReadError) -> String {
    match error {
        CodexUsageReadError::Request { code } => {
            format!("Codex could not read usage (JSON-RPC {code}).")
        }
        CodexUsageReadError::Spawn => "Codex could not be started to read usage.".into(),
        CodexUsageReadError::ProcessExited => "Codex exited before it could report usage.".into(),
        CodexUsageReadError::Other => "Codex did not answer the usage request.".into(),
    }
}

fn kind_for_duration(mins: u32) -> UsageWindowKind {
    if mins >= MONTH_MINS {
        UsageWindowKind::Monthly
    } else if mins >= WEEK_MINS {
        UsageWindowKind::Weekly
    } else {
        UsageWindowKind::Session
    }
}

fn label_for_kind(kind: UsageWindowKind) -> &'static str {
    match kind {
        UsageWindowKind::Session => "Session",
        UsageWindowKind::Weekly => "Weekly",
        UsageWindowKind::Monthly => "Monthly",
        UsageWindowKind::Other => "Other",
    }
}

/// `primary`/`secondary` are positions; without `windowDurationMins`, paid
/// plans use 5-hour/weekly windows and Free/Go use monthly.
fn codex_rate_limits_to_windows(snapshot: &CodexRateLimitSnapshot) -> Vec<UsageWindow> {
    if snapshot.limit_id.as_deref().is_some_and(|id| id != "codex") {
        return Vec::new();
    }
    let is_monthly_plan = matches!(snapshot.plan_type.as_deref(), Some("free" | "go"));
    let positions = [
        (
            "primary",
            snapshot.primary.as_ref(),
            if is_monthly_plan {
                MONTH_MINS
            } else {
                SESSION_MINS
            },
        ),
        ("secondary", snapshot.secondary.as_ref(), WEEK_MINS),
    ];
    let mut windows = Vec::new();
    for (id, window, fallback_mins) in positions {
        let Some(window) = window else {
            continue;
        };
        if !window.used_percent.is_finite() {
            continue;
        }
        let window_duration_mins = window.window_duration_mins.unwrap_or(fallback_mins);
        let kind = kind_for_duration(window_duration_mins);
        windows.push(UsageWindow {
            id: id.into(),
            kind,
            label: label_for_kind(kind).into(),
            model_ids: None,
            provider_ids: None,
            used_percent: clamp_percent(window.used_percent),
            resets_at: iso_from_epoch_seconds(window.resets_at.map(|value| value as f64)),
            window_duration_mins: Some(window_duration_mins),
        });
    }
    if let Some(individual) = snapshot.individual_limit.as_ref()
        && individual.remaining_percent.is_finite()
    {
        windows.push(UsageWindow {
            id: "individual".into(),
            kind: UsageWindowKind::Monthly,
            label: "Monthly credit limit".into(),
            model_ids: None,
            provider_ids: None,
            used_percent: clamp_percent(100.0 - individual.remaining_percent),
            resets_at: iso_from_epoch_seconds(Some(individual.resets_at as f64)),
            window_duration_mins: None,
        });
    }
    windows
}

fn codex_reset_credits_to_contract(
    summary: Option<&CodexResetCreditsSummary>,
) -> Option<ProviderResetCredits> {
    let summary = summary?;
    let next_expires_at = summary
        .credits
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|credit| credit.status == "available")
        .filter_map(|credit| credit.expires_at)
        .min()
        .and_then(|value| format_iso_millis(value.saturating_mul(1000)));
    Some(ProviderResetCredits {
        available_count: summary.available_count,
        next_expires_at,
        next_credit_id: None,
        options: None,
    })
}

/// Translate a full snapshot into the shared report.
pub fn codex_rate_limits_to_limits(
    checked_at: &str,
    snapshot: &CodexRateLimitSnapshot,
    rate_limits_by_limit_id: Option<&BTreeMap<String, CodexRateLimitSnapshot>>,
    reset_credits: Option<&CodexResetCreditsSummary>,
) -> ProviderUsageLimits {
    let reset_credits = codex_reset_credits_to_contract(reset_credits);
    let selected = rate_limits_by_limit_id
        .and_then(|by_id| by_id.get("codex"))
        .unwrap_or(snapshot);
    let mut limits = make_usage_limits(checked_at, codex_rate_limits_to_windows(selected));
    limits.reset_credits = reset_credits;
    limits
}

/// A sparse update only carries windows that differ from what was published.
pub fn codex_rate_limits_to_update(
    snapshot: &CodexRateLimitSnapshot,
    previous: Option<&CodexRateLimitSnapshot>,
) -> Option<UsageLimitsUpdate> {
    let previous_windows = previous
        .map(codex_rate_limits_to_windows)
        .unwrap_or_default();
    let windows: Vec<UsageWindow> = codex_rate_limits_to_windows(snapshot)
        .into_iter()
        .filter(|window| {
            let existing = previous_windows
                .iter()
                .find(|candidate| candidate.id == window.id);
            existing.is_none_or(|existing| {
                existing.kind != window.kind
                    || existing.used_percent != window.used_percent
                    || existing.resets_at != window.resets_at
                    || existing.window_duration_mins != window.window_duration_mins
            })
        })
        .collect();
    (!windows.is_empty()).then_some(UsageLimitsUpdate { windows })
}

/// Merge a partial notification into the session snapshot.
///
/// Model-specific snapshots must not replace the main allowance.
pub fn merge_codex_rate_limits(
    previous: Option<&CodexRateLimitSnapshot>,
    update: &CodexRateLimitPatch,
) -> Option<CodexRateLimitSnapshot> {
    if update
        .limit_id
        .as_ref()
        .and_then(|value| value.as_deref())
        .is_some_and(|id| id != "codex")
    {
        return previous.cloned();
    }
    let mut merged = previous.cloned().unwrap_or_default();
    if let Some(value) = &update.limit_id {
        merged.limit_id = value.clone();
    }
    if let Some(value) = &update.plan_type {
        merged.plan_type = value.clone();
    }
    if let Some(value) = &update.rate_limit_reached_type {
        merged.rate_limit_reached_type = value.clone();
    }
    if let Some(value) = &update.primary {
        merged.primary = value.clone();
    }
    if let Some(value) = &update.secondary {
        merged.secondary = value.clone();
    }
    if let Some(value) = &update.individual_limit {
        merged.individual_limit = value.clone();
    }
    Some(merged)
}

/// All exhausted windows must reset before a continuation can run.
pub fn codex_usage_limit_reset_at(snapshot: Option<&CodexRateLimitSnapshot>) -> Option<String> {
    let snapshot = snapshot?;
    let exhausted: Vec<UsageWindow> = codex_rate_limits_to_windows(snapshot)
        .into_iter()
        .filter(|window| window.used_percent >= 100.0)
        .collect();
    if exhausted.is_empty() || exhausted.iter().any(|window| window.resets_at.is_none()) {
        return None;
    }
    exhausted
        .iter()
        .filter_map(|window| window.resets_at.as_deref())
        .max_by_key(|reset| parse_ms(reset).unwrap_or(i64::MIN))
        .map(str::to_owned)
}

/// The zone Codex prints reset times in: its own `TZ`, else the server's.
pub fn codex_time_zone(environment: &BTreeMap<String, String>) -> jiff::tz::TimeZone {
    environment
        .get("TZ")
        .map(|zone| zone.trim_start_matches(':'))
        .and_then(|zone| jiff::tz::TimeZone::get(zone).ok())
        .unwrap_or_else(jiff::tz::TimeZone::system)
}

fn month_index(month: &str) -> Option<i8> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    MONTHS
        .iter()
        .position(|candidate| *candidate == month)
        .map(|index| index as i8 + 1)
}

fn take_digits(input: &str, max: usize) -> Option<(u32, &str)> {
    let count = input
        .bytes()
        .take(max)
        .take_while(u8::is_ascii_digit)
        .count();
    if count == 0 {
        return None;
    }
    let (digits, rest) = input.split_at(count);
    Some((digits.parse().ok()?, rest))
}

/// Parse `Nov 11th, 2026` and leave the time that follows.
fn take_month_day_year(input: &str) -> Option<(i8, i8, i16, &str)> {
    if input.len() < 4 {
        return None;
    }
    let (month, rest) = input.split_at(3);
    let month = month_index(month)?;
    let rest = rest.strip_prefix(' ')?;
    let (day, rest) = take_digits(rest, 2)?;
    let rest = rest
        .strip_prefix("st")
        .or_else(|| rest.strip_prefix("nd"))
        .or_else(|| rest.strip_prefix("rd"))
        .or_else(|| rest.strip_prefix("th"))?;
    let rest = rest.strip_prefix(", ")?;
    let (year, rest) = take_digits(rest, 4)?;
    let year: i16 = year.try_into().ok()?;
    let rest = rest.strip_prefix(' ')?;
    Some((month, day as i8, year, rest))
}

/// Parse the clock portion `3:37 PM.` into a 24-hour hour and minute.
fn take_clock(input: &str) -> Option<(i8, i8)> {
    let (hour, rest) = take_digits(input, 2)?;
    let rest = rest.strip_prefix(':')?;
    let (minute, rest) = take_digits(rest, 2)?;
    let (meridiem, rest) = if let Some(rest) = rest.strip_prefix(" PM") {
        ("PM", rest)
    } else {
        ("AM", rest.strip_prefix(" AM")?)
    };
    rest.strip_prefix('.')?;
    let hour = (hour % 12) as i8 + if meridiem == "PM" { 12 } else { 0 };
    Some((hour, minute as i8))
}

/// Codex writes a usage limit's reset into its error text in its own local
/// time: "Try again at 3:37 PM." for today, else "try again at Nov 11th, 2026
/// 3:37 PM.". `failed_at` fixes the date of the short form.
///
/// The timestamp is the minute after the printed time because Codex drops the
/// seconds. `None` means the text has no usable future reset.
pub fn codex_message_reset_at(
    message: &str,
    failed_at: &str,
    zone: &jiff::tz::TimeZone,
) -> Option<String> {
    let failed_ms = parse_ms(failed_at)?;
    let failed = failed_at.parse::<jiff::Timestamp>().ok()?;
    let failed_zoned = failed.to_zoned(zone.clone());

    let rest = message
        .find("Try again at ")
        .map(|index| &message[index + "Try again at ".len()..])
        .or_else(|| {
            message
                .find("try again at ")
                .map(|index| &message[index + "try again at ".len()..])
        })?;

    let (year, month, day, rest) = match take_month_day_year(rest) {
        Some((month, day, year, rest)) => (year, month, day, rest),
        None => (
            failed_zoned.year(),
            failed_zoned.month(),
            failed_zoned.day(),
            rest,
        ),
    };
    let (hour, minute) = take_clock(rest)?;

    let civil = jiff::civil::DateTime::new(year, month, day, hour, minute, 0, 0).ok()?;
    // The repeated hour at a DST fall-back takes its later instant so a resume
    // never fires early.
    let reset = zone.to_ambiguous_zoned(civil).later().ok()?;
    let reset_ms = reset.timestamp().as_millisecond().checked_add(60_000)?;
    (reset_ms > failed_ms).then(|| format_iso_millis(reset_ms))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const CHECKED_AT: &str = "2026-07-18T10:00:00.000Z";

    fn window(used: f64, reset: Option<i64>, mins: Option<u32>) -> CodexRateLimitWindow {
        CodexRateLimitWindow {
            used_percent: used,
            resets_at: reset,
            window_duration_mins: mins,
        }
    }

    fn snapshot(
        primary: Option<CodexRateLimitWindow>,
        secondary: Option<CodexRateLimitWindow>,
    ) -> CodexRateLimitSnapshot {
        CodexRateLimitSnapshot {
            primary,
            secondary,
            ..Default::default()
        }
    }

    #[test]
    fn shows_an_enterprise_monthly_credit_allowance_without_a_duration() {
        let enterprise = CodexRateLimitSnapshot {
            limit_id: Some("codex".into()),
            plan_type: Some("enterprise_cbp_usage_based".into()),
            individual_limit: Some(CodexIndividualLimit {
                remaining_percent: 99.0,
                resets_at: 1_784_500_000,
            }),
            ..Default::default()
        };
        let mut by_id = BTreeMap::new();
        by_id.insert("codex".into(), enterprise);
        let main = CodexRateLimitSnapshot {
            limit_id: Some("spark".into()),
            ..Default::default()
        };
        let expected = vec![UsageWindow {
            id: "individual".into(),
            kind: UsageWindowKind::Monthly,
            label: "Monthly credit limit".into(),
            model_ids: None,
            provider_ids: None,
            used_percent: 1.0,
            resets_at: Some("2026-07-19T22:26:40.000Z".into()),
            window_duration_mins: None,
        }];
        assert_eq!(
            codex_rate_limits_to_limits(CHECKED_AT, &main, Some(&by_id), None).windows,
            expected
        );
        assert_eq!(
            codex_rate_limits_to_update(by_id.get("codex").unwrap(), None)
                .unwrap()
                .windows,
            expected
        );
    }

    #[test]
    fn maps_primary_and_secondary_onto_the_session_and_weekly_windows() {
        let snapshot = CodexRateLimitSnapshot {
            plan_type: Some("plus".into()),
            ..snapshot(
                Some(window(12.0, Some(1_784_000_000), Some(300))),
                Some(window(47.0, Some(1_784_500_000), Some(10_080))),
            )
        };
        assert_eq!(
            codex_rate_limits_to_limits(CHECKED_AT, &snapshot, None, None).windows,
            vec![
                UsageWindow {
                    id: "primary".into(),
                    kind: UsageWindowKind::Session,
                    label: "Session".into(),
                    model_ids: None,
                    provider_ids: None,
                    used_percent: 12.0,
                    resets_at: Some("2026-07-14T03:33:20.000Z".into()),
                    window_duration_mins: Some(300),
                },
                UsageWindow {
                    id: "secondary".into(),
                    kind: UsageWindowKind::Weekly,
                    label: "Weekly".into(),
                    model_ids: None,
                    provider_ids: None,
                    used_percent: 47.0,
                    resets_at: Some("2026-07-19T22:26:40.000Z".into()),
                    window_duration_mins: Some(10_080),
                },
            ]
        );
    }

    #[test]
    fn treats_a_lone_duration_less_primary_as_monthly_on_free_and_go() {
        let snapshot = CodexRateLimitSnapshot {
            plan_type: Some("free".into()),
            ..snapshot(Some(window(80.0, None, None)), None)
        };
        assert_eq!(
            codex_rate_limits_to_limits(CHECKED_AT, &snapshot, None, None).windows,
            vec![UsageWindow {
                id: "primary".into(),
                kind: UsageWindowKind::Monthly,
                label: "Monthly".into(),
                model_ids: None,
                provider_ids: None,
                used_percent: 80.0,
                resets_at: None,
                window_duration_mins: Some(43_200),
            }]
        );
    }

    #[test]
    fn selects_the_main_allowance_and_leaves_spark_out() {
        let spark = CodexRateLimitSnapshot {
            limit_id: Some("codex_bengalfox".into()),
            individual_limit: Some(CodexIndividualLimit {
                remaining_percent: 99.0,
                resets_at: 1_784_500_000,
            }),
            ..snapshot(
                Some(window(0.0, None, Some(300))),
                Some(window(90.0, None, Some(10_080))),
            )
        };
        let mut by_id = BTreeMap::new();
        by_id.insert("codex_bengalfox".into(), spark.clone());
        by_id.insert(
            "codex".into(),
            snapshot(None, Some(window(42.0, None, Some(10_080)))),
        );
        assert_eq!(
            codex_rate_limits_to_limits(CHECKED_AT, &spark, Some(&by_id), None).windows,
            vec![UsageWindow {
                id: "secondary".into(),
                kind: UsageWindowKind::Weekly,
                label: "Weekly".into(),
                model_ids: None,
                provider_ids: None,
                used_percent: 42.0,
                resets_at: None,
                window_duration_mins: Some(10_080),
            }]
        );
    }

    #[test]
    fn does_not_republish_cached_windows_over_a_newer_account_probe() {
        let cached = snapshot(
            Some(window(0.0, Some(1_784_000_000), Some(300))),
            Some(window(20.0, Some(1_784_500_000), Some(10_080))),
        );
        assert_eq!(
            codex_rate_limits_to_update(&cached, None)
                .unwrap()
                .windows
                .len(),
            2
        );
        assert!(codex_rate_limits_to_update(&cached, Some(&cached)).is_none());
        let changed = CodexRateLimitSnapshot {
            secondary: Some(window(21.0, Some(1_784_500_000), Some(10_080))),
            ..cached.clone()
        };
        assert_eq!(
            codex_rate_limits_to_update(&changed, Some(&cached))
                .unwrap()
                .windows
                .iter()
                .map(|window| window.id.as_str())
                .collect::<Vec<_>>(),
            vec!["secondary"]
        );
        let primary_only = CodexRateLimitSnapshot {
            primary: cached.primary.clone(),
            ..Default::default()
        };
        assert!(codex_rate_limits_to_update(&primary_only, Some(&cached)).is_none());
        let changed_reset = CodexRateLimitSnapshot {
            primary: Some(window(0.0, Some(1_784_001_000), Some(300))),
            ..Default::default()
        };
        assert_eq!(
            codex_rate_limits_to_update(&changed_reset, Some(&cached))
                .unwrap()
                .windows
                .len(),
            1
        );
        let changed_percent = CodexRateLimitSnapshot {
            primary: Some(window(76.0, Some(1_784_000_000), Some(300))),
            ..Default::default()
        };
        assert_eq!(
            codex_rate_limits_to_update(&changed_percent, Some(&cached))
                .unwrap()
                .windows[0]
                .used_percent,
            76.0
        );
    }

    #[test]
    fn carries_only_the_windows_a_notification_names() {
        assert_eq!(
            codex_rate_limits_to_update(
                &snapshot(None, Some(window(51.0, None, Some(10_080)))),
                None
            )
            .unwrap()
            .windows,
            vec![UsageWindow {
                id: "secondary".into(),
                kind: UsageWindowKind::Weekly,
                label: "Weekly".into(),
                model_ids: None,
                provider_ids: None,
                used_percent: 51.0,
                resets_at: None,
                window_duration_mins: Some(10_080),
            }]
        );
        assert!(
            codex_rate_limits_to_update(
                &CodexRateLimitSnapshot {
                    plan_type: Some("plus".into()),
                    ..Default::default()
                },
                None
            )
            .is_none()
        );
    }

    #[test]
    fn ignores_spark_notifications_so_they_cannot_overwrite_the_main_allowance() {
        let spark = CodexRateLimitSnapshot {
            limit_id: Some("codex_bengalfox".into()),
            individual_limit: Some(CodexIndividualLimit {
                remaining_percent: 99.0,
                resets_at: 1_784_500_000,
            }),
            ..snapshot(
                Some(window(0.0, None, Some(300))),
                Some(window(90.0, None, Some(10_080))),
            )
        };
        assert!(codex_rate_limits_to_update(&spark, None).is_none());
        assert_eq!(
            codex_rate_limits_to_update(
                &CodexRateLimitSnapshot {
                    limit_id: Some("codex".into()),
                    ..snapshot(None, Some(window(42.0, None, Some(10_080))))
                },
                None
            )
            .unwrap()
            .windows,
            vec![UsageWindow {
                id: "secondary".into(),
                kind: UsageWindowKind::Weekly,
                label: "Weekly".into(),
                model_ids: None,
                provider_ids: None,
                used_percent: 42.0,
                resets_at: None,
                window_duration_mins: Some(10_080),
            }]
        );
    }

    #[test]
    fn keeps_the_json_rpc_code_and_nothing_else_from_a_request_failure() {
        assert_eq!(
            codex_rate_limits_failure_message(&CodexUsageReadError::Request { code: -32603 }),
            "Codex could not read usage (JSON-RPC -32603)."
        );
    }

    #[test]
    fn updates_monthly_credit_usage_retains_it_when_omitted_and_clears_an_explicit_null() {
        let base = CodexRateLimitSnapshot {
            limit_id: Some("codex".into()),
            primary: Some(CodexRateLimitWindow {
                used_percent: 12.0,
                resets_at: None,
                window_duration_mins: None,
            }),
            individual_limit: Some(CodexIndividualLimit {
                remaining_percent: 99.0,
                resets_at: 1_784_500_000,
            }),
            ..Default::default()
        };
        let individual_limit = CodexIndividualLimit {
            remaining_percent: 75.0,
            resets_at: 1_787_000_000,
        };
        let changed = merge_codex_rate_limits(
            Some(&base),
            &CodexRateLimitPatch {
                individual_limit: Some(Some(individual_limit.clone())),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            codex_rate_limits_to_update(&changed, None).unwrap().windows,
            vec![
                UsageWindow {
                    id: "primary".into(),
                    kind: UsageWindowKind::Session,
                    label: "Session".into(),
                    model_ids: None,
                    provider_ids: None,
                    used_percent: 12.0,
                    resets_at: None,
                    window_duration_mins: Some(300),
                },
                UsageWindow {
                    id: "individual".into(),
                    kind: UsageWindowKind::Monthly,
                    label: "Monthly credit limit".into(),
                    model_ids: None,
                    provider_ids: None,
                    used_percent: 25.0,
                    resets_at: Some("2026-08-17T20:53:20.000Z".into()),
                    window_duration_mins: None,
                },
            ]
        );
        let retained = merge_codex_rate_limits(
            Some(&changed),
            &CodexRateLimitPatch {
                primary: Some(None),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(retained.primary, None);
        assert_eq!(retained.individual_limit, Some(individual_limit.clone()));
        let cleared = merge_codex_rate_limits(
            Some(&retained),
            &CodexRateLimitPatch {
                individual_limit: Some(None),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(cleared.primary, None);
        assert_eq!(cleared.individual_limit, None);
        assert!(
            codex_rate_limits_to_limits(CHECKED_AT, &cleared, None, None)
                .windows
                .is_empty()
        );
    }

    #[test]
    fn ignores_a_model_specific_snapshot_so_it_cannot_replace_the_main_allowance() {
        let main = CodexRateLimitSnapshot {
            limit_id: Some("codex".into()),
            primary: Some(window(100.0, Some(1_800_000_000), Some(300))),
            ..Default::default()
        };
        let merged = merge_codex_rate_limits(
            Some(&main),
            &CodexRateLimitPatch {
                limit_id: Some(Some("spark".into())),
                individual_limit: Some(Some(CodexIndividualLimit {
                    remaining_percent: 99.0,
                    resets_at: 1_784_500_000,
                })),
                primary: Some(Some(window(3.0, Some(1_800_000_000), Some(300)))),
                ..Default::default()
            },
        );
        assert_eq!(merged.as_ref(), Some(&main));
    }

    #[test]
    fn waits_for_every_exhausted_window_and_never_invents_an_unknown_reset() {
        assert_eq!(
            codex_usage_limit_reset_at(Some(&snapshot(
                Some(window(100.0, Some(2_000_000_000), None)),
                Some(window(100.0, Some(2_000_100_000), None)),
            ))),
            Some("2033-05-19T07:20:00.000Z".into())
        );
        assert_eq!(
            codex_usage_limit_reset_at(Some(&snapshot(Some(window(100.0, None, None)), None))),
            None
        );
        assert_eq!(
            codex_usage_limit_reset_at(Some(&snapshot(
                Some(window(50.0, Some(2_000_000_000), None)),
                None
            ))),
            None
        );
    }

    #[test]
    fn reads_codex_same_day_and_dated_reset_text_in_codex_zone() {
        let dhaka = jiff::tz::TimeZone::get("Asia/Dhaka").unwrap();
        let failed_at = "2026-11-07T03:00:00Z"; // 09:00 in Dhaka
        assert_eq!(
            codex_message_reset_at(
                "You’ve hit your usage limit. Try again at 3:37 PM.",
                failed_at,
                &dhaka,
            ),
            Some("2026-11-07T09:38:00.000Z".into())
        );
        assert_eq!(
            codex_message_reset_at(
                "Upgrade, or try again at Dec 1st, 2026 12:05 AM.",
                failed_at,
                &dhaka,
            ),
            Some("2026-11-30T18:06:00.000Z".into())
        );
        assert_eq!(
            codex_message_reset_at("Try again at 12:30 PM.", failed_at, &dhaka),
            Some("2026-11-07T06:31:00.000Z".into())
        );
    }

    #[test]
    fn dates_todays_form_by_when_codex_failed_across_local_midnight() {
        let dhaka = jiff::tz::TimeZone::get("Asia/Dhaka").unwrap();
        assert_eq!(
            codex_message_reset_at("Try again at 11:59 PM.", "2026-11-07T17:59:59Z", &dhaka),
            Some("2026-11-07T18:00:00.000Z".into())
        );
    }

    #[test]
    fn takes_the_later_instant_of_a_repeated_dst_hour() {
        let new_york = jiff::tz::TimeZone::get("America/New_York").unwrap();
        for failed_at in ["2026-11-01T05:15:00Z", "2026-11-01T06:15:00Z"] {
            assert_eq!(
                codex_message_reset_at("Try again at 1:30 AM.", failed_at, &new_york),
                Some("2026-11-01T06:31:00.000Z".into())
            );
        }
    }

    #[test]
    fn leaves_the_reset_unknown_without_reset_text_or_once_it_has_passed() {
        let dhaka = jiff::tz::TimeZone::get("Asia/Dhaka").unwrap();
        let failed_at = "2026-11-07T03:00:00Z";
        assert_eq!(
            codex_message_reset_at(
                "You’ve hit your usage limit. Try again later.",
                failed_at,
                &dhaka
            ),
            None
        );
        assert_eq!(
            codex_message_reset_at(
                "exceeded retry limit, last status: 429 Too Many Requests",
                failed_at,
                &dhaka
            ),
            None
        );
        assert_eq!(
            codex_message_reset_at("Try again at 8:15 AM.", failed_at, &dhaka),
            None
        );
    }

    #[test]
    fn uses_codex_tz_and_falls_back_to_the_server_zone() {
        let mut environment = BTreeMap::new();
        environment.insert("TZ".into(), ":Asia/Dhaka".into());
        assert_eq!(
            codex_time_zone(&environment).iana_name(),
            Some("Asia/Dhaka")
        );
        let mut invalid = BTreeMap::new();
        invalid.insert("TZ".into(), "not/a-zone".into());
        let system = jiff::tz::TimeZone::system();
        assert_eq!(codex_time_zone(&invalid).iana_name(), system.iana_name());
        assert_eq!(
            codex_time_zone(&BTreeMap::new()).iana_name(),
            system.iana_name()
        );
    }
}
