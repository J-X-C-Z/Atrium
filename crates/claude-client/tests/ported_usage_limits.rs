// Ported from apps/server/src/provider/claudeUsageLimits.test.ts (T3 Code v0.1.11, MIT).

//! T3's Claude usage-limit tests, with the same cases and expected values.

use claude_client::events::RateLimitInfo;
use claude_client::{
    ScopedLimitNames, UsageResponse, claude_rate_limit_event_to_update,
    claude_usage_response_to_limits,
};
use serde_json::{Value, json};

const CHECKED_AT: &str = "2026-07-18T10:00:00.000Z";

fn report(value: Value) -> (Value, ScopedLimitNames) {
    let response = UsageResponse::from_value(&value);
    let (limits, names) = claude_usage_response_to_limits(&response, CHECKED_AT);
    (serde_json::to_value(limits).unwrap(), names)
}

fn window(value: &Value, index: usize) -> Value {
    value["windows"][index].clone()
}

#[test]
fn maps_the_session_weekly_and_model_scoped_windows() {
    let (limits, names) = report(json!({
        "rate_limits_available": true,
        "rate_limits": {
            "five_hour": {"utilization": 54, "resets_at": "2026-07-18T14:39:00Z"},
            "seven_day": {"utilization": 18.4, "resets_at": "2026-07-24T08:59:00+00:00"},
            "seven_day_opus": {"utilization": 3, "resets_at": null},
            "model_scoped": [
                {"display_name": "Fable", "utilization": 73, "resets_at": "2026-07-24T08:59:00Z"},
                {"display_name": "Ghost", "utilization": null, "resets_at": null}
            ],
            "extra_usage": {
                "is_enabled": false,
                "monthly_limit": null,
                "used_credits": null,
                "utilization": null
            }
        }
    }));
    assert_eq!(names.overage_included.as_deref(), Some("Fable"));
    assert_eq!(
        limits,
        json!({
            "checkedAt": CHECKED_AT,
            "windows": [
                {
                    "id": "five_hour",
                    "kind": "session",
                    "label": "Session",
                    "usedPercent": 54,
                    "windowDurationMins": 300,
                    "resetsAt": "2026-07-18T14:39:00.000Z"
                },
                {
                    "id": "seven_day",
                    "kind": "weekly",
                    "label": "Weekly",
                    "usedPercent": 18.4,
                    "windowDurationMins": 10080,
                    "resetsAt": "2026-07-24T08:59:00.000Z"
                },
                {
                    "id": "seven_day_fable",
                    "kind": "weekly",
                    "label": "Weekly · Fable",
                    "usedPercent": 73,
                    "windowDurationMins": 10080,
                    "resetsAt": "2026-07-24T08:59:00.000Z"
                }
            ]
        })
    );
}

#[test]
fn names_the_overage_bucket_only_from_a_scoped_entry_that_drew_a_row() {
    let (_, names) = report(json!({
        "rate_limits_available": true,
        "rate_limits": {
            "model_scoped": [
                {"display_name": "Ghost", "utilization": null, "resets_at": null},
                {"display_name": "Fable", "utilization": 5, "resets_at": null}
            ]
        }
    }));
    assert_eq!(names.overage_included.as_deref(), Some("Fable"));
}

