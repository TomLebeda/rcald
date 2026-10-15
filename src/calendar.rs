//! Turns VEVENTs into concrete occurrences with their alarms.

use std::collections::{HashMap, HashSet};

use anyhow::Context;
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, TimeZone};
use rrule::{RRule, RRuleSet, Tz, Unvalidated};
use serde::Serialize;
use tracing::warn;

use crate::{
    config::CalendarConfig,
    ics::{self, Component, Property},
};

/// Upper bound on occurrences of one recurring event inside the window.
const MAX_OCCURRENCES: u16 = 10_000;

/// One occurrence of a calendar event.
#[derive(Debug, Serialize)]
pub struct Event {
    pub calendar: String,
    #[serde(rename = "event_uid")]
    pub uid: String,
    pub title: String,
    pub location: Option<String>,
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    pub all_day: bool,
    #[serde(skip)]
    pub alarms: Vec<Alarm>,
}

#[derive(Debug)]
pub struct Alarm {
    pub at: DateTime<Local>,
    pub source: Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// VALARM from the calendar itself.
    Ical,
    /// Offset from the config file.
    Default,
}

impl Event {
    /// Stable identity of one alarm of this occurrence, used to never emit it twice.
    pub fn alarm_key(&self, alarm: &Alarm) -> String {
        let (start, at) = (self.start.timestamp(), alarm.at.timestamp());
        format!("{}|{}|{start}|{at}", self.calendar, self.uid)
    }
}

/// Parses an iCalendar document and returns the occurrences overlapping `from..to`.
pub fn events(
    cfg: &CalendarConfig,
    force_defaults: bool,
    ics: &str,
    from: DateTime<Local>,
    to: DateTime<Local>,
) -> anyhow::Result<Vec<Event>> {
    let root = ics::parse(ics)?;
    let vevents: Vec<_> = root
        .children("VCALENDAR")
        .flat_map(|cal| cal.children("VEVENT"))
        .filter(|e| e.text("STATUS").as_deref() != Some("CANCELLED"))
        .collect();

    // Individually edited instances replace the matching occurrence of their recurring master.
    let mut overridden: HashMap<String, HashSet<i64>> = HashMap::new();
    for vevent in &vevents {
        if let (Some(uid), Some((id, _))) = (vevent.text("UID"), vevent.prop("RECURRENCE-ID").and_then(time)) {
            overridden.entry(uid).or_default().insert(id.timestamp());
        }
    }

    let expander = Expander { cfg, force_defaults, overridden, from, to };
    let mut events = Vec::new();
    for vevent in vevents {
        match expander.expand(vevent) {
            Ok(occurrences) => events.extend(occurrences),
            Err(e) => warn!(calendar = %cfg.name, uid = vevent.text("UID"), "skipping event: {e:#}"),
        }
    }
    Ok(events)
}

struct Expander<'a> {
    cfg: &'a CalendarConfig,
    force_defaults: bool,
    overridden: HashMap<String, HashSet<i64>>,
    from: DateTime<Local>,
    to: DateTime<Local>,
}

impl Expander<'_> {
    fn expand(&self, vevent: &Component) -> anyhow::Result<Vec<Event>> {
        let uid = vevent.text("UID").context("missing UID")?;
        let (start, all_day) = vevent.prop("DTSTART").and_then(time).context("missing or invalid DTSTART")?;
        let duration = duration(vevent, start, all_day)?;
        let end_of = |start: DateTime<Tz>| {
            // All-day events span whole days, so a DST change doesn't move the end off midnight.
            let end = all_day.then(|| midnight(start.date_naive() + duration)).flatten();
            end.unwrap_or(start + duration)
        };

        let starts = if vevent.prop("RRULE").is_some() && vevent.prop("RECURRENCE-ID").is_none() {
            let overridden = self.overridden.get(&uid);
            recurrences(vevent, start, self.from - duration, self.to)?
                .into_iter()
                .filter(|s| !overridden.is_some_and(|ids| ids.contains(&s.timestamp())))
                .collect()
        } else {
            vec![start]
        };

        let title = vevent.text("SUMMARY").unwrap_or_default();
        let location = vevent.text("LOCATION").filter(|l| !l.is_empty());
        Ok(starts
            .into_iter()
            .map(|s| (s.with_timezone(&Local), end_of(s).with_timezone(&Local)))
            .filter(|&(start, end)| end > self.from && start < self.to)
            .map(|(start, end)| Event {
                calendar: self.cfg.name.clone(),
                uid: uid.clone(),
                title: title.clone(),
                location: location.clone(),
                alarms: self.alarms(vevent, start, end, all_day),
                start,
                end,
                all_day,
            })
            .collect())
    }

    fn alarms(&self, vevent: &Component, start: DateTime<Local>, end: DateTime<Local>, all_day: bool) -> Vec<Alarm> {
        let mut alarms: Vec<_> = vevent
            .children("VALARM")
            .filter_map(|a| trigger(a.prop("TRIGGER")?, start, end))
            .map(|at| Alarm { at, source: Source::Ical })
            .collect();
        if alarms.is_empty() || self.force_defaults {
            let offsets = if all_day { &self.cfg.all_day_offsets } else { &self.cfg.offsets };
            alarms.extend(
                offsets
                    .iter()
                    .filter_map(|&o| TimeDelta::from_std(o).ok())
                    .map(|o| Alarm { at: start - o, source: Source::Default }),
            );
        }
        alarms.sort_by_key(|a| a.at);
        alarms.dedup_by_key(|a| a.at);
        alarms
    }
}

