//! JSON lines on stdout: the daemon's only interface to consumers.

use std::io::{self, Write};

use chrono::{DateTime, Local};
use serde::Serialize;

use crate::calendar::{Alarm, Event, Source};

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Line<'a> {
    Alarm {
        #[serde(flatten)]
        event: &'a Event,
        #[serde(flatten)]
        remaining: Remaining,
        alarm_at: DateTime<Local>,
        offset_secs: i64,
        source: Source,
        late: bool,
    },
    Event {
        #[serde(flatten)]
        event: &'a Event,
        #[serde(flatten)]
        remaining: Remaining,
    },
}

#[derive(Serialize)]
struct Remaining {
    remaining: String,
    remaining_secs: i64,
}

impl Remaining {
    fn until(event: &Event, now: DateTime<Local>) -> Self {
        let secs = (event.start - now).num_seconds();
        Self { remaining: format_remaining(secs), remaining_secs: secs }
    }
}

pub fn alarm(event: &Event, alarm: &Alarm, now: DateTime<Local>, late: bool) -> io::Result<()> {
    write(&Line::Alarm {
        event,
        remaining: Remaining::until(event, now),
        alarm_at: alarm.at,
        offset_secs: (event.start - alarm.at).num_seconds(),
        source: alarm.source,
        late,
    })
}

pub fn event(event: &Event, now: DateTime<Local>) -> io::Result<()> {
    write(&Line::Event { event, remaining: Remaining::until(event, now) })
}

fn write(line: &Line) -> io::Result<()> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, line)?;
    out.write_all(b"\n")?;
    out.flush()
}

/// `"2 d 3 h 15 min"`, leading zero units omitted; `"now"` once the event has started.
fn format_remaining(secs: i64) -> String {
    if secs <= 0 {
        return "now".into();
    }
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3_600, secs % 3_600 / 60);
    match (d, h) {
        (0, 0) => format!("{m} min"),
        (0, _) => format!("{h} h {m} min"),
        _ => format!("{d} d {h} h {m} min"),
    }
}

#[cfg(test)]
mod tests {
    use super::format_remaining;

    #[test]
    fn remaining() {
        assert_eq!(format_remaining(-5), "now");
        assert_eq!(format_remaining(59), "0 min");
        assert_eq!(format_remaining(3_600), "1 h 0 min");
        assert_eq!(format_remaining(2 * 86_400 + 5 * 60), "2 d 0 h 5 min");
        assert_eq!(format_remaining(184_500), "2 d 3 h 15 min");
    }
}
