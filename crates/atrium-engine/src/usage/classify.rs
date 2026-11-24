// Ported from apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/provider/CodexManagedErrors.ts (T3 Code v0.1.11, MIT).
//! Recognize a failed turn that stopped on a usage limit.
//!
//! Providers give the same fact in different shapes: Claude sets an HTTP 429
//! on the result or a rejected `rate_limit_info`, and Codex names a structured
//! error code. Both sometimes print the reset only in prose. Classification
//! returns the shared [`ProviderFailure`] with a reset time when one is known;
//! nothing here sleeps or dispatches.

use crate::contracts::item::{ProviderFailure, ProviderFailureClass};

use super::codex::codex_message_reset_at;
use super::limits::{format_iso_millis, parse_ms};

/// The result fields Claude's adapter reads when a turn ends.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClaudeFailureInput {
    /// `success` or one of the SDK's error subtypes.
    pub subtype: String,
    pub is_error: bool,
    pub terminal_reason: Option<String>,
    pub api_error_status: Option<u16>,
    pub result: String,
    pub errors: Vec<String>,
    pub failure_hint: Option<String>,
    /// Set when a `rate_limit_info` frame said the turn was rejected.
    pub usage_limited: bool,
}

/// T3's `SDKAPIRetryMessage` fields that matter here.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaudeApiRetry {
    pub error: String,
    pub error_status: Option<u16>,
}

fn make_failure(
    class: ProviderFailureClass,
    message: impl Into<String>,
    code: Option<String>,
    retryable: Option<bool>,
    reset_at: Option<String>,
) -> ProviderFailure {
    ProviderFailure {
        class,
        message: message.into(),
        code,
        retryable,
        reset_at,
    }
}

fn result_user_facing_error(input: &ClaudeFailureInput) -> Option<&str> {
    if input.subtype == "success" && !input.is_error {
        return None;
    }
    input
        .errors
        .iter()
        .find(|error| !error.starts_with("[ede_diagnostic]"))
        .map(String::as_str)
}

const CONTEXT_WINDOW_FULL_ERROR: &str = "Claude stopped: the prompt exceeds the model's context window. Run /compact or start a new thread.";

fn terminal_result_error(reason: Option<&str>, failure_hint: Option<&str>) -> Option<String> {
    let text = match reason? {
        "api_error" => {
            return Some(
                failure_hint
                    .map(str::to_owned)
                    .unwrap_or_else(|| "Claude gave up after repeated API errors.".into()),
            );
        }
        "malformed_tool_use_exhausted" => "Claude gave up after repeated malformed tool calls.",
        "budget_exhausted" => "Claude stopped: the turn's token budget was exhausted.",
        "structured_output_retry_exhausted" => {
            "Claude could not produce the requested structured output."
        }
        "tool_deferred_unavailable" => {
            "Claude could not resume a deferred tool call: the tool is no longer available."
        }
        "turn_setup_failed" => "Claude could not start the turn.",
        "blocking_limit" => {
            return Some(
                failure_hint
                    .map(str::to_owned)
                    .unwrap_or_else(|| CONTEXT_WINDOW_FULL_ERROR.into()),
            );
        }
        "rapid_refill_breaker" => {
            "Claude stopped: the context refilled too quickly after compaction."
        }
        "prompt_too_long" => CONTEXT_WINDOW_FULL_ERROR,
        "image_error" => "Claude stopped: an image in the conversation could not be processed.",
        "model_error" => "Claude stopped: the model returned an error.",
        _ => return None,
    };
    Some(text.into())
}

/// Parse the `(reset after 2h 30m 5s)` form. Zero seconds is not a reset.
fn cooldown_seconds(text: &str) -> Option<i64> {
    let index = text.find("(reset after ")?;
    let mut rest = &text[index + "(reset after ".len()..];
    let mut seconds = 0_i64;
    if let Some((value, tail)) = take_number(rest, 'h') {
        seconds += value * 3_600;
        rest = tail;
    }
    if let Some((value, tail)) = take_number(rest, 'm') {
        seconds += value * 60;
        rest = tail;
    }
    if let Some((value, tail)) = take_number(rest, 's') {
        seconds += value;
        rest = tail;
    }
    let _ = rest;
    (seconds > 0).then_some(seconds)
}

