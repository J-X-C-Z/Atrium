//! How fast a turn writes: time to the first token and output tokens per
//! second, measured from the text as it streams in.
//!
//! Backends report exact token counts only per request, and those include
//! output that never streams (tool-call arguments, hidden reasoning), so the
//! speed comes from the streamed text itself: tokens are estimated from it and
//! timed only while it flows. Pauses longer than [`GAP`] (a tool running, a
//! new request starting) do not count as writing time.

use std::time::{Duration, Instant};

/// A longer silence between two pieces of output is a pause, not writing.
pub const GAP: Duration = Duration::from_secs(2);

/// Roughly how many tokens `text` is: about four ASCII characters a token,
/// and about one token for each other character (CJK, symbols).
pub fn estimate_tokens(text: &str) -> f64 {
    let (ascii, other) = text.chars().fold((0usize, 0usize), |(a, o), c| {
        if c.is_ascii() { (a + 1, o) } else { (a, o + 1) }
    });
    ascii as f64 / 4. + other as f64
}

/// The measured pace of one turn.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TurnPace {
    /// From the turn's start to its first streamed output.
    pub first_token: Option<Duration>,
    /// Streamed output, estimated in tokens.
    pub streamed_tokens: f64,
    /// The output tokens the backend reported for the turn, when it did.
    pub output_tokens: Option<u64>,
    /// Time spent writing: the flowing stretches only.
    pub writing: Duration,
    /// Estimated tokens over `writing`, for the speed.
    timed_tokens: f64,
    /// From start to end (or to now, while running).
    pub elapsed: Duration,
    pub done: bool,
    /// Output arrived within the last [`GAP`]: the agent is writing now.
    pub writing_now: bool,
}

impl TurnPace {
    /// Output tokens a second, once there is enough to measure.
    pub fn tokens_per_sec(&self) -> Option<f64> {
        (self.writing >= Duration::from_millis(300) && self.timed_tokens >= 3.)
            .then(|| self.timed_tokens / self.writing.as_secs_f64())
    }

    /// The best count of what the turn wrote: reported, else estimated.
    pub fn tokens_out(&self) -> Option<(u64, bool)> {
        match self.output_tokens {
            Some(exact) if exact > 0 => Some((exact, true)),
            _ => (self.streamed_tokens >= 1.).then(|| (self.streamed_tokens.round() as u64, false)),
        }
    }
}

/// Times one turn as its events arrive.
#[derive(Clone, Debug)]
pub(crate) struct PaceClock {
    started: Instant,
    last_output: Option<Instant>,
    ended: Option<Instant>,
    /// The conversation's reported output total when the turn began.
    output_before: Option<u64>,
    pub(crate) pace: TurnPace,
}

impl PaceClock {
    pub(crate) fn start(now: Instant, output_before: Option<u64>) -> Self {
        Self {
            started: now,
            last_output: None,
            ended: None,
            output_before,
            pace: TurnPace::default(),
        }
    }

    /// Some streamed output arrived.
    pub(crate) fn output(&mut self, text: &str, now: Instant) {
        if self.ended.is_some() || text.is_empty() {
            return;
        }
        let tokens = estimate_tokens(text);
        let pace = &mut self.pace;
        pace.streamed_tokens += tokens;
        if pace.first_token.is_none() {
            pace.first_token = Some(now.saturating_duration_since(self.started));
        }
        if let Some(last) = self.last_output {
            let gap = now.saturating_duration_since(last);
            // The first piece after a pause arrived untimed: count only what
            // came within a flowing stretch.
            if gap <= GAP {
                pace.writing += gap;
                pace.timed_tokens += tokens;
            }
        }
        self.last_output = Some(now);
    }

    /// The backend reported the conversation's output total.
    pub(crate) fn reported(&mut self, output_total: u64) {
        if self.ended.is_some() {
            return;
        }
        if let Some(before) = self.output_before {
            self.pace.output_tokens = Some(output_total.saturating_sub(before));
        }
    }

    pub(crate) fn end(&mut self, now: Instant) {
        if self.ended.is_none() {
            self.ended = Some(now);
            self.pace.elapsed = now.saturating_duration_since(self.started);
            self.pace.done = true;
        }
    }

    /// The pace as of `now`.
    pub(crate) fn at(&self, now: Instant) -> TurnPace {
        let mut pace = self.pace;
        if self.ended.is_none() {
            pace.elapsed = now.saturating_duration_since(self.started);
            pace.writing_now = self
                .last_output
                .is_some_and(|last| now.saturating_duration_since(last) <= GAP);
        }
        pace
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_ascii_and_cjk() {
        assert_eq!(estimate_tokens("abcdefgh"), 2.);
        assert_eq!(estimate_tokens("你好"), 2.);
    }

    #[test]
    fn pauses_do_not_count_as_writing() {
        let t0 = Instant::now();
        let mut clock = PaceClock::start(t0, Some(100));
        let ms = |n| t0 + Duration::from_millis(n);
        // First token after 1.5s, then 40 chars (10 tokens) a 100ms, five times.
        clock.output("x".repeat(40).as_str(), ms(1500));
        for i in 1..=5 {
            clock.output("x".repeat(40).as_str(), ms(1500 + i * 100));
        }
        // A tool runs for 10s; writing resumes.
        clock.output("x".repeat(40).as_str(), ms(12000));
        clock.output("x".repeat(40).as_str(), ms(12100));
        clock.reported(400);
        clock.end(ms(12500));
        let pace = clock.at(ms(20000));
        assert_eq!(pace.first_token, Some(Duration::from_millis(1500)));
        assert_eq!(pace.writing, Duration::from_millis(600));
        // 6 timed pieces of 10 tokens in 0.6s.
        assert!((pace.tokens_per_sec().unwrap() - 100.).abs() < 1e-9);
        assert_eq!(pace.tokens_out(), Some((300, true)));
        assert_eq!(pace.elapsed, Duration::from_millis(12500));
    }

    #[test]
    fn too_little_to_measure() {
        let t0 = Instant::now();
        let mut clock = PaceClock::start(t0, None);
        clock.output("hello", t0 + Duration::from_millis(500));
        clock.reported(50);
        let pace = clock.at(t0 + Duration::from_secs(1));
        assert_eq!(pace.tokens_per_sec(), None);
        // Without a total from before the turn the reported count is unknown.
        assert_eq!(pace.output_tokens, None);
        assert_eq!(pace.tokens_out(), Some((1, false)));
    }
}
