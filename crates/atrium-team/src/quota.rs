//! Whose limit is hit, and until when. Nothing finer: a member is either
//! usable or spent until a time, from the backend's usage report or a
//! turn that failed with a usage-limit message.

use std::collections::BTreeMap;

/// When a limit was hit without saying when it resets, try again after this.
pub const UNKNOWN_RESET_SECS: i64 = 60 * 60;

/// Spent members, by member name, with when each is usable again.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Quotas {
    pub spent: BTreeMap<String, i64>,
}

impl Quotas {
    /// A member's limit was hit (a usage report saying so, or a failed
    /// turn whose message [`is_limit_message`]); `resets_at` if known.
    pub fn hit(&mut self, member: &str, resets_at: Option<i64>, now: i64) {
        self.spent.insert(
            member.to_string(),
            resets_at
                .filter(|at| *at > now)
                .unwrap_or(now + UNKNOWN_RESET_SECS),
        );
    }

    /// A usage report says the member is below its limit again.
    pub fn cleared(&mut self, member: &str) {
        self.spent.remove(member);
    }

    /// When `member` is usable again, if it is spent now.
    pub fn spent_until(&self, member: &str, now: i64) -> Option<i64> {
        self.spent.get(member).copied().filter(|at| *at > now)
    }
}

/// Whether a failed turn's message says a usage limit was hit.
pub fn is_limit_message(text: &str) -> bool {
    let text = text.to_lowercase();
    [
        "usage limit",
        "rate limit",
        "quota",
        "limit reached",
        "out of credits",
        "insufficient balance",
        "too many requests",
        "额度",
        "用量上限",
    ]
    .iter()
    .any(|p| text.contains(p))
}
