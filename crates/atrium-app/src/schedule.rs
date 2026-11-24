//! Scheduled chats: a prompt Atrium sends by itself at a time of day, after an
//! interval, or once, in a folder, to an agent, such as "check CI and summarise
//! new issues" every weekday morning. They run while Atrium is open; a run
//! missed while it was closed starts when it opens, if it is a fixed-time chat
//! that is still within six hours of its time.
//!
//! Ported from T3 Code v0.1.11 `apps/server/src/scheduledTasks/Schedule.ts`
//! (next-run calculation and structural comparison) and
//! `packages/contracts/src/scheduledTask.ts` (interval and one-time schedule
//! kinds, run status). Atrium keeps its own `Days` grouping and settings file;
//! the decision logic here is GPUI-free and takes `now` as an argument.
//! See `crates/atrium-engine/UPSTREAM.md`, "Ported outside atrium-engine".

use jiff::civil::{Time, Weekday};
use jiff::{SignedDuration, Timestamp, Zoned};
use serde::{Deserialize, Serialize};

/// How late a missed fixed-time run may still start.
const GRACE_MINUTES: i32 = 6 * 60;

/// The shortest interval T3 allows: one minute.
pub const MIN_INTERVAL_MINUTES: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Days {
    #[default]
    Daily,
    Weekdays,
    Weekends,
}

impl Days {
    pub const ALL: [Days; 3] = [Days::Daily, Days::Weekdays, Days::Weekends];

    pub fn label(self) -> &'static str {
        match self {
            Days::Daily => "Every day",
            Days::Weekdays => "Weekdays",
            Days::Weekends => "Weekends",
        }
    }

    fn includes(self, day: Weekday) -> bool {
        let weekend = matches!(day, Weekday::Saturday | Weekday::Sunday);
        match self {
            Days::Daily => true,
            Days::Weekdays => !weekend,
            Days::Weekends => weekend,
        }
    }

    /// A stable key for structural comparison.
    fn key(self) -> u8 {
        match self {
            Days::Daily => 0,
            Days::Weekdays => 1,
            Days::Weekends => 2,
        }
    }
}

/// How a scheduled chat repeats.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// On `days` at `time`, local time.
    #[default]
    Fixed,
    /// Every `every_minutes`, counted as elapsed time.
    Interval,
    /// Once, at the instant in `run_at`.
    Once,
}

/// How the last run ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    #[default]
    Never,
    Succeeded,
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub folder: String,
    pub backend: String,
    /// `HH:MM`, local time, for fixed-time chats.
    pub time: String,
    pub days: Days,
    pub off: bool,
    /// The local date it last ran, `YYYY-MM-DD`.
    pub last_run: Option<String>,
    /// Defaults to `Fixed`, so settings written before interval and one-time
    /// chats still load.
    pub kind: Kind,
    /// Interval chats: minutes between runs, at least one.
    pub every_minutes: u32,
    /// One-time chats: the RFC 3339 instant to run at.
    pub run_at: Option<String>,
    /// The instant the next run is due, RFC 3339. Kept for interval and
    /// one-time chats so a restart neither loses nor repeats a run; fixed-time
    /// chats use their local date and time instead.
    pub next_run: Option<String>,
    /// When the last run was dispatched, RFC 3339.
    pub last_run_at: Option<String>,
    /// How many times it has been dispatched.
    pub run_count: u64,
    pub last_status: RunStatus,
    /// Why the last run failed, such as its folder being gone.
    pub last_error: Option<String>,
    /// True while the run is being dispatched. A crash leaves it set, so the
    /// next start can report the run as interrupted.
    pub running: bool,
}

/// `9:05` or `09:05` as a time of day.
pub fn parse_time(text: &str) -> Option<Time> {
    let (h, m) = text.trim().split_once([':', '：'])?;
    let (h, m): (i8, i8) = (h.trim().parse().ok()?, m.trim().parse().ok()?);
    Time::new(h, m, 0, 0).ok()
}

/// A one-time run time as an instant: RFC 3339 with an offset, or
/// `YYYY-MM-DD HH:MM` read as local time, the form the settings page asks for.
pub fn parse_once(text: &str) -> Option<Timestamp> {
    let text = text.trim();
    if let Ok(at) = text.parse::<Timestamp>() {
        return Some(at);
    }
    let civil = jiff::civil::DateTime::strptime("%Y-%m-%d %H:%M", text).ok()?;
    civil
        .to_zoned(jiff::tz::TimeZone::system())
        .ok()
        .map(|zoned| zoned.timestamp())
}

fn minutes(t: Time) -> i32 {
    i32::from(t.hour()) * 60 + i32::from(t.minute())
}

