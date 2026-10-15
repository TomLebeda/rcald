# rcald

A small Linux daemon that watches iCalendar (`.ics`) feeds and prints a JSON line on stdout whenever an alarm is due.
It doesn't show notifications itself. Pipe its output into your own scripts.

```sh
rcald | while read -r line; do
  notify-send "$(jq -r .title <<<"$line")" "$(jq -r '"in " + .remaining' <<<"$line")"
done
```

It only reads calendars, so a Google Calendar "secret address in iCal format" (or any public `.ics` URL) is enough.

## Build

```sh
cargo build --release   # → target/release/rcald
```

## Usage

```
rcald [-c CONFIG] [--state FILE] [-v]   # run as a daemon
rcald --once [N]                        # fetch once, print the N nearest events (default 10), exit
```

- Config: `$XDG_CONFIG_HOME/rcald/config.toml` by default, or `-c path`. `rcald.toml` in the repo root is git-ignored for a local config with private calendar URLs:

  ```toml
  fetch_period = "30m"             # how often calendars are re-downloaded
  late_grace = "1h"                # missed alarms younger than this are still emitted, with "late": true
  force_default_reminders = false  # add the offsets even to events that have their own alarms

  [[calendar]]
  name = "Personal"
  url = "https://calendar.google.com/calendar/ical/…/basic.ics"
  offsets = ["20m", "1h", "1d"]    # timed events: before the start
  all_day_offsets = ["1d"]         # all-day events: before local midnight of the first day
  ```
- State: `$XDG_STATE_HOME/rcald/state.json` remembers emitted alarms, so a restart never repeats them.
- stdout carries only JSON lines; logs go to stderr.
- Signals: `SIGHUP` reloads the config, `SIGUSR1` fetches now, `SIGTERM`/`SIGINT` exit.

## Output

One JSON object per line. Times are RFC 3339 in local time.

```json
{"type":"alarm","calendar":"Personal","event_uid":"abc@google.com","title":"Dentist","location":null,
 "start":"2026-10-14T09:00:00+02:00","end":"2026-10-14T10:00:00+02:00","all_day":false,
 "remaining":"30 min","remaining_secs":1800,
 "alarm_at":"2026-10-14T08:30:00+02:00","offset_secs":1800,"source":"ical","late":false}
```

(Shown wrapped here; real output is a single line.)

| field | meaning |
|---|---|
| `type` | `alarm` from the daemon, `event` from `--once` (which has no alarm fields) |
| `remaining` | time until start: `2 d 3 h 15 min`, `3 h 0 min`, `15 min`, or `now` once started |
| `remaining_secs` | the same in seconds, negative once started |
| `offset_secs` | how long before the start the alarm is set |
| `source` | `ical` = alarm from the calendar, `default` = offset from the config |
| `late` | the alarm was missed (suspend, daemon not running) and is emitted afterwards |

## Alarms

- Alarms defined in the calendar (`VALARM`) are used as they are, including ones relative to the end and ones at a fixed time.
- The config offsets are added when an event has no alarms of its own, or always with `force_default_reminders = true`.
- All-day offsets count back from local midnight of the event's first day.
- An alarm missed by less than `late_grace` is still emitted, flagged `late`. A missed all-day alarm is still emitted any time on its day.
- Recurring events support `RRULE`, `RDATE`, `EXDATE` and individually edited occurrences (`RECURRENCE-ID`).
