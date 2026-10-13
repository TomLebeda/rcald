# rcald - agent notes

Rust rewrite of the Go daemon `gcald` (../calendar_daemon). Single binary,
CLI-only, no desktop dependencies.

## Invariants (do not break)

- **stdout is a machine interface**: one JSON line per reminder (JSONL), flushed
  immediately. Never println!/print! anything else to stdout. All logs go to
  stderr via the `log` facade (env_logger, `Target::Stderr`).
- **VALARMs always win**: fallback offsets from TOML apply only to events with
  zero VALARMs. Never merge both.
- **Late-fire rule**: emit reminders with `trigger <= now < event.end` even if
  the process just started; mark them `"late": true` when emitted >90s after
  trigger.
- **In-memory state only**: dedup via `FiredSet` (uid, occurrence start,
  trigger). No state files on disk.
- chrono discipline: aware `DateTime<Tz>` everywhere, no naive types in domain
  logic, no `Local::now()` (use `Utc::now()` + `chrono_tz`). rrule<->chrono_tz
  conversions go through UTC (see `recur.rs`).
- Cross-timezone `DateTime` subtraction is not implemented by chrono; convert
  to a common tz before subtraction/comparison of diffs.

## Build / test

```sh
cargo build
cargo test          # 23 tests, no network needed
cargo clippy --all-targets
```

Integration test `tests/cli_once.rs` runs the binary with a generated ICS feed
(local file URL = test feature, not a bug). Fixtures in `tests/fixtures/` are
trimmed real Google exports; keep them small.

## Layout

- `main.rs`: clap args (+ `--once`, `-v`), init, dispatch
- `config.rs`: TOML load + validation (humantime durations)
- `ics.rs`: fetch (ureq/https or local file), ical parse into `EventSeries`
  (masters, RECURRENCE-ID overrides, STATUS:CANCELLED cancellations)
- `alarm.rs`: RFC 5545 DURATION parser + TRIGGER handling (RELATED=END,
  absolute DATE-TIME)
- `recur.rs`: rrule expansion (the only rrule/chrono_tz boundary)
- `schedule.rs`: fallback application, due/next-trigger computation, late flag
- `emit.rs`: JSONL writer; on broken stdout logs and exits(1) (supervision
  restarts it)
- `daemon.rs`: sleep-until-trigger-or-fetch loop; fetch failure keeps old data

## Config schema

`rcald.toml.example` is the reference. `rcald.toml` (real private URLs) is
gitignored on purpose; do not commit it.