impl Schedule {
    /// Whether the chat should run at local time `now`.
    ///
    /// Interval and one-time chats compare their stored next run, so a long
    /// closure runs them once when Atrium opens rather than once per missed
    /// occurrence. Fixed-time chats keep Atrium's same-day six-hour grace.
    pub fn due(&self, now: &Zoned) -> bool {
        if self.off || self.running {
            return false;
        }
        match self.kind {
            Kind::Fixed => self.fixed_due(now),
            Kind::Interval | Kind::Once => self
                .next_run_instant()
                .is_some_and(|at| at <= now.timestamp()),
        }
    }

    fn fixed_due(&self, now: &Zoned) -> bool {
        let Some(at) = parse_time(&self.time) else {
            return false;
        };
        let today = now.date().to_string();
        if self.last_run.as_deref() == Some(today.as_str()) || !self.days.includes(now.weekday()) {
            return false;
        }
        let late = minutes(now.time()) - minutes(at);
        (0..=GRACE_MINUTES).contains(&late)
    }

    /// The next run strictly after `from`, in `from`'s time zone.
    ///
    /// Fixed-time chats stay on the local calendar, so 09:00 stays 09:00 across
    /// a daylight-saving change. Interval chats advance by elapsed time, so a
    /// two-hour interval is two hours even across a change. Ported from
    /// `nextScheduledRunAt`.
    pub fn next_after(&self, from: &Zoned) -> Option<Zoned> {
        match self.kind {
            Kind::Fixed => {
                let at = parse_time(&self.time)?;
                let zone = from.time_zone().clone();
                let mut date = from.date();
                for offset in 0..=7 {
                    if offset > 0 {
                        date = date.tomorrow().ok()?;
                    }
                    if !self.days.includes(date.weekday()) {
                        continue;
                    }
                    let candidate = date
                        .at(at.hour(), at.minute(), 0, 0)
                        .to_zoned(zone.clone())
                        .ok()?;
                    if candidate.timestamp() > from.timestamp() {
                        return Some(candidate);
                    }
                }
                None
            }
            Kind::Interval => {
                let seconds = i64::from(self.interval_minutes()) * 60;
                let next = from
                    .timestamp()
                    .checked_add(SignedDuration::from_secs(seconds))
                    .ok()?;
                Some(next.to_zoned(from.time_zone().clone()))
            }
            Kind::Once => {
                let at = self.run_at_instant()?;
                (at > from.timestamp()).then(|| at.to_zoned(from.time_zone().clone()))
            }
        }
    }

    /// The interval in minutes, clamped to T3's one-minute minimum.
    pub fn interval_minutes(&self) -> u32 {
        self.every_minutes.max(MIN_INTERVAL_MINUTES)
    }

    fn run_at_instant(&self) -> Option<Timestamp> {
        self.run_at.as_deref()?.parse().ok()
    }

    fn next_run_instant(&self) -> Option<Timestamp> {
        self.next_run.as_deref()?.parse().ok()
    }

    /// Gives interval and one-time chats a next run when they have none, and
    /// keeps a paused chat without one. Fixed-time chats get one for display.
    pub fn ensure_next_run(&mut self, now: &Zoned) {
        if self.off {
            self.next_run = None;
            return;
        }
        if self.next_run_instant().is_none() {
            self.next_run = self.next_after(now).map(|at| at.timestamp().to_string());
        }
    }

    /// Marks a dispatch as starting now and advances the next run. Returns
    /// false when the previous dispatch has not finished, so two runs of one
    /// chat never overlap. The caller saves before dispatching.
    pub fn begin_run(&mut self, now: &Zoned) -> bool {
        if self.running {
            return false;
        }
        self.running = true;
        // A manual fixed-time run before its time must not consume that day's
        // slot; any other run advances the schedule.
        let keeps_today = self.kind != Kind::Fixed
            || parse_time(&self.time).is_some_and(|at| {
                self.days.includes(now.weekday()) && minutes(now.time()) >= minutes(at)
            });
        if keeps_today {
            self.last_run = Some(now.date().to_string());
        }
        self.last_run_at = Some(now.timestamp().to_string());
        self.run_count = self.run_count.saturating_add(1);
        self.next_run = self.next_after(now).map(|at| at.timestamp().to_string());
        true
    }

    /// Records how the dispatch ended and clears the overlap guard.
    pub fn finish_run(&mut self, error: Option<&str>) {
        self.running = false;
        self.last_status = if error.is_some() {
            RunStatus::Failed
        } else {
            RunStatus::Succeeded
        };
        self.last_error = error.map(str::to_string);
    }

