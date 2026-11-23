//! Claude's plan usage, read from the two places it shows up: the OAuth
//! usage endpoint (`five_hour` / `seven_day` objects) and whatever rate-limit
//! hints an ACP agent puts in `_meta`. Both become the [`RateLimits`] Codex
//! uses: primary is the five-hour window, secondary the weekly one.

use serde_json::Value;

use crate::{RateLimits, RateWindow};

const FIVE_HOURS: u64 = 300;
const WEEK: u64 = 10080;

/// `{"five_hour": {"utilization": 37.0, "resets_at": "2025-…Z"}, "seven_day": {…}}`
/// from `GET /api/oauth/usage`. `None` when neither window is there.
pub fn parse_oauth_usage(body: &Value) -> Option<RateLimits> {
    limits_from(body)
}

/// Looks through an ACP `_meta` (or a result holding one) for rate-limit
/// info under `claude/rateLimit`, `_claude/rateLimit`, `rateLimits` or
/// `rateLimit`, also nested one level in `claude` / `_claude`.
pub fn parse_meta_limits(meta: &Value) -> Option<RateLimits> {
    const KEYS: [&str; 4] = [
        "claude/rateLimit",
        "_claude/rateLimit",
        "rateLimits",
        "rateLimit",
    ];
    let find = |value: &Value| -> Option<RateLimits> {
        KEYS.iter()
            .filter_map(|key| value.get(*key))
            .find_map(limits_from)
    };
    find(meta)
        .or_else(|| find(&meta["_meta"]))
        .or_else(|| ["claude", "_claude"].iter().find_map(|k| find(&meta[*k])))
}

fn limits_from(value: &Value) -> Option<RateLimits> {
    let pick = |names: &[&str]| {
        names.iter().find_map(|n| value.get(*n)).and_then(|w| {
            window(
                w,
                if names[0] == "five_hour" {
                    FIVE_HOURS
                } else {
                    WEEK
                },
            )
        })
    };
    let primary = pick(&["five_hour", "fiveHour", "5h", "primary"]);
    let secondary = pick(&["seven_day", "sevenDay", "7d", "weekly", "secondary"]);
    if primary.is_none() && secondary.is_none() {
        return None;
    }
    let reached = [primary, secondary]
        .into_iter()
        .flatten()
        .any(|w| w.used_percent >= 100.0);
    Some(RateLimits {
        name: Some("Claude".into()),
        primary,
        secondary,
        reached,
        ..RateLimits::default()
    })
}

fn window(value: &Value, mins: u64) -> Option<RateWindow> {
    let used = [
        "used_percentage",
        "usedPercentage",
        "used_percent",
        "usedPercent",
        "utilization",
    ]
    .iter()
    .find_map(|k| number(&value[*k]))?;
    let resets = ["resets_at", "resetsAt", "reset_at"]
        .iter()
        .find_map(|k| epoch_seconds(&value[*k]));
    Some(RateWindow {
        used_percent: used.clamp(0.0, 1000.0),
        window_mins: Some(mins),
        resets_at: resets,
    })
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| {
            value
                .as_str()
                .and_then(|s| s.trim().trim_end_matches('%').parse().ok())
        })
        .filter(|n| n.is_finite())
}

/// Epoch seconds (or milliseconds, when too big to be seconds) or an ISO
/// 8601 timestamp, as Unix seconds.
fn epoch_seconds(value: &Value) -> Option<i64> {
    if let Some(n) = value.as_f64().filter(|n| n.is_finite()) {
        let n = n as i64;
        return Some(if n > 100_000_000_000 { n / 1000 } else { n });
    }
    let text = value.as_str()?.trim();
    if let Ok(n) = text.parse::<i64>() {
        return Some(if n > 100_000_000_000 { n / 1000 } else { n });
    }
    text.parse::<jiff::Timestamp>().ok().map(|t| t.as_second())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn oauth_usage_has_two_windows() {
        let limits = parse_oauth_usage(&json!({
            "five_hour": {"utilization": 37.5, "resets_at": "2025-11-04T04:59:59.943648+00:00"},
            "seven_day": {"utilization": 100, "resets_at": "2025-11-06T03:59:59Z"},
            "seven_day_opus": null
        }))
        .unwrap();
        let p = limits.primary.unwrap();
        assert_eq!(p.used_percent, 37.5);
        assert_eq!(p.window_mins, Some(300));
        assert_eq!(p.resets_at, Some(1762232399));
        assert_eq!(limits.secondary.unwrap().window_mins, Some(10080));
        assert!(limits.reached);
    }

    #[test]
    fn oauth_usage_without_windows_is_none() {
        assert!(parse_oauth_usage(&json!({"five_hour": null})).is_none());
        assert!(parse_oauth_usage(&json!("x")).is_none());
    }

    #[test]
    fn meta_shapes_are_accepted() {
        let a = parse_meta_limits(&json!({"claude/rateLimit": {
            "five_hour": {"used_percentage": 12, "resets_at": 1762232399}
        }}))
        .unwrap();
        assert_eq!(a.primary.unwrap().resets_at, Some(1762232399));
        assert!(!a.reached);
        let b = parse_meta_limits(&json!({"rateLimits": {
            "fiveHour": {"usedPercentage": "80%", "resetsAt": 1762232399000i64},
            "weekly": {"utilization": 5, "resetsAt": "2025-11-06T03:59:59Z"}
        }}))
        .unwrap();
        assert_eq!(b.primary.unwrap().used_percent, 80.0);
        assert_eq!(b.primary.unwrap().resets_at, Some(1762232399));
        assert_eq!(b.secondary.unwrap().resets_at, Some(1762401599));
        let c = parse_meta_limits(&json!({"_meta": {"_claude/rateLimit": {
            "seven_day": {"used_percentage": 100}
        }}}))
        .unwrap();
        assert!(c.reached && c.primary.is_none());
        assert!(parse_meta_limits(&json!({"other": 1})).is_none());
    }
}