/// Event length from DTEND or DURATION; RFC 5545 defaults are one day (all-day) or zero.
fn duration(vevent: &Component, start: DateTime<Tz>, all_day: bool) -> anyhow::Result<TimeDelta> {
    let duration = if let Some((end, _)) = vevent.prop("DTEND").and_then(time) {
        if all_day { TimeDelta::days((end.date_naive() - start.date_naive()).num_days()) } else { end - start }
    } else if let Some(prop) = vevent.prop("DURATION") {
        parse_duration(&prop.value).context("invalid DURATION")?
    } else {
        TimeDelta::days(all_day.into())
    };
    Ok(duration.max(TimeDelta::zero()))
}

fn recurrences(
    vevent: &Component,
    start: DateTime<Tz>,
    from: DateTime<Local>,
    to: DateTime<Local>,
) -> anyhow::Result<Vec<DateTime<Tz>>> {
    let tz = start.timezone();
    let mut set = RRuleSet::new(start).after(from.with_timezone(&tz)).before(to.with_timezone(&tz));
    for prop in vevent.props("RRULE") {
        let rule: RRule<Unvalidated> = prop.value.parse()?;
        // The rrule crate wants UNTIL in UTC when DTSTART has a zone; UTC is valid for any DTSTART.
        let rule = match rule.get_until().copied() {
            Some(until) => rule.until(until.with_timezone(&Tz::UTC)),
            None => rule,
        };
        set = set.rrule(rule.validate(start)?);
    }
    let dates = |name| vevent.props(name).flat_map(times).map(|(t, _)| t);
    set = dates("RDATE").fold(set, RRuleSet::rdate);
    set = dates("EXDATE").fold(set, RRuleSet::exdate);
    Ok(set.all(MAX_OCCURRENCES).dates)
}

fn trigger(prop: &Property, start: DateTime<Local>, end: DateTime<Local>) -> Option<DateTime<Local>> {
    if prop.param("VALUE") == Some("DATE-TIME") {
        return time(prop).map(|(t, _)| t.with_timezone(&Local));
    }
    let base = if prop.param("RELATED") == Some("END") { end } else { start };
    Some(base + parse_duration(&prop.value)?)
}

fn time(prop: &Property) -> Option<(DateTime<Tz>, bool)> {
    times(prop).next()
}

/// The (comma-separated) DATE or DATE-TIME values of a property; `true` marks a date-only value.
fn times(prop: &Property) -> impl Iterator<Item = (DateTime<Tz>, bool)> + '_ {
    prop.value.split(',').filter_map(|value| {
        let value = value.trim();
        if prop.param("VALUE") == Some("DATE") || value.len() == 8 {
            let date = NaiveDate::parse_from_str(value, "%Y%m%d").ok()?;
            return Some((midnight(date)?, true));
        }
        let (value, utc) = value.strip_suffix('Z').map_or((value, false), |v| (v, true));
        let naive = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S").ok()?;
        let tz = match (utc, prop.param("TZID")) {
            (true, _) => Tz::UTC,
            (false, Some(tzid)) => timezone(tzid),
            (false, None) => Tz::LOCAL,
        };
        Some((tz.from_local_datetime(&naive).earliest()?, false))
    })
}

fn timezone(tzid: &str) -> Tz {
    tzid.parse().map(Tz::Tz).unwrap_or_else(|_| {
        warn!("unknown TZID {tzid:?}, using local time");
        Tz::LOCAL
    })
}

fn midnight(date: NaiveDate) -> Option<DateTime<Tz>> {
    Tz::LOCAL.from_local_datetime(&date.and_time(NaiveTime::MIN)).earliest()
}