fn take_number(input: &str, suffix: char) -> Option<(i64, &str)> {
    let input = input.trim_start_matches(' ');
    let digits = input.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let (number, rest) = input.split_at(digits);
    let rest = rest.strip_prefix(suffix)?;
    Some((number.parse().ok()?, rest.trim_start_matches(' ')))
}

fn cooldown_reset_at(seconds: Option<i64>, received_at: &str) -> Option<String> {
    let seconds = seconds?;
    let received_ms = parse_ms(received_at)?;
    let reset_ms = received_ms.checked_add(seconds.checked_mul(1_000)?)?;
    format_iso_millis(reset_ms)
}

fn gateway_cooldown(message: &str, received_at: &str) -> Option<String> {
    if !message.starts_with("API Error: 503 ") {
        return None;
    }
    if !message.contains("[429]:") || !message.contains("rate_limit_error") {
        return None;
    }
    cooldown_reset_at(cooldown_seconds(message), received_at)
}

/// T3's `providerFailureFromResult`.
pub fn claude_failure_from_result(
    input: &ClaudeFailureInput,
    received_at: &str,
) -> Option<ProviderFailure> {
    let gateway_reset_at = if input.failure_hint.is_none()
        && input.subtype == "success"
        && input.is_error
        && input.terminal_reason.as_deref() == Some("api_error")
        && input.api_error_status == Some(503)
    {
        gateway_cooldown(&input.result, received_at)
    } else {
        None
    };
    if let Some(reset_at) = gateway_reset_at {
        return Some(make_failure(
            ProviderFailureClass::UsageLimit,
            "Claude API rate limit reached. Try again later.",
            Some("api_error_429".into()),
            Some(true),
            Some(reset_at),
        ));
    }

    let usage_limited =
        (input.subtype == "success" && input.api_error_status == Some(429)) || input.usage_limited;
    let failure_class = if usage_limited {
        ProviderFailureClass::UsageLimit
    } else {
        ProviderFailureClass::ProviderError
    };
    let listed_error = result_user_facing_error(input);
    let overloaded = input.subtype == "success" && input.api_error_status == Some(529);
    let structured_error = if overloaded {
        Some("Claude API is overloaded (529). Try again shortly.".to_owned())
    } else if input.subtype == "success" && input.api_error_status == Some(429) {
        Some("Claude API rate limit reached. Try again later.".to_owned())
    } else {
        terminal_result_error(
            input.terminal_reason.as_deref(),
            input.failure_hint.as_deref(),
        )
    };

    if input.subtype != "success" {
        let message = listed_error
            .map(str::to_owned)
            .or(structured_error)
            .unwrap_or_else(|| input.errors.join("\n"));
        return Some(make_failure(
            failure_class,
            message,
            Some(input.subtype.clone()),
            None,
            None,
        ));
    }
    if !input.is_error && structured_error.is_none() {
        return None;
    }

    let api_error_status = input.api_error_status;
    // API-key sessions get no rate_limit_info, so a gateway's 429 carries its
    // reset only as text.
    let reset_at = if api_error_status == Some(429) && input.result.starts_with("API Error:") {
        cooldown_reset_at(cooldown_seconds(&input.result), received_at)
    } else {
        None
    };
    Some(make_failure(
        failure_class,
        listed_error
            .map(str::to_owned)
            .or(structured_error)
            .or_else(|| input.failure_hint.clone())
            .unwrap_or_else(|| input.result.clone()),
        Some(
            api_error_status
                .map(|status| format!("api_error_{status}"))
                .or_else(|| input.terminal_reason.clone())
                .unwrap_or_else(|| "sdk_result_error".into()),
        ),
        matches!(api_error_status, Some(429 | 529)).then_some(true),
        reset_at,
    ))
}

