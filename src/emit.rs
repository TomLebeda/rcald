use std::io::{self, Write};
use std::sync::OnceLock;

use chrono::{DateTime, Offset, SecondsFormat};
use chrono_tz::Tz;
use log::{debug, error, info};
use serde::Serialize;

use crate::model::Reminder;

#[derive(Serialize)]
struct Line<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    calendar: &'a str,
    uid: &'a str,
    summary: &'a str,
    start: String,
    end: String,
    all_day: bool,
    trigger: String,
    lead_seconds: i64,
    late: bool,
}

/// Emit one reminder as a JSON line on stdout. Exits the process if stdout is
/// broken (dead consumer), so supervision can restart us instead of writing
/// into the void forever.
pub fn emit(r: &Reminder) {
    let line = Line {
        kind: "reminder",
        calendar: &r.calendar,
        uid: &r.uid,
        summary: &r.summary,
        start: fmt(&r.start),
        end: fmt(&r.end),
        all_day: r.all_day,
        trigger: fmt(&r.trigger),
        lead_seconds: (r.start - r.trigger).num_seconds(),
        late: r.late,
    };
    let json = serde_json::to_string(&line).expect("reminder serialization cannot fail");
    debug!("emit reminder for '{}' (late={})", r.summary, r.late);
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    if writeln!(lock, "{json}").and_then(|_| lock.flush()).is_err() {
        error!("cannot write to stdout (broken pipe?), exiting");
        std::process::exit(1);
    }
    info!("reminder emitted: '{}' at {}", r.summary, line.trigger);
}

/// RFC 3339 with a guaranteed numeric `+hh:mm` offset. Converting to
/// `FixedOffset` first keeps the output format independent of how `Tz`
/// chooses to display its offset.
///
/// Times kept in UTC (feed values with a `Z` suffix, see `ics::parse_datetime`)
/// are shown in the system timezone, which is what the user reads.
fn fmt(dt: &DateTime<Tz>) -> String {
    static LOCAL_TZ: OnceLock<Tz> = OnceLock::new();
    let dt = if dt.timezone() == Tz::UTC {
        dt.with_timezone(LOCAL_TZ.get_or_init(crate::ics::local_tz))
    } else {
        *dt
    };
    dt.with_timezone(&dt.offset().fix())
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn formats_rfc3339_with_colon_offset() {
        let dt = Tz::Europe__Prague
            .with_ymd_and_hms(2026, 10, 15, 20, 0, 0)
            .unwrap();
        assert_eq!(fmt(&dt), "2026-10-15T20:00:00+02:00");
        let winter = Tz::Europe__Prague
            .with_ymd_and_hms(2026, 1, 7, 19, 0, 0)
            .unwrap();
        assert_eq!(fmt(&winter), "2026-01-07T19:00:00+01:00");
    }
}