#[test]
fn skips_a_window_without_a_utilization_and_clamps_the_rest() {
    let (limits, _) = report(json!({
        "rate_limits_available": true,
        "rate_limits": {
            "five_hour": {"utilization": null, "resets_at": null},
            "seven_day": {"utilization": 250, "resets_at": null}
        }
    }));
    assert_eq!(
        limits["windows"],
        json!([{
            "id": "seven_day",
            "kind": "weekly",
            "label": "Weekly",
            "usedPercent": 100,
            "windowDurationMins": 10080
        }])
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
                "used": {"amount_minor": 4631, "currency": "USD", "exponent": 2},
                "limit": {"amount_minor": 50000, "currency": "USD", "exponent": 2},
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

fn assert_close_to(actual: f64, expected: f64, tolerance: f64) {
    assert!(
        (actual - expected).abs() < tolerance,
        "expected {expected}, got {actual}"
    );
}

#[test]
fn maps_the_enterprise_budget_precisely_beside_rolling_windows() {
    let (limits, _) = report(enterprise_response());
    assert_eq!(limits["windows"].as_array().unwrap().len(), 1);
    let monthly = window(&limits, 0);
    assert_eq!(monthly["id"], "monthly_spend");
    assert_eq!(monthly["kind"], "monthly");
    assert_eq!(monthly["label"], "Monthly spend");
    assert_close_to(monthly["usedPercent"].as_f64().unwrap(), 9.262, 1e-10);

    let mut response = enterprise_response();
    response["rate_limits"]["five_hour"] = json!({"utilization": 12, "resets_at": null});
    response["rate_limits"]["extra_usage"]["used_credits"] = json!(30_000);
    response["rate_limits"]["extra_usage"]["utilization"] = json!(60);
    let (limits, _) = report(response);
    assert_eq!(window(&limits, 0)["id"], "five_hour");
    assert_eq!(window(&limits, 0)["usedPercent"], 12);
    assert_eq!(window(&limits, 1)["id"], "monthly_spend");
    assert_close_to(
        window(&limits, 1)["usedPercent"].as_f64().unwrap(),
        9.262,
        1e-10,
    );
}

#[test]
fn falls_back_to_enabled_extra_usage_amounts_or_utilization() {
    let mut response = enterprise_response();
    response["rate_limits"]["spend"]["enabled"] = json!(false);
    response["rate_limits"]["spend"]["used"] =
        json!({"amount_minor": 30_000, "currency": "USD", "exponent": 2});
    response["rate_limits"]["extra_usage"]["utilization"] = json!(9);
    let (limits, _) = report(response);
    assert_close_to(
        window(&limits, 0)["usedPercent"].as_f64().unwrap(),
        9.262,
        1e-10,
    );

    let mut response = enterprise_response();
    response["rate_limits"]["spend"] =
        json!({"enabled": true, "used": null, "limit": null, "percent": 9});
    response["rate_limits"]["extra_usage"]["used_credits"] = Value::Null;
    response["rate_limits"]["extra_usage"]["monthly_limit"] = Value::Null;
    let (limits, _) = report(response);
    assert_close_to(
        window(&limits, 0)["usedPercent"].as_f64().unwrap(),
        9.262,
        1e-10,
    );
}

#[test]
fn flags_a_zero_spend_cap_instead_of_a_monthly_window() {
    let (limits, _) = report(json!({
        "rate_limits_available": true,
        "rate_limits": {
            "five_hour": null,
            "seven_day": null,
            "extra_usage": {"is_enabled": true, "used_credits": 0, "monthly_limit": 0, "utilization": null}
        }
    }));
    assert_eq!(
        limits,
        json!({"checkedAt": CHECKED_AT, "windows": [], "zeroSpendCap": true})
    );
}

#[test]
fn omits_disabled_budgets_and_refuses_invalid_money() {
    let mut response = enterprise_response();
    response["rate_limits"]["spend"]["enabled"] = json!(false);
    response["rate_limits"]["extra_usage"]["is_enabled"] = json!(false);
    let (limits, _) = report(response.clone());
    assert_eq!(limits["windows"], json!([]));

    response["rate_limits"]["spend"]["enabled"] = json!(true);
    response["rate_limits"]["spend"]["limit"]["currency"] = json!("EUR");
    let (limits, _) = report(response.clone());
    assert_eq!(limits["windows"], json!([]));

    response["rate_limits"]["spend"]["limit"]["amount_minor"] = json!(0);
    let (limits, _) = report(response);
    assert_eq!(limits["windows"], json!([]));
}

#[test]
fn scales_the_rate_limit_event_onto_the_probes_window_id() {
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
    assert_eq!(
        serde_json::to_value(update).unwrap(),
        json!({
            "windows": [{
                "id": "seven_day",
                "kind": "weekly",
                "label": "Weekly",
                "usedPercent": 85,
                "windowDurationMins": 10080,
                "resetsAt": "2026-07-14T03:33:20.000Z"
            }]
        })
    );
}

#[test]
fn lands_the_streamed_overage_bucket_on_the_probes_row() {
    let event = RateLimitInfo {
        status: "allowed".into(),
        rate_limit_type: Some("seven_day_overage_included".into()),
        utilization: Some(0.4),
        ..Default::default()
    };
    assert!(claude_rate_limit_event_to_update(&event, &ScopedLimitNames::default()).is_none());
    let update = claude_rate_limit_event_to_update(
        &event,
        &ScopedLimitNames {
            overage_included: Some("Fable".into()),
        },
    )
    .expect("named");
    assert_eq!(
        serde_json::to_value(update).unwrap(),
        json!({
            "windows": [{
                "id": "seven_day_fable",
                "kind": "weekly",
                "label": "Weekly · Fable",
                "usedPercent": 40,
                "windowDurationMins": 10080
            }]
        })
    );
}

#[test]
fn a_model_scoped_window_blocks_only_its_own_model() {
    use claude_client::usage::window_limits_model;
    assert!(window_limits_model("five_hour", None));
    assert!(window_limits_model(
        "seven_day_fable",
        Some("claude-fable-4")
    ));
    assert!(!window_limits_model(
        "seven_day_fable",
        Some("claude-opus-5")
    ));
    assert!(!window_limits_model("seven_day_fable", None));
}
