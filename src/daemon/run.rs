//! The loop behind `hall daemon run` (what `hall daemon start` launches).
//!
//! - One instance: the loop holds `daemon/daemon.lock` for its whole life.
//! - Every pass: settle an expired pause, run every registered repo in
//!   parallel, record a new pause, and keep the last pass per repo.
//! - A heartbeat (and what it is doing now) every few seconds, also while
//!   a long pass runs, so the inbox only flags a daemon that is really gone.
//! - It reads the state file every second and exits cleanly once `wanted`
//!   is false (`hall daemon stop`): no signals, so it works on every OS.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use chrono::Utc;

use crate::error::{ErrorCode, HallError, Result};

use super::state::{self, DaemonState};
use super::targets::{self, Target};
use super::{Outcome, progress_line, record_pause};

/// The default pause between passes.
pub const DEFAULT_POLL: Duration = Duration::from_secs(5);
/// How often the heartbeat is written while nothing changes.
const BEAT: Duration = Duration::from_secs(5);
/// How often the loop reads `wanted`.
const CHECK: Duration = Duration::from_secs(1);

pub struct Options {
    /// Only these workers.
    pub only: Vec<String>,
    /// Only this repository; else every registered one, re-read each pass.
    pub repo: Option<Target>,
    pub poll: Duration,
}

/// `HALL_DAEMON_POLL` ("250ms", "2s", "1m", or seconds), else 5s.
pub fn poll_interval() -> Duration {
    std::env::var("HALL_DAEMON_POLL")
        .ok()
        .and_then(|v| parse_poll(&v))
        .unwrap_or(DEFAULT_POLL)
}

pub fn parse_poll(s: &str) -> Option<Duration> {
    let s = s.trim();
    if let Some(ms) = s.strip_suffix("ms") {
        return ms.trim().parse().ok().map(Duration::from_millis);
    }
    if let Ok(secs) = s.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    crate::config::parse_duration(s).map(|n| Duration::from_secs(n as u64))
}

