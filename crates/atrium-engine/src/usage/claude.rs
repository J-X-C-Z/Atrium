// Ported from apps/server/src/provider/claudeUsageLimits.ts (T3 Code v0.1.11, MIT).
//! Claude's `get_usage` probe and its streamed `rate_limit_event`s.
//!
//! The probe reports percentages already on a 0-100 scale and an optional
//! model-scoped weekly allowance. Streamed events report utilization on a 0-1
//! scale. Both land on the same window ids so a mid-turn update replaces the
//! probe's row instead of adding a second one.

use serde_json::Value;

use super::limits::{
    ProviderUsageLimits, UsageLimitUnavailableReason, UsageLimitsUpdate, UsageWindow,
    UsageWindowKind, canonical_iso, clamp_percent, iso_from_epoch_seconds,
    make_unavailable_usage_limits, make_usage_limits, parse_ms,
};

const SESSION_MINS: u32 = 5 * 60;
const WEEK_MINS: u32 = 7 * 24 * 60;
const SCOPED_WINDOW_PREFIX: &str = "seven_day_";

/// The type the CLI uses for the overage-included weekly bucket.
const OVERAGE_INCLUDED_EVENT_TYPE: &str = "seven_day_overage_included";

/// Display names learned from a probe, reused for streamed bucket types.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClaudeScopedLimitNames {
    pub overage_included: Option<String>,
}

/// The pieces of `SDKControlGetUsageResponse` this reader needs.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaudeUsageProbe {
    pub limits: ProviderUsageLimits,
    pub names: ClaudeScopedLimitNames,
}

/// The pieces of `SDKRateLimitInfo` this reader needs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClaudeRateLimitEvent {
    pub rate_limit_type: Option<String>,
    pub utilization: Option<f64>,
    /// Epoch seconds.
    pub resets_at: Option<f64>,
}

fn scope_token(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut in_separator = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            in_separator = false;
        } else if !in_separator {
            out.push('_');
            in_separator = true;
        }
    }
    out
}

fn scoped_window_id(display_name: &str) -> String {
    format!("{SCOPED_WINDOW_PREFIX}{}", scope_token(display_name))
}

/// A model-scoped weekly allowance blocks only its own model; an unknown
/// model is not blocked.
pub fn window_limits_model(window: &UsageWindow, model: Option<&str>) -> bool {
    if !window.id.starts_with(SCOPED_WINDOW_PREFIX) {
        return true;
    }
    let suffix = &window.id[SCOPED_WINDOW_PREFIX.len()..];
    model.is_some_and(|model| scope_token(model).contains(suffix))
}

fn scoped_window(display_name: &str, used_percent: f64, resets_at: Option<String>) -> UsageWindow {
    UsageWindow {
        id: scoped_window_id(display_name),
        kind: UsageWindowKind::Weekly,
        label: format!("Weekly · {display_name}"),
        model_ids: None,
        provider_ids: None,
        used_percent: clamp_percent(used_percent),
        resets_at,
        window_duration_mins: Some(WEEK_MINS),
    }
}

fn percent_from_amounts(used: Option<f64>, limit: Option<f64>) -> Option<f64> {
    let (used, limit) = (used?, limit?);
    if !used.is_finite() || !limit.is_finite() || used < 0.0 || limit <= 0.0 {
        return None;
    }
    let percent = used / limit * 100.0;
    percent.is_finite().then(|| clamp_percent(percent))
}

fn number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|value| value.is_finite())
}

fn string(value: &Value) -> Option<&str> {
    value.as_str()
}

fn read_spend_percent(rate_limits: &Value) -> Option<f64> {
    let spend = rate_limits.get("spend").filter(|value| value.is_object());
    if let Some(spend) = spend {
        let enabled = spend
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let (used, limit) = (spend.get("used"), spend.get("limit"));
        if enabled && used.is_some_and(Value::is_object) && limit.is_some_and(Value::is_object) {
            let used = used.unwrap();
            let limit = limit.unwrap();
            let used_currency = used
                .get("currency")
                .and_then(string)
                .map(str::trim)
                .unwrap_or("");
            let limit_currency = limit
                .get("currency")
                .and_then(string)
                .map(str::trim)
                .unwrap_or("");
            let same_currency = !used_currency.is_empty() && used_currency == limit_currency;
            let same_exponent =
                used.get("exponent").and_then(number) == limit.get("exponent").and_then(number);
            if same_currency
                && same_exponent
                && let Some(percent) = percent_from_amounts(
                    used.get("amount_minor").and_then(number),
                    limit.get("amount_minor").and_then(number),
                )
            {
                return Some(percent);
            }
        }
    }

    let extra = rate_limits.get("extra_usage")?;
    if extra.get("is_enabled").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let percent = percent_from_amounts(
        extra.get("used_credits").and_then(number),
        extra.get("monthly_limit").and_then(number),
    )
    .or_else(|| extra.get("utilization").and_then(number));
    percent.filter(|percent| *percent >= 0.0).map(clamp_percent)
}

