//! The daemon's state file, `<user data dir>/hallmark/daemon/state.json`.
//! The inbox reads it to flag a daemon that stopped without being asked to.
//!
//! Several processes write it (the loop, `start`, `stop`, `run --once`), so
//! every read-modify-write goes through [`update`], under `state.lock`.
//! Whether a daemon is running at all is answered by the instance lock
//! (`daemon.lock`), which the loop holds for its whole life: see
//! [`instance_running`].

use std::fs::{File, OpenOptions};
use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};
use fs4::FileExt;
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
    /// Refreshed every few seconds while the loop runs; cleared when it
    /// stops cleanly.
    pub heartbeat: Option<DateTime<Utc>>,
    /// True between `hall daemon start` and `hall daemon stop`.
    pub wanted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<Pause>,
    /// What it is doing now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// When the loop last exited cleanly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<DateTime<Utc>>,
    /// Passes run since `started_at`.
    #[serde(default)]
    pub passes: u64,
    /// The last pass over each repository.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<RepoPass>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Pause {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<DateTime<Utc>>,
    pub since: DateTime<Utc>,
    /// The repository whose worker asked for the pause.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The journals (`hallmark` directories) that recorded `paused`; they
    /// get the matching `unpaused`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub journals: Vec<String>,
}

/// The last pass over one repository.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RepoPass {
    pub name: String,
    /// The main checkout, as registered.
    pub path: String,
    pub at: DateTime<Utc>,
    /// idle, worked, paused, error, missing, not_initialized.
    pub result: String,
    /// What the workers did, or why the repo was skipped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

pub fn dir() -> PathBuf {
    user_dir().join("daemon")
}

pub fn path() -> PathBuf {
    dir().join("state.json")
}

/// Held by the running loop for its whole life.
pub fn lock_path() -> PathBuf {
    dir().join("daemon.lock")
}

/// Where `hall daemon start` sends the loop's stdout and stderr.
pub fn log_path() -> PathBuf {
    dir().join("daemon.log")
}

pub fn load() -> Option<DaemonState> {
    serde_json::from_str(&std::fs::read_to_string(path()).ok()?).ok()
}

pub fn save(s: &DaemonState) -> Result<()> {
    std::fs::create_dir_all(dir())?;
    let p = path();
    let tmp = p.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_string_pretty(s)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

/// Reads, changes and saves the state under `state.lock`, so concurrent
/// writers (the loop's heartbeat, `stop`) never undo each other.
pub fn update<R>(f: impl FnOnce(&mut DaemonState) -> R) -> Result<R> {
    std::fs::create_dir_all(dir())?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir().join("state.lock"))?;
    FileExt::lock(&lock)?;
    let r = (|| {
        let mut s = load().unwrap_or_default();
        let before = s.clone();
        let r = f(&mut s);
        if s != before {
            save(&s)?;
        }
        Ok(r)
    })();
    let _ = FileExt::unlock(&lock);
    r
}

/// The single-instance lock. `Ok(Some(file))` means this process now holds
/// it (keep the file open); `Ok(None)` means another process does.
pub fn try_instance_lock() -> Result<Option<File>> {
    std::fs::create_dir_all(dir())?;
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path())?;
    match FileExt::try_lock(&f) {
        Ok(()) => Ok(Some(f)),
        Err(fs4::TryLockError::WouldBlock) => Ok(None),
        Err(fs4::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// True when a daemon loop holds the instance lock. More exact than the
/// heartbeat: a loop that crashed releases the lock at once.
pub fn instance_running() -> bool {
    match try_instance_lock() {
        Ok(Some(f)) => {
            let _ = FileExt::unlock(&f);
            false
        }
        Ok(None) => true,
        // Can't tell: trust the heartbeat.
        Err(_) => load().is_some_and(|s| s.alive(Utc::now())),
    }
}

impl DaemonState {
    /// Running: a fresh heartbeat.
    pub fn alive(&self, now: DateTime<Utc>) -> bool {
        self.heartbeat
            .is_some_and(|h| now - h < Duration::seconds(STALE_AFTER_SECS))
    }

    /// Wanted, but no fresh heartbeat: since when it has been silent.
    pub fn stopped_unexpectedly(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        (self.wanted && !self.alive(now)).then(|| {
            self.heartbeat
                .or(self.stopped_at)
                .or(self.started_at)
                .unwrap_or(now)
        })
    }

    /// The pause in force at `now`, if any (an expired one doesn't count).
    pub fn pause_at(&self, now: DateTime<Utc>) -> Option<&Pause> {
        self.paused
            .as_ref()
            .filter(|p| p.until.is_none_or(|u| u > now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_silent_wanted_daemon_is_flagged() {
        let now = Utc::now();
        let mut s = DaemonState {
            wanted: true,
            heartbeat: Some(now - Duration::seconds(STALE_AFTER_SECS + 5)),
            ..Default::default()
        };
        assert!(!s.alive(now));
        assert_eq!(s.stopped_unexpectedly(now), s.heartbeat);
        s.heartbeat = Some(now);
        assert!(s.alive(now));
        assert_eq!(s.stopped_unexpectedly(now), None);
        // A clean stop clears `wanted` first, so it is never flagged.
        s.wanted = false;
        s.heartbeat = None;
        assert_eq!(s.stopped_unexpectedly(now), None);
    }

    #[test]
    fn an_expired_pause_is_not_in_force() {
        let now = Utc::now();
        let mut s = DaemonState {
            paused: Some(Pause {
                reason: "rate limited".into(),
                until: Some(now - Duration::seconds(1)),
                since: now - Duration::minutes(30),
                repo: None,
                journals: vec![],
            }),
            ..Default::default()
        };
        assert!(s.pause_at(now).is_none());
        s.paused.as_mut().unwrap().until = Some(now + Duration::minutes(1));
        assert!(s.pause_at(now).is_some());
    }
}
