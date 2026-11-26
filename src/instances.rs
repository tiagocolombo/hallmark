//! Running hall processes: `hall ps` and `hall kill` (ADR 0072).
//!
//! Each hall that keeps running (the web app, `watch`, `trace --follow`,
//! the terminal app) registers itself under `<user dir>/run/` for as long as
//! it runs:
//!
//! - `<pid>.lock` is created and locked first, and held until the process
//!   exits. The OS releases it however the process ends (a crash, a kill),
//!   so a lister that can take the lock knows the process is gone, and
//!   removes its files. A recycled pid can't fool it.
//! - `<pid>.json` says what it is, written once the lock is held.
//!
//! The daemon has its own instance lock and state file (ADR 0017), so it is
//! read from there, and `hall kill` stops it the way `hall daemon stop` does.
//!
//! Registration never fails a command: when the directory can't be written,
//! the process just isn't listed.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use clap::ValueEnum;
use fs4::FileExt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::repo::user_dir;

/// How long `hall kill` waits for a process to exit before it reports it
/// still running.
const KILL_WAIT: Duration = Duration::from_secs(5);

/// What a running hall is.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// `hall web`.
    Web,
    /// The review loop (`hall daemon run`, or `start`'s background one).
    Daemon,
    /// `hall watch`.
    Watch,
    /// `hall trace --follow`.
    Trace,
    /// The terminal app (`hall` on a terminal).
    App,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Web => "web",
            Kind::Daemon => "daemon",
            Kind::Watch => "watch",
            Kind::Trace => "trace",
            Kind::App => "app",
        }
    }
}

/// One running hall.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Instance {
    pub pid: u32,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    /// The directory it was started in.
    #[serde(default)]
    pub cwd: String,
    /// Its arguments, after `hall`.
    #[serde(default)]
    pub args: Vec<String>,
    /// The port it listens on (web).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// The address to open, with its token (web). Only you can read the
    /// file it comes from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Where its output goes, when it runs detached (`hall web -d`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
}

/// Where the run files live.
pub fn dir() -> PathBuf {
    user_dir().join("run")
}

fn json_path(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("{pid}.json"))
}

fn lock_path(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("{pid}.lock"))
}

/// Creates the run directory, readable only by you: a web entry carries
/// its token.
pub fn ensure_dir() -> std::io::Result<PathBuf> {
    let d = dir();
    std::fs::create_dir_all(&d)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
    }
    Ok(d)
}

/// Writes `body` to a new file only you can read.
pub fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut o = OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(&tmp)?;
    f.write_all(body)?;
    drop(f);
    std::fs::rename(&tmp, path)
}

/// This process's registration; dropping it removes the entry.
pub struct Registration {
    dir: PathBuf,
    pid: u32,
    lock: Option<File>,
    entry: Instance,
}

impl Registration {
    /// Records more about this process (a web server's port and address,
    /// once it is listening).
    pub fn update(&mut self, f: impl FnOnce(&mut Instance)) {
        f(&mut self.entry);
        if let Ok(body) = serde_json::to_vec_pretty(&self.entry) {
            let _ = write_private(&json_path(&self.dir, self.pid), &body);
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(json_path(&self.dir, self.pid));
        if let Some(f) = self.lock.take() {
            let _ = FileExt::unlock(&f);
            drop(f);
        }
        let _ = std::fs::remove_file(lock_path(&self.dir, self.pid));
    }
}

/// Registers this process as a running `kind`. `None` when it can't be
/// recorded; the caller carries on regardless.
pub fn register(kind: Kind, cwd: &Path) -> Option<Registration> {
    let dir = ensure_dir().ok()?;
    let pid = std::process::id();
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path(&dir, pid))
        .ok()?;
    FileExt::try_lock(&lock).ok()?;
    let entry = Instance {
        pid,
        kind,
        started_at: Some(Utc::now()),
        cwd: crate::paths::display(cwd),
        args: std::env::args().skip(1).collect(),
        port: None,
        url: None,
        log: std::env::var(LOG_ENV).ok().filter(|l| !l.is_empty()),
    };
    let mut r = Registration {
        dir,
        pid,
        lock: Some(lock),
        entry,
    };
    r.update(|_| {});
    Some(r)
}

/// Set by `hall web -d` for the server it starts: the log its output goes
/// to, so `hall ps` can show it and remove it once the server is gone.
pub const LOG_ENV: &str = "HALL_RUN_LOG";

/// Whether the process that wrote `pid`'s entry still holds its lock.
fn holds_lock(dir: &Path, pid: u32) -> bool {
    let Ok(f) = OpenOptions::new()
        .write(true)
        .truncate(false)
        .open(lock_path(dir, pid))
    else {
        // No lock file: the entry is written after the lock, so it's gone.
        return false;
    };
    match FileExt::try_lock(&f) {
        Ok(()) => {
            let _ = FileExt::unlock(&f);
            false
        }
        Err(fs4::TryLockError::WouldBlock) => true,
        // Can't tell: keep it listed.
        Err(fs4::TryLockError::Error(_)) => true,
    }
}

