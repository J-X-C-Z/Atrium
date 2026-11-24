// Ported from apps/server/src/provider/claudeUsageLimits.ts (T3 Code v0.1.11, MIT).

//! Claude subscription usage-limit mapping.
//!
//! The CLI reports usage two ways: `get_usage` (percentages already 0–100)
//! and `rate_limit_event` (0–1 utilization with epoch-second resets). T3
//! normalizes both into the same `usageLimits` windows; this module does the
//! same without depending on the engine's contract crate.

use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

use crate::events::{RateLimitInfo, normalize_iso};

const SESSION_MINS: u32 = 5 * 60;
const WEEK_MINS: u32 = 7 * 24 * 60;
const OVERAGE_INCLUDED_EVENT_TYPE: &str = "seven_day_overage_included";
const SCOPED_WINDOW_PREFIX: &str = "seven_day_";

/// The names T3 remembers between a `get_usage` probe and streamed
/// `rate_limit_event` frames, so both target the same row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopedLimitNames {
    pub overage_included: Option<String>,
}

/// A normalized usage-limits report.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimits {
    pub checked_at: String,
    pub windows: Vec<UsageWindow>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub zero_spend_cap: bool,
    /// Set when the provider could not be probed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// JSON has one number type; write whole percentages without a `.0`, as T3's
/// `JSON.stringify` does.
fn serialize_percent<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
    if value.fract() == 0.0 && *value >= i64::MIN as f64 && *value <= i64::MAX as f64 {
        serializer.serialize_i64(*value as i64)
    } else {
        serializer.serialize_f64(*value)
    }
}

/// One rate-limit window.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub id: String,
    /// `session`, `weekly` or `monthly`.
    pub kind: String,
    pub label: String,
    #[serde(serialize_with = "serialize_percent")]
    pub used_percent: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_duration_mins: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
}

/// A mid-turn update from a `rate_limit_event`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitsUpdate {
    pub windows: Vec<UsageWindow>,
}

/// The `get_usage` reply, kept as a typed view over its JSON.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageResponse {
    pub rate_limits_available: bool,
    pub rate_limits: Option<Value>,
}

impl UsageResponse {
    /// Reads the `get_usage` control response payload.
    pub fn from_value(value: &Value) -> Self {
        Self {
            rate_limits_available: value
                .get("rate_limits_available")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            rate_limits: value.get("rate_limits").cloned(),
        }
    }
}

/// A model-scoped weekly allowance blocks only its own model; an unknown
/// model is not blocked.
pub fn window_limits_model(window_id: &str, model: Option<&str>) -> bool {
    if !window_id.starts_with(SCOPED_WINDOW_PREFIX) {
        return true;
    }
    model.is_some_and(|model| scope_token(model).contains(&window_id[SCOPED_WINDOW_PREFIX.len()..]))
}

/// Streamed utilization is 0–1. Drop unnamed overage buckets until the probe
/// named them, so a mid-turn update cannot create a row the probe did not.
pub fn claude_rate_limit_event_to_update(
    info: &RateLimitInfo,
    names: &ScopedLimitNames,
) -> Option<UsageLimitsUpdate> {
    let limit_type = info.rate_limit_type.as_deref()?;
    let utilization = info.utilization?;
    let used_percent = utilization * 100.0;
    let resets_at = info
        .resets_at
        .filter(|value| *value > 0)
        .map(iso_millis_from_epoch_seconds);
    if matches!(limit_type, "five_hour" | "seven_day") {
        return Some(UsageLimitsUpdate {
            windows: vec![make_window(limit_type, used_percent, resets_at)],
        });
    }
    if limit_type == OVERAGE_INCLUDED_EVENT_TYPE {
        let display_name = names.overage_included.as_deref()?;
        return Some(UsageLimitsUpdate {
            windows: vec![scoped_window(display_name, used_percent, resets_at)],
        });
    }
    None
}

