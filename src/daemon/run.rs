//! The loop behind `hall daemon run` (what `hall daemon start` launches).
//!
//! - One instance: the loop holds `daemon/daemon.lock` for its whole life.
//! - Every pass: settle an expired pause, run every registered repo in
//!   parallel, record a new pause, and keep the last pass per repo.
//! - A heartbeat (and what it is doing now) every few seconds, also while
//!   a long pass runs, so the inbox only flags a daemon that is really gone.
//! - It reads the state file every second and exits cleanly once `wanted`
//!   is false (`hall daemon stop`): no signals, so it works on every OS.

use std::time::{Duration, Instant};

use chrono::Utc;

use crate::error::{ErrorCode, HallError, Result};

use super::state::{self, DaemonState};
use super::targets::{self, Target};
use super::{Outcome, pass_all, progress_line, record_pause};

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
    loop {
        ctl.check();
        if ctl.stop {
            break;
        }
        let targets = match &opts.repo {
            Some(t) => vec![t.clone()],
            None => targets::registered(),
        };
        for t in &targets {
            let key = crate::paths::display(&t.path);
            if !announced.contains(&key) {
                if let Ok(r) = targets::open(t) {
                    r.hall.live(&format!("daemon {pid} is watching {}", t.name));
                }
                announced.push(key);
            }
        }
        let t0 = Instant::now();
        let now = Utc::now();
        let paused = state::update(|s| -> Result<_> {
            if let Some(p) = super::pause::settle(s, now)? {
                log(&format!("pause over ({})", p.reason));
            }
            Ok(s.pause_at(now).cloned())
        })??;
        let outcomes = pass_all(&targets, &opts.only, paused.as_ref(), &mut |c| ctl.tick(c));
        for line in outcomes.iter().flat_map(progress_lines) {
            log(&line);
        }
        let now = Utc::now();
        let new_pause = state::update(|s| -> Result<_> {
            let p = record_pause(s, &outcomes, now)?;
            for o in &outcomes {
                let sum = o.summary();
                s.repos.retain(|r| r.path != sum.path);
                s.repos.push(sum);
            }
            s.passes += 1;
            s.heartbeat = Some(now);
            s.current = None;
            Ok(p)
        })??;
        if let Some(p) = new_pause {
            log(&format!(
                "paused until {}: {}",
                p.until.map(|u| u.to_rfc3339()).unwrap_or_default(),
                p.reason
            ));
        }
        ctl.current = None;
        ctl.last_beat = Instant::now();
        if t0.elapsed() > Duration::from_secs(1) {
            log(&format!("pass took {}s", t0.elapsed().as_secs()));
        }
        // Wait for the next pass, still beating and listening for stop.
        let next = Instant::now() + opts.poll;
        while !ctl.stop {
            let left = next.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            std::thread::sleep(left.min(Duration::from_millis(250)));
            ctl.tick(None);
        }
        if ctl.stop {
            break;
        }
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
