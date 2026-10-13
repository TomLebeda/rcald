use std::collections::HashMap;
use std::io::BufReader;
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use ical::parser::ical::component::IcalEvent;
use ical::property::Property;
use ical::IcalParser;
use log::{debug, warn};

use crate::alarm;
use crate::model::{EventSeries, RecurOverride};
use crate::schedule::CalendarState;

/// System local timezone (iana-time-zone with UTC fallback).
pub fn local_tz() -> Tz {
    iana_time_zone::get_timezone()
        .ok()
        .and_then(|name| Tz::from_str(&name).ok())
        .unwrap_or_else(|| {
            warn!("could not detect system timezone, using UTC");
            Tz::UTC
        })
}

pub fn make_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(30)))
        .build()
        .into()
}

/// Fetch one calendar and replace its series on success. Keeps old data on failure.
pub fn refresh_state(st: &mut CalendarState, agent: &ureq::Agent, tz: Tz) -> Result<()> {
    let body = read_source(&st.url, agent)
        .with_context(|| format!("calendar '{}': fetch failed", st.name))?;
    let series = parse_calendar(&body, tz)
        .with_context(|| format!("calendar '{}': parse failed", st.name))?;
    debug!(
        "calendar '{}' refreshed: {} event series",
        st.name,
        series.len()
    );
    st.series = series;
    Ok(())
}

/// url with http(s) scheme is fetched, anything else is read as a local file
/// (used by tests and for local-only setups).
fn read_source(url: &str, agent: &ureq::Agent) -> Result<String> {
    if url.starts_with("http://") || url.starts_with("https://") {
        let mut resp = agent
            .get(url)
            .call()
            .with_context(|| format!("GET {url}"))?;
        resp.body_mut()
            .read_to_string()
            .with_context(|| format!("reading body of {url}"))
    } else {
        std::fs::read_to_string(url).with_context(|| format!("read file {url}"))
    }
}

/// A VEVENT plays one of three roles in a feed. Overrides and cancellations
/// reference their series master by UID; they are merged into the master only
/// after the whole feed has been seen, since event order in a file is not
/// guaranteed.
enum ParsedInput {
    /// A one-off event, or the master definition of a recurring series.
    Master(EventSeries),
    /// RECURRENCE-ID event: a modified single instance of the series.
    Override { uid: String, over: RecurOverride },
    /// STATUS:CANCELLED + RECURRENCE-ID: one instance of the series removed.
    Cancellation { uid: String, rid: DateTime<Tz> },
}

pub fn parse_calendar(text: &str, local_tz: Tz) -> Result<Vec<EventSeries>> {
    let parser = IcalParser::new(BufReader::new(text.as_bytes()));
    let mut masters: Vec<EventSeries> = Vec::new();
    let mut overrides: HashMap<String, Vec<RecurOverride>> = HashMap::new();
    let mut cancellations: HashMap<String, Vec<DateTime<Tz>>> = HashMap::new();

    for cal in parser {
        let cal = cal.context("invalid ical component")?;
        for ev in cal.events {
            match parse_event(&ev, local_tz) {
                Ok(Some(ParsedInput::Master(m))) => masters.push(m),
                Ok(Some(ParsedInput::Override { uid, over })) => {
                    overrides.entry(uid).or_default().push(over)
                }
                Ok(Some(ParsedInput::Cancellation { uid, rid })) => {
                    cancellations.entry(uid).or_default().push(rid)
                }
                Ok(None) => {}
                Err(e) => warn!("skipping event: {e:#}"),
            }
        }
    }

    // Masters may arrive in any order relative to their overrides; merge now.
    for m in masters.iter_mut() {
        if let Some(os) = overrides.remove(&m.uid) {
            m.overrides = os;
        }
        if let Some(cs) = cancellations.remove(&m.uid) {
            m.cancelled = cs;
        }
    }
    // overrides without their master in the feed: treat as standalone single events
    for (uid, os) in overrides {
        for o in os {
            masters.push(EventSeries {
                uid: uid.clone(),
                summary: o.summary,
                start: o.start,
                end: o.end,
                all_day: o.all_day,
                rrule: None,
                exdates: vec![],
                cancelled: vec![],
                alarms: o.alarms,
                overrides: vec![],
            });
        }
    }

    Ok(masters)
}

