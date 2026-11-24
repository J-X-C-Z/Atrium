// Ported from packages/contracts/src/providerUsageLimits.ts (T3 Code v0.1.11, MIT).
// Ported from packages/provider-core/src/server/usageLimits.ts (T3 Code v0.1.11, MIT).
//! One report shape for every provider's usage windows.
//!
//! T3 keeps a sparse streamed update separate from a full probe: providers
//! repeat partial windows on token ticks, and the merge keeps the row ids,
//! reset times and credits already published. The same rules live here so the
//! Claude and Codex readers only have to translate their own payload.

use serde::{Deserialize, Serialize};

use crate::contracts::base::IsoDateTime;

/// T3's `ServerProviderUsageWindow["kind"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageWindowKind {
    Session,
    Weekly,
    Monthly,
    Other,
}

impl UsageWindowKind {
    fn order(self) -> u8 {
        match self {
            Self::Session => 0,
            Self::Weekly => 1,
            Self::Monthly => 2,
            Self::Other => 3,
        }
    }
}

/// T3's `ServerProviderUsageWindow`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub id: String,
    pub kind: UsageWindowKind,
    pub label: String,
    /// When present, this limit applies only to these provider model IDs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_ids: Option<Vec<String>>,
    pub used_percent: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<IsoDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_duration_mins: Option<u32>,
}

/// One kind of banked reset, such as Claude's weekly reset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderResetCreditOption {
    pub credit_id: String,
    pub label: String,
    pub description: String,
    pub available_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<IsoDateTime>,
}

