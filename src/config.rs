use std::{collections::HashSet, path::Path, time::Duration};

use anyhow::Context;
use serde::{Deserialize, Deserializer, de::Error};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_fetch_period", deserialize_with = "duration")]
    pub fetch_period: Duration,
    /// How long after its time a missed alarm is still emitted (flagged `late`).
    #[serde(default = "default_late_grace", deserialize_with = "duration")]
    pub late_grace: Duration,
    /// Add the configured offsets even to events that carry their own alarms.
    #[serde(default)]
    pub force_default_reminders: bool,
    #[serde(rename = "calendar")]
    pub calendars: Vec<CalendarConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarConfig {
    pub name: String,
    pub url: String,
    /// Default alarms for timed events, as offsets before the start.
    #[serde(default, deserialize_with = "durations")]
    pub offsets: Vec<Duration>,
    /// Default alarms for all-day events, as offsets before local midnight of the first day.
    #[serde(default, deserialize_with = "durations")]
    pub all_day_offsets: Vec<Duration>,
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let config: Self = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let mut names = HashSet::new();
        if let Some(dup) = config.calendars.iter().find(|c| !names.insert(&c.name)) {
            anyhow::bail!("duplicate calendar name {:?}", dup.name);
        }
        Ok(config)
    }
}

fn default_fetch_period() -> Duration {
    Duration::from_mins(30)
}

fn default_late_grace() -> Duration {
    Duration::from_hours(1)
}

fn duration<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    humantime::parse_duration(&String::deserialize(d)?).map_err(D::Error::custom)
}

fn durations<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Duration>, D::Error> {
    Vec::<String>::deserialize(d)?.iter().map(|s| humantime::parse_duration(s).map_err(D::Error::custom)).collect()
}
