//! Scheduled chats: a prompt Atrium sends by itself at a time of day, in a
//! folder, to an agent, such as "check CI and summarise new issues" every
//! weekday morning. They run while Atrium is open; one missed while it was
//! closed runs when it opens, if that is the same day and within six hours.

use jiff::civil::{DateTime, Time, Weekday};
use serde::{Deserialize, Serialize};

/// How late a missed run may still start.
const GRACE_MINUTES: i32 = 6 * 60;

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
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub folder: String,
    pub backend: String,
    /// `HH:MM`, local time.
    pub time: String,
    pub days: Days,
    pub off: bool,
    /// The local date it last ran, `YYYY-MM-DD`.
    pub last_run: Option<String>,
}

/// `9:05` or `09:05` as a time of day.
pub fn parse_time(text: &str) -> Option<Time> {
    let (h, m) = text.trim().split_once([':', '：'])?;
    let (h, m): (i8, i8) = (h.trim().parse().ok()?, m.trim().parse().ok()?);
    Time::new(h, m, 0, 0).ok()
}

impl Schedule {
    /// Whether it should run at local time `now`.
    pub fn due(&self, now: DateTime) -> bool {
        let Some(at) = parse_time(&self.time) else {
            return false;
        };
        let today = now.date().to_string();
        if self.off || !self.days.includes(now.weekday()) || self.last_run.as_deref() == Some(today.as_str()) {
            return false;
        }
        let minutes = |t: Time| i32::from(t.hour()) * 60 + i32::from(t.minute());
        let late = minutes(now.time()) - minutes(at);
        (0..=GRACE_MINUTES).contains(&late)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn at(day: i8, h: i8, m: i8) -> DateTime {
        // 2026-11-16 is a Monday.
        date(2026, 11, day).at(h, m, 0, 0)
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
    fn runs_once_on_its_days_after_its_time() {
        let mut s = weekday_morning();
        assert!(!s.due(at(16, 8, 59)));
        assert!(s.due(at(16, 9, 0)));
        assert!(s.due(at(16, 14, 0)), "missed runs start late the same day");
        assert!(!s.due(at(16, 15, 30)), "but not too late");
        assert!(!s.due(at(21, 9, 30)), "Saturday is not a weekday");
        s.last_run = Some("2026-11-16".into());
        assert!(!s.due(at(16, 10, 0)), "once a day");
        assert!(s.due(at(17, 9, 0)));
        s.off = true;
        assert!(!s.due(at(17, 9, 0)));
    }
}
