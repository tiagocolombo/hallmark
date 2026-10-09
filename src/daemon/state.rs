//! The daemon's state file, `<user data dir>/hallmark/daemon/state.json`.
//! The inbox reads it to flag a daemon that stopped without being asked to.

use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::repo::user_dir;

/// A heartbeat older than this means the daemon is gone.
pub const STALE_AFTER_SECS: i64 = 180;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct DaemonState {
    pub pid: u32,
    pub started_at: Option<DateTime<Utc>>,
    pub heartbeat: Option<DateTime<Utc>>,
    /// True between `hall daemon start` and `hall daemon stop`.
    pub wanted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<Pause>,
    /// What it is doing now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Pause {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<DateTime<Utc>>,
    pub since: DateTime<Utc>,
}

pub fn dir() -> PathBuf {
    user_dir().join("daemon")
}

pub fn path() -> PathBuf {
    dir().join("state.json")
}

pub fn load() -> Option<DaemonState> {
    serde_json::from_str(&std::fs::read_to_string(path()).ok()?).ok()
}

pub fn save(s: &DaemonState) -> Result<()> {
    std::fs::create_dir_all(dir())?;
    let p = path();
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(s)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

impl DaemonState {
    /// Running: a fresh heartbeat.
    pub fn alive(&self, now: DateTime<Utc>) -> bool {
        self.heartbeat
            .is_some_and(|h| now - h < Duration::seconds(STALE_AFTER_SECS))
    }

    /// Wanted, but no fresh heartbeat: since when it has been silent.
    pub fn stopped_unexpectedly(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        (self.wanted && !self.alive(now)).then(|| self.heartbeat.or(self.started_at).unwrap_or(now))
    }
}
