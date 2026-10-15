//! Alarms already emitted, persisted so a restart neither repeats nor loses them.

use std::{collections::HashMap, fs, io, path::Path};

use anyhow::Context;
use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;

#[derive(Default, Serialize, Deserialize)]
pub struct State {
    /// Alarm key → alarm time.
    fired: HashMap<String, DateTime<Utc>>,
}

impl State {
    pub fn load(path: &Path) -> Self {
        let parsed = match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(anyhow::Error::from),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Self::default(),
            Err(e) => Err(e.into()),
        };
        parsed.unwrap_or_else(|e| {
            warn!("ignoring state file {}: {e}", path.display());
            Self::default()
        })
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, serde_json::to_vec(self)?)?;
        fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
    }

    pub fn is_fired(&self, key: &str) -> bool {
        self.fired.contains_key(key)
    }

    pub fn mark_fired(&mut self, key: String, at: DateTime<Local>) {
        self.fired.insert(key, at.to_utc());
    }

    /// Forgets alarms older than `before`; they can no longer become due.
    pub fn prune(&mut self, before: DateTime<Local>) {
        self.fired.retain(|_, at| *at >= before);
    }
}