/// `get_usage` percentages are already 0–100.
pub fn claude_usage_response_to_limits(
    response: &UsageResponse,
    checked_at: &str,
) -> (UsageLimits, ScopedLimitNames) {
    let unavailable = |reason: &str| {
        (
            UsageLimits {
                checked_at: checked_at.to_owned(),
                windows: Vec::new(),
                zero_spend_cap: false,
                unavailable: Some(reason.to_owned()),
            },
            ScopedLimitNames::default(),
        )
    };
    if !response.rate_limits_available {
        return unavailable("unsupported");
    }
    let Some(rate_limits) = response.rate_limits.as_ref() else {
        return unavailable("probeFailed");
    };

    let mut windows = Vec::new();
    // T3's `WINDOWS` record covers only these two; other bucket names are
    // read from `model_scoped` or ignored.
    for (id, kind, label, duration) in [
        ("five_hour", "session", "Session", Some(SESSION_MINS)),
        ("seven_day", "weekly", "Weekly", Some(WEEK_MINS)),
    ] {
        let Some(bucket) = rate_limits.get(id) else {
            continue;
        };
        let Some(utilization) = bucket.get("utilization").and_then(Value::as_f64) else {
            continue;
        };
        let resets_at = bucket
            .get("resets_at")
            .and_then(Value::as_str)
            .and_then(normalize_iso);
        windows.push(UsageWindow {
            id: id.to_owned(),
            kind: kind.to_owned(),
            label: label.to_owned(),
            used_percent: clamp_percent(utilization),
            window_duration_mins: duration,
            resets_at,
        });
    }

    // The CLI's overage allowlist currently has one model; streamed events
    // refer to its first entry.
    let mut overage_included = None;
    if let Some(entries) = rate_limits.get("model_scoped").and_then(Value::as_array) {
        for entry in entries {
            let Some(display_name) = entry.get("display_name").and_then(Value::as_str) else {
                continue;
            };
            let Some(utilization) = entry.get("utilization").and_then(Value::as_f64) else {
                continue;
            };
            let resets_at = entry
                .get("resets_at")
                .and_then(Value::as_str)
                .and_then(normalize_iso);
            windows.push(scoped_window(display_name, utilization, resets_at));
            overage_included.get_or_insert_with(|| display_name.to_owned());
        }
    }

    let spend_percent = read_spend_percent(rate_limits);
    let extra_enabled = rate_limits
        .get("extra_usage")
        .and_then(|extra| extra.get("is_enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let zero_monthly = rate_limits
        .get("extra_usage")
        .and_then(|extra| extra.get("monthly_limit"))
        .and_then(Value::as_f64)
        == Some(0.0);
    if let Some(percent) = spend_percent {
        windows.push(UsageWindow {
            id: "monthly_spend".into(),
            kind: "monthly".into(),
            label: "Monthly spend".into(),
            used_percent: clamp_percent(percent),
            window_duration_mins: None,
            resets_at: None,
        });
    }

    (
        UsageLimits {
            checked_at: checked_at.to_owned(),
            windows,
            zero_spend_cap: extra_enabled && zero_monthly,
            unavailable: None,
        },
        ScopedLimitNames { overage_included },
    )
}

fn read_spend_percent(rate_limits: &Value) -> Option<f64> {
    if let Some(spend) = rate_limits.get("spend") {
        let enabled = spend
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let used_currency = spend
            .get("used")
            .and_then(|used| used.get("currency"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let limit_currency = spend
            .get("limit")
            .and_then(|limit| limit.get("currency"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let used_exponent = spend
            .get("used")
            .and_then(|used| used.get("exponent"))
            .and_then(Value::as_u64);
        let limit_exponent = spend
            .get("limit")
            .and_then(|limit| limit.get("exponent"))
            .and_then(Value::as_u64);
        let used_amount = spend
            .get("used")
            .and_then(|used| used.get("amount_minor"))
            .and_then(Value::as_f64);
        let limit_amount = spend
            .get("limit")
            .and_then(|limit| limit.get("amount_minor"))
            .and_then(Value::as_f64);
        // Minor-unit ratios require matching currency and scale.
        if enabled
            && !used_currency.is_empty()
            && used_currency == limit_currency
            && used_exponent.is_some()
            && used_exponent == limit_exponent
            && let Some(percent) = percent_from_amounts(used_amount, limit_amount)
        {
            return Some(percent);
        }
    }

    let extra = rate_limits.get("extra_usage")?;
    if extra.get("is_enabled").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let used = extra.get("used_credits").and_then(Value::as_f64);
    let limit = extra.get("monthly_limit").and_then(Value::as_f64);
    let utilization = extra.get("utilization").and_then(Value::as_f64);
    let percent = percent_from_amounts(used, limit).or(utilization)?;
    (percent.is_finite() && percent >= 0.0).then(|| clamp_percent(percent))
}

fn percent_from_amounts(used: Option<f64>, limit: Option<f64>) -> Option<f64> {
    let used = used?;
    let limit = limit?;
    if !used.is_finite() || !limit.is_finite() || used < 0.0 || limit <= 0.0 {
        return None;
    }
    let percent = used / limit * 100.0;
    percent.is_finite().then(|| clamp_percent(percent))
}

fn clamp_percent(value: f64) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    value.clamp(0.0, 100.0)
}

fn scope_token(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn scoped_window_id(display_name: &str) -> String {
    format!("{SCOPED_WINDOW_PREFIX}{}", scope_token(display_name))
}

fn scoped_window(display_name: &str, used_percent: f64, resets_at: Option<String>) -> UsageWindow {
    UsageWindow {
        id: scoped_window_id(display_name),
        kind: "weekly".into(),
        label: format!("Weekly · {display_name}"),
        used_percent: clamp_percent(used_percent),
        window_duration_mins: Some(WEEK_MINS),
        resets_at,
    }
}

fn make_window(id: &str, used_percent: f64, resets_at: Option<String>) -> UsageWindow {
    let (kind, label, duration) = match id {
        "five_hour" => ("session", "Session", SESSION_MINS),
        _ => ("weekly", "Weekly", WEEK_MINS),
    };
    UsageWindow {
        id: id.to_owned(),
        kind: kind.into(),
        label: label.into(),
        used_percent: clamp_percent(used_percent),
        window_duration_mins: Some(duration),
        resets_at,
    }
}

/// RFC 3339 with milliseconds, matching T3's `DateTime.formatIso`.
pub(crate) fn iso_millis_from_epoch_seconds(seconds: i64) -> String {
    let timestamp = jiff::Timestamp::from_second(seconds)
        .unwrap_or_else(|_| jiff::Timestamp::from_second(0).expect("epoch is valid"));
    format_timestamp(timestamp)
}

/// Parses an RFC 3339 timestamp and normalizes it to UTC milliseconds.
pub(crate) fn iso_millis_from_string(value: &str) -> Option<String> {
    value.parse::<jiff::Timestamp>().ok().map(format_timestamp)
}

fn format_timestamp(timestamp: jiff::Timestamp) -> String {
    let text = timestamp.to_string();
    let Some(stem) = text.strip_suffix('Z') else {
        return text;
    };
    match stem.split_once('.') {
        Some((whole, fraction)) => {
            let millis: String = fraction.chars().take(3).collect();
            format!("{whole}.{millis:0<3}Z")
        }
        None => format!("{stem}.000Z"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn iso_values_use_milliseconds() {
        assert_eq!(
            iso_millis_from_epoch_seconds(1_784_000_000),
            "2026-07-14T03:33:20.000Z"
        );
        assert_eq!(
            iso_millis_from_string("2026-07-18T14:39:00Z").as_deref(),
            Some("2026-07-18T14:39:00.000Z")
        );
        assert_eq!(
            iso_millis_from_string("2026-07-24T08:59:00+00:00").as_deref(),
            Some("2026-07-24T08:59:00.000Z")
        );
    }

    #[test]
    fn a_rate_limit_event_scales_to_percent() {
        let update = claude_rate_limit_event_to_update(
            &RateLimitInfo {
                status: "allowed_warning".into(),
                rate_limit_type: Some("seven_day".into()),
                utilization: Some(0.85),
                resets_at: Some(1_784_000_000),
                ..Default::default()
            },
            &ScopedLimitNames::default(),
        )
        .expect("mapped");
        assert_eq!(update.windows[0].used_percent, 85.0);
        assert_eq!(
            update.windows[0].resets_at.as_deref(),
            Some("2026-07-14T03:33:20.000Z")
        );
    }

    #[test]
    fn an_unnamed_overage_bucket_is_dropped() {
        let info = RateLimitInfo {
            status: "allowed".into(),
            rate_limit_type: Some("seven_day_overage_included".into()),
            utilization: Some(0.4),
            ..Default::default()
        };
        assert!(claude_rate_limit_event_to_update(&info, &ScopedLimitNames::default()).is_none());
        let update = claude_rate_limit_event_to_update(
            &info,
            &ScopedLimitNames {
                overage_included: Some("Fable".into()),
            },
        )
        .expect("named");
        assert_eq!(update.windows[0].id, "seven_day_fable");
        assert_eq!(update.windows[0].used_percent, 40.0);
    }

    #[test]
    fn the_session_and_weekly_windows_map() {
        let response = UsageResponse::from_value(&json!({
            "rate_limits_available": true,
            "rate_limits": {
                "five_hour": {"utilization": 54, "resets_at": "2026-07-18T14:39:00Z"},
                "seven_day": {"utilization": 18.4, "resets_at": "2026-07-24T08:59:00+00:00"}
            }
        }));
        let (limits, _) = claude_usage_response_to_limits(&response, "now");
        assert_eq!(limits.windows.len(), 2);
        assert_eq!(limits.windows[0].id, "five_hour");
        assert_eq!(limits.windows[0].window_duration_mins, Some(300));
        assert_eq!(limits.windows[1].used_percent, 18.4);
    }
}
