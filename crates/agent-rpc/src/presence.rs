// Ported from packages/client-runtime/src/connection/supervisor.ts (T3 Code v0.1.11, MIT).
// Ported from packages/client-runtime/src/connection/wakeups.ts (T3 Code v0.1.11, MIT).
//!
//! Presence from the live node link, and results that name one device each.
//!
//! A node sends a WebSocket ping every 15 seconds (see
//! `docs/node-protocol.md`). Any frame, including a ping, is a heartbeat; two
//! missed heartbeats mean the link is reconnecting. A computer that wakes
//! probes at once instead of waiting for the next ping, and a long sleep
//! restarts an in-flight retry. Time enters as `now` in milliseconds so the
//! transitions are pure and testable.
//!
//! The per-device helper calls every device at once and bounds each call, so a
//! slow device becomes a `Timeout` row instead of holding up the list.

use std::{
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::node::{LinkState, NodeError};

/// The node's WebSocket ping interval.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
/// A client that misses this many pings considers the link lost.
pub const MISSED_HEARTBEATS_BEFORE_DETACH: u32 = 2;
/// T3 resets the retry ladder only after a connection stays up this long.
pub const BACKOFF_RESET_AFTER: Duration = Duration::from_secs(30);
/// T3's `RETRY_BASE_DELAY_MS`.
pub const RETRY_BASE_DELAY: Duration = Duration::from_secs(1);
/// T3's `RETRY_MAX_DELAY_MS`.
pub const RETRY_MAX_DELAY: Duration = Duration::from_secs(300);
/// A full probe deadline away from loopback and LAN.
pub const CONNECTION_PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// A probe deadline on loopback and LAN.
pub const QUICK_CONNECTION_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// After a long suspension stale data is worse than reconnecting quickly.
pub const LONG_RESUME_PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
/// A multi-device call waits this long per device.
pub const DEVICE_CALL_TIMEOUT: Duration = Duration::from_secs(3);

/// Where a node sits on the network; it changes how long a probe may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteKind {
    Relay,
    Loopback,
    Lan,
    Tailnet,
    Vpn,
}

/// Why the connection owner was woken, from T3's `ConnectionWakeup`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeupReason {
    ApplicationActive,
    ApplicationActiveProbe,
    ApplicationActiveLong,
    ApplicationFocused,
    CredentialsChanged,
    NetworkChanged,
}

impl WakeupReason {
    fn is_application_active(self) -> bool {
        matches!(
            self,
            Self::ApplicationActive | Self::ApplicationActiveProbe | Self::ApplicationActiveLong
        )
    }
}

/// T3's `resetsRetryBackoff`.
pub fn resets_retry_backoff(reason: WakeupReason) -> bool {
    reason.is_application_active() || reason == WakeupReason::NetworkChanged
}

/// T3's `shouldResubscribeAfterWakeup`.
pub fn should_resubscribe_after_wakeup(reason: WakeupReason) -> bool {
    reason.is_application_active()
}

/// T3's `shouldRelistAccountAfterWakeup`; a brief mobile probe is not enough.
pub fn should_relist_account_after_wakeup(reason: WakeupReason) -> bool {
    matches!(
        reason,
        WakeupReason::ApplicationActive
            | WakeupReason::ApplicationActiveLong
            | WakeupReason::ApplicationFocused
    )
}

/// T3's `reachesConnectionSupervisor`; focus fires on every window switch.
pub fn reaches_connection_supervisor(reason: WakeupReason) -> bool {
    reason != WakeupReason::ApplicationFocused
}

/// One signal that starts a probe of an established link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeSignal {
    ConnectRequested,
    DisconnectRequested,
    RetryRequested,
    NetworkChanged { offline: bool },
    Wakeup(WakeupReason),
    BetterRouteAvailable,
}