fn parse_event(ev: &IcalEvent, tz: Tz) -> Result<Option<ParsedInput>> {
    let status = prop(ev, "STATUS")
        .and_then(|p| p.value.as_deref())
        .unwrap_or("");

    // RECURRENCE-ID marks this VEVENT as a modification (or cancellation) of
    // a single instance of a recurring series; its value is the *original*
    // start time of the instance it replaces.
    let rid = prop(ev, "RECURRENCE-ID")
        .map(|p| {
            parse_datetime(
                p.value.as_deref().context("RECURRENCE-ID without value")?,
                p.params.as_ref(),
                tz,
            )
        })
        .transpose()?
        .map(|(dt, _all_day)| dt);

    let uid = prop(ev, "UID")
        .and_then(|p| p.value.clone())
        .context("event without UID")?;

    if status.eq_ignore_ascii_case("CANCELLED") {
        // With RECURRENCE-ID this cancels one instance of a series; without
        // it the whole series is cancelled and there is nothing to keep.
        if let Some(rid) = rid {
            return Ok(Some(ParsedInput::Cancellation { uid, rid }));
        }
        debug!("skipping cancelled event {uid}");
        return Ok(None);
    }

    let summary = prop(ev, "SUMMARY")
        .and_then(|p| p.value.as_deref())
        .map(unescape)
        .unwrap_or_else(|| "(no title)".to_string());

    let (start, all_day) = {
        let p = prop(ev, "DTSTART").context("event without DTSTART")?;
        parse_datetime(
            p.value.as_deref().context("DTSTART without value")?,
            p.params.as_ref(),
            tz,
        )?
    };

    let end = match prop(ev, "DTEND") {
        Some(p) => {
            parse_datetime(
                p.value.as_deref().context("DTEND without value")?,
                p.params.as_ref(),
                tz,
            )?
            .0
        }
        None => {
            // RFC 5545: missing DTEND means zero duration (date-time form)
            // or one day (date-only form)
            if all_day {
                start + chrono::Duration::days(1)
            } else {
                start
            }
        }
    };

    let mut alarms = Vec::new();
    for a in &ev.alarms {
        match prop_in(&a.properties, "TRIGGER") {
            Some(t) => match t.value.as_deref() {
                Some(v) => match alarm::spec_from_trigger(v, t.params.as_ref(), tz) {
                    Ok(spec) => alarms.push(spec),
                    Err(e) => warn!("event {uid}: bad TRIGGER '{v}': {e:#}"),
                },
                None => warn!("event {uid}: TRIGGER without value"),
            },
            None => debug!("event {uid}: VALARM without TRIGGER, ignored"),
        }
    }

    match rid {
        Some(rid) => Ok(Some(ParsedInput::Override {
            uid,
            over: RecurOverride {
                recurrence_id: rid,
                start,
                end,
                all_day,
                summary,
                alarms,
            },
        })),
        None => {
            let rrule = prop(ev, "RRULE").and_then(|p| p.value.clone());
            let mut exdates = Vec::new();
            for p in props_all(ev, "EXDATE") {
                if let Some(v) = p.value.as_deref() {
                    for piece in v.split(',') {
                        let piece = piece.trim();
                        if piece.is_empty() {
                            continue;
                        }
                        match parse_datetime(piece, p.params.as_ref(), tz) {
                            Ok((dt, _)) => exdates.push(dt),
                            Err(e) => warn!("event {uid}: bad EXDATE '{piece}': {e:#}"),
                        }
                    }
                }
            }
            Ok(Some(ParsedInput::Master(EventSeries {
                uid,
                summary,
                start,
                end,
                all_day,
                rrule,
                exdates,
                cancelled: vec![],
                alarms,
                overrides: vec![],
            })))
        }
    }
}

