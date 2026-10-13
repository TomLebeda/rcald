use std::collections::HashSet;
use std::time::Duration;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;

use crate::model::{AlarmSpec, EventSeries, Reminder};
use crate::recur::occurrences;

/// Triggers emitted more than this long after their due time count as "late".
const LATE_GRACE: chrono::Duration = chrono::Duration::seconds(90);

/// How far back from `now` recurring series are expanded.
/// Additional slack on top of event duration + longest alarm lead.
const WINDOW_MARGIN: chrono::Duration = chrono::Duration::days(1);

/// Per-calendar state: config-derived fields + last successfully fetched series.
pub struct CalendarState {
    pub name: String,
    pub url: String,
    pub fallback_offsets: Vec<Duration>,
    pub fallback_all_day_offsets: Vec<Duration>,
    pub series: Vec<EventSeries>,
}

impl CalendarState {
    pub fn from_configs(cfg: &crate::config::RuntimeConfig) -> Vec<CalendarState> {
        cfg.calendars
            .iter()
            .map(|c| CalendarState {
                name: c.name.clone(),
                url: c.url.clone(),
                fallback_offsets: c.fallback_offsets.clone(),
                fallback_all_day_offsets: c.fallback_all_day_offsets.clone(),
                series: Vec::new(),
            })
            .collect()
    }
}

/// Keys of reminders already emitted (see `Reminder::key`). In-memory on
/// purpose: combined with the late-fire rule in `collect`, a restart replays
/// everything still relevant instead of losing reminders.
pub type FiredSet = HashSet<(String, i64, i64)>;

/// Collect the reminders due at `now` (trigger at or before `horizon` while the
/// event has not ended yet), plus the earliest future trigger (sleep target).
///
/// `fired` holds dedup keys of already emitted reminders; due but already fired
/// reminders are skipped.
pub fn collect(
    states: &[CalendarState],
    now: DateTime<Utc>,
    horizon: DateTime<Utc>,
    fired: &FiredSet,
) -> (Vec<Reminder>, Option<DateTime<Utc>>) {
    let mut due: Vec<Reminder> = Vec::new();
    let mut next: Option<DateTime<Utc>> = None;

    for st in states {
        for series in &st.series {
            let tz: Tz = series.start.timezone();
            // Look back far enough to still see the occurrence that may be in
            // progress right now (duration) and any trigger we may have missed
            // while down (longest alarm lead), plus a day of slack.
            let from = now.with_timezone(&tz)
                - series.duration()
                - series.max_alarm_lead()
                - WINDOW_MARGIN;
            // 3 occurrences: the possibly in-progress one plus the next
            // upcoming ones; also bounds absurdly frequent rules.
            for occ in occurrences(series, from, 3) {
                // VALARMs always win: fallback offsets apply only to
                // occurrences with no alarms of their own, never in addition.
                let alarms: Vec<AlarmSpec> = if occ.alarms.is_empty() {
                    let src = if occ.all_day {
                        &st.fallback_all_day_offsets
                    } else {
                        &st.fallback_offsets
                    };
                    src.iter()
                        .map(|d| AlarmSpec::Relative {
                            offset_secs: neg_secs(*d),
                            to_end: false,
                        })
                        .collect()
                } else {
                    occ.alarms.clone()
                };

                // Two VALARMs on one event may resolve to the same instant;
                // emit such duplicates only once.
                let mut seen = HashSet::new();
                for a in &alarms {
                    let trigger = a.trigger(&occ.start, &occ.end);
                    if !seen.insert(trigger.timestamp()) {
                        continue;
                    }
                    let key = (occ.uid.clone(), occ.start.timestamp(), trigger.timestamp());
                    if fired.contains(&key) {
                        continue;
                    }
                    // Late-fire rule: emit any trigger up to `horizon` as long
                    // as the event has not ended yet. Reminders missed while
                    // the daemon was down therefore still fire on restart
                    // (marked late), while triggers of ended events are
                    // dropped for good.
                    if trigger <= horizon && now < occ.end {
                        due.push(Reminder {
                            calendar: st.name.clone(),
                            uid: occ.uid.clone(),
                            summary: occ.summary.clone(),
                            start: occ.start,
                            end: occ.end,
                            all_day: occ.all_day,
                            trigger,
                            late: now - trigger.with_timezone(&Utc) > LATE_GRACE,
                        });
                    } else if trigger > now {
                        // Not due yet: a candidate for the next wake-up.
                        let t_utc = trigger.with_timezone(&Utc);
                        if next.is_none_or(|n| t_utc < n) {
                            next = Some(t_utc);
                        }
                    }
                }
            }
        }
    }

    due.sort_by_key(|r| r.trigger);
    (due, next)
}

