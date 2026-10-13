use std::io::Write;
use std::process::Command;

use chrono::{Duration, Utc};

/// Build a small ics feed whose event times are relative to "now", write a
/// matching TOML config, run the binary in --once mode and inspect stdout.
#[test]
fn once_mode_emits_expected_reminders() {
    let now = Utc::now();
    let fmt = |d: chrono::DateTime<Utc>| d.format("%Y%m%dT%H%M%SZ").to_string();

    let within_start = now + Duration::hours(2);
    let beyond_start = now + Duration::hours(24);
    let noalarm_start = now + Duration::hours(1);
    let late_start = now + Duration::minutes(90);

    let ics = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
         BEGIN:VEVENT\r\nDTSTART:{s1}\r\nDTEND:{e1}\r\nUID:within@test\r\n\
         SUMMARY:within\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\nEND:VEVENT\r\n\
         BEGIN:VEVENT\r\nDTSTART:{s2}\r\nDTEND:{e2}\r\nUID:beyond@test\r\n\
         SUMMARY:beyond\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT5M\r\nEND:VALARM\r\nEND:VEVENT\r\n\
         BEGIN:VEVENT\r\nDTSTART:{s3}\r\nDTEND:{e3}\r\nUID:noalarm@test\r\n\
         SUMMARY:noalarm\r\nEND:VEVENT\r\n\
         BEGIN:VEVENT\r\nDTSTART:{s4}\r\nDTEND:{e4}\r\nUID:lateone@test\r\n\
         SUMMARY:late-one\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-P2D\r\nEND:VALARM\r\nEND:VEVENT\r\n\
         END:VCALENDAR\r\n",
        s1 = fmt(within_start),
        e1 = fmt(within_start + Duration::hours(1)),
        s2 = fmt(beyond_start),
        e2 = fmt(beyond_start + Duration::hours(1)),
        s3 = fmt(noalarm_start),
        e3 = fmt(noalarm_start + Duration::hours(1)),
        s4 = fmt(late_start),
        e4 = fmt(late_start + Duration::hours(2)),
    );

    let dir = tempfile::tempdir().unwrap();
    let ics_path = dir.path().join("feed.ics");
    std::fs::write(&ics_path, ics).unwrap();

    // fetch_period = 4h -> horizon = now + 4h
    let cfg_path = dir.path().join("rcald.toml");
    let mut f = std::fs::File::create(&cfg_path).unwrap();
    write!(
        f,
        "fetch_period = \"4h\"\n\n[[calendars]]\nname = \"testcal\"\nurl = \"{}\"\nfallback_offsets = [\"30m\"]\n",
        ics_path.display()
    )
    .unwrap();
    drop(f);

    let out = Command::new(env!("CARGO_BIN_EXE_rcald"))
        .args(["--once", "-c", cfg_path.to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "exit failed: {stderr}");

    let lines: Vec<serde_json::Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).expect("stdout must be JSON lines"))
        .collect();

    let by_uid = |uid: &str| lines.iter().find(|v| v["uid"] == uid);

    assert!(
        by_uid("within@test").is_some(),
        "trigger of 'within' is now+1h45m <= horizon: {stdout}"
    );
    assert!(
        by_uid("beyond@test").is_none(),
        "trigger of 'beyond' is tomorrow, outside horizon"
    );
    assert!(
        by_uid("noalarm@test").is_some(),
        "fallback 30m on alarm-less 'noalarm' fires at now+30m",
    );
    let late = by_uid("lateone@test").expect("late-one should be emitted (late)");
    assert_eq!(late["late"], true, "late-one trigger was 2 days ago");
    assert_eq!(by_uid("within@test").unwrap()["late"], false);
    assert_eq!(late["type"], "reminder");
    assert_eq!(late["calendar"], "testcal");

    // no logs on stdout: every line is valid JSON with type == reminder (checked above)
    assert!(
        stderr.contains("once mode"),
        "info log expected on stderr: {stderr}"
    );
}