/// T3's `providerFailureFromApiRetry`.
pub fn claude_failure_from_api_retry(retry: &ClaudeApiRetry) -> ProviderFailure {
    let class = match retry.error_status {
        Some(429) => ProviderFailureClass::UsageLimit,
        None => ProviderFailureClass::TransportError,
        _ => ProviderFailureClass::ProviderError,
    };
    make_failure(
        class,
        format!("Claude API {}.", retry.error.replace('_', " ")),
        Some(match retry.error_status {
            Some(status) => format!("api_error_{status}"),
            None => retry.error.clone(),
        }),
        Some(true),
        None,
    )
}

/// Codex's structured error info. The adapter only reads the HTTP status of
/// the transport and retry-exhaustion variants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodexErrorInfo {
    ResponseTooManyFailedAttempts { http_status_code: Option<u16> },
    ResponseStreamDisconnected { http_status_code: Option<u16> },
    ResponseStreamConnectionFailed { http_status_code: Option<u16> },
    HttpConnectionFailed { http_status_code: Option<u16> },
    Other(String),
}

impl CodexErrorInfo {
    fn http_status(&self) -> Option<u16> {
        match self {
            Self::ResponseTooManyFailedAttempts { http_status_code }
            | Self::ResponseStreamDisconnected { http_status_code }
            | Self::ResponseStreamConnectionFailed { http_status_code }
            | Self::HttpConnectionFailed { http_status_code } => *http_status_code,
            Self::Other(_) => None,
        }
    }

    fn exhaustion_http_status(&self) -> Option<u16> {
        match self {
            Self::ResponseTooManyFailedAttempts { http_status_code } => *http_status_code,
            _ => None,
        }
    }

    fn is_transport(&self) -> bool {
        matches!(
            self,
            Self::ResponseStreamDisconnected { .. }
                | Self::ResponseStreamConnectionFailed { .. }
                | Self::HttpConnectionFailed { .. }
        )
    }
}

/// Retry exhaustion on 429 is a usage or rate limit; other statuses stay
/// provider or transport errors.
pub fn codex_usage_limit_error(code: Option<&str>, error_info: Option<&CodexErrorInfo>) -> bool {
    matches!(code, Some("usageLimitExceeded" | "rateLimitExceeded"))
        || error_info.and_then(CodexErrorInfo::exhaustion_http_status) == Some(429)
}

/// T3's `codexTerminalFailureClass`.
pub fn codex_terminal_failure_class(
    code: Option<&str>,
    error_info: Option<&CodexErrorInfo>,
) -> ProviderFailureClass {
    if codex_usage_limit_error(code, error_info) {
        return ProviderFailureClass::UsageLimit;
    }
    if error_info.is_some_and(|info| info.is_transport() && info.http_status().is_none()) {
        return ProviderFailureClass::TransportError;
    }
    ProviderFailureClass::ProviderError
}

/// T3's `codexTerminalErrorNeedsEvidence`: a generic error code, or retry
/// exhaustion without an HTTP status, must be corroborated before it is shown
/// as the failure.
pub fn codex_error_needs_evidence(code: Option<&str>, error_info: Option<&CodexErrorInfo>) -> bool {
    code == Some("other")
        || (code == Some("responseTooManyFailedAttempts")
            && error_info
                .and_then(CodexErrorInfo::exhaustion_http_status)
                .is_none())
}

/// A ChatGPT-sharing failure Codex reports inside its generic turn error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexManagedFailure {
    pub code: String,
    pub message: String,
    pub usage_limit: bool,
}

const SHARING_USAGE_LIMIT_EXCEEDED: &str = "subscription_sharing_usage_limit_exceeded";
const SHARING_WORKSPACE_USAGE_LIMIT_EXCEEDED: &str =
    "subscription_sharing_workplace_individual_usage_limit_exceeded";