/// A line for `daemon/daemon.log`, and for the terminal when the loop runs
/// in the foreground. The log is written directly, so a login service
/// (whose output goes nowhere) still leaves one.
pub fn log(line: &str) {
    use std::io::{IsTerminal, Write};
    let line = format!("{} {line}", Utc::now().format("%Y-%m-%dT%H:%M:%SZ"));
    if std::io::stderr().is_terminal() {
        eprintln!("{line}");
    }
    let path = state::log_path();
    if let Ok(meta) = std::fs::metadata(&path)
        && meta.len() > 1024 * 1024
    {
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Takes the single-instance lock, retrying briefly (a `status` probe may
/// hold it for a moment).
fn acquire() -> Result<std::fs::File> {
    for _ in 0..5 {
        if let Some(f) = state::try_instance_lock()? {
            return Ok(f);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let pid = state::load().map(|s| s.pid).unwrap_or(0);
    Err(HallError::new(
        ErrorCode::Claimed,
        format!("the Hallmark daemon is already running (pid {pid})"),
    )
    .next(["hall daemon status", "hall daemon stop"]))
}

/// Keeps the heartbeat and watches for `stop`.
struct Ctl {
    pid: u32,
    last_beat: Instant,
    last_check: Instant,
    current: Option<String>,
    stop: bool,
    misses: u32,
}

impl Ctl {
    fn tick(&mut self, current: Option<String>) {
        if current != self.current || self.last_beat.elapsed() >= BEAT {
            let now = Utc::now();
            let c = current.clone();
            let _ = state::update(|s| {
                s.heartbeat = Some(now);
                s.current = c;
            });
            self.current = current;
            self.last_beat = Instant::now();
        }
        if self.last_check.elapsed() >= CHECK {
            self.check();
        }
    }

    fn check(&mut self) {
        self.last_check = Instant::now();
        match state::load() {
            Some(s) => {
                self.misses = 0;
                if !s.wanted || s.pid != self.pid {
                    self.stop = true;
                }
            }
            // The state file is gone (or unreadable three times running):
            // nobody can stop us any more, so stop now.
            None => {
                self.misses += 1;
                if self.misses >= 3 {
                    self.stop = true;
                }
            }
        }
    }
}

/// Runs passes until `hall daemon stop`. Returns the final state.
pub fn run_loop(opts: &Options) -> Result<DaemonState> {
    let lock = acquire()?;
    let pid = std::process::id();
    let started = Utc::now();
    state::update(|s| {
        s.pid = pid;
        s.started_at = Some(started);
        s.heartbeat = Some(started);
        s.wanted = true;
        s.stopped_at = None;
        s.current = None;
        s.passes = 0;
    })?;
    log(&format!(
        "daemon started (pid {pid}, a pass every {}ms)",
        opts.poll.as_millis()
    ));
    let mut ctl = Ctl {
        pid,
        last_beat: Instant::now(),
        last_check: Instant::now(),
        current: None,
        stop: false,
        misses: 0,
    };
    let mut announced = Vec::<String>::new();
    // Each repo runs on its own schedule: its next pass starts one poll
    // interval after its own last pass ended, so a slow repo (a long review,
    // a cold build) never holds another repo's queue.
    let current: Arc<Mutex<BTreeMap<String, String>>> = Arc::default();
    let (done_tx, done_rx) = mpsc::channel::<Outcome>();
    let mut running: BTreeMap<String, std::thread::JoinHandle<()>> = BTreeMap::new();
    let mut due: BTreeMap<String, Instant> = BTreeMap::new();
    let mut targets: Vec<Target> = Vec::new();
    let mut refreshed: Option<Instant> = None;
    loop {
        ctl.check();
        // The registry is read again every poll interval, so a repo added
        // with `hall init` joins without a restart.
        if refreshed.is_none_or(|r| r.elapsed() >= opts.poll) {
            targets = match &opts.repo {
                Some(t) => vec![t.clone()],
                None => targets::registered(),
            };
            refreshed = Some(Instant::now());
            for t in &targets {
                let key = crate::paths::display(&t.path);
                if !announced.contains(&key) {
                    if let Ok(r) = targets::open(t) {
                        r.hall.live(&format!("daemon {pid} is watching {}", t.name));
                    }
                    announced.push(key);
                }
            }
        }
        // Start the repos whose turn it is.
        if !ctl.stop {
            let now = Utc::now();
            let paused = state::update(|s| -> Result<_> {
                if let Some(p) = super::pause::settle(s, now)? {
                    log(&format!("pause over ({})", p.reason));
                }
                Ok(s.pause_at(now).cloned())
            })??;
            for t in &targets {
                let key = crate::paths::display(&t.path);
                if running.contains_key(&key) || due.get(&key).is_some_and(|d| *d > Instant::now())
                {
                    continue;
                }
                let (t, only, paused) = (t.clone(), opts.only.clone(), paused.clone());
                let (cur, tx) = (Arc::clone(&current), done_tx.clone());
                let handle = std::thread::spawn(move || {
                    let o = super::one(&t, &only, paused.as_ref(), &cur);
                    let _ = tx.send(o);
                });
                running.insert(key, handle);
            }
        }
        // Collect the passes that ended.
        while let Ok(o) = done_rx.try_recv() {
            let key = crate::paths::display(&o.target.path);
            if let Some(h) = running.remove(&key) {
                let _ = h.join();
            }
            due.insert(key, Instant::now() + opts.poll);
            for line in progress_lines(&o) {
                log(&line);
            }
            let now = Utc::now();
            let new_pause = state::update(|s| -> Result<_> {
                let outcomes = std::slice::from_ref(&o);
                let p = record_pause(s, outcomes, now)?;
                let sum = o.summary();
                s.repos.retain(|r| r.path != sum.path);
                s.repos.push(sum);
                s.passes += 1;
                s.heartbeat = Some(now);
                Ok(p)
            })??;
            if let Some(p) = new_pause {
                log(&format!(
                    "paused until {}: {}",
                    p.until.map(|u| u.to_rfc3339()).unwrap_or_default(),
                    p.reason
                ));
            }
        }
        // A thread that died without reporting: free its slot.
        let dead: Vec<String> = running
            .iter()
            .filter(|(_, h)| h.is_finished())
            .map(|(k, _)| k.clone())
            .collect();
        for k in dead {
            if let Some(h) = running.remove(&k)
                && h.join().is_err()
            {
                log(&format!("the pass over {k} panicked"));
            }
            due.insert(k, Instant::now() + opts.poll);
        }
        if ctl.stop && running.is_empty() {
            break;
        }
        let doing = current.lock().map(|m| m.clone()).unwrap_or_default();
        let now_doing = (!doing.is_empty()).then(|| {
            doing
                .iter()
                .map(|(r, w)| format!("{r}: {w}"))
                .collect::<Vec<_>>()
                .join(" · ")
        });
        ctl.tick(now_doing);
        std::thread::sleep(Duration::from_millis(200));
    }
    let stopped = Utc::now();
    let st = state::update(|s| {
        if s.pid == pid {
            s.heartbeat = None;
            s.current = None;
            s.stopped_at = Some(stopped);
        }
        s.clone()
    })?;
    for t in match &opts.repo {
        Some(t) => vec![t.clone()],
        None => targets::registered(),
    } {
        if let Ok(r) = targets::open(&t) {
            r.hall.live(&format!("daemon {pid} stopped"));
        }
    }
    log("daemon stopped");
    drop(lock);
    Ok(st)
}

/// What a pass did, one line per worker that did something.
fn progress_lines(o: &Outcome) -> Vec<String> {
    if let Some(s) = &o.skip {
        return vec![format!("{}: skipped: {}", o.target.name, s.note())];
    }
    o.steps
        .iter()
        .filter_map(|s| progress_line(s).map(|l| format!("{} {:9} {l}", s.repo, s.worker)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_intervals() {
        assert_eq!(parse_poll("250ms"), Some(Duration::from_millis(250)));
        assert_eq!(parse_poll("2s"), Some(Duration::from_secs(2)));
        assert_eq!(parse_poll("3"), Some(Duration::from_secs(3)));
        assert_eq!(parse_poll("1m"), Some(Duration::from_secs(60)));
        assert_eq!(parse_poll("soon"), None);
    }
}
