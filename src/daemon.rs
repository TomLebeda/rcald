use std::cmp;
use std::thread;
use std::time::Duration as StdDuration;

use anyhow::{Context, Result};
use chrono::Utc;
use chrono_tz::Tz;
use log::{debug, info, warn};

use crate::config::RuntimeConfig;
use crate::emit;
use crate::ics;
use crate::schedule::{self, CalendarState, FiredSet};

/// Upper bound for one sleep. `thread::sleep` runs on the monotonic clock,
/// which stops while the machine is suspended, so a long sleep could overshoot
/// a trigger by the whole suspend time. Waking up regularly and re-checking
/// the wall clock bounds that error.
const MAX_SLEEP: StdDuration = StdDuration::from_secs(60);

/// Long-running loop: fetch calendars periodically, sleep until the nearest
/// reminder trigger, emit reminders as JSON lines on stdout.
pub fn run(cfg: &RuntimeConfig, local_tz: Tz) -> Result<()> {
    let agent = ics::make_agent();
    let mut states = CalendarState::from_configs(cfg);
    let mut fired: FiredSet = FiredSet::new();

    // Fetch times are tracked on the wall clock too, so a suspend counts
    // towards the fetch period.
    let fetch_period =
        chrono::Duration::from_std(cfg.fetch_period).context("fetch_period too large")?;
    refresh_all(&agent, &mut states, local_tz);
    let mut next_fetch = Utc::now() + fetch_period;

    loop {
        let now = Utc::now();
        // horizon = now: only triggers already due are emitted this round.
        let (due, next_trigger) = schedule::collect(&states, now, now, &fired);
        for r in due {
            emit::emit(&r);
            schedule::mark_fired(&mut fired, &r);
        }
        schedule::prune_fired(&mut fired, now);

        // Sleep until the earliest of the next scheduled refetch, the next
        // reminder trigger and MAX_SLEEP; the loop above runs again on every
        // wake-up.
        let fetch_in = (next_fetch - now).to_std().unwrap_or(StdDuration::ZERO);
        let next_in = next_trigger
            .and_then(|t| (t - now).to_std().ok())
            .unwrap_or(fetch_in);
        let sleep_for = cmp::min(cmp::min(fetch_in, next_in), MAX_SLEEP);

        debug!(
            "sleeping {:?} (next trigger in {:?}, next fetch in {:?})",
            sleep_for, next_in, fetch_in
        );
        thread::sleep(sleep_for);

        if Utc::now() >= next_fetch {
            refresh_all(&agent, &mut states, local_tz);
            next_fetch = Utc::now() + fetch_period;
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
