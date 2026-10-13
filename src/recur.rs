use chrono::{DateTime, LocalResult, TimeZone, Utc};
use chrono_tz::Tz;
use rrule::{RRule, RRuleSet, Tz as RTz, Unvalidated};

use crate::model::{EventSeries, Occurrence};

/// Hard cap for the occurrence iterator hops (safety net for pathological rules).
const MAX_ITER: usize = 100_000;

/// Compute up to `take` occurrences of the series with occurrence start at or after
/// `from`, applying RECURRENCE-ID overrides and cancellations. Non-recurring series
/// always yield their single occurrence regardless of `from`.
pub fn occurrences(series: &EventSeries, from: DateTime<Tz>, take: usize) -> Vec<Occurrence> {
    let starts: Vec<DateTime<Tz>> = match &series.rrule {
        None => vec![series.start],
        Some(_) => rrule_starts(series, from, take),
    };

    let dur = series.duration();
    let mut out: Vec<Occurrence> = Vec::new();
    for st in starts {
        if series
            .cancelled
            .iter()
            .any(|c| c.timestamp() == st.timestamp())
        {
            continue;
        }
        let occ = match series
            .overrides
            .iter()
            .find(|o| o.recurrence_id.timestamp() == st.timestamp())
        {
            Some(ov) => Occurrence {
                uid: series.uid.clone(),
                summary: ov.summary.clone(),
                start: ov.start,
                end: ov.end,
                all_day: ov.all_day,
                alarms: if ov.alarms.is_empty() {
                    series.alarms.clone()
                } else {
                    ov.alarms.clone()
                },
            },
            None => Occurrence {
                uid: series.uid.clone(),
                summary: series.summary.clone(),
                start: st,
                end: st + dur,
                all_day: series.all_day,
                alarms: series.alarms.clone(),
            },
        };
        out.push(occ);
        if out.len() >= take {
            break;
        }
    }
    out.sort_by_key(|o| o.start);
    out
}

fn rrule_starts(series: &EventSeries, from: DateTime<Tz>, take: usize) -> Vec<DateTime<Tz>> {
    let Some(set) = build_set(series) else {
        return vec![];
    };
    let tz = series.start.timezone();
    let mut out = Vec::new();
    for (i, dt) in (&set).into_iter().enumerate() {
        // Hop count is tracked by hand (instead of .take(MAX_ITER)) so that
        // hitting the cap produces a warning instead of silent truncation.
        if i >= MAX_ITER {
            log::warn!("rrule of '{}' hit iteration cap", series.uid);
            break;
        }
        let d = from_rtz(dt, tz);
        if d >= from {
            out.push(d);
            if out.len() >= take {
                break;
            }
        }
    }
    out
}

fn build_set(series: &EventSeries) -> Option<RRuleSet> {
    let rule_str = series.rrule.as_ref()?;
    let dt_start = to_rtz(series.start);
    // Accept the RRULE value with or without the "RRULE:" prefix; the ical
    // parser strips it, other sources may not.
    let rule: RRule<Unvalidated> = match rule_str
        .parse()
        .or_else(|_| format!("RRULE:{rule_str}").parse())
    {
        Ok(r) => r,
        Err(e) => {
            log::warn!("invalid RRULE '{}' on {}: {}", rule_str, series.uid, e);
            return None;
        }
    };
    let rule = match rule.validate(dt_start) {
        Ok(r) => r,
        Err(e) => {
            log::warn!("invalid RRULE '{}' on {}: {}", rule_str, series.uid, e);
            return None;
        }
    };
    let mut set = RRuleSet::new(dt_start).limit().rrule(rule);
    for ex in &series.exdates {
        set = set.exdate(to_rtz(*ex));
    }
    Some(set)
}

// chrono_tz <-> rrule::Tz conversions, always through the UTC timeline (lossless).
// The rrule crate iterates wall-clock recurrences in the event's own timezone,
// so occurrences land on the correct local time across DST transitions; only
// the final boundary crossing above needs the UTC timestamp as the anchor.

fn to_rtz(dt: DateTime<Tz>) -> DateTime<RTz> {
    let rtz: RTz = dt.timezone().into();
    to_utc(dt).with_timezone(&rtz)
}

fn from_rtz(dt: DateTime<RTz>, tz: Tz) -> DateTime<Tz> {
    to_utc(dt).with_timezone(&tz)
}

fn to_utc<TzOut: chrono::TimeZone>(dt: DateTime<TzOut>) -> DateTime<Utc> {
    match Utc.timestamp_opt(dt.timestamp(), 0) {
        LocalResult::Single(d) => d,
        _ => unreachable!("UTC mapping is always unambiguous"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const P: Tz = Tz::Europe__Prague;

    fn series(rrule: &str) -> EventSeries {
        EventSeries {
            uid: "u1".into(),
            summary: "s".into(),
            start: P.with_ymd_and_hms(2026, 1, 5, 9, 0, 0).unwrap(), // Monday
            end: P.with_ymd_and_hms(2026, 1, 5, 10, 0, 0).unwrap(),
            all_day: false,
            rrule: Some(rrule.into()),
            exdates: vec![],
            cancelled: vec![],
            alarms: vec![],
            overrides: vec![],
        }
    }

    #[test]
    fn daily_rrule_counts() {
        let s = series("FREQ=DAILY;COUNT=3");
        let occs = occurrences(&s, s.start, 10);
        assert_eq!(occs.len(), 3);
        assert_eq!(occs[0].start, s.start);
        assert_eq!(occs[2].start, s.start + chrono::Duration::days(2));
        assert_eq!(
            occs[2].end,
            s.start + chrono::Duration::days(2) + chrono::Duration::hours(1)
        );
    }

    #[test]
    fn exdate_suppresses() {
        let mut s = series("FREQ=DAILY;COUNT=3");
        s.exdates.push(s.start + chrono::Duration::days(1));
        let occs = occurrences(&s, s.start, 10);
        assert_eq!(occs.len(), 2);
        assert_eq!(occs[1].start, s.start + chrono::Duration::days(2));
    }

    #[test]
    fn override_moves_instance() {
        let mut s = series("FREQ=DAILY;COUNT=3");
        let rid = s.start + chrono::Duration::days(1);
        let new_start = rid + chrono::Duration::hours(3);
        s.overrides.push(crate::model::RecurOverride {
            recurrence_id: rid,
            start: new_start,
            end: new_start + chrono::Duration::hours(1),
            all_day: false,
            summary: "moved".into(),
            alarms: vec![],
        });
        let occs = occurrences(&s, s.start, 10);
        assert_eq!(occs.len(), 3);
        assert_eq!(occs[1].start, new_start);
        assert_eq!(occs[1].summary, "moved");
    }

    #[test]
    fn cancelled_instance_dropped() {
        let mut s = series("FREQ=DAILY;COUNT=4");
        let rid = s.start + chrono::Duration::days(2);
        s.cancelled.push(rid);
        let occs = occurrences(&s, s.start, 10);
        assert_eq!(occs.len(), 3);
        assert!(!occs.iter().any(|o| o.start == rid));
    }

    #[test]
    fn non_recurring_always_yields_single() {
        let mut s = series("FREQ=DAILY;COUNT=3");
        s.rrule = None;
        let far_future = s.start + chrono::Duration::days(3650);
        let occs = occurrences(&s, far_future, 1);
        assert_eq!(occs.len(), 1);
        assert_eq!(occs[0].start, s.start);
    }
}
