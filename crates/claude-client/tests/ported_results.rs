// Ported from apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.test.ts (T3 Code v0.1.11, MIT).

//! Protocol-level result classification cases from T3's Claude adapter tests.
//!
//! T3 sends a `rate_limit_event` beside some gateway failures and prefers its
//! reset time. This crate classifies the `result` frame alone; the adapter
//! that owns both events will pass the earlier reset through in item 23's
//! second half. The cases below therefore cover the text-derived resets.

use claude_client::events::{ClaudeEvent, FailureClass, TurnResult, TurnStatus, event};
use serde_json::{Value, json};

const NOW: i64 = 1_790_000_000;

fn parse_result(value: Value) -> TurnResult {
    match event(&value) {
        Some(ClaudeEvent::Result(result)) => result,
        other => panic!("not a result frame: {other:?}"),
    }
}

fn reset_seconds(result: &TurnResult) -> Option<i64> {
    result
        .failure_at(false, NOW)?
        .reset_at?
        .parse::<jiff::Timestamp>()
        .ok()
        .map(|timestamp| timestamp.as_second() - NOW)
}

fn base_result(extra: Value) -> Value {
    let mut value = json!({
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "api_error_status": null,
        "num_turns": 1,
        "result": "",
        "stop_reason": "end_turn",
        "session_id": "s1",
        "terminal_reason": "completed",
        "uuid": "u1"
    });
    if let Value::Object(extra) = extra {
        for (key, entry) in extra {
            value_of(&mut value).insert(key, entry);
        }
    }
    value
}

fn value_of(value: &mut Value) -> &mut serde_json::Map<String, Value> {
    value.as_object_mut().expect("object")
}

#[test]
fn a_plain_success_completes() {
    let result = parse_result(base_result(json!({})));
    assert_eq!(result.terminal_status(), TurnStatus::Completed);
    assert!(result.failure_at(false, NOW).is_none());
}

#[test]
fn an_explicit_abort_interrupts_the_turn() {
    for reason in ["aborted_streaming", "aborted_tools"] {
        let result = parse_result(base_result(json!({"terminal_reason": reason})));
        assert_eq!(
            result.terminal_status(),
            TurnStatus::Interrupted,
            "{reason}"
        );
    }
}

#[test]
fn known_terminal_reasons_fail_even_in_a_success_envelope() {
    let result = parse_result(base_result(json!({"terminal_reason": "prompt_too_long"})));
    assert_eq!(result.terminal_status(), TurnStatus::Failed);
    assert_eq!(
        result.failure_at(false, NOW).unwrap().message,
        "Claude stopped: the prompt exceeds the model's context window. Run /compact or start a new thread."
    );
}

#[test]
fn error_subtypes_classify_interrupt_and_cancel_text() {
    let interrupted = parse_result(json!({
        "type": "result", "subtype": "error_during_execution", "is_error": true,
        "errors": ["Request was interrupted by the user"], "session_id": "s1"
    }));
    assert_eq!(interrupted.terminal_status(), TurnStatus::Interrupted);
    let cancelled = parse_result(json!({
        "type": "result", "subtype": "error_during_execution", "is_error": true,
        "errors": ["the task was cancelled"], "session_id": "s1"
    }));
    assert_eq!(cancelled.terminal_status(), TurnStatus::Cancelled);
    let failed = parse_result(json!({
        "type": "result", "subtype": "error_max_turns", "is_error": true,
        "errors": ["too many turns"], "session_id": "s1"
    }));
    assert_eq!(failed.terminal_status(), TurnStatus::Failed);
    assert_eq!(
        failed.failure_at(false, NOW).unwrap().code,
        "error_max_turns"
    );
}

#[test]
fn an_api_key_429_carries_its_reset_from_the_message_text() {
    let result = parse_result(base_result(json!({
        "is_error": true,
        "api_error_status": 429,
        "terminal_reason": "api_error",
        "result": "API Error: 429 rate limit reached (reset after 1h 0m 0s)"
    })));
    assert_eq!(result.terminal_status(), TurnStatus::Failed);
    let failure = result.failure_at(false, NOW).unwrap();
    assert_eq!(failure.class, FailureClass::UsageLimit);
    assert_eq!(failure.code, "api_error_429");
    assert_eq!(failure.retryable, Some(true));
    assert_eq!(
        failure.message,
        "Claude API rate limit reached. Try again later."
    );
    assert_eq!(reset_seconds(&result), Some(3_600));
}

