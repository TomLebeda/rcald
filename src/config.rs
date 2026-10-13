use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct FileConfig {
    fetch_period: String,
    #[serde(default)]
    calendars: Vec<FileCalendar>,
}

#[derive(Debug, Deserialize)]
struct FileCalendar {
    name: String,
    url: String,
    #[serde(default)]
    fallback_offsets: Vec<String>,
    #[serde(default)]
    fallback_all_day_offsets: Vec<String>,
}

/// Validated config with durations parsed.
pub struct RuntimeConfig {
    pub fetch_period: Duration,
    pub calendars: Vec<RuntimeCalendar>,
}

pub struct RuntimeCalendar {
    pub name: String,
    pub url: String,
    pub fallback_offsets: Vec<Duration>,
    pub fallback_all_day_offsets: Vec<Duration>,
}

pub fn load(path: &Path) -> Result<RuntimeConfig> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read config {}", path.display()))?;
    let cfg: FileConfig =
        toml::from_str(&raw).with_context(|| format!("cannot parse {}", path.display()))?;

    let fetch_period = humantime::parse_duration(&cfg.fetch_period)
        .with_context(|| format!("invalid fetch_period '{}'", cfg.fetch_period))?;

    let mut calendars = Vec::new();
    for c in cfg.calendars {
        let fallback_offsets = parse_durations(&c.fallback_offsets)
            .with_context(|| format!("calendar '{}': invalid fallback_offsets", c.name))?;
        let fallback_all_day_offsets = parse_durations(&c.fallback_all_day_offsets)
            .with_context(|| format!("calendar '{}': invalid fallback_all_day_offsets", c.name))?;
        calendars.push(RuntimeCalendar {
            name: c.name,
            url: c.url,
            fallback_offsets,
            fallback_all_day_offsets,
        });
    }

    if calendars.is_empty() {
        log::warn!("config contains no calendars");
    }

    Ok(RuntimeConfig {
        fetch_period,
        calendars,
    })
}

fn parse_durations(list: &[String]) -> Result<Vec<Duration>> {
    list.iter()
        .map(|s| humantime::parse_duration(s).with_context(|| format!("bad duration '{s}'")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_valid_config() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            r#"
fetch_period = "30m"

[[calendars]]
name = "personal"
url = "https://example.com/basic.ics"
fallback_offsets = ["20m", "1h", "24h"]
fallback_all_day_offsets = ["24h", "48h"]

[[calendars]]
name = "other"
url = "file.ics"
"#,
        )
        .unwrap();
        let cfg = load(f.path()).unwrap();
        assert_eq!(cfg.fetch_period, Duration::from_secs(30 * 60));
        assert_eq!(cfg.calendars.len(), 2);
        assert_eq!(cfg.calendars[0].name, "personal");
        assert_eq!(
            cfg.calendars[0].fallback_offsets,
            vec![
                Duration::from_secs(20 * 60),
                Duration::from_secs(3600),
                Duration::from_secs(24 * 3600)
            ]
        );
        assert!(cfg.calendars[1].fallback_offsets.is_empty());
    }

    #[test]
    fn rejects_bad_duration() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "fetch_period = \"soon\"").unwrap();
        assert!(load(f.path()).is_err());
    }
}
