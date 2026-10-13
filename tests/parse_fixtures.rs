use chrono::{Duration, TimeZone};
use chrono_tz::Tz;
use rcald::{ics, recur};

const P: Tz = Tz::Europe__Prague;

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn valarm_allday_event() {
    let text = fixture("valarm_allday.ics");
    let series = ics::parse_calendar(&text, P).unwrap();
    assert_eq!(series.len(), 1);
    let s = &series[0];
    assert!(s.all_day);
    assert_eq!(s.summary, "konec platnosti Plzeňky za 1 měsíc!");
    assert_eq!(s.start.to_rfc3339(), "2027-01-04T00:00:00+01:00");
    assert_eq!(s.end.to_rfc3339(), "2027-01-05T00:00:00+01:00");
    assert_eq!(s.alarms.len(), 3);

    // first trigger: -P0DT15H0M0S -> day before at 09:00
    let t = s.alarms[0].trigger(&s.start, &s.end);
    assert_eq!(t.to_rfc3339(), "2027-01-03T09:00:00+01:00");
}

#[test]
fn recurrence_merging() {
    let text = fixture("recurrence.ics");
    let series = ics::parse_calendar(&text, P).unwrap();
    assert_eq!(series.len(), 1);
    let s = &series[0];
    assert_eq!(s.exdates.len(), 2);
    assert_eq!(s.overrides.len(), 2);
    assert_eq!(s.cancelled.len(), 1);
    assert!(s.rrule.is_some());

    // expand from a fixed point; the series is weekly on Wednesday
    let from = P.with_ymd_and_hms(2026, 1, 5, 10, 0, 0).unwrap();
    let occs = recur::occurrences(s, from, 5);
    let starts: Vec<String> = occs.iter().map(|o| o.start.to_rfc3339()).collect();

    // 2026-01-07 and 2026-01-14 are EXDATE'd, 2026-02-11 cancelled
    assert!(!starts.iter().any(|d| d.starts_with("2026-01-07")));
    assert!(!starts.iter().any(|d| d.starts_with("2026-01-14")));
    assert!(!starts.iter().any(|d| d.starts_with("2026-02-11")));

    // moved instance: 2026-01-21 at 20:00 with changed title and its own alarm
    let moved = occs
        .iter()
        .find(|o| o.start.to_rfc3339().starts_with("2026-01-21"))
        .unwrap();
    assert_eq!(moved.start.to_rfc3339(), "2026-01-21T20:00:00+01:00");
    assert_eq!(moved.summary, "Plavání s K.");
    assert_eq!(moved.alarms.len(), 1, "override keeps its own alarm");

    // same-time override replaces the 2026-01-28 slot
    assert!(starts.iter().any(|d| d == "2026-01-28T19:00:00+01:00"));

    // regular next occurrence after overrides
    assert!(starts.iter().any(|d| d == "2026-02-04T19:00:00+01:00"));
}

#[test]
fn plain_fixture_shapes() {
    let text = fixture("plain.ics");
    let series = ics::parse_calendar(&text, P).unwrap();
    assert_eq!(series.len(), 4);

    let folded = series
        .iter()
        .find(|s| s.uid == "folded-escaped@local")
        .unwrap();
    assert!(folded.summary.contains("seventy five character mark"));
    assert!(folded.summary.contains("semicolon;comma,"));
    assert!(folded.summary.contains("a line break marker"));
    assert_eq!(folded.start.to_rfc3339(), "2027-01-01T11:00:00+01:00");

    let floating = series
        .iter()
        .find(|s| s.uid == "floating-single@local")
        .unwrap();
    assert!(!floating.all_day);
    assert_eq!(floating.start.to_rfc3339(), "2026-10-15T18:30:00+02:00");

    let blik = series.iter().find(|s| s.summary == "Blik-Blik").unwrap();
    assert!(blik.all_day);
    assert_eq!(blik.end - blik.start, Duration::days(3));
}