/// RFC 5545 duration, e.g. `-PT15M`, `P1D`, `P1DT2H30M`, `P2W`.
fn parse_duration(s: &str) -> Option<TimeDelta> {
    let s = s.trim();
    let (sign, s) = match s.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, s.strip_prefix('+').unwrap_or(s)),
    };
    let (mut secs, mut num) = (0i64, None::<i64>);
    for c in s.strip_prefix('P')?.chars() {
        if let Some(digit) = c.to_digit(10) {
            num = Some(num.unwrap_or(0) * 10 + i64::from(digit));
            continue;
        }
        let unit = match c {
            'T' => continue,
            'W' => 7 * 86_400,
            'D' => 86_400,
            'H' => 3_600,
            'M' => 60,
            'S' => 1,
            _ => return None,
        };
        secs += num.take()? * unit;
    }
    num.is_none().then(|| TimeDelta::seconds(sign * secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> CalendarConfig {
        CalendarConfig {
            name: "test".into(),
            url: String::new(),
            offsets: vec![std::time::Duration::from_secs(600)],
            all_day_offsets: vec![std::time::Duration::from_secs(86_400)],
        }
    }

    fn local(s: &str) -> DateTime<Local> {
        Local.from_local_datetime(&NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()).unwrap()
    }

    fn parse(body: &str, from: &str, to: &str) -> Vec<Event> {
        let ics = format!("BEGIN:VCALENDAR\n{body}\nEND:VCALENDAR\n");
        events(&cfg(), false, &ics, local(from), local(to)).unwrap()
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("-PT15M"), Some(TimeDelta::minutes(-15)));
        assert_eq!(parse_duration("P1DT2H30M"), Some(TimeDelta::minutes(26 * 60 + 30)));
        assert_eq!(parse_duration("P2W"), Some(TimeDelta::weeks(2)));
        assert_eq!(parse_duration("PT0S"), Some(TimeDelta::zero()));
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("-PT15"), None);
    }

    #[test]
    fn timed_event_with_ical_alarms() {
        let events = parse(
            "BEGIN:VEVENT\nUID:a\nSUMMARY:Meet\nDTSTART:20261014T090000\nDTEND:20261014T100000\n\
             BEGIN:VALARM\nTRIGGER:-PT30M\nEND:VALARM\n\
             BEGIN:VALARM\nTRIGGER;RELATED=END:PT0S\nEND:VALARM\nEND:VEVENT",
            "2026-10-01 00:00",
            "2026-11-01 00:00",
        );
        let [event] = &events[..] else { panic!("{events:?}") };
        assert!(!event.all_day);
        let ats: Vec<_> = event.alarms.iter().map(|a| a.at).collect();
        assert_eq!(ats, [local("2026-10-14 08:30"), local("2026-10-14 10:00")]);
        assert!(event.alarms.iter().all(|a| a.source == Source::Ical));
    }

    #[test]
    fn multi_day_all_day_event_gets_default_alarm() {
        let events = parse(
            "BEGIN:VEVENT\nUID:b\nDTSTART;VALUE=DATE:20261020\nDTEND;VALUE=DATE:20261023\nEND:VEVENT",
            "2026-10-01 00:00",
            "2026-11-01 00:00",
        );
        let [event] = &events[..] else { panic!("{events:?}") };
        assert!(event.all_day);
        assert_eq!((event.start, event.end), (local("2026-10-20 00:00"), local("2026-10-23 00:00")));
        assert_eq!(event.alarms[0].at, local("2026-10-19 00:00"));
        assert_eq!(event.alarms[0].source, Source::Default);
    }

    #[test]
    fn recurrence_with_exdate_and_override() {
        let events = parse(
            "BEGIN:VEVENT\nUID:c\nSUMMARY:Daily\nDTSTART:20261012T080000\nDTEND:20261012T083000\n\
             RRULE:FREQ=DAILY;UNTIL=20261016T235959Z\nEXDATE:20261013T080000\nEND:VEVENT\n\
             BEGIN:VEVENT\nUID:c\nSUMMARY:Moved\nRECURRENCE-ID:20261014T080000\n\
             DTSTART:20261014T120000\nDTEND:20261014T123000\nEND:VEVENT",
            "2026-10-01 00:00",
            "2026-11-01 00:00",
        );
        let mut got: Vec<_> = events.iter().map(|e| (e.start, e.title.as_str())).collect();
        got.sort();
        assert_eq!(
            got,
            [
                (local("2026-10-12 08:00"), "Daily"),
                (local("2026-10-14 12:00"), "Moved"),
                (local("2026-10-15 08:00"), "Daily"),
                (local("2026-10-16 08:00"), "Daily"),
            ]
        );
    }

    #[test]
    fn window_excludes_finished_and_far_events() {
        let events = parse(
            "BEGIN:VEVENT\nUID:d\nDTSTART:20261012T080000\nDTEND:20261012T090000\nRRULE:FREQ=WEEKLY\nEND:VEVENT",
            "2026-10-20 00:00",
            "2026-11-01 00:00",
        );
        let starts: Vec<_> = events.iter().map(|e| e.start).collect();
        assert_eq!(starts, [local("2026-10-26 08:00")]);
    }
}
