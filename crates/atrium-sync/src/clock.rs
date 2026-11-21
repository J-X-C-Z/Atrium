use serde::{Deserialize, Serialize};

/// One hybrid logical clock reading: wall time, a counter for ties inside
/// the same millisecond, and the device that wrote it.
///
/// Ordering is lexicographic on the fields in this order, which makes the
/// clock total and deterministic across devices.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Hlc {
    pub wall_ms: i64,
    pub counter: u32,
    pub device: String,
}

impl Hlc {
    pub fn new(wall_ms: i64, counter: u32, device: impl Into<String>) -> Self {
        Self {
            wall_ms,
            counter,
            device: device.into(),
        }
    }

    /// A reading that sorts before every real one.
    pub fn zero(device: impl Into<String>) -> Self {
        Self::new(i64::MIN, 0, device)
    }
}

/// Issues monotonically increasing clocks for one device.
///
/// `observe` folds in clocks seen from peers, so a local write after a
/// remote write sorts after it even when the wall clock is behind.
#[derive(Clone, Debug)]
pub struct HybridClock {
    device: String,
    last: Hlc,
}

impl HybridClock {
    pub fn new(device: impl Into<String>, last: Hlc) -> Self {
        let device = device.into();
        let last = if last.device.is_empty() {
            Hlc::zero(device.clone())
        } else {
            last
        };
        Self { device, last }
    }

    pub fn device(&self) -> &str {
        &self.device
    }

    pub fn last(&self) -> &Hlc {
        &self.last
    }

    /// Advances the clock to `now_ms` and returns the new reading.
    pub fn tick(&mut self, now_ms: i64) -> Hlc {
        if now_ms > self.last.wall_ms {
            self.last = Hlc::new(now_ms, 0, self.device.clone());
        } else {
            self.last.counter = self.last.counter.saturating_add(1);
        }
        self.last.clone()
    }

    /// Folds a remote clock into the local one. The next local tick sorts
    /// strictly after `remote`.
    pub fn observe(&mut self, remote: &Hlc) {
        if remote > &self.last {
            self.last = remote.clone();
            self.last.device = self.device.clone();
        } else if remote.wall_ms == self.last.wall_ms {
            self.last.counter = self.last.counter.max(remote.counter);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks_advance_inside_one_millisecond() {
        let mut clock = HybridClock::new("a", Hlc::default());
        let first = clock.tick(1_000);
        let second = clock.tick(1_000);
        assert_eq!(first.wall_ms, 1_000);
        assert_eq!(first.counter, 0);
        assert_eq!(second.wall_ms, 1_000);
        assert_eq!(second.counter, 1);
        assert!(second > first);
    }

    #[test]
    fn a_remote_clock_that_is_ahead_wins() {
        let mut clock = HybridClock::new("a", Hlc::default());
        let _ = clock.tick(1_000);
        clock.observe(&Hlc::new(5_000, 7, "b"));
        let next = clock.tick(1_001);
        assert_eq!(next.wall_ms, 5_000);
        assert_eq!(next.counter, 8);
        assert_eq!(next.device, "a");
        assert!(next > Hlc::new(5_000, 7, "b"));
    }

    #[test]
    fn ties_break_by_device() {
        let a = Hlc::new(10, 0, "a");
        let b = Hlc::new(10, 0, "b");
        assert!(a < b);
    }

    #[test]
    fn wall_clock_moves_backwards_do_not_move_the_clock_back() {
        let mut clock = HybridClock::new("a", Hlc::default());
        let first = clock.tick(10_000);
        let second = clock.tick(9_000);
        assert!(second > first);
        assert_eq!(second.wall_ms, 10_000);
        assert_eq!(second.counter, 1);
    }
}