fn neg_secs(d: Duration) -> i64 {
    -(d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const P: Tz = Tz::Europe__Prague;

    fn event_series(
        summary: &str,
        start: DateTime<Tz>,
        end: DateTime<Tz>,
        alarms: Vec<AlarmSpec>,
    ) -> EventSeries {
        EventSeries {
            uid: summary.into(),
            summary: summary.into(),
            start,
            end,
            all_day: false,
            rrule: None,
            exdates: vec![],
            cancelled: vec![],
            alarms,
            overrides: vec![],
        }
    }

    fn state(series: Vec<EventSeries>, offsets: &[u64]) -> CalendarState {
        CalendarState {
            name: "test".into(),
            url: "unused".into(),
            fallback_offsets: offsets.iter().map(|s| Duration::from_secs(*s)).collect(),
            fallback_all_day_offsets: vec![],
            series,
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap()
    }

    fn at(h: u32, m: u32) -> DateTime<Tz> {
        P.with_ymd_and_hms(2026, 10, 11, h, m, 0).unwrap()
    }

    #[test]
    fn due_includes_triggered_and_future_within_horizon() {
        let n = now();
        // event 13:30-14:30 local (= 11:30-12:30 UTC, Prague CEST): in progress at 12:00 UTC
        let in_progress = event_series(
            "in-progress",
            at(13, 30),
            at(14, 30),
            vec![
                AlarmSpec::Relative {
                    offset_secs: -3600,
                    to_end: false,
                }, // triggered 12:30 local = 10:30 UTC (past)
            ],
        );
        // future event at 15:00 local with -10m alarm -> trigger 14:50 local = 12:50 UTC
        let fut = event_series(
            "future-in-horizon",
            at(15, 0),
            at(16, 0),
            vec![AlarmSpec::Relative {
                offset_secs: -600,
                to_end: false,
            }],
        );
        let st = state(vec![in_progress, fut], &[]);
        let empty = HashSet::new();

        // horizon = now: only the already triggered one is due (late catch-up)
        let (due, next) = collect(std::slice::from_ref(&st), n, n, &empty);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].summary, "in-progress");
        assert!(due[0].late);
        assert!(next.is_some());

        // horizon = now + 1h: the future trigger is due too
        let (due, _) = collect(
            std::slice::from_ref(&st),
            n,
            n + chrono::Duration::hours(1),
            &empty,
        );
        assert_eq!(due.len(), 2);
        assert!(!due[1].late);

        // fired set suppresses re-emit
        let fired: HashSet<_> = due.iter().map(|r| r.key()).collect();
        let (due2, _) = collect(&[st], n, n + chrono::Duration::hours(1), &fired);
        assert!(due2.is_empty());
    }

    #[test]
    fn fallback_applies_only_without_alarms() {
        let n = now();
        let noalarm = event_series("plain", at(15, 0), at(16, 0), vec![]);
        let withalarm = event_series(
            "armed",
            at(15, 0),
            at(16, 0),
            vec![AlarmSpec::Relative {
                offset_secs: -900,
                to_end: false,
            }],
        );
        // fallback 30m
        let st = state(vec![noalarm, withalarm], &[1800]);
        let horizon = n + chrono::Duration::hours(4);
        let (due, _) = collect(&[st], n, horizon, &HashSet::new());
        let plain: Vec<_> = due.iter().filter(|r| r.summary == "plain").collect();
        let armed: Vec<_> = due.iter().filter(|r| r.summary == "armed").collect();
        assert_eq!(plain.len(), 1, "exactly one fallback trigger");
        assert_eq!(plain[0].trigger, at(14, 30));
        assert_eq!(armed.len(), 1, "VALARM wins, no fallback added");
        assert_eq!(armed[0].trigger, at(14, 45));
    }

    #[test]
    fn missed_triggers_of_ended_events_are_dropped() {
        let n = now();
        let past = event_series(
            "gone",
            at(8, 0),
            at(9, 0),
            vec![AlarmSpec::Relative {
                offset_secs: -600,
                to_end: false,
            }],
        );
        let st = state(vec![past], &[]);
        let (due, next) = collect(&[st], n, n, &HashSet::new());
        assert!(due.is_empty());
        assert!(next.is_none());
    }
}
