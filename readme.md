# rcald

Lightweight calendar reminder daemon. It watches online calendars in iCal (ics)
format and emits reminders as JSON lines on stdout, so you can route them
however you like: pipe to a notification script, journal, TUI, anything.

Replaces my old Go daemon [gcald](../calendar_daemon). No systray, no desktop
integration, no built-in notifications; just a well-behaved CLI process.

## How it works

- On startup (and then every `fetch_period`) all configured calendars are
  downloaded and parsed.
- Reminders come from VALARM blocks in the ics feed. If an event has no
  VALARMs, per-calendar `fallback_offsets` are used instead. Fallbacks never
  override real VALARMs.
- Recurring events are expanded properly: `RRULE` + `EXDATE` exclusions,
  moved/cancelled instances (`RECURRENCE-ID`, `STATUS:CANCELLED`), alarms
  inherited from the series master unless the instance defines its own.
- All-day and multi-day events (`VALUE=DATE`) are supported.
- The daemon sleeps until the nearest trigger or the next fetch (waking at
  least once a minute, so suspend/resume cannot delay reminders), then emits.
- Reminders whose trigger passed while the daemon was down (but whose event
  has not ended yet) fire immediately at startup with `"late": true`.
- State is in-memory only: restart = re-evaluate. Given the late-fire rule
  above, restarts do not lose upcoming reminders.
- Logs go to stderr (plain text), reminders go to stdout (JSON lines), so
  piping stdout is always safe.

## Build

```sh
cargo build --release
```

## Usage

```sh
rcald [-c <config.toml>] [-v|-vv] [--once]
```

- `-c, --config <path>`: TOML config path (default `./rcald.toml`).
- `-v`: debug logs, `-vv`: trace.
- `--once`: fetch all calendars, emit every reminder due in the window
  `[past, now + fetch_period]` (overdue ones marked `"late": true`), then
  exit. Ideal for cron or a systemd timer instead of the long-running daemon.

## Configuration

See [rcald.toml.example](rcald.toml.example). Structure:

```toml
fetch_period = "30m"

[[calendars]]
name = "personal"
url = "https://calendar.google.com/calendar/ical/.../basic.ics"
fallback_offsets = ["20m", "1h", "24h"]          # used only for events without VALARMs
fallback_all_day_offsets = ["24h", "48h"]
```

- `fetch_period`: how often calendars are re-fetched; humantime format
  (`"30m"`, `"1h"`). Also the emission horizon in `--once` mode.
- `url`: an `http(s)://` or `webcal://` ICS URL (read-only secret URL works,
  no auth needed), or a plain local file path (handy for tests).
- `fallback_offsets`: reminder times before event start, humantime format
  (`"15m"`, `"1h30m"`, `"24h"`, `"7d"`). Applied only when the event has no
  alarms of its own.
- `fallback_all_day_offsets`: same, but for all-day events.

Changes require a restart (or a rerun of `--once`), there is no config reload
or runtime interface by design.

## Output format

One JSON object per line on stdout:

```json
{"type":"reminder","calendar":"sporty","uid":"6kou7g27fub4ohksjpc7ldnpqd@google.com","summary":"Plavání s K.","start":"2026-01-21T20:00:00+01:00","end":"2026-01-21T21:00:00+01:00","all_day":false,"trigger":"2026-01-21T19:45:00+01:00","lead_seconds":900,"late":false}
```

- `start`/`end`/`trigger`: RFC 3339 with numeric offset, in the event's
  timezone (times the feed gives in UTC are shown in the system timezone).
  For all-day events these are local midnights and `all_day` is true; `end`
  is exclusive (per ical spec).
- `lead_seconds`: `start - trigger` in seconds.
- `late`: the trigger time already passed (daemon restart, sleep, catch-up);
  downstream may choose to ignore or down-rank these.
- `uid` + `start` identifies an event instance; `uid` + `start` + `trigger`
  identifies a reminder (useful for downstream dedup).

## systemd examples

Daemon service:

```ini
[Unit]
Description=rcald calendar reminder daemon
After=network-online.target

[Service]
ExecStart=/usr/local/bin/rcald -c %h/.config/rcald/rcald.toml
Restart=on-failure

[Install]
WantedBy=default.target
```

One-shot timer variant (`--once` every 30 minutes, matching `fetch_period`):

```ini
[Unit]
Description=rcald one-shot calendar check

[Service]
Type=oneshot
ExecStart=/usr/local/bin/rcald --once -c %h/.config/rcald/rcald.toml
```

```ini
[Unit]
Description=run rcald --once periodically

[Timer]
OnCalendar=*:0/30
Persistent=true

[Install]
WantedBy=timers.target
```

## Notes on calendar compatibility

Built and tested against Google Calendar ICS exports. Reminders you set in the
Google Calendar GUI are exported as VALARMs only when set **explicitly per
event**; calendar-level default notifications are never exported and only the
fallback offsets cover those events. This is a Google limitation, not a bug.

## Development

Tests: `cargo test` (unit + fixture-based + one CLI integration test, no
network access needed). Fixtures in `tests/fixtures/` are trimmed-down real
Google Calendar exports.