/// T3's `ServerProviderResetCredits`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderResetCredits {
    pub available_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_expires_at: Option<IsoDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_credit_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<ProviderResetCreditOption>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalUsage {
    pub label: String,
    pub url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UsageLimitUnavailableReason {
    Unsupported,
    ProbeFailed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitUnavailable {
    pub reason: UsageLimitUnavailableReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// T3's `ServerProviderUsageLimits`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsageLimits {
    pub checked_at: IsoDateTime,
    pub windows: Vec<UsageWindow>,
    /// A $0 monthly spend cap has no percentage, so it cannot be a window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zero_spend_cap: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_credits: Option<ProviderResetCredits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_usage: Option<ExternalUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<UsageLimitUnavailable>,
}

/// T3's `ProviderUsageLimitsUpdate`; partial updates merge windows by id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitsUpdate {
    pub windows: Vec<UsageWindow>,
}

/// T3's `clampPercent`.
pub fn clamp_percent(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

fn sort_windows(windows: impl IntoIterator<Item = UsageWindow>) -> Vec<UsageWindow> {
    let mut windows: Vec<UsageWindow> = windows.into_iter().collect();
    windows.sort_by(|left, right| {
        left.kind
            .order()
            .cmp(&right.kind.order())
            .then_with(|| left.id.cmp(&right.id))
    });
    windows
}

/// T3's `makeUsageLimits`.
pub fn make_usage_limits(
    checked_at: impl Into<IsoDateTime>,
    windows: impl IntoIterator<Item = UsageWindow>,
) -> ProviderUsageLimits {
    ProviderUsageLimits {
        checked_at: checked_at.into(),
        windows: sort_windows(windows),
        zero_spend_cap: None,
        credential_fingerprint: None,
        reset_credits: None,
        external_usage: None,
        unavailable: None,
    }
}

/// T3's `makeUnavailableUsageLimits`.
pub fn make_unavailable_usage_limits(
    checked_at: impl Into<IsoDateTime>,
    reason: UsageLimitUnavailableReason,
    message: Option<String>,
) -> ProviderUsageLimits {
    ProviderUsageLimits {
        checked_at: checked_at.into(),
        windows: Vec::new(),
        zero_spend_cap: None,
        credential_fingerprint: None,
        reset_credits: None,
        external_usage: None,
        unavailable: Some(UsageLimitUnavailable { reason, message }),
    }
}

fn usage_window_equals(left: &UsageWindow, right: &UsageWindow) -> bool {
    left.id == right.id
        && left.kind == right.kind
        && left.label == right.label
        && left.used_percent == right.used_percent
        && left.model_ids == right.model_ids
        && left.provider_ids == right.provider_ids
        && left.resets_at == right.resets_at
        && left.window_duration_mins == right.window_duration_mins
}

/// Merge a sparse streamed update into the published report.
///
/// Returns the previous report unchanged when the update is empty, the
/// account is known to be unsupported, or no window actually moved. A first
/// sparse update is marked `probeFailed`, as T3 does, so the UI keeps asking
/// for a full probe.
pub fn apply_usage_limits_update(
    previous: Option<&ProviderUsageLimits>,
    update: &UsageLimitsUpdate,
    checked_at: &str,
) -> Option<ProviderUsageLimits> {
    if update.windows.is_empty()
        || previous
            .and_then(|limits| limits.unavailable.as_ref())
            .is_some_and(|unavailable| {
                unavailable.reason == UsageLimitUnavailableReason::Unsupported
            })
    {
        return previous.cloned();
    }

    let mut merged: Vec<UsageWindow> = previous
        .map(|limits| limits.windows.clone())
        .unwrap_or_default();
    let mut changed = false;
    for window in &update.windows {
        let existing = merged
            .iter()
            .position(|candidate| candidate.id == window.id);
        let next = UsageWindow {
            used_percent: clamp_percent(window.used_percent),
            resets_at: window
                .resets_at
                .clone()
                .or_else(|| existing.and_then(|index| merged[index].resets_at.clone())),
            window_duration_mins: window
                .window_duration_mins
                .or_else(|| existing.and_then(|index| merged[index].window_duration_mins)),
            ..window.clone()
        };
        match existing {
            Some(index) if usage_window_equals(&merged[index], &next) => {}
            Some(index) => {
                merged[index] = next;
                changed = true;
            }
            None => {
                merged.push(next);
                changed = true;
            }
        }
    }
    if !changed {
        return previous.cloned();
    }

    let unavailable = match previous {
        Some(limits) => limits.unavailable.clone(),
        None => Some(UsageLimitUnavailable {
            reason: UsageLimitUnavailableReason::ProbeFailed,
            message: None,
        }),
    };
    let mut result = make_usage_limits(checked_at, merged);
    if let Some(limits) = previous {
        result.reset_credits = limits.reset_credits.clone();
        result.zero_spend_cap = limits.zero_spend_cap;
        result.external_usage = limits.external_usage.clone();
        result.credential_fingerprint = limits.credential_fingerprint.clone();
    }
    result.unavailable = unavailable;
    Some(result)
}

/// Choose the report to publish after a full probe.
///
/// A failed probe keeps the last good windows; a newer live report keeps its
/// window values and read time while metadata comes from the successful probe.
pub fn resolve_usage_limits_after_probe(
    published: Option<&ProviderUsageLimits>,
    probed: Option<ProviderUsageLimits>,
) -> Option<ProviderUsageLimits> {
    let probe_failed = probed
        .as_ref()
        .and_then(|limits| limits.unavailable.as_ref())
        .is_some_and(|unavailable| unavailable.reason == UsageLimitUnavailableReason::ProbeFailed);
    if probe_failed && published.is_some_and(|limits| limits.unavailable.is_none()) {
        return published.cloned();
    }
    if let (Some(published), Some(probed)) = (published, probed.as_ref())
        && published.unavailable.is_none()
        && probed.unavailable.is_none()
        && let (Some(published_ms), Some(probed_ms)) = (
            parse_ms(&published.checked_at),
            parse_ms(&probed.checked_at),
        )
        && published_ms > probed_ms
    {
        return Some(ProviderUsageLimits {
            checked_at: published.checked_at.clone(),
            windows: published.windows.clone(),
            ..probed.clone()
        });
    }
    probed
}

/// Parse an ISO timestamp into epoch milliseconds, as T3's `Date.parse` does.
pub(crate) fn parse_ms(value: &str) -> Option<i64> {
    value
        .parse::<jiff::Timestamp>()
        .ok()
        .map(|ts| ts.as_millisecond())
}

/// Format epoch milliseconds exactly as T3's `DateTime.formatIso` does.
pub(crate) fn format_iso_millis(ms: i64) -> Option<String> {
    let timestamp = jiff::Timestamp::from_millisecond(ms).ok()?;
    Some(format_timestamp(timestamp))
}

pub(crate) fn format_timestamp(timestamp: jiff::Timestamp) -> String {
    let zoned = timestamp.to_zoned(jiff::tz::TimeZone::UTC);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        zoned.year(),
        zoned.month(),
        zoned.day(),
        zoned.hour(),
        zoned.minute(),
        zoned.second(),
        zoned.millisecond()
    )
}

/// Epoch seconds to T3's ISO string.
pub(crate) fn iso_from_epoch_seconds(value: Option<f64>) -> Option<String> {
    let value = value?;
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let ms = (value * 1000.0) as i64;
    format_iso_millis(ms)
}

/// An ISO input to T3's canonical ISO form, or `None` when it does not parse.
pub(crate) fn canonical_iso(value: &str) -> Option<String> {
    let timestamp = value.parse::<jiff::Timestamp>().ok()?;
    Some(format_timestamp(timestamp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CHECKED_AT: &str = "2026-09-03T12:00:00.000Z";

    fn session() -> UsageWindow {
        UsageWindow {
            id: "five_hour".into(),
            kind: UsageWindowKind::Session,
            label: "Session".into(),
            model_ids: None,
            provider_ids: None,
            used_percent: 40.0,
            resets_at: Some("2026-09-03T14:00:00.000Z".into()),
            window_duration_mins: Some(300),
        }
    }

    fn weekly() -> UsageWindow {
        UsageWindow {
            id: "seven_day".into(),
            kind: UsageWindowKind::Weekly,
            label: "Weekly".into(),
            model_ids: None,
            provider_ids: None,
            used_percent: 20.0,
            resets_at: None,
            window_duration_mins: Some(10_080),
        }
    }

    fn published() -> ProviderUsageLimits {
        make_usage_limits(CHECKED_AT, [session(), weekly()])
    }

    fn update(windows: Vec<UsageWindow>) -> UsageLimitsUpdate {
        UsageLimitsUpdate { windows }
    }

    #[test]
    fn returns_the_published_report_when_no_window_moved() {
        let previous = published();
        let next = apply_usage_limits_update(
            Some(&previous),
            &update(vec![
                weekly(),
                UsageWindow {
                    id: "five_hour".into(),
                    kind: UsageWindowKind::Session,
                    label: "Session".into(),
                    model_ids: None,
                    provider_ids: None,
                    used_percent: 40.0,
                    resets_at: None,
                    window_duration_mins: None,
                },
            ]),
            "2026-09-03T12:00:05.000Z",
        );
        assert_eq!(next, Some(previous));

        let scoped = make_usage_limits(
            CHECKED_AT,
            [UsageWindow {
                model_ids: Some(vec!["opencode/a".into()]),
                ..weekly()
            }],
        );
        let provider_scoped = make_usage_limits(
            CHECKED_AT,
            [UsageWindow {
                provider_ids: Some(vec!["opencode".into()]),
                ..weekly()
            }],
        );
        let changed_provider = apply_usage_limits_update(
            Some(&provider_scoped),
            &update(vec![UsageWindow {
                provider_ids: Some(vec!["console-custom".into()]),
                ..weekly()
            }]),
            CHECKED_AT,
        )
        .unwrap();
        assert_eq!(
            changed_provider.windows[0].provider_ids,
            Some(vec!["console-custom".into()])
        );
        assert_eq!(
            apply_usage_limits_update(
                Some(&scoped),
                &update(vec![UsageWindow {
                    model_ids: Some(vec!["opencode/a".into()]),
                    ..weekly()
                }]),
                CHECKED_AT,
            ),
            Some(scoped.clone())
        );
        let changed_model = apply_usage_limits_update(
            Some(&scoped),
            &update(vec![UsageWindow {
                model_ids: Some(vec!["opencode/b".into()]),
                ..weekly()
            }]),
            CHECKED_AT,
        )
        .unwrap();
        assert_eq!(
            changed_model.windows[0].model_ids,
            Some(vec!["opencode/b".into()])
        );
    }

    #[test]
    fn upserts_by_id_and_keeps_the_reset_a_percent_only_update_omits() {
        let next = apply_usage_limits_update(
            Some(&published()),
            &update(vec![UsageWindow {
                resets_at: None,
                window_duration_mins: None,
                used_percent: 55.0,
                ..session()
            }]),
            "2026-09-03T12:00:05.000Z",
        )
        .unwrap();
        assert_eq!(
            next,
            make_usage_limits(
                "2026-09-03T12:00:05.000Z",
                [
                    UsageWindow {
                        used_percent: 55.0,
                        ..session()
                    },
                    weekly()
                ]
            )
        );
    }

    #[test]
    fn leaves_an_unsupported_account_and_an_empty_update_alone() {
        let unsupported = make_unavailable_usage_limits(
            CHECKED_AT,
            UsageLimitUnavailableReason::Unsupported,
            None,
        );
        assert_eq!(
            apply_usage_limits_update(Some(&unsupported), &update(vec![session()]), CHECKED_AT),
            Some(unsupported)
        );
        let previous = published();
        assert_eq!(
            apply_usage_limits_update(Some(&previous), &update(Vec::new()), CHECKED_AT),
            Some(previous)
        );
    }

    #[test]
    fn marks_a_first_sparse_update_as_unavailable() {
        let next = apply_usage_limits_update(None, &update(vec![weekly()]), CHECKED_AT).unwrap();
        assert_eq!(
            next,
            ProviderUsageLimits {
                unavailable: Some(UsageLimitUnavailable {
                    reason: UsageLimitUnavailableReason::ProbeFailed,
                    message: None,
                }),
                ..make_usage_limits(CHECKED_AT, [weekly()])
            }
        );
    }

    #[test]
    fn preserves_reset_credits_and_a_zero_cap_when_a_window_update_changes_usage() {
        let reset_credits = ProviderResetCredits {
            available_count: 2,
            next_expires_at: Some("2026-10-01T00:00:00.000Z".into()),
            next_credit_id: None,
            options: None,
        };
        let previous = ProviderUsageLimits {
            reset_credits: Some(reset_credits.clone()),
            zero_spend_cap: Some(true),
            ..published()
        };
        let next = apply_usage_limits_update(
            Some(&previous),
            &update(vec![UsageWindow {
                used_percent: 55.0,
                ..session()
            }]),
            "2026-09-03T12:00:05.000Z",
        )
        .unwrap();
        assert_eq!(
            next,
            ProviderUsageLimits {
                reset_credits: Some(reset_credits),
                zero_spend_cap: Some(true),
                ..make_usage_limits(
                    "2026-09-03T12:00:05.000Z",
                    [
                        UsageWindow {
                            used_percent: 55.0,
                            ..session()
                        },
                        weekly()
                    ]
                )
            }
        );
    }

    #[test]
    fn uses_read_order_for_windows_while_taking_metadata_from_the_successful_probe() {
        let live = ProviderUsageLimits {
            reset_credits: Some(ProviderResetCredits {
                available_count: 2,
                next_expires_at: None,
                next_credit_id: None,
                options: None,
            }),
            external_usage: Some(ExternalUsage {
                label: "Old dashboard".into(),
                url: "https://example.com/old".into(),
            }),
            ..make_usage_limits(
                "2026-09-03T12:00:05.000Z",
                [
                    UsageWindow {
                        used_percent: 95.0,
                        ..session()
                    },
                    UsageWindow {
                        used_percent: 70.0,
                        ..weekly()
                    },
                ],
            )
        };
        let probed = ProviderUsageLimits {
            reset_credits: Some(ProviderResetCredits {
                available_count: 1,
                next_expires_at: None,
                next_credit_id: None,
                options: None,
            }),
            external_usage: Some(ExternalUsage {
                label: "Usage dashboard".into(),
                url: "https://example.com/usage".into(),
            }),
            ..make_usage_limits(
                CHECKED_AT,
                [UsageWindow {
                    used_percent: 0.0,
                    ..session()
                }],
            )
        };
        let resolved = resolve_usage_limits_after_probe(Some(&live), Some(probed.clone())).unwrap();
        assert_eq!(resolved.checked_at, live.checked_at);
        assert_eq!(resolved.windows, live.windows);
        assert_eq!(resolved.external_usage, probed.external_usage);
        assert_eq!(resolved.reset_credits, probed.reset_credits);

        let same_time = ProviderUsageLimits {
            checked_at: live.checked_at.clone(),
            ..probed.clone()
        };
        assert_eq!(
            resolve_usage_limits_after_probe(Some(&live), Some(same_time.clone())),
            Some(same_time)
        );
        let newer = ProviderUsageLimits {
            checked_at: "2026-09-03T12:00:06.000Z".into(),
            ..probed.clone()
        };
        assert_eq!(
            resolve_usage_limits_after_probe(Some(&live), Some(newer.clone())),
            Some(newer)
        );
    }

    #[test]
    fn keeps_the_last_good_windows_through_a_failed_probe_but_not_an_unsupported_one() {
        let previous = published();
        let failed = make_unavailable_usage_limits(
            CHECKED_AT,
            UsageLimitUnavailableReason::ProbeFailed,
            None,
        );
        let unsupported = make_unavailable_usage_limits(
            CHECKED_AT,
            UsageLimitUnavailableReason::Unsupported,
            None,
        );
        assert_eq!(
            resolve_usage_limits_after_probe(Some(&previous), Some(failed.clone())),
            Some(previous)
        );
        assert_eq!(
            resolve_usage_limits_after_probe(Some(&published()), Some(unsupported.clone())),
            Some(unsupported)
        );
        assert_eq!(
            resolve_usage_limits_after_probe(None, Some(failed.clone())),
            Some(failed)
        );
    }

    #[test]
    fn report_shape_keeps_t3_field_names() {
        let value = serde_json::to_value(published()).unwrap();
        assert_eq!(
            value,
            json!({
                "checkedAt": CHECKED_AT,
                "windows": [
                    {
                        "id": "five_hour",
                        "kind": "session",
                        "label": "Session",
                        "usedPercent": 40.0,
                        "resetsAt": "2026-09-03T14:00:00.000Z",
                        "windowDurationMins": 300
                    },
                    {
                        "id": "seven_day",
                        "kind": "weekly",
                        "label": "Weekly",
                        "usedPercent": 20.0,
                        "windowDurationMins": 10080
                    }
                ]
            })
        );
    }
}