/// T3's `probeTimeoutFor`: `None` means the signal does not probe.
pub fn probe_timeout_for(signal: ProbeSignal, route: RouteKind) -> Option<Duration> {
    match signal {
        ProbeSignal::RetryRequested => Some(QUICK_CONNECTION_PROBE_TIMEOUT),
        ProbeSignal::NetworkChanged { offline } => {
            offline.then_some(QUICK_CONNECTION_PROBE_TIMEOUT)
        }
        ProbeSignal::Wakeup(reason) => match reason {
            WakeupReason::ApplicationActive => Some(CONNECTION_PROBE_TIMEOUT),
            WakeupReason::ApplicationActiveLong => Some(LONG_RESUME_PROBE_TIMEOUT),
            WakeupReason::ApplicationActiveProbe | WakeupReason::NetworkChanged => {
                Some(if answers_quickly(route) {
                    QUICK_CONNECTION_PROBE_TIMEOUT
                } else {
                    CONNECTION_PROBE_TIMEOUT
                })
            }
            WakeupReason::ApplicationFocused | WakeupReason::CredentialsChanged => None,
        },
        ProbeSignal::ConnectRequested
        | ProbeSignal::DisconnectRequested
        | ProbeSignal::BetterRouteAvailable => None,
    }
}

/// Only loopback and LAN answer within 3 seconds reliably.
pub fn answers_quickly(route: RouteKind) -> bool {
    matches!(route, RouteKind::Loopback | RouteKind::Lan)
}

/// T3's `isForegroundProbe`.
pub fn is_foreground_probe(signal: ProbeSignal) -> bool {
    signal == ProbeSignal::Wakeup(WakeupReason::ApplicationActive)
}

/// T3's `retryDelayMs`: exponential from 1 s to 5 min, spread over 50-100% by
/// `random` (0.0..1.0) so many clients do not retry in step.
pub fn retry_delay_ms(failure_count: u32, random: f64) -> u64 {
    let exponent = failure_count.saturating_add(1).min(20);
    let ceiling = (RETRY_BASE_DELAY.as_millis() as u64)
        .saturating_mul(1u64 << exponent)
        .min(RETRY_MAX_DELAY.as_millis() as u64);
    let random = if random.is_finite() {
        random.clamp(0.0, 1.0)
    } else {
        0.0
    };
    (ceiling as f64 * 0.5 + ceiling as f64 * 0.5 * random).round() as u64
}

/// What the app should do after a wake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WakeAction {
    /// Probe the established link now instead of waiting for the next ping.
    pub probe_now: bool,
    /// Restart the retry ladder, as T3 does after the app returns.
    pub reset_backoff: bool,
    /// List the account again.
    pub relist_account: bool,
    /// Resubscribe the live stream.
    pub resubscribe: bool,
}

/// What the live link looks like to the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PresencePhase {
    Connecting,
    Live,
    Reconnecting,
    Offline,
}

/// The pure presence state behind one link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presence {
    pub phase: PresencePhase,
    pub attempt: u32,
    pub missed_heartbeats: u32,
    last_frame_at: Option<i64>,
    live_since: Option<i64>,
    retry_at: Option<i64>,
}

impl Default for Presence {
    fn default() -> Self {
        Self {
            phase: PresencePhase::Connecting,
            attempt: 0,
            missed_heartbeats: 0,
            last_frame_at: None,
            live_since: None,
            retry_at: None,
        }
    }
}

impl Presence {
    pub fn new() -> Self {
        Self::default()
    }

    /// A link is open; it is live once the first frame arrives, but count it
    /// live now so the list does not flicker while the first frame is in
    /// flight.
    pub fn connected(&mut self, now: i64) {
        self.phase = PresencePhase::Live;
        self.missed_heartbeats = 0;
        self.retry_at = None;
        self.live_since.get_or_insert(now);
        self.last_frame_at.get_or_insert(now);
    }

    /// Any text frame, ping or pong proves the link.
    pub fn frame(&mut self, now: i64) {
        self.last_frame_at = Some(now);
        self.missed_heartbeats = 0;
        self.phase = PresencePhase::Live;
        self.retry_at = None;
        let Some(live_since) = self.live_since else {
            self.live_since = Some(now);
            return;
        };
        if now.saturating_sub(live_since)
            >= i64::try_from(BACKOFF_RESET_AFTER.as_millis()).unwrap_or(i64::MAX)
        {
            self.attempt = 0;
        }
    }