fn managed_message(code: &str) -> Option<&'static str> {
    Some(match code {
        "subscription_sharing_v2_user_not_eligible" | "subscription_sharing_user_not_eligible" => {
            "ChatGPT sharing is unavailable for this account or workspace. Use another provider or check its sharing policy."
        }
        SHARING_USAGE_LIMIT_EXCEEDED => {
            "Your ChatGPT usage limit was reached. Check ChatGPT Usage settings for your available allowance."
        }
        SHARING_WORKSPACE_USAGE_LIMIT_EXCEEDED => {
            "You reached your workspace's limit for using ChatGPT in other apps. Ask your workspace admin to increase it."
        }
        "subscription_sharing_usage_unavailable" => {
            "ChatGPT usage is temporarily unavailable. Try again shortly."
        }
        "subscription_sharing_unsupported_capability" => {
            "Codex used a feature that ChatGPT sharing does not support. Use another provider for this request."
        }
        "subscription_sharing_v2_client_not_enabled" => {
            "This app is not enabled for this ChatGPT connection. Use your existing CLI or another provider."
        }
        "subscription_sharing_v2_route_not_supported"
        | "subscription_sharing_route_not_supported" => {
            "ChatGPT does not support this request route."
        }
        "subscription_sharing_v2_invalid_user" | "subscription_sharing_invalid_user" => {
            "ChatGPT could not validate this connection. Check the selected account and sharing permissions."
        }
        "subscription_sharing_v2_user_unavailable" | "subscription_sharing_user_unavailable" => {
            "ChatGPT is temporarily unavailable. Try again shortly."
        }
        "chatpass_v2_scope_not_authorized" => {
            "This ChatGPT grant does not authorize the request. Check the connection's sharing permissions."
        }
        "chatpass_v2_invalid_authorization_context" => {
            "ChatGPT could not authorize this connection. Check the client and sharing permissions."
        }
        _ => return None,
    })
}

fn unwrap_error(value: &serde_json::Value) -> serde_json::Value {
    if let Some(text) = value.as_str() {
        return serde_json::from_str(text).unwrap_or_else(|_| value.clone());
    }
    match value.get("error") {
        Some(error) => error.clone(),
        None => value.clone(),
    }
}

fn sharing_code(value: &serde_json::Value) -> Option<String> {
    let error = unwrap_error(value);
    let code = match error {
        serde_json::Value::String(code) => Some(code),
        serde_json::Value::Object(object) => object
            .get("code")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        _ => None,
    }?;
    managed_message(&code).map(|_| code)
}

/// T3's `classifyCodexManagedError`. The sharing limit codes upgrade a Codex
/// failure to `usage_limit`; the other sharing failures keep their class.
pub fn classify_codex_managed_error(value: &serde_json::Value) -> Option<CodexManagedFailure> {
    let error = unwrap_error(value);
    let messages: Vec<String> = match &error {
        serde_json::Value::String(message) => vec![message.clone()],
        serde_json::Value::Object(object) => ["message", "additionalDetails"]
            .iter()
            .filter_map(|key| object.get(*key).and_then(serde_json::Value::as_str))
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    };
    let code = sharing_code(&error)
        .or_else(|| {
            messages
                .iter()
                .find_map(|message| sharing_code(&serde_json::Value::String(message.clone())))
        })
        .or_else(|| {
            messages
                .iter()
                .any(|message| {
                    message.contains("has reached their Subscription Sharing usage limit")
                })
                .then(|| SHARING_USAGE_LIMIT_EXCEEDED.to_owned())
        })?;
    let mut message = managed_message(&code)?.to_owned();
    let text = messages.join(" ");
    if code == "subscription_sharing_unsupported_capability" {
        if text.contains("tool 'namespace'") {
            message = "Codex sent a tool namespace that ChatGPT sharing does not support. Use another provider for this request.".into();
        } else if text.contains("additional_tools") {
            message = "Codex sent an input item that ChatGPT sharing does not support. Use another provider for this request.".into();
        }
    }
    Some(CodexManagedFailure {
        usage_limit: code == SHARING_USAGE_LIMIT_EXCEEDED
            || code == SHARING_WORKSPACE_USAGE_LIMIT_EXCEEDED,
        code,
        message,
    })
}

/// T3's `withCodexManagedFailure`.
pub fn with_codex_managed_failure(
    failure: ProviderFailure,
    native_error: &serde_json::Value,
) -> ProviderFailure {
    let Some(managed) = classify_codex_managed_error(native_error) else {
        return failure;
    };
    ProviderFailure {
        class: if managed.usage_limit {
            ProviderFailureClass::UsageLimit
        } else {
            failure.class
        },
        message: managed.message,
        code: Some(managed.code),
        ..failure
    }
}

