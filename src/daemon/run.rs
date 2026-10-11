//! The loop behind `hall daemon run` (what `hall daemon start` launches).
//!
//! - One instance: the loop holds `daemon/daemon.lock` for its whole life.
//! - Every repo, worker and slot has its own schedule (ADR 0026): a job's
//!   next run starts one poll interval after its own last run ended, on its
//!   own thread, so a slow job never holds another's queue. The loop settles
//!   an expired pause, records a new one in every served repo's journal, and
//!   keeps the latest news per repo (`passes` counts finished jobs). The
//!   registry and each repo's `[parallel]` are re-read once per poll interval.
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
use super::targets::{self, Skip, Target};
use super::{Outcome, progress_line, record_pause};
use crate::journal::Hall;

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
    let (done_tx, done_rx) = mpsc::channel::<(String, Outcome)>();
    // Jobs are keyed `<repo path>|<worker>|<slot>` (ADR 0026): each repo,
    // worker and slot runs on its own schedule.
    let mut running: BTreeMap<String, (Instant, Target, std::thread::JoinHandle<()>)> =
        BTreeMap::new();
    let mut halls: BTreeMap<String, Hall> = BTreeMap::new();
    let mut due: BTreeMap<String, Instant> = BTreeMap::new();
    // When each repo's worker last did something, to wake its extra slots.
    let mut busy: BTreeMap<String, Instant> = BTreeMap::new();
    let mut targets: Vec<(Target, crate::config::Parallel)> = Vec::new();
    let mut refreshed: Option<Instant> = None;
    loop {
        ctl.check();
        // The registry and each repo's [parallel] are read again every poll
        // interval, so a repo added with `hall init` joins without a restart.
        if refreshed.is_none_or(|r| r.elapsed() >= opts.poll) {
            let list = match &opts.repo {
                Some(t) => vec![t.clone()],
                None => targets::registered(),
            };
            targets = list
                .into_iter()
                .map(|t| {
                    let par = targets::open(&t)
                        .map(|r| r.config.parallel.clone())
                        .unwrap_or_default();
                    (t, par)
                })
                .collect();
            refreshed = Some(Instant::now());
            for (t, _) in &targets {
                let key = crate::paths::display(&t.path);
                if !announced.contains(&key) {
                    if let Ok(r) = targets::open(t) {
                        r.hall.live(&format!("daemon {pid} is watching {}", t.name));
                    }
                    announced.push(key);
                }
            }
        }
        // Start the jobs whose turn it is.
        if !ctl.stop {
            let now = Utc::now();
            let paused = state::update(|s| -> Result<_> {
                if let Some(p) = super::pause::settle(s, now)? {
                    log(&format!("pause over ({})", p.reason));
                }
                Ok(s.pause_at(now).cloned())
            })??;
            for (t, par) in &targets {
                let repo_key = crate::paths::display(&t.path);
                for (worker, _) in super::worker_order() {
                    if !opts.only.is_empty() && !opts.only.iter().any(|o| o == worker) {
                        continue;
                    }
                    let group = format!("{repo_key}|{worker}");
                    // Extra slots wake only while the worker has work: one
                    // of its jobs runs long, or one did something lately.
                    let working = busy
                        .get(&group)
                        .is_some_and(|t| t.elapsed() < opts.poll * 2)
                        || running.iter().any(|(k, (started, _, _))| {
                            k.starts_with(&format!("{group}|"))
                                && started.elapsed() > Duration::from_secs(1)
                        });
                    let slots = if working { par.slots(worker) } else { 1 };
                    for slot in 1..=slots {
                        let key = format!("{group}|{slot}");
                        if running.contains_key(&key)
                            || due.get(&key).is_some_and(|d| *d > Instant::now())
                        {
                            continue;
                        }
                        let t_key = t.clone();
                        let (t, paused) = (t.clone(), paused.clone());
                        let (cur, tx, k) = (Arc::clone(&current), done_tx.clone(), key.clone());
                        let handle = std::thread::spawn(move || {
                            let o = super::one_slot(&t, worker, slot, paused.as_ref(), &cur);
                            let _ = tx.send((k, o));
                        });
                        running.insert(key, (Instant::now(), t_key, handle));
                    }
                }
            }
        }
        // A thread that died without reporting: record a panic for it. This
        // runs before the drain, so an outcome sent just before a thread
        // finished is still in the channel when the drain looks.
        let mut ended: Vec<(String, Outcome)> = Vec::new();
        let dead: Vec<String> = running
            .iter()
            .filter(|(_, (_, _, h))| h.is_finished())
            .map(|(k, _)| k.clone())
            .collect();
        for k in dead {
            if let Some((_, t, h)) = running.remove(&k)
                && h.join().is_err()
            {
                log(&format!("the job {k} panicked"));
                ended.push((
                    k.clone(),
                    Outcome {
                        target: t,
                        hall: None,
                        steps: vec![],
                        skip: Some(Skip::Error("the job panicked".into())),
                    },
                ));
            }
            due.insert(k, Instant::now() + opts.poll);
        }
        // Collect the jobs that ended.
        while let Ok((key, o)) = done_rx.try_recv() {
            if let Some((_, _, h)) = running.remove(&key) {
                let _ = h.join();
            }
            ended.push((key, o));
        }
        for (key, o) in ended {
            due.insert(key.clone(), Instant::now() + opts.poll);
            if let Some(h) = &o.hall {
                halls.insert(crate::paths::display(&o.target.path), h.clone());
            }
            let did = o
                .steps
                .iter()
                .any(|s| s.error.is_some() || !matches!(s.progress, crate::worker::Progress::Idle));
            if did && let Some((group, _)) = key.rsplit_once('|') {
                busy.insert(group.to_string(), Instant::now());
            }
            for line in progress_lines(&o) {
                log(&line);
            }
            let now = Utc::now();
            // A pause reaches every served repo's journal, not just the
            // reporting one (ADR 0017).
            let journals: Vec<Hall> = targets
                .iter()
                .filter_map(|(t, _)| halls.get(&crate::paths::display(&t.path)).cloned())
                .collect();
            let new_pause = state::update(|s| -> Result<_> {
                let p = record_pause(s, std::slice::from_ref(&o), &journals, now)?;
                let sum = o.summary();
                // Keep the latest news per repo: an idle job doesn't hide
                // what another job just did.
                if did || o.skip.is_some() || !s.repos.iter().any(|r| r.path == sum.path) {
                    s.repos.retain(|r| r.path != sum.path);
                    s.repos.push(sum);
                }
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
