use std::{collections::HashMap, path::PathBuf, time::Duration};

use anyhow::Context;
use chrono::{DateTime, Local, TimeDelta};
use tokio::signal::unix::{SignalKind, signal};
use tracing::{error, info, warn};

use crate::{
    calendar::{self, Alarm, Event},
    config::{CalendarConfig, Config},
    output,
    state::State,
};

/// How far ahead events are expanded.
const HORIZON: TimeDelta = TimeDelta::days(366);
/// Longest single sleep, so wall-clock jumps (suspend, clock changes) are noticed quickly.
const MAX_SLEEP: TimeDelta = TimeDelta::minutes(1);
/// Alarms emitted later than this after their time are flagged `late`.
const LATE_AFTER: TimeDelta = TimeDelta::minutes(1);

pub struct Daemon {
    config_path: PathBuf,
    config: Config,
    state_path: PathBuf,
    state: State,
    client: reqwest::Client,
    /// Occurrences by calendar name; kept when a refetch fails.
    events: HashMap<String, Vec<Event>>,
    next_fetch: DateTime<Local>,
}

impl Daemon {
    pub fn new(config_path: PathBuf, config: Config, state_path: PathBuf) -> anyhow::Result<Self> {
        Ok(Self {
            config_path,
            config,
            state: State::load(&state_path),
            state_path,
            client: http_client()?,
            events: HashMap::new(),
            next_fetch: Local::now(),
        })
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        let mut hup = signal(SignalKind::hangup())?;
        let mut usr1 = signal(SignalKind::user_defined1())?;
        let mut term = signal(SignalKind::terminate())?;
        let mut int = signal(SignalKind::interrupt())?;
        info!("started");
        loop {
            if Local::now() >= self.next_fetch {
                self.fetch().await;
            }
            self.fire_due()?;

            let now = Local::now();
            let wake = self.next_alarm(now).unwrap_or(self.next_fetch).min(self.next_fetch).min(now + MAX_SLEEP);
            tokio::select! {
                () = tokio::time::sleep((wake - now).to_std().unwrap_or_default()) => {}
                _ = hup.recv() => self.reload(),
                _ = usr1.recv() => self.next_fetch = now,
                _ = term.recv() => break,
                _ = int.recv() => break,
            }
        }
        info!("stopping");
        Ok(())
    }

    async fn fetch(&mut self) {
        let now = Local::now();
        for cal in &self.config.calendars {
            match fetch(&self.client, cal, &self.config, now).await {
                Ok(events) => {
                    info!(calendar = %cal.name, events = events.len(), "fetched");
                    self.events.insert(cal.name.clone(), events);
                }
                Err(e) => warn!(calendar = %cal.name, "fetch failed, keeping previous events: {e:#}"),
            }
        }
        self.next_fetch = now + self.config.fetch_period;
    }

    /// Emits every alarm that is due and not yet emitted, oldest first.
    fn fire_due(&mut self) -> anyhow::Result<()> {
        let now = Local::now();
        let grace = TimeDelta::from_std(self.config.late_grace)?;
        let mut due: Vec<(&Event, &Alarm)> = self
            .events
            .values()
            .flatten()
            .flat_map(|e| e.alarms.iter().map(move |a| (e, a)))
            .filter(|&(e, a)| a.at <= now && !expired(e, a, now, grace) && !self.state.is_fired(&e.alarm_key(a)))
            .collect();
        if due.is_empty() {
            return Ok(());
        }
        due.sort_by_key(|(_, a)| a.at);
        for (event, alarm) in due {
            output::alarm(event, alarm, now, now - alarm.at > LATE_AFTER)?;
            self.state.mark_fired(event.alarm_key(alarm), alarm.at);
        }
        self.state.prune(now - grace - TimeDelta::days(1));
        if let Err(e) = self.state.save(&self.state_path) {
            error!("failed to save state: {e:#}");
        }
        Ok(())
    }

    fn next_alarm(&self, now: DateTime<Local>) -> Option<DateTime<Local>> {
        self.events.values().flatten().flat_map(|e| &e.alarms).map(|a| a.at).filter(|&at| at > now).min()
    }

    fn reload(&mut self) {
        match Config::load(&self.config_path) {
            Ok(config) => {
                self.events.retain(|name, _| config.calendars.iter().any(|c| &c.name == name));
                self.config = config;
                self.next_fetch = Local::now();
                info!("config reloaded");
            }
            Err(e) => error!("reload failed, keeping current config: {e:#}"),
        }
    }
}

/// Fetches all calendars once and prints the `n` nearest events (running ones first).
pub async fn once(config: &Config, n: usize) -> anyhow::Result<()> {
    let (client, now) = (http_client()?, Local::now());
    let mut events = Vec::new();
    let mut fetched_any = config.calendars.is_empty();
    for cal in &config.calendars {
        match fetch(&client, cal, config, now).await {
            Ok(evs) => {
                events.extend(evs);
                fetched_any = true;
            }
            Err(e) => warn!(calendar = %cal.name, "fetch failed: {e:#}"),
        }
    }
    anyhow::ensure!(fetched_any, "no calendar could be fetched");
    events.retain(|e| e.end > now);
    events.sort_by_key(|e| e.start);
    for event in events.iter().take(n) {
        output::event(event, now)?;
    }
    Ok(())
}

async fn fetch(
    client: &reqwest::Client,
    cal: &CalendarConfig,
    config: &Config,
    now: DateTime<Local>,
) -> anyhow::Result<Vec<Event>> {
    let body = client.get(&cal.url).send().await?.error_for_status()?.text().await?;
    let from = now - TimeDelta::from_std(config.late_grace)?;
    calendar::events(cal, config.force_default_reminders, &body, from, now + HORIZON)
}

fn http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("building HTTP client")
}

/// Missed alarms are emitted up to `grace` late; all-day ones for the rest of their day.
fn expired(event: &Event, alarm: &Alarm, now: DateTime<Local>, grace: TimeDelta) -> bool {
    now - alarm.at > grace && !(event.all_day && alarm.at.date_naive() == now.date_naive())
}
