use chrono::{DateTime, Duration};
use chrono_tz::Tz;

/// How a single event reminder is derived, from an ical VALARM TRIGGER.
#[derive(Debug, Clone)]
pub enum AlarmSpec {
    /// trigger = base + offset_secs, where base is event start (RELATED=START,
    /// the default) or event end (RELATED=END). Negative offset = before base.
    Relative { offset_secs: i64, to_end: bool },
    /// TRIGGER;VALUE=DATE-TIME:...
    Absolute(DateTime<Tz>),
}

impl AlarmSpec {
    pub fn trigger(&self, start: &DateTime<Tz>, end: &DateTime<Tz>) -> DateTime<Tz> {
        match self {
            AlarmSpec::Relative {
                offset_secs,
                to_end,
            } => {
                let base = if *to_end { end } else { start };
                *base + Duration::seconds(*offset_secs)
            }
            AlarmSpec::Absolute(dt) => *dt,
        }
    }

    /// How far before event start this alarm may trigger (0 when not applicable).
    /// Used to size the look-back window when expanding recurring events.
    pub fn lead_secs(&self) -> i64 {
        match self {
            AlarmSpec::Relative {
                offset_secs,
                to_end,
            } => {
                let lead = if *to_end { 0 } else { -*offset_secs };
                lead.max(0)
            }
            AlarmSpec::Absolute(_) => 0,
        }
    }
}

/// A VEVENT carrying RECURRENCE-ID: a modified single instance of a recurring series.
#[derive(Debug, Clone)]
pub struct RecurOverride {
    /// The original occurrence start this instance replaces.
    pub recurrence_id: DateTime<Tz>,
    pub start: DateTime<Tz>,
    pub end: DateTime<Tz>,
    pub all_day: bool,
    pub summary: String,
    /// Alarms defined on the override itself. When empty, master's alarms are inherited.
    pub alarms: Vec<AlarmSpec>,
}

/// A parsed event (series), as read from the ics feed.
#[derive(Debug, Clone)]
pub struct EventSeries {
    pub uid: String,
    pub summary: String,
    pub start: DateTime<Tz>,
    pub end: DateTime<Tz>,
    pub all_day: bool,
    pub rrule: Option<String>,
    pub exdates: Vec<DateTime<Tz>>,
    /// recurrence_ids of instances cancelled via STATUS:CANCELLED override events
    pub cancelled: Vec<DateTime<Tz>>,
    pub alarms: Vec<AlarmSpec>,
    pub overrides: Vec<RecurOverride>,
}

impl EventSeries {
    pub fn duration(&self) -> Duration {
        self.end - self.start
    }

    /// Longest lead time among the series' relative alarms (0 if none).
    pub fn max_alarm_lead(&self) -> Duration {
        let secs = self
            .alarms
            .iter()
            .map(AlarmSpec::lead_secs)
            .max()
            .unwrap_or(0);
        Duration::seconds(secs)
    }
}

/// One concrete occurrence of an event (single or recurring instance).
#[derive(Debug, Clone)]
pub struct Occurrence {
    pub uid: String,
    pub summary: String,
    pub start: DateTime<Tz>,
    pub end: DateTime<Tz>,
    pub all_day: bool,
    /// Resolved alarms for this instance: the override's own if it defines
    /// any, otherwise inherited from the series master (see recur.rs).
    pub alarms: Vec<AlarmSpec>,
}

/// A reminder ready to be emitted.
#[derive(Debug, Clone)]
pub struct Reminder {
    pub calendar: String,
    pub uid: String,
    pub summary: String,
    pub start: DateTime<Tz>,
    pub end: DateTime<Tz>,
    pub all_day: bool,
    pub trigger: DateTime<Tz>,
    /// true when emitted noticeably later than the trigger time (catch-up after downtime)
    pub late: bool,
}

impl Reminder {
    /// Stable dedup key: one reminder per (event, occurrence, trigger).
    pub fn key(&self) -> (String, i64, i64) {
        (
            self.uid.clone(),
            self.start.timestamp(),
            self.trigger.timestamp(),
        )
    }
}