/// Codex's reset: an exhausted snapshot wins, otherwise the printed text.
/// Sharing caps have no reset timestamp in the ordinary snapshot.
pub fn codex_failure_reset_at(
    failure: &ProviderFailure,
    snapshot_reset_at: Option<String>,
    text: Option<(&str, &jiff::tz::TimeZone)>,
) -> Option<String> {
    if classify_codex_managed_error(&serde_json::Value::String(
        failure.code.clone().unwrap_or_default(),
    ))
    .is_some_and(|managed| managed.usage_limit)
    {
        return None;
    }
    snapshot_reset_at.or_else(|| {
        let (failed_at, zone) = text?;
        codex_message_reset_at(&failure.message, failed_at, zone)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const RECEIVED_AT: &str = "2026-07-18T10:00:00.000Z";

    fn claude_subtype(subtype: &str) -> ClaudeFailureInput {
        ClaudeFailureInput {
            subtype: subtype.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_429_result_is_a_usage_limit_with_a_structured_code() {
        let failure = claude_failure_from_result(
            &ClaudeFailureInput {
                api_error_status: Some(429),
                ..claude_subtype("success")
            },
            RECEIVED_AT,
        )
        .unwrap();
        assert_eq!(failure.class, ProviderFailureClass::UsageLimit);
        assert_eq!(failure.code.as_deref(), Some("api_error_429"));
        assert_eq!(failure.retryable, Some(true));
        assert_eq!(failure.reset_at, None);
    }

    #[test]
    fn a_gateway_429_carries_its_cooldown_from_the_text() {
        let failure = claude_failure_from_result(
            &ClaudeFailureInput {
                api_error_status: Some(429),
                is_error: true,
                result: "API Error: Request rejected (429) · All credentials for model claude-opus are cooling down (reset after 2h 30m 5s)".into(),
                ..claude_subtype("success")
            },
            RECEIVED_AT,
        )
        .unwrap();
        assert_eq!(failure.class, ProviderFailureClass::UsageLimit);
        assert_eq!(failure.code.as_deref(), Some("api_error_429"));
        assert_eq!(
            failure.reset_at.as_deref(),
            Some("2026-07-18T12:30:05.000Z")
        );
    }

    #[test]
    fn a_503_gateway_that_names_429_is_a_limit_even_though_the_status_is_503() {
        let failure = claude_failure_from_result(
            &ClaudeFailureInput {
                api_error_status: Some(503),
                is_error: true,
                terminal_reason: Some("api_error".into()),
                result: r#"API Error: 503 [upstream] [429]: {"type":"error","error":{"type":"rate_limit_error","message":"slow down"}} (reset after 5m)"#.into(),
                ..claude_subtype("success")
            },
            RECEIVED_AT,
        )
        .unwrap();
        assert_eq!(failure.class, ProviderFailureClass::UsageLimit);
        assert_eq!(failure.code.as_deref(), Some("api_error_429"));
        assert_eq!(
            failure.reset_at.as_deref(),
            Some("2026-07-18T10:05:00.000Z")
        );
    }

    #[test]
    fn a_non_429_result_stays_a_provider_error() {
        let failure = claude_failure_from_result(
            &ClaudeFailureInput {
                api_error_status: Some(500),
                is_error: true,
                result: "API Error: 500".into(),
                ..claude_subtype("success")
            },
            RECEIVED_AT,
        )
        .unwrap();
        assert_eq!(failure.class, ProviderFailureClass::ProviderError);
        assert_eq!(failure.code.as_deref(), Some("api_error_500"));
        assert_eq!(failure.retryable, None);
    }

    #[test]
    fn a_rate_limit_frame_marks_the_result_as_a_usage_limit() {
        let failure = claude_failure_from_result(
            &ClaudeFailureInput {
                is_error: true,
                result: "Your limit was reached".into(),
                usage_limited: true,
                ..claude_subtype("success")
            },
            RECEIVED_AT,
        )
        .unwrap();
        assert_eq!(failure.class, ProviderFailureClass::UsageLimit);
    }

    #[test]
    fn an_api_retry_with_429_is_a_usage_limit() {
        let failure = claude_failure_from_api_retry(&ClaudeApiRetry {
            error: "rate_limit_error".into(),
            error_status: Some(429),
        });
        assert_eq!(failure.class, ProviderFailureClass::UsageLimit);
        assert_eq!(failure.code.as_deref(), Some("api_error_429"));
        assert_eq!(failure.retryable, Some(true));
    }

    #[test]
    fn codex_structured_codes_and_exhaustion_429_are_limits() {
        assert!(codex_usage_limit_error(Some("usageLimitExceeded"), None));
        assert!(codex_usage_limit_error(Some("rateLimitExceeded"), None));
        assert!(codex_usage_limit_error(
            Some("responseTooManyFailedAttempts"),
            Some(&CodexErrorInfo::ResponseTooManyFailedAttempts {
                http_status_code: Some(429)
            })
        ));
        assert_eq!(
            codex_terminal_failure_class(Some("usageLimitExceeded"), None),
            ProviderFailureClass::UsageLimit
        );
        assert_eq!(
            codex_terminal_failure_class(
                Some("responseTooManyFailedAttempts"),
                Some(&CodexErrorInfo::ResponseTooManyFailedAttempts {
                    http_status_code: Some(500)
                })
            ),
            ProviderFailureClass::ProviderError
        );
    }

    #[test]
    fn codex_transport_without_a_status_is_a_transport_error() {
        assert_eq!(
            codex_terminal_failure_class(
                Some("responseStreamDisconnected"),
                Some(&CodexErrorInfo::ResponseStreamDisconnected {
                    http_status_code: None
                })
            ),
            ProviderFailureClass::TransportError
        );
        assert!(codex_error_needs_evidence(Some("other"), None));
        assert!(codex_error_needs_evidence(
            Some("responseTooManyFailedAttempts"),
            None
        ));
        assert!(!codex_error_needs_evidence(
            Some("responseTooManyFailedAttempts"),
            Some(&CodexErrorInfo::ResponseTooManyFailedAttempts {
                http_status_code: Some(429)
            })
        ));
    }

    #[test]
    fn a_sharing_usage_limit_upgrades_a_codex_failure() {
        let native = json!({ "error": { "code": SHARING_USAGE_LIMIT_EXCEEDED } });
        let managed = classify_codex_managed_error(&native).unwrap();
        assert!(managed.usage_limit);
        assert_eq!(managed.code, SHARING_USAGE_LIMIT_EXCEEDED);
        let upgraded = with_codex_managed_failure(
            ProviderFailure {
                class: ProviderFailureClass::ProviderError,
                message: "Codex failed".into(),
                code: Some("other".into()),
                retryable: None,
                reset_at: None,
            },
            &native,
        );
        assert_eq!(upgraded.class, ProviderFailureClass::UsageLimit);
        assert_eq!(upgraded.code.as_deref(), Some(SHARING_USAGE_LIMIT_EXCEEDED));
    }

    #[test]
    fn the_sharing_prose_without_its_code_is_still_recognized() {
        let managed = classify_codex_managed_error(&json!({
            "message": "This account has reached their Subscription Sharing usage limit."
        }))
        .unwrap();
        assert!(managed.usage_limit);
        assert_eq!(managed.code, SHARING_USAGE_LIMIT_EXCEEDED);
    }

    #[test]
    fn a_sharing_limit_has_no_snapshot_reset() {
        let failure = ProviderFailure {
            class: ProviderFailureClass::UsageLimit,
            message: SHARING_USAGE_LIMIT_EXCEEDED.into(),
            code: Some(SHARING_USAGE_LIMIT_EXCEEDED.into()),
            retryable: None,
            reset_at: None,
        };
        assert_eq!(
            codex_failure_reset_at(&failure, Some("2026-07-18T12:00:00.000Z".into()), None),
            None
        );
    }
}
