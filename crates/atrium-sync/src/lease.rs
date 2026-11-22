use serde::{Deserialize, Serialize};

/// Which device may run a project's scheduler and autopilot, until when.
///
/// The value is stored under `project/<id>/scheduler`. Each device renews it
/// every minute while it runs the scheduler. A different device may take over
/// only after the lease has been expired for ten minutes and the user
/// confirms.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub device: String,
    /// Unix milliseconds.
    pub until_ms: i64,
}

pub const LEASE_RENEW_MS: i64 = 60 * 1_000;
pub const LEASE_TAKEOVER_GRACE_MS: i64 = 10 * 60 * 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseDecision {
    /// The caller already owns a live lease.
    Keep,
    /// Another device's lease is live; do not run.
    Wait,
    /// The lease is expired but the old device may still be reachable.
    ConfirmWithOwner,
    /// The lease has been expired past the grace window; the user may take it.
    CanTakeOver,
}

/// Decides what a device may do with a project's scheduler lease.
///
/// `owner_reachable` is the result of checking the old device directly when
/// it is reachable. A reachable owner always wins while its lease is not
/// expired; after expiry the caller must still ask the user.
pub fn lease_can_be_taken(
    lease: Option<&Lease>,
    me: &str,
    now_ms: i64,
    owner_reachable: bool,
) -> LeaseDecision {
    let Some(lease) = lease else {
        return LeaseDecision::CanTakeOver;
    };
    if lease.device == me {
        return if lease.until_ms >= now_ms {
            LeaseDecision::Keep
        } else {
            // Its own expired lease is not a takeover from another device;
            // renew immediately after a sleep or a paused loop.
            LeaseDecision::CanTakeOver
        };
    }
    if lease.until_ms >= now_ms {
        return LeaseDecision::Wait;
    }
    if owner_reachable || now_ms < lease.until_ms.saturating_add(LEASE_TAKEOVER_GRACE_MS) {
        return LeaseDecision::ConfirmWithOwner;
    }
    LeaseDecision::CanTakeOver
}

/// Whether a lease still protects its owner.
pub fn lease_is_live(lease: &Lease, now_ms: i64) -> bool {
    lease.until_ms >= now_ms
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease(device: &str, until_ms: i64) -> Lease {
        Lease {
            device: device.into(),
            until_ms,
        }
    }

    #[test]
    fn the_owner_keeps_its_live_lease() {
        assert_eq!(
            lease_can_be_taken(Some(&lease("a", 2_000)), "a", 1_000, false),
            LeaseDecision::Keep
        );
        assert_eq!(
            lease_can_be_taken(Some(&lease("a", 2_000)), "b", 1_000, false),
            LeaseDecision::Wait
        );
    }

    #[test]
    fn takeover_waits_for_the_grace_window() {
        let old = lease("a", 1_000);
        assert_eq!(
            lease_can_be_taken(Some(&old), "b", 1_001, false),
            LeaseDecision::ConfirmWithOwner
        );
        assert_eq!(
            lease_can_be_taken(Some(&old), "b", 1_001 + LEASE_TAKEOVER_GRACE_MS, true),
            LeaseDecision::ConfirmWithOwner
        );
        assert_eq!(
            lease_can_be_taken(Some(&old), "b", 1_001 + LEASE_TAKEOVER_GRACE_MS, false),
            LeaseDecision::CanTakeOver
        );
    }

    #[test]
    fn no_lease_can_be_taken() {
        assert_eq!(
            lease_can_be_taken(None, "b", 1_000, true),
            LeaseDecision::CanTakeOver
        );
    }

    #[test]
    fn a_device_renews_its_own_expired_lease_immediately() {
        assert_eq!(
            lease_can_be_taken(Some(&lease("a", 900)), "a", 1_000, false),
            LeaseDecision::CanTakeOver
        );
    }
}