    /// Count a heartbeat interval with no frame. Returns true once the link is
    /// considered lost.
    pub fn heartbeat_missed(&mut self, now: i64) -> bool {
        if self.phase != PresencePhase::Live {
            return false;
        }
        self.missed_heartbeats += 1;
        if self.missed_heartbeats < MISSED_HEARTBEATS_BEFORE_DETACH {
            return false;
        }
        self.lost(now);
        true
    }

    /// Check the sustained silence against the protocol's two intervals.
    pub fn heartbeat_tick(&mut self, now: i64) -> bool {
        let Some(last) = self.last_frame_at else {
            return false;
        };
        let limit = i64::try_from(HEARTBEAT_INTERVAL.as_millis())
            .unwrap_or(i64::MAX)
            .saturating_mul(i64::from(MISSED_HEARTBEATS_BEFORE_DETACH));
        if now.saturating_sub(last) < limit {
            return false;
        }
        self.missed_heartbeats = MISSED_HEARTBEATS_BEFORE_DETACH;
        self.lost(now);
        true
    }

    /// The socket failed; retry after the jittered backoff.
    pub fn lost(&mut self, now: i64) {
        if matches!(self.phase, PresencePhase::Offline | PresencePhase::Live) {
            self.attempt = self.attempt.saturating_add(1);
        } else {
            self.attempt = self.attempt.max(1);
        }
        self.phase = PresencePhase::Reconnecting;
        self.missed_heartbeats = 0;
        self.retry_at =
            Some(now.saturating_add(
                i64::try_from(retry_delay_ms(self.attempt, 0.0)).unwrap_or(i64::MAX),
            ));
    }

    /// The OS or the user says there is no network.
    pub fn offline(&mut self) {
        self.phase = PresencePhase::Offline;
        self.missed_heartbeats = 0;
    }

    /// A wake probes immediately. A long sleep also restarts an in-flight
    /// retry; a short one leaves it alone (T3's supervisor).
    pub fn wake(&mut self, now: i64, reason: WakeupReason) -> WakeAction {
        let action = WakeAction {
            probe_now: matches!(
                reason,
                WakeupReason::ApplicationActive
                    | WakeupReason::ApplicationActiveProbe
                    | WakeupReason::ApplicationActiveLong
                    | WakeupReason::NetworkChanged
            ),
            reset_backoff: resets_retry_backoff(reason),
            relist_account: should_relist_account_after_wakeup(reason),
            resubscribe: should_resubscribe_after_wakeup(reason),
        };
        if action.reset_backoff {
            self.attempt = 0;
        }
        if action.reset_backoff && self.phase == PresencePhase::Reconnecting {
            self.phase = PresencePhase::Connecting;
            self.retry_at = Some(now);
        }
        if reason == WakeupReason::ApplicationActiveLong
            && self.phase == PresencePhase::Reconnecting
        {
            self.phase = PresencePhase::Connecting;
            self.retry_at = Some(now);
        }
        action
    }

    /// Move to `Connecting` when the retry is due.
    pub fn retry_due(&mut self, now: i64) -> bool {
        if self.phase != PresencePhase::Reconnecting {
            return false;
        }
        if self.retry_at.is_some_and(|retry_at| now >= retry_at) {
            self.phase = PresencePhase::Connecting;
            true
        } else {
            false
        }
    }

    /// Map the transport's own state; the UI shows one presence per link.
    pub fn from_link(state: &LinkState) -> PresencePhase {
        match state {
            LinkState::Connecting => PresencePhase::Connecting,
            LinkState::Connected => PresencePhase::Live,
            LinkState::Reconnecting { .. } => PresencePhase::Reconnecting,
            LinkState::Ended { .. } | LinkState::Gone(_) => PresencePhase::Offline,
        }
    }
}

/// One device's result in a multi-device call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceOutcome {
    /// The call answered.
    Ok,
    /// The device could not be reached.
    Offline,
    /// The device did not answer within the per-device deadline.
    Timeout,
    /// The device answered with an error.
    Error,
}

/// A result labelled with the device it came from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceResult<T> {
    pub device: String,
    pub outcome: DeviceOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<T>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl<T> DeviceResult<T> {
    pub fn ok(device: impl Into<String>, value: T) -> Self {
        Self {
            device: device.into(),
            outcome: DeviceOutcome::Ok,
            value: Some(value),
            message: None,
        }
    }

    pub fn failed(
        device: impl Into<String>,
        outcome: DeviceOutcome,
        message: impl Into<String>,
    ) -> Self {
        Self {
            device: device.into(),
            outcome,
            value: None,
            message: Some(message.into()),
        }
    }
}