    /// Clears a run a crash left marked as running, as T3 does at startup.
    ///
    /// Interval and fixed-time chats advance from `now`; a one-time chat keeps
    /// the run time it had, so a manual run that began before its time does not
    /// use it up.
    pub fn recover_interrupted(&mut self, now: &Zoned) {
        if !self.running {
            return;
        }
        self.running = false;
        self.last_status = RunStatus::Failed;
        self.last_error = Some("Run was interrupted by a restart.".to_string());
        if self.kind != Kind::Once {
            self.next_run = self.next_after(now).map(|at| at.timestamp().to_string());
        }
    }
}

/// Whether two schedules run at the same times. Ported from `isSameSchedule`;
/// a padded hour and a different weekday order still compare equal.
///
/// Atrium's settings page has no edit path yet, so this is used by the ported
/// tests and kept for the edit path the comparison calls for.
#[allow(dead_code)]
pub fn same_schedule(a: &Schedule, b: &Schedule) -> bool {
    match (a.kind, b.kind) {
        (Kind::Interval, Kind::Interval) => a.every_minutes == b.every_minutes,
        (Kind::Once, Kind::Once) => a.run_at_instant() == b.run_at_instant(),
        (Kind::Fixed, Kind::Fixed) => {
            let (Some(a_time), Some(b_time)) = (parse_time(&a.time), parse_time(&b.time)) else {
                return false;
            };
            a_time.hour() == b_time.hour()
                && a_time.minute() == b_time.minute()
                && a.days.key() == b.days.key()
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;
    use jiff::tz::TimeZone;

    fn at(day: i8, h: i8, m: i8) -> Zoned {
        date(2026, 11, day)
            .at(h, m, 0, 0)
            .to_zoned(TimeZone::get("UTC").expect("UTC"))
            .expect("zoned")
    }

    fn zoned(zone: &str, text: &str) -> Zoned {
        let zone = TimeZone::get(zone).expect(zone);
        text.parse::<Timestamp>().expect(text).to_zoned(zone)
    }

    fn stamp(z: &Zoned) -> String {
        z.timestamp().to_string()
    }

    fn weekday_morning() -> Schedule {
        Schedule {
            time: "9:00".into(),
            days: Days::Weekdays,
            ..Schedule::default()
        }
    }

    #[test]
    fn times_parse_loosely() {
        assert_eq!(parse_time("09:05"), Time::new(9, 5, 0, 0).ok());
        assert_eq!(parse_time(" 7：30 "), Time::new(7, 30, 0, 0).ok());
        assert_eq!(parse_time("25:00"), None);
        assert_eq!(parse_time("nine"), None);
    }

    #[test]
    fn one_time_times_parse_as_an_instant_or_local_time() {
        assert_eq!(
            parse_once("2026-11-01T03:00:00-05:00"),
            "2026-11-01T08:00:00Z".parse::<Timestamp>().ok()
        );
        assert!(parse_once("2026-11-01 09:00").is_some());
        assert_eq!(parse_once("tomorrow"), None);
    }

    #[test]
    fn runs_once_on_its_days_after_its_time() {
        let mut s = weekday_morning();
        assert!(!s.due(&at(16, 8, 59)));
        assert!(s.due(&at(16, 9, 0)));
        assert!(s.due(&at(16, 14, 0)), "missed runs start late the same day");
        assert!(!s.due(&at(16, 15, 30)), "but not too late");
        assert!(!s.due(&at(21, 9, 30)), "Saturday is not a weekday");
        s.last_run = Some("2026-11-16".into());
        assert!(!s.due(&at(16, 10, 0)), "once a day");
        assert!(s.due(&at(17, 9, 0)));
        s.off = true;
        assert!(!s.due(&at(17, 9, 0)));
    }

    // Ported from `apps/server/src/scheduledTasks/Schedule.test.ts`:
    // "skips to the next matching fixed-time weekday".
    #[test]
    fn skips_to_the_next_matching_fixed_time_weekday() {
        let schedule = Schedule {
            time: "09:00".into(),
            days: Days::Weekdays,
            ..Schedule::default()
        };
        let from = zoned("America/Los_Angeles", "2026-07-03T17:00:00Z");
        let next = schedule.next_after(&from).expect("a weekday run");
        assert_eq!(next.weekday(), Weekday::Monday);
        assert_eq!(next.hour(), 9);
        assert_eq!(next.minute(), 0);
    }

    // Ported from "compares schedules structurally", with Atrium's day groups.
    #[test]
    fn compares_schedules_structurally() {
        let once = |run_at: &str| Schedule {
            kind: Kind::Once,
            run_at: Some(run_at.into()),
            ..Schedule::default()
        };
        assert!(same_schedule(
            &once("2026-11-01T03:00:00-05:00"),
            &once("2026-11-01T08:00:00.000Z")
        ));
        let interval = |minutes: u32| Schedule {
            kind: Kind::Interval,
            every_minutes: minutes,
            ..Schedule::default()
        };
        assert!(same_schedule(&interval(1), &interval(1)));
        assert!(!same_schedule(&interval(1), &interval(2)));
        let fixed = |time: &str, days: Days| Schedule {
            time: time.into(),
            days,
            ..Schedule::default()
        };
        assert!(same_schedule(
            &fixed("09:00", Days::Weekdays),
            &fixed("09:00", Days::Weekdays)
        ));
        assert!(same_schedule(
            &fixed("09:00", Days::Daily),
            &fixed("09:00", Days::Daily)
        ));
        assert!(!same_schedule(
            &fixed("09:00", Days::Weekdays),
            &fixed("09:00", Days::Weekends)
        ));
        assert!(!same_schedule(
            &fixed("09:00", Days::Daily),
            &fixed("09:30", Days::Daily)
        ));
        assert!(!same_schedule(&interval(1), &fixed("09:00", Days::Daily)));
    }

    // Ported from `ScheduledTaskService.interval.test.ts`: intervals advance
    // elapsed time, including across daylight-saving changes.
    #[test]
    fn intervals_advance_elapsed_time_across_daylight_saving() {
        let cases = [
            (
                "America/New_York",
                "2026-03-08T06:30:00Z",
                120,
                "2026-03-08T08:30:00Z",
            ),
            (
                "America/New_York",
                "2026-11-01T05:30:00Z",
                120,
                "2026-11-01T07:30:00Z",
            ),
            (
                "America/New_York",
                "2026-11-01T06:30:00Z",
                1,
                "2026-11-01T06:31:00Z",
            ),
            ("UTC", "2026-11-01T06:30:00Z", 1, "2026-11-01T06:31:00Z"),
            (
                "Asia/Tokyo",
                "2026-03-08T06:30:00Z",
                120,
                "2026-03-08T08:30:00Z",
            ),
        ];
        for (zone, from, minutes, expected) in cases {
            let schedule = Schedule {
                kind: Kind::Interval,
                every_minutes: minutes,
                ..Schedule::default()
            };
            let from = zoned(zone, from);
            assert_eq!(
                stamp(&schedule.next_after(&from).expect("an interval run")),
                expected,
                "{zone} from {from}"
            );
        }
    }

    // Ported from `ScheduledTaskService.interval.test.ts`: fixed-time chats
    // stay on the local calendar across daylight-saving changes.
    #[test]
    fn fixed_times_stay_on_the_local_calendar() {
        let schedule = Schedule {
            time: "09:00".into(),
            days: Days::Daily,
            ..Schedule::default()
        };
        for (from, expected) in [
            ("2026-03-07T23:30:00Z", "2026-03-08T13:00:00Z"),
            ("2026-10-31T23:30:00Z", "2026-11-01T14:00:00Z"),
        ] {
            let from = zoned("America/New_York", from);
            assert_eq!(
                stamp(&schedule.next_after(&from).expect("a daily run")),
                expected
            );
        }
    }

    // Ported from `ScheduledTaskService.interval.test.ts`: a one-time chat runs
    // once; a manual run before its time does not use it up.
    #[test]
    fn one_time_runs_once_and_keeps_an_early_manual_run() {
        let mut schedule = Schedule {
            kind: Kind::Once,
            run_at: Some("2026-11-01T03:00:00-05:00".into()),
            next_run: Some("2026-11-01T08:00:00Z".into()),
            ..Schedule::default()
        };
        let before = zoned("UTC", "2026-11-01T07:59:00Z");
        let at = zoned("UTC", "2026-11-01T08:00:00Z");

        assert!(!schedule.due(&before));
        assert!(schedule.due(&at));

        assert!(schedule.begin_run(&before), "a manual run before the time");
        assert_eq!(schedule.next_run.as_deref(), Some("2026-11-01T08:00:00Z"));
        schedule.finish_run(None);

        assert!(schedule.due(&at));
        assert!(schedule.begin_run(&at));
        assert_eq!(schedule.next_run, None, "a one-time chat runs once");
        schedule.finish_run(None);
        assert!(!schedule.due(&at));
        assert!(
            schedule.begin_run(&at),
            "a manual run is still allowed after the scheduled one"
        );
    }

    // Ported from `ScheduledTaskService.interval.test.ts`: a manual run that
    // starts before a one-time or fixed run keeps the scheduled run.
    #[test]
    fn an_early_manual_fixed_run_keeps_the_scheduled_slot() {
        let mut schedule = weekday_morning();
        assert!(schedule.begin_run(&at(16, 8, 0)));
        schedule.finish_run(None);
        assert!(schedule.due(&at(16, 9, 0)), "the 09:00 slot still runs");
        assert!(schedule.begin_run(&at(16, 9, 0)));
        schedule.finish_run(None);
        assert!(
            !schedule.due(&at(16, 9, 1)),
            "after its time a manual run consumes the day"
        );
    }

    #[test]
    fn checks_a_run_once_with_minutes_clamped_to_the_minimum() {
        let schedule = Schedule {
            kind: Kind::Interval,
            every_minutes: 0,
            ..Schedule::default()
        };
        assert_eq!(schedule.interval_minutes(), MIN_INTERVAL_MINUTES);
    }

    #[test]
    fn does_not_overlap_runs_or_let_a_paused_chat_fire() {
        let mut schedule = Schedule {
            kind: Kind::Interval,
            every_minutes: 1,
            next_run: Some("2026-11-16T09:00:00Z".into()),
            ..Schedule::default()
        };
        let now = at(16, 9, 0);
        assert!(schedule.due(&now));
        assert!(schedule.begin_run(&now));
        assert_eq!(schedule.run_count, 1);
        assert!(
            !schedule.begin_run(&now),
            "the first dispatch has not ended"
        );
        assert!(!schedule.due(&now));
        schedule.finish_run(Some("folder is gone"));
        assert_eq!(schedule.last_status, RunStatus::Failed);
        assert_eq!(schedule.last_error.as_deref(), Some("folder is gone"));
        assert_eq!(schedule.next_run.as_deref(), Some("2026-11-16T09:01:00Z"));

        schedule.off = true;
        assert!(!schedule.due(&at(16, 9, 2)));
        schedule.ensure_next_run(&now);
        assert_eq!(schedule.next_run, None, "a paused chat stores no next run");
    }

    // Ported from `ScheduledTaskService.ts`: a missed interval runs once and
    // advances from now, so a long closure does not accumulate a backlog.
    #[test]
    fn a_missed_interval_runs_once_without_a_backlog() {
        let mut schedule = Schedule {
            kind: Kind::Interval,
            every_minutes: 60,
            next_run: Some("2026-11-16T07:00:00Z".into()),
            ..Schedule::default()
        };
        let now = at(16, 9, 0);
        assert!(schedule.due(&now));
        assert!(schedule.begin_run(&now));
        schedule.finish_run(None);
        assert_eq!(
            schedule.next_run.as_deref(),
            Some("2026-11-16T10:00:00Z"),
            "the next run counts from now, not from the missed time"
        );
        assert!(!schedule.due(&at(16, 9, 30)));
    }

    #[test]
    fn recovers_a_run_a_restart_interrupted() {
        let mut interval = Schedule {
            kind: Kind::Interval,
            every_minutes: 1,
            next_run: Some("2026-11-16T08:00:00Z".into()),
            running: true,
            last_status: RunStatus::Succeeded,
            ..Schedule::default()
        };
        let now = at(16, 9, 0);
        interval.recover_interrupted(&now);
        assert!(!interval.running);
        assert_eq!(interval.last_status, RunStatus::Failed);
        assert_eq!(
            interval.next_run.as_deref(),
            Some("2026-11-16T09:01:00Z"),
            "an interrupted interval advances from now"
        );

        let mut once = Schedule {
            kind: Kind::Once,
            run_at: Some("2026-11-16T09:00:00Z".into()),
            next_run: Some("2026-11-16T09:00:00Z".into()),
            running: true,
            ..Schedule::default()
        };
        once.recover_interrupted(&at(16, 9, 5));
        assert_eq!(
            once.next_run.as_deref(),
            Some("2026-11-16T09:00:00Z"),
            "an early manual one-time run keeps its scheduled run"
        );
    }

    #[test]
    fn gives_old_interval_chats_a_next_run() {
        let mut schedule = Schedule {
            kind: Kind::Interval,
            every_minutes: 5,
            ..Schedule::default()
        };
        let now = at(16, 9, 0);
        schedule.ensure_next_run(&now);
        assert_eq!(schedule.next_run.as_deref(), Some("2026-11-16T09:05:00Z"));
        schedule.ensure_next_run(&at(16, 9, 3));
        assert_eq!(
            schedule.next_run.as_deref(),
            Some("2026-11-16T09:05:00Z"),
            "an existing next run is left alone"
        );
    }
}
