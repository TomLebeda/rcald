use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;

use crate::model::{AlarmSpec, EventSeries, Reminder};
use crate::recur::occurrences;

/// Triggers emitted more than this long after their due time count as "late".
const LATE_GRACE: chrono::Duration = chrono::Duration::seconds(90);

/// Slack added to both ends of the window in which recurring series are
/// expanded (see `collect`).
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
    /// Longest fallback offset (all-day or not), as a lead before event start.
    fn max_fallback_lead(&self) -> chrono::Duration {
        let secs = self
            .fallback_offsets
            .iter()
            .chain(&self.fallback_all_day_offsets)
            .map(|d| d.as_secs() as i64)
            .max()
            .unwrap_or(0);
        chrono::Duration::seconds(secs)
    }

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

/// Keys of reminders already emitted (see `Reminder::key`), mapped to the end
/// timestamp of their occurrence so ended ones can be pruned. In-memory on
/// purpose: combined with the late-fire rule in `collect`, a restart replays
/// everything still relevant instead of losing reminders.
pub type FiredSet = HashMap<(String, i64, i64), i64>;

/// Record an emitted reminder in the dedup set.
pub fn mark_fired(fired: &mut FiredSet, r: &Reminder) {
    fired.insert(r.key(), r.end.timestamp());
}

/// Forget reminders of occurrences that have ended: `collect` never emits
/// those again, so their keys are dead weight.
pub fn prune_fired(fired: &mut FiredSet, now: DateTime<Utc>) {
    fired.retain(|_, end| *end > now.timestamp());
}

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
            // Only occurrences that have not ended can emit, so look back just
            // far enough to see the one possibly in progress. Look ahead far
            // enough that every occurrence whose trigger is <= horizon is seen:
            // the longest lead among VALARMs and fallbacks. The margin on top
            // also yields the next wake-up target.
            let lead = series.max_alarm_lead().max(st.max_fallback_lead());
            let from = now.with_timezone(&tz) - series.duration() - WINDOW_MARGIN;
            let until = horizon.with_timezone(&tz) + lead + WINDOW_MARGIN;
            for occ in occurrences(series, from, until) {
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
                    // Shown in the event's tz (absolute TRIGGERs are UTC).
                    let trigger = a
                        .trigger(&occ.start, &occ.end)
                        .with_timezone(&occ.start.timezone());
                    if !seen.insert(trigger.timestamp()) {
                        continue;
                    }
                    let key = (occ.uid.clone(), occ.start.timestamp(), trigger.timestamp());
                    if fired.contains_key(&key) {
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
        let empty = FiredSet::new();

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
        let mut fired = FiredSet::new();
        for r in &due {
            mark_fired(&mut fired, r);
        }
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
        let (due, _) = collect(&[st], n, horizon, &FiredSet::new());
        let plain: Vec<_> = due.iter().filter(|r| r.summary == "plain").collect();
        let armed: Vec<_> = due.iter().filter(|r| r.summary == "armed").collect();
        assert_eq!(plain.len(), 1, "exactly one fallback trigger");
        assert_eq!(plain[0].trigger, at(14, 30));
        assert_eq!(armed.len(), 1, "VALARM wins, no fallback added");
        assert_eq!(armed[0].trigger, at(14, 45));
    }

    fn daily(summary: &str, alarms: Vec<AlarmSpec>) -> EventSeries {
        // daily 18:00-19:00 local since a year ago
        let start = P.with_ymd_and_hms(2025, 10, 1, 18, 0, 0).unwrap();
        let mut s = event_series(summary, start, start + chrono::Duration::hours(1), alarms);
        s.rrule = Some("FREQ=DAILY".into());
        s
    }

    #[test]
    fn daily_series_with_long_valarm_keeps_all_reminders() {
        // Regression: a count-based window started `lead` before now and only
        // saw ended occurrences, losing every reminder of the series.
        let n = now(); // 14:00 local
        let week = AlarmSpec::Relative {
            offset_secs: -7 * 86400,
            to_end: false,
        };
        let quarter = AlarmSpec::Relative {
            offset_secs: -900,
            to_end: false,
        };
        let st = state(vec![daily("d", vec![week, quarter])], &[]);
        let horizon = n + chrono::Duration::hours(4) + chrono::Duration::minutes(2);
        let (due, _) = collect(&[st], n, horizon, &FiredSet::new());
        let on_time: Vec<_> = due
            .iter()
            .filter(|r| !r.late)
            .map(|r| (r.start, r.trigger))
            .collect();
        // today's 15m reminder and the 7-day one for 2026-10-18, both 18:00-ish
        let next_week = P.with_ymd_and_hms(2026, 10, 18, 18, 0, 0).unwrap();
        assert_eq!(
            on_time,
            vec![(at(18, 0), at(17, 45)), (next_week, at(18, 0))]
        );
        // 7-day reminders of the coming week's occurrences already triggered:
        // fresh start, so they are caught up as late (late-fire rule).
        assert!(due.iter().filter(|r| r.late).all(|r| r.start > at(14, 0)));
    }

    #[test]
    fn long_fallback_on_daily_series_wakes_on_time() {
        // 168h fallback: the reminder for the occurrence a week out must be
        // the next wake-up target, not caught up a day before the event.
        let n = now();
        let st = state(vec![daily("d", vec![])], &[168 * 3600]);
        let fired = {
            let (due, _) = collect(std::slice::from_ref(&st), n, n, &FiredSet::new());
            let mut f = FiredSet::new();
            for r in &due {
                mark_fired(&mut f, r);
            }
            f
        };
        let (due, next) = collect(std::slice::from_ref(&st), n, n, &fired);
        assert!(due.is_empty());
        assert_eq!(next, Some(at(18, 0).with_timezone(&Utc)));
        let at_six = at(18, 0).with_timezone(&Utc);
        let (due, _) = collect(&[st], at_six, at_six, &fired);
        assert_eq!(due.len(), 1);
        assert!(!due[0].late);
        assert_eq!(
            due[0].start,
            P.with_ymd_and_hms(2026, 10, 18, 18, 0, 0).unwrap()
        );
    }

    #[test]
    fn prune_drops_only_ended_occurrences() {
        let n = now();
        let mut fired = FiredSet::new();
        fired.insert(
            ("ended".into(), 0, 0),
            (n - chrono::Duration::minutes(1)).timestamp(),
        );
        fired.insert(
            ("running".into(), 0, 0),
            (n + chrono::Duration::minutes(1)).timestamp(),
        );
        prune_fired(&mut fired, n);
        assert_eq!(fired.len(), 1);
        assert!(fired.keys().all(|k| k.0 == "running"));
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
        let (due, next) = collect(&[st], n, n, &FiredSet::new());
        assert!(due.is_empty());
        assert!(next.is_none());
    }
}