/// Removes a gone process's files, its detached log included.
fn prune(dir: &Path, pid: u32, entry: Option<&Instance>) {
    if let Some(log) = entry.and_then(|e| e.log.as_deref()) {
        let log = Path::new(log);
        // Only a log in the run directory is ours to remove.
        if log.parent().is_some_and(|p| same_dir(p, dir)) {
            let _ = std::fs::remove_file(log);
        }
    }
    let _ = std::fs::remove_file(json_path(dir, pid));
    let _ = std::fs::remove_file(lock_path(dir, pid));
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (dunce::canonicalize(a), dunce::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The running halls, oldest first, `kind` only when given. Entries of
/// processes that are gone are removed on the way.
pub fn list(kind: Option<Kind>) -> Vec<Instance> {
    let mut out = Vec::new();
    if kind.is_none_or(|k| k == Kind::Daemon)
        && let Some(d) = daemon()
    {
        out.push(d);
    }
    let dir = dir();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return out;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(pid) = name
            .strip_suffix(".json")
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        let entry: Option<Instance> = std::fs::read_to_string(e.path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());
        if !holds_lock(&dir, pid) {
            prune(&dir, pid, entry.as_ref());
            continue;
        }
        if let Some(i) = entry
            && kind.is_none_or(|k| k == i.kind)
        {
            out.push(i);
        }
    }
    out.sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.pid.cmp(&b.pid)));
    out
}

/// The daemon, when its loop holds the instance lock.
fn daemon() -> Option<Instance> {
    if !crate::daemon::state::instance_running() {
        return None;
    }
    let st = crate::daemon::state::load()?;
    Some(Instance {
        pid: st.pid,
        kind: Kind::Daemon,
        started_at: st.started_at,
        cwd: crate::paths::display(&crate::daemon::state::dir()),
        args: vec!["daemon".into(), "run".into()],
        port: None,
        url: None,
        log: Some(crate::paths::display(&crate::daemon::state::log_path())),
    })
}

/// What `stop` did to one process.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Stopped {
    pub pid: u32,
    pub kind: Kind,
    /// True once it is gone.
    pub stopped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Stops one listed process and waits for it to go. The daemon is asked
/// to stop the way `hall daemon stop` does, so it finishes its pass and
/// isn't reported as stopped unexpectedly.
pub fn stop(i: &Instance) -> Stopped {
    if i.kind == Kind::Daemon {
        let r = crate::commands::daemon::stop_loop();
        return Stopped {
            pid: i.pid,
            kind: i.kind,
            stopped: r.is_ok_and(|running| !running),
            note: Some("asked to stop, as `hall daemon stop` does".into()),
        };
    }
    let dir = dir();
    terminate(i.pid);
    let deadline = Instant::now() + KILL_WAIT;
    let mut alive = holds_lock(&dir, i.pid);
    while alive && Instant::now() < deadline {
        crate::daemon::nap(Duration::from_millis(50));
        alive = holds_lock(&dir, i.pid);
    }
    if !alive {
        prune(&dir, i.pid, Some(i));
    }
    Stopped {
        pid: i.pid,
        kind: i.kind,
        stopped: !alive,
        note: alive.then(|| format!("still running after {}s", KILL_WAIT.as_secs())),
    }
}

/// Ends the process: SIGTERM on Unix, `taskkill /T /F` on Windows.
fn terminate(pid: u32) {
    let pid = pid.to_string();
    #[cfg(unix)]
    let _ = crate::process::command("kill")
        .args(["-TERM", &pid])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    #[cfg(windows)]
    let _ = crate::process::command("taskkill")
        .args(["/T", "/F", "/PID", &pid])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    #[cfg(not(any(unix, windows)))]
    let _ = (pid, Stdio::null());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_serialize_as_their_names() {
        for k in [Kind::Web, Kind::Daemon, Kind::Watch, Kind::Trace, Kind::App] {
            assert_eq!(serde_json::to_value(k).unwrap(), k.as_str());
        }
    }

    #[test]
    fn an_entry_without_its_lock_is_gone() {
        let d = tempfile::tempdir().unwrap();
        assert!(!holds_lock(d.path(), 42));
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(lock_path(d.path(), 42))
            .unwrap();
        assert!(!holds_lock(d.path(), 42), "an unlocked lock file is gone");
        FileExt::try_lock(&f).unwrap();
        assert!(holds_lock(d.path(), 42));
        FileExt::unlock(&f).unwrap();
        assert!(!holds_lock(d.path(), 42));
    }
}