/// Offline is an unreachable node; a refused request is an error. A timeout is
/// decided by the caller's deadline, not by the error text alone, but ureq's
/// transport timeout also lands here.
pub fn classify(error: &NodeError) -> DeviceOutcome {
    match error {
        NodeError::Unreachable(message)
            if message.to_ascii_lowercase().contains("timed out")
                || message.to_ascii_lowercase().contains("timeout") =>
        {
            DeviceOutcome::Timeout
        }
        NodeError::Unreachable(_) => DeviceOutcome::Offline,
        NodeError::Refused { .. } => DeviceOutcome::Error,
    }
}

pub fn device_result<T>(
    device: impl Into<String>,
    result: Result<T, NodeError>,
) -> DeviceResult<T> {
    match result {
        Ok(value) => DeviceResult::ok(device, value),
        Err(error) => DeviceResult::failed(device, classify(&error), error.to_string()),
    }
}

/// Call every device at once and wait at most `timeout` for each. All calls
/// start together, so one deadline bounds the whole list; a device that has
/// not answered becomes a `Timeout` row and the list still returns.
pub fn call_devices<D, T, F>(
    devices: Vec<(String, D)>,
    timeout: Duration,
    call: F,
) -> Vec<DeviceResult<T>>
where
    D: Send + 'static,
    T: Send + 'static,
    F: Fn(&D) -> Result<T, NodeError> + Send + Sync + 'static,
{
    let labels: Vec<String> = devices.iter().map(|(label, _)| label.clone()).collect();
    let (sender, receiver) = mpsc::channel();
    let call = Arc::new(call);
    for (index, (_, device)) in devices.into_iter().enumerate() {
        let sender = sender.clone();
        let call = Arc::clone(&call);
        thread::spawn(move || {
            let result = call(&device);
            let _ = sender.send((index, result));
        });
    }
    drop(sender);

    let deadline = Instant::now() + timeout;
    let mut answers: Vec<Option<Result<T, NodeError>>> = (0..labels.len()).map(|_| None).collect();
    let mut remaining = labels.len();
    while remaining > 0 {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match receiver.recv_timeout(deadline - now) {
            Ok((index, result)) => {
                if answers[index].is_none() {
                    answers[index] = Some(result);
                    remaining -= 1;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    labels
        .into_iter()
        .enumerate()
        .map(|(index, label)| match answers[index].take() {
            Some(result) => device_result(label, result),
            None => DeviceResult::failed(
                label,
                DeviceOutcome::Timeout,
                format!("no answer within {} ms", timeout.as_millis()),
            ),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: i64 = 1_000;

    #[test]
    fn two_missed_heartbeats_report_reconnecting_until_a_frame_returns() {
        // T3: the connection supervisor treats a silent socket as lost and
        // replaces it; the node protocol detaches after two missed pings.
        let mut presence = Presence::new();
        presence.connected(0);
        assert_eq!(presence.phase, PresencePhase::Live);

        assert!(!presence.heartbeat_tick(15 * SECOND));
        assert_eq!(presence.phase, PresencePhase::Live);
        assert!(presence.heartbeat_tick(30 * SECOND));
        assert_eq!(presence.phase, PresencePhase::Reconnecting);
        assert!(presence.retry_at.is_some());

        presence.frame(31 * SECOND);
        assert_eq!(presence.phase, PresencePhase::Live);
        assert_eq!(presence.missed_heartbeats, 0);
    }

    #[test]
    fn a_live_link_ignores_missing_heartbeats_until_it_is_connected() {
        let mut presence = Presence::new();
        assert!(!presence.heartbeat_missed(0));
        assert_eq!(presence.phase, PresencePhase::Connecting);
        presence.connected(1);
        assert!(!presence.heartbeat_missed(2));
        assert!(presence.heartbeat_missed(3));
        assert_eq!(presence.phase, PresencePhase::Reconnecting);
    }

    #[test]
    fn waking_from_a_long_sleep_probes_immediately_and_resets_the_ladder() {
        // T3: "replaces a silent remote socket without the wait after a long
        // resume" and "restarts the retry ladder when a resume replaces a
        // silent socket".
        let mut presence = Presence::new();
        presence.connected(0);
        presence.lost(1);
        let retry = presence.retry_at.unwrap();
        assert!(retry > 1);
        let action = presence.wake(2, WakeupReason::ApplicationActiveLong);
        assert!(action.probe_now);
        assert!(action.reset_backoff);
        assert!(action.relist_account);
        assert!(action.resubscribe);
        assert_eq!(presence.phase, PresencePhase::Connecting);
        assert_eq!(presence.attempt, 0);
        assert_eq!(presence.retry_at, Some(2));
    }

    #[test]
    fn a_wake_that_does_not_reset_backoff_leaves_the_retry_in_place() {
        // T3's `ConnectionWakeups`: only application-active and network
        // changes reset the ladder; credentials and focus do not.
        let mut presence = Presence::new();
        presence.connected(0);
        presence.lost(1);
        let retry = presence.retry_at;
        let action = presence.wake(2, WakeupReason::CredentialsChanged);
        assert!(!action.probe_now);
        assert!(!action.reset_backoff);
        assert_eq!(presence.phase, PresencePhase::Reconnecting);
        assert_eq!(presence.retry_at, retry);
    }

    #[test]
    fn a_short_probe_wakes_a_pending_retry_without_starting_another_attempt() {
        // T3's foreground probes wake a pending retry immediately; the wake
        // itself does not become an extra attempt. T3 also keeps an in-flight
        // attempt (Connecting) from being restarted by platform wakeups.
        let mut presence = Presence::new();
        presence.connected(0);
        presence.lost(1);
        let action = presence.wake(2, WakeupReason::ApplicationActiveProbe);
        assert!(action.probe_now);
        assert!(action.reset_backoff);
        assert_eq!(presence.phase, PresencePhase::Connecting);
        assert_eq!(presence.retry_at, Some(2));

        let action = presence.wake(3, WakeupReason::ApplicationFocused);
        assert!(!action.probe_now);
        assert_eq!(presence.phase, PresencePhase::Connecting);
    }

    #[test]
    fn wakeup_classification_matches_t3() {
        use WakeupReason::*;
        for reason in [
            ApplicationActive,
            ApplicationActiveProbe,
            ApplicationActiveLong,
        ] {
            assert!(resets_retry_backoff(reason));
            assert!(should_resubscribe_after_wakeup(reason));
        }
        assert!(resets_retry_backoff(NetworkChanged));
        assert!(!should_resubscribe_after_wakeup(NetworkChanged));
        assert!(should_relist_account_after_wakeup(ApplicationActive));
        assert!(should_relist_account_after_wakeup(ApplicationActiveLong));
        assert!(should_relist_account_after_wakeup(ApplicationFocused));
        assert!(!should_relist_account_after_wakeup(ApplicationActiveProbe));
        assert!(!should_relist_account_after_wakeup(NetworkChanged));
        assert!(!reaches_connection_supervisor(ApplicationFocused));
        assert!(reaches_connection_supervisor(ApplicationActive));
    }

    #[test]
    fn probe_deadlines_follow_the_wake_reason_and_route() {
        // T3 `probeTimeoutFor`: loopback and LAN answer in 3 s; a tailnet over
        // DERP may need the full 15 s; a long resume uses 1.5 s.
        assert_eq!(
            probe_timeout_for(
                ProbeSignal::Wakeup(WakeupReason::ApplicationActiveProbe),
                RouteKind::Lan
            ),
            Some(QUICK_CONNECTION_PROBE_TIMEOUT)
        );
        assert_eq!(
            probe_timeout_for(
                ProbeSignal::Wakeup(WakeupReason::ApplicationActiveProbe),
                RouteKind::Tailnet
            ),
            Some(CONNECTION_PROBE_TIMEOUT)
        );
        assert_eq!(
            probe_timeout_for(
                ProbeSignal::Wakeup(WakeupReason::ApplicationActive),
                RouteKind::Tailnet
            ),
            Some(CONNECTION_PROBE_TIMEOUT)
        );
        assert_eq!(
            probe_timeout_for(
                ProbeSignal::Wakeup(WakeupReason::ApplicationActiveLong),
                RouteKind::Relay
            ),
            Some(LONG_RESUME_PROBE_TIMEOUT)
        );
        assert_eq!(
            probe_timeout_for(ProbeSignal::RetryRequested, RouteKind::Relay),
            Some(QUICK_CONNECTION_PROBE_TIMEOUT)
        );
        assert_eq!(
            probe_timeout_for(
                ProbeSignal::NetworkChanged { offline: true },
                RouteKind::Relay
            ),
            Some(QUICK_CONNECTION_PROBE_TIMEOUT)
        );
        assert_eq!(
            probe_timeout_for(
                ProbeSignal::NetworkChanged { offline: false },
                RouteKind::Lan
            ),
            None
        );
        assert!(is_foreground_probe(ProbeSignal::Wakeup(
            WakeupReason::ApplicationActive
        )));
        assert!(!is_foreground_probe(ProbeSignal::Wakeup(
            WakeupReason::ApplicationActiveLong
        )));
    }

    #[test]
    fn retry_delay_grows_from_one_second_to_five_minutes_with_jitter() {
        // T3's `retryDelayMs`.
        assert_eq!(retry_delay_ms(0, 0.0), 1_000);
        assert_eq!(retry_delay_ms(0, 1.0), 2_000);
        assert_eq!(retry_delay_ms(1, 0.0), 2_000);
        assert_eq!(retry_delay_ms(2, 0.0), 4_000);
        assert_eq!(retry_delay_ms(20, 1.0), 300_000);
        assert_eq!(retry_delay_ms(0, f64::NAN), 1_000);
    }

    #[test]
    fn retry_due_only_moves_a_reconnecting_link() {
        let mut presence = Presence::new();
        presence.connected(0);
        presence.lost(1);
        let retry = presence.retry_at.unwrap();
        assert!(!presence.retry_due(retry - 1));
        assert!(presence.retry_due(retry));
        assert_eq!(presence.phase, PresencePhase::Connecting);
        assert!(!presence.retry_due(retry));
    }

    #[test]
    fn link_states_map_to_one_presence_each() {
        assert_eq!(
            Presence::from_link(&LinkState::Connected),
            PresencePhase::Live
        );
        assert_eq!(
            Presence::from_link(&LinkState::Reconnecting {
                attempt: 2,
                error: "wifi".into()
            }),
            PresencePhase::Reconnecting
        );
        assert_eq!(
            Presence::from_link(&LinkState::Gone("closed".into())),
            PresencePhase::Offline
        );
        assert_eq!(
            Presence::from_link(&LinkState::Ended { code: Some(0) }),
            PresencePhase::Offline
        );
    }

    #[test]
    fn one_slow_device_does_not_block_the_list() {
        let devices = vec![
            ("fast".to_owned(), 0u64),
            ("slow".to_owned(), 1),
            ("offline".to_owned(), 2),
            ("error".to_owned(), 3),
        ];
        let results = call_devices(devices, Duration::from_millis(80), |device| match device {
            0 => Ok("ready"),
            1 => {
                thread::sleep(Duration::from_millis(250));
                Ok("late")
            }
            2 => Err(NodeError::Unreachable("connection refused".into())),
            _ => Err(NodeError::Refused {
                status: 500,
                code: "bad_request".into(),
                message: "boom".into(),
            }),
        });
        assert_eq!(
            results
                .iter()
                .map(|r| r.device.as_str())
                .collect::<Vec<_>>(),
            ["fast", "slow", "offline", "error"],
            "results keep the device order"
        );
        assert_eq!(results[0].outcome, DeviceOutcome::Ok);
        assert_eq!(results[0].value, Some("ready"));
        assert_eq!(results[1].outcome, DeviceOutcome::Timeout);
        assert_eq!(results[2].outcome, DeviceOutcome::Offline);
        assert_eq!(results[3].outcome, DeviceOutcome::Error);
    }

    #[test]
    fn a_transport_timeout_is_reported_as_timeout() {
        assert_eq!(
            classify(&NodeError::Unreachable("timed out reading response".into())),
            DeviceOutcome::Timeout
        );
    }
}