fn read_model_scoped(rate_limits: &Value) -> Vec<(String, Option<f64>, Option<String>)> {
    let Some(entries) = rate_limits.get("model_scoped").and_then(Value::as_array) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let display_name = entry.get("display_name").and_then(string)?.to_owned();
            Some((
                display_name,
                entry.get("utilization").and_then(number),
                entry
                    .get("resets_at")
                    .and_then(string)
                    .and_then(canonical_iso),
            ))
        })
        .collect()
}

fn make_window(id: &str, used_percent: f64, resets_at: Option<String>) -> UsageWindow {
    let (kind, label, window_duration_mins) = if id == "five_hour" {
        (UsageWindowKind::Session, "Session", SESSION_MINS)
    } else {
        (UsageWindowKind::Weekly, "Weekly", WEEK_MINS)
    };
    UsageWindow {
        id: id.to_owned(),
        kind,
        label: label.to_owned(),
        model_ids: None,
        provider_ids: None,
        used_percent: clamp_percent(used_percent),
        resets_at,
        window_duration_mins: Some(window_duration_mins),
    }
}

/// Translate the `get_usage` response into the shared report.
pub fn claude_usage_response_to_limits(response: &Value, checked_at: &str) -> ClaudeUsageProbe {
    let available = response
        .get("rate_limits_available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let rate_limits = response.get("rate_limits").filter(|value| !value.is_null());
    let Some(rate_limits) = rate_limits else {
        return ClaudeUsageProbe {
            limits: make_unavailable_usage_limits(
                checked_at,
                if available {
                    UsageLimitUnavailableReason::ProbeFailed
                } else {
                    UsageLimitUnavailableReason::Unsupported
                },
                None,
            ),
            names: ClaudeScopedLimitNames::default(),
        };
    };
    if !available {
        return ClaudeUsageProbe {
            limits: make_unavailable_usage_limits(
                checked_at,
                UsageLimitUnavailableReason::Unsupported,
                None,
            ),
            names: ClaudeScopedLimitNames::default(),
        };
    }

    let mut windows = Vec::new();
    for id in ["five_hour", "seven_day"] {
        let Some(window) = rate_limits.get(id).filter(|value| !value.is_null()) else {
            continue;
        };
        let Some(utilization) = window.get("utilization").and_then(number) else {
            continue;
        };
        let resets_at = window
            .get("resets_at")
            .and_then(string)
            .and_then(canonical_iso);
        windows.push(make_window(id, utilization, resets_at));
    }

    let mut overage_included = None;
    for (display_name, utilization, resets_at) in read_model_scoped(rate_limits) {
        let Some(utilization) = utilization else {
            continue;
        };
        windows.push(scoped_window(&display_name, utilization, resets_at));
        if overage_included.is_none() {
            overage_included = Some(display_name);
        }
    }

    if let Some(spend_percent) = read_spend_percent(rate_limits) {
        windows.push(UsageWindow {
            id: "monthly_spend".into(),
            kind: UsageWindowKind::Monthly,
            label: "Monthly spend".into(),
            model_ids: None,
            provider_ids: None,
            used_percent: spend_percent,
            resets_at: None,
            window_duration_mins: None,
        });
    }

    let zero_spend_cap = rate_limits.get("extra_usage").is_some_and(|extra| {
        extra.get("is_enabled").and_then(Value::as_bool) == Some(true)
            && extra.get("monthly_limit").and_then(number) == Some(0.0)
    });

    ClaudeUsageProbe {
        limits: ProviderUsageLimits {
            zero_spend_cap: zero_spend_cap.then_some(true),
            ..make_usage_limits(checked_at, windows)
        },
        names: ClaudeScopedLimitNames { overage_included },
    }
}

/// Translate a streamed `rate_limit_event` into a sparse window update.
///
/// Utilization is 0-1. Unnamed overage buckets are dropped until a probe has
/// named the row, so two events cannot create an unreconcilable pair.
pub fn claude_rate_limit_event_to_update(
    info: &ClaudeRateLimitEvent,
    names: &ClaudeScopedLimitNames,
) -> Option<UsageLimitsUpdate> {
    let kind = info.rate_limit_type.as_deref()?;
    let utilization = info.utilization?;
    if !utilization.is_finite() {
        return None;
    }
    let used_percent = utilization * 100.0;
    let resets_at = iso_from_epoch_seconds(info.resets_at);
    if kind == "five_hour" || kind == "seven_day" {
        return Some(UsageLimitsUpdate {
            windows: vec![make_window(kind, used_percent, resets_at)],
        });
    }
    if kind == OVERAGE_INCLUDED_EVENT_TYPE {
        let display_name = names.overage_included.as_deref()?;
        return Some(UsageLimitsUpdate {
            windows: vec![scoped_window(display_name, used_percent, resets_at)],
        });
    }
    None
}

/// The reset of an event in epoch seconds, for a caller that wants the time
/// without the rest of the update.
pub fn claude_event_reset_at(info: &ClaudeRateLimitEvent) -> Option<String> {
    iso_from_epoch_seconds(info.resets_at)
}

/// True while a reset time is in the future, used by callers deciding whether
/// a streamed window still blocks a turn.
pub fn claude_window_resets_after(window: &UsageWindow, now_ms: i64) -> bool {
    window
        .resets_at
        .as_deref()
        .and_then(parse_ms)
        .is_some_and(|reset| reset > now_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CHECKED_AT: &str = "2026-07-18T10:00:00.000Z";

    #[test]
    fn maps_the_session_weekly_and_model_scoped_weekly_windows() {
        let response = json!({
            "rate_limits_available": true,
            "rate_limits": {
                "five_hour": { "utilization": 54, "resets_at": "2026-07-18T14:39:00Z" },
                "seven_day": { "utilization": 18.4, "resets_at": "2026-07-24T08:59:00+00:00" },
                "seven_day_opus": { "utilization": 3, "resets_at": null },
                "model_scoped": [
                    { "display_name": "Fable", "utilization": 73, "resets_at": "2026-07-24T08:59:00Z" },
                    { "display_name": "Ghost", "utilization": null, "resets_at": null }
                ],
                "extra_usage": {
                    "is_enabled": false,
                    "monthly_limit": null,
                    "used_credits": null,
                    "utilization": null
                }
            }
        });
        let probe = claude_usage_response_to_limits(&response, CHECKED_AT);
        assert_eq!(
            probe.names,
            ClaudeScopedLimitNames {
                overage_included: Some("Fable".into())
            }
        );
        assert_eq!(
            probe.limits,
            make_usage_limits(
                CHECKED_AT,
                [
                    UsageWindow {
                        id: "five_hour".into(),
                        kind: UsageWindowKind::Session,
                        label: "Session".into(),
                        model_ids: None,
                        provider_ids: None,
                        used_percent: 54.0,
                        resets_at: Some("2026-07-18T14:39:00.000Z".into()),
                        window_duration_mins: Some(300),
                    },
                    UsageWindow {
                        id: "seven_day".into(),
                        kind: UsageWindowKind::Weekly,
                        label: "Weekly".into(),
                        model_ids: None,
                        provider_ids: None,
                        used_percent: 18.4,
                        resets_at: Some("2026-07-24T08:59:00.000Z".into()),
                        window_duration_mins: Some(10_080),
                    },
                    scoped_window("Fable", 73.0, Some("2026-07-24T08:59:00.000Z".into())),
                ]
            )
        );
    }

    #[test]
    fn names_the_overage_bucket_only_from_a_scoped_entry_that_drew_a_row() {
        let response = json!({
            "rate_limits_available": true,
            "rate_limits": {
                "model_scoped": [
                    { "display_name": "Ghost", "utilization": null, "resets_at": null },
                    { "display_name": "Fable", "utilization": 5, "resets_at": null }
                ]
            }
        });
        assert_eq!(
            claude_usage_response_to_limits(&response, CHECKED_AT).names,
            ClaudeScopedLimitNames {
                overage_included: Some("Fable".into())
            }
        );
    }

    #[test]
    fn skips_a_window_the_endpoint_reports_without_a_utilization() {
        let response = json!({
            "rate_limits_available": true,
            "rate_limits": {
                "five_hour": { "utilization": null, "resets_at": null },
                "seven_day": { "utilization": 250, "resets_at": null }
            }
        });
        assert_eq!(
            claude_usage_response_to_limits(&response, CHECKED_AT)
                .limits
                .windows,
            vec![UsageWindow {
                id: "seven_day".into(),
                kind: UsageWindowKind::Weekly,
                label: "Weekly".into(),
                model_ids: None,
                provider_ids: None,
                used_percent: 100.0,
                resets_at: None,
                window_duration_mins: Some(10_080),
            }]
        );
    }

    fn enterprise_response() -> Value {
        json!({
            "rate_limits_available": true,
            "rate_limits": {
                "five_hour": null,
                "seven_day": null,
                "spend": {
                    "enabled": true,
                    "used": { "amount_minor": 4631, "currency": "USD", "exponent": 2 },
                    "limit": { "amount_minor": 50000, "currency": "USD", "exponent": 2 },
                    "percent": 9
                },
                "extra_usage": {
                    "is_enabled": true,
                    "used_credits": 4631,
                    "monthly_limit": 50000,
                    "utilization": 9.261999999999999,
                    "currency": "USD",
                    "decimal_places": 2
                }
            }
        })
    }

    fn monthly_window() -> UsageWindow {
        UsageWindow {
            id: "monthly_spend".into(),
            kind: UsageWindowKind::Monthly,
            label: "Monthly spend".into(),
            model_ids: None,
            provider_ids: None,
            used_percent: 4631.0 / 50_000.0 * 100.0,
            resets_at: None,
            window_duration_mins: None,
        }
    }

    #[test]
    fn maps_the_captured_enterprise_budget_precisely_beside_rolling_windows() {
        let probe = claude_usage_response_to_limits(&enterprise_response(), CHECKED_AT);
        assert_eq!(probe.limits.windows, vec![monthly_window()]);

        let mut response = enterprise_response();
        response["rate_limits"]["five_hour"] = json!({ "utilization": 12, "resets_at": null });
        response["rate_limits"]["extra_usage"]["used_credits"] = json!(30_000);
        response["rate_limits"]["extra_usage"]["utilization"] = json!(60);
        let windows = claude_usage_response_to_limits(&response, CHECKED_AT)
            .limits
            .windows;
        assert_eq!(
            windows,
            vec![
                UsageWindow {
                    id: "five_hour".into(),
                    kind: UsageWindowKind::Session,
                    label: "Session".into(),
                    model_ids: None,
                    provider_ids: None,
                    used_percent: 12.0,
                    resets_at: None,
                    window_duration_mins: Some(300),
                },
                monthly_window(),
            ]
        );
    }

    #[test]
    fn falls_back_to_enabled_extra_usage_amounts_or_utilization_when_spend_is_unusable() {
        let mut response = enterprise_response();
        response["rate_limits"]["spend"]["enabled"] = json!(false);
        response["rate_limits"]["spend"]["used"]["amount_minor"] = json!(30_000);
        response["rate_limits"]["extra_usage"]["utilization"] = json!(9);
        assert_eq!(
            claude_usage_response_to_limits(&response, CHECKED_AT)
                .limits
                .windows,
            vec![monthly_window()]
        );

        let mut utilization_only = response;
        utilization_only["rate_limits"]["spend"] = json!({
            "enabled": true, "used": null, "limit": null, "percent": 9
        });
        utilization_only["rate_limits"]["extra_usage"]["used_credits"] = json!(null);
        utilization_only["rate_limits"]["extra_usage"]["monthly_limit"] = json!(null);
        let windows = claude_usage_response_to_limits(&utilization_only, CHECKED_AT)
            .limits
            .windows;
        // Utilization-only falls back to 9, the same value the ratio produced.
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].used_percent, 9.0);
    }

    #[test]
    fn flags_a_zero_cap_instead_of_reporting_a_monthly_window() {
        let response = json!({
            "rate_limits_available": true,
            "rate_limits": {
                "five_hour": null,
                "seven_day": null,
                "extra_usage": {
                    "is_enabled": true,
                    "used_credits": 0,
                    "monthly_limit": 0,
                    "utilization": null
                }
            }
        });
        assert_eq!(
            claude_usage_response_to_limits(&response, CHECKED_AT).limits,
            ProviderUsageLimits {
                zero_spend_cap: Some(true),
                ..make_usage_limits(CHECKED_AT, Vec::new())
            }
        );
    }

    #[test]
    fn omits_disabled_budgets_and_refuses_invalid_money_without_an_enabled_fallback() {
        let mut response = enterprise_response();
        response["rate_limits"]["spend"]["enabled"] = json!(false);
        response["rate_limits"]["extra_usage"]["is_enabled"] = json!(false);
        assert!(
            claude_usage_response_to_limits(&response, CHECKED_AT)
                .limits
                .windows
                .is_empty()
        );
        response["rate_limits"]["spend"]["enabled"] = json!(true);
        response["rate_limits"]["spend"]["limit"]["currency"] = json!("EUR");
        assert!(
            claude_usage_response_to_limits(&response, CHECKED_AT)
                .limits
                .windows
                .is_empty()
        );
        response["rate_limits"]["spend"]["limit"]["currency"] = json!("USD");
        response["rate_limits"]["spend"]["limit"]["amount_minor"] = json!(0);
        assert!(
            claude_usage_response_to_limits(&response, CHECKED_AT)
                .limits
                .windows
                .is_empty()
        );
    }

    #[test]
    fn scales_the_streamed_utilization_and_epoch_reset_onto_the_probe_window() {
        let update = claude_rate_limit_event_to_update(
            &ClaudeRateLimitEvent {
                rate_limit_type: Some("seven_day".into()),
                utilization: Some(0.85),
                resets_at: Some(1_784_000_000.0),
            },
            &ClaudeScopedLimitNames::default(),
        )
        .unwrap();
        assert_eq!(
            update.windows,
            vec![UsageWindow {
                id: "seven_day".into(),
                kind: UsageWindowKind::Weekly,
                label: "Weekly".into(),
                model_ids: None,
                provider_ids: None,
                used_percent: 85.0,
                resets_at: Some("2026-07-14T03:33:20.000Z".into()),
                window_duration_mins: Some(10_080),
            }]
        );
    }

    #[test]
    fn lands_the_streamed_overage_bucket_on_the_row_the_probe_named() {
        let event = ClaudeRateLimitEvent {
            rate_limit_type: Some(OVERAGE_INCLUDED_EVENT_TYPE.into()),
            utilization: Some(0.4),
            resets_at: None,
        };
        assert!(
            claude_rate_limit_event_to_update(&event, &ClaudeScopedLimitNames::default()).is_none()
        );
        assert_eq!(
            claude_rate_limit_event_to_update(
                &event,
                &ClaudeScopedLimitNames {
                    overage_included: Some("Fable".into())
                }
            )
            .unwrap()
            .windows,
            vec![scoped_window("Fable", 40.0, None)]
        );
    }

    #[test]
    fn a_scoped_weekly_window_blocks_only_its_own_model() {
        let window = scoped_window("Fable", 100.0, None);
        assert!(window_limits_model(&window, Some("claude-fable-5")));
        assert!(!window_limits_model(&window, Some("claude-opus-5")));
        assert!(!window_limits_model(&window, None));
        assert!(window_limits_model(
            &make_window("seven_day", 100.0, None),
            None
        ));
    }

    #[test]
    fn unavailable_probe_reports_unsupported_or_failed() {
        let unsupported = claude_usage_response_to_limits(
            &json!({ "rate_limits_available": false, "rate_limits": null }),
            CHECKED_AT,
        );
        assert_eq!(
            unsupported.limits.unavailable.unwrap().reason,
            UsageLimitUnavailableReason::Unsupported
        );
        let failed = claude_usage_response_to_limits(
            &json!({ "rate_limits_available": true, "rate_limits": null }),
            CHECKED_AT,
        );
        assert_eq!(
            failed.limits.unavailable.unwrap().reason,
            UsageLimitUnavailableReason::ProbeFailed
        );
    }
}