#[test]
fn a_gateway_503_is_a_usage_limit_only_for_rate_limit_errors() {
    let cases = [
        ("4m 6s", 246, "rate_limit_error"),
        ("1h 2m 3s", 3_723, "rate_limit_error"),
        ("7s", 7, "rate_limit_error"),
        ("0s", 0, "rate_limit_error"),
        ("unknown", 0, "rate_limit_error"),
        ("999999999999999999999999h", 0, "rate_limit_error"),
        ("4m 6s", 0, "overloaded_error"),
    ];
    for (cooldown, seconds, error_type) in cases {
        let value = base_result(json!({
            "is_error": true,
            "api_error_status": 503,
            "terminal_reason": "api_error",
            "result": format!(
                "API Error: 503 [claude/claude-opus-5-5] [429]: {{\"type\":\"error\",\"error\":{{\"type\":\"{error_type}\",\"message\":\"Please try again later.\"}}}} (reset after {cooldown})."
            )
        }));
        let result = parse_result(value);
        let failure = result.failure_at(false, NOW).unwrap();
        let expected_class = if seconds > 0 {
            FailureClass::UsageLimit
        } else {
            FailureClass::ProviderError
        };
        assert_eq!(
            failure.class, expected_class,
            "cooldown {cooldown}, error {error_type}"
        );
        assert_eq!(
            reset_seconds(&result),
            (seconds > 0).then_some(seconds),
            "cooldown {cooldown}, error {error_type}"
        );
    }
}

#[test]
fn a_401_with_a_cooldown_stays_a_provider_error() {
    let result = parse_result(base_result(json!({
        "is_error": true,
        "api_error_status": 401,
        "terminal_reason": "api_error",
        "result": "API Error: 401 [claude] [429]: {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\"}} (reset after 4m 6s)."
    })));
    let failure = result.failure_at(false, NOW).unwrap();
    assert_eq!(failure.class, FailureClass::ProviderError);
    assert_eq!(failure.code, "api_error_401");
}

#[test]
fn a_wrapped_proxy_429_uses_its_outer_cooldown() {
    let nested = base_result(json!({
        "is_error": true,
        "api_error_status": 429,
        "terminal_reason": "api_error",
        "result": "API Error: Request rejected (429) · All credentials for model claude-opus-5-5 are cooling down (last error: rate_limit_error (reset after 1h))"
    }));
    let result = parse_result(nested);
    let failure = result.failure_at(false, NOW).unwrap();
    assert_eq!(failure.class, FailureClass::UsageLimit);
    assert_eq!(reset_seconds(&result), None);

    let outer = base_result(json!({
        "is_error": true,
        "api_error_status": 429,
        "terminal_reason": "api_error",
        "result": "API Error: Request rejected (429) · All credentials for model claude-opus-5-5 are cooling down (reset after 1m) (last error: All credentials for model upstream are cooling down (reset after 1h))"
    }));
    let result = parse_result(outer);
    assert_eq!(reset_seconds(&result), Some(60));
}

#[test]
fn a_prior_rate_limit_evidence_reclasses_a_plain_failure() {
    let result = parse_result(base_result(json!({
        "is_error": true,
        "terminal_reason": "api_error",
        "result": "API Error"
    })));
    assert_eq!(
        result.failure_at(false, NOW).unwrap().class,
        FailureClass::ProviderError
    );
    assert_eq!(
        result.failure_at(true, NOW).unwrap().class,
        FailureClass::UsageLimit
    );
}

#[test]
fn internal_diagnostics_are_not_user_facing_errors() {
    let result = parse_result(base_result(json!({
        "is_error": true,
        "terminal_reason": "model_error",
        "errors": ["[ede_diagnostic] internal", "the model failed"]
    })));
    assert_eq!(result.user_facing_error(), Some("the model failed"));
}
