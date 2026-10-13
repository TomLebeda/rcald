use std::cmp;
use std::thread;
use std::time::{Duration as StdDuration, Instant};

use anyhow::Result;
use chrono::Utc;
use chrono_tz::Tz;
use log::{debug, info, warn};

use crate::config::RuntimeConfig;
use crate::emit;
use crate::ics;
use crate::schedule::{self, CalendarState, FiredSet};

/// Long-running loop: fetch calendars periodically, sleep until the nearest
/// reminder trigger, emit reminders as JSON lines on stdout.
pub fn run(cfg: &RuntimeConfig, local_tz: Tz) -> Result<()> {
    let agent = ics::make_agent();
    let mut states = CalendarState::from_configs(cfg);
    let mut fired: FiredSet = FiredSet::new();

    refresh_all(&agent, &mut states, local_tz);
    let mut last_fetch = Instant::now();

    loop {
        let now = Utc::now();
        // horizon = now: only triggers already due are emitted this round.
        let (due, next_trigger) = schedule::collect(&states, now, now, &fired);
        for r in due {
            emit::emit(&r);
            fired.insert(r.key());
        }

        // Sleep until the earlier of the next scheduled refetch and the next
        // reminder trigger; the loop above runs again on every wake-up.
        let fetch_in = cfg
            .fetch_period
            .checked_sub(last_fetch.elapsed())
            .unwrap_or(StdDuration::ZERO);
        let next_in = next_trigger
            .and_then(|t| (t - now).to_std().ok())
            .unwrap_or(fetch_in);
        let sleep_for = cmp::min(fetch_in, next_in);

        debug!(
            "sleeping {:?} (next trigger in {:?}, next fetch in {:?})",
            sleep_for, next_in, fetch_in
        );
        thread::sleep(sleep_for);

        if last_fetch.elapsed() >= cfg.fetch_period {
            refresh_all(&agent, &mut states, local_tz);
            last_fetch = Instant::now();
        }
    }
}

/// Re-fetch every calendar, logging individual failures. A failed refresh
/// keeps the previously fetched data (see `ics::refresh_state`), so a flaky
/// network never blanks the schedule. Shared by the daemon and `--once`.
pub fn refresh_all(agent: &ureq::Agent, states: &mut [CalendarState], local_tz: Tz) {
    info!("refreshing {} calendar(s)", states.len());
    for st in states.iter_mut() {
        if let Err(e) = ics::refresh_state(st, agent, local_tz) {
            warn!("{e:#}");
        }
    }
}