/// Parse an ical DATE / DATE-TIME property value plus its params.
/// Returns (datetime, is_date_only). Supported forms:
///   20260319              (with VALUE=DATE)
///   20260319T130000       (floating = local tz)
///   ...;TZID=Europe/Prague:20260319T130000
///   20260319T110000Z      (UTC)
pub(crate) fn parse_datetime(
    value: &str,
    params: Option<&Vec<(String, Vec<String>)>>,
    local_tz: Tz,
) -> Result<(DateTime<Tz>, bool)> {
    if alarm::param_is(params, "VALUE", "DATE") {
        let digits: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
        let nd = NaiveDate::parse_from_str(&digits, "%Y%m%d")
            .with_context(|| format!("invalid DATE '{value}'"))?;
        let naive = nd.and_hms_opt(0, 0, 0).expect("00:00:00 is valid");
        let dt = resolve_local(local_tz, naive)?;
        return Ok((dt, true));
    }

    let is_utc = value.ends_with('Z') || value.ends_with('z');
    let compact: String = value
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == 'T' || *c == 't')
        .collect();
    let naive = NaiveDateTime::parse_from_str(&compact, "%Y%m%dT%H%M%S")
        .with_context(|| format!("invalid DATE-TIME '{value}'"))?;

    if is_utc {
        return Ok((
            Utc.from_utc_datetime(&naive).with_timezone(&local_tz),
            false,
        ));
    }
    if let Some(tzid) = alarm::param_value(params, "TZID") {
        match Tz::from_str(tzid) {
            Ok(tz) => return Ok((resolve_local(tz, naive)?, false)),
            Err(_) => warn!("unknown TZID '{tzid}', using local timezone"),
        }
    }
    Ok((resolve_local(local_tz, naive)?, false))
}

fn resolve_local(tz: Tz, naive: NaiveDateTime) -> Result<DateTime<Tz>> {
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => Ok(dt),
        // ambiguous (DST overlap): take the earlier interpretation
        LocalResult::Ambiguous(a, _) => Ok(a),
        LocalResult::None => bail!("local time {naive} does not exist in {tz} (DST gap)"),
    }
}

/// Unescape ical text values (\, \; \n \\).
pub(crate) fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn prop<'a>(ev: &'a IcalEvent, name: &str) -> Option<&'a Property> {
    ev.properties.iter().find(|p| p.name == name)
}

fn props_all<'a>(ev: &'a IcalEvent, name: &str) -> Vec<&'a Property> {
    ev.properties.iter().filter(|p| p.name == name).collect()
}

fn prop_in<'a>(props: &'a [Property], name: &str) -> Option<&'a Property> {
    props.iter().find(|p| p.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TZ: Tz = Tz::Europe__Prague;

    #[test]
    fn parses_date_only() {
        let params = vec![("VALUE".to_string(), vec!["DATE".to_string()])];
        let (dt, all_day) = parse_datetime("20270319", Some(&params), TZ).unwrap();
        assert!(all_day);
        assert_eq!(dt.to_rfc3339(), "2027-03-19T00:00:00+01:00");
    }

    #[test]
    fn parses_utc() {
        let (dt, all_day) = parse_datetime("20261015T110000Z", None, TZ).unwrap();
        assert!(!all_day);
        // Prague is CEST (+02:00) in October 2026
        assert_eq!(dt.to_rfc3339(), "2026-10-15T13:00:00+02:00");
    }

    #[test]
    fn parses_tzid_and_floating() {
        let params = vec![("TZID".to_string(), vec!["Europe/Prague".to_string()])];
        let (dt, _) = parse_datetime("20260107T190000", Some(&params), Tz::UTC).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-01-07T19:00:00+01:00");

        let (dt2, _) = parse_datetime("20260107T190000", None, TZ).unwrap();
        assert_eq!(dt2, dt);
    }

    #[test]
    fn unknown_tzid_falls_back_to_local() {
        let params = vec![("TZID".to_string(), vec!["Custom/Zone-99".to_string()])];
        let (dt, _) = parse_datetime("20260107T190000", Some(&params), TZ).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-01-07T19:00:00+01:00");
    }

    #[test]
    fn unescapes_text() {
        assert_eq!(unescape("POSS - cvi\\;eni"), "POSS - cvi;eni");
        assert_eq!(unescape("a\\,b\\nc\\\\d"), "a,b\nc\\d");
        assert_eq!(unescape("trailing\\"), "trailing\\");
    }
}
