//! `hall daemon start|stop|status|run` (ADR 0014, ADR 0017).
//!
//! - `run` is the loop itself, in the foreground; `run --once` is one pass,
//!   for tests and for driving workers by hand.
//! - `start` launches `run` detached and waits for its first heartbeat;
//!   `stop` asks it to exit through the state file and waits.
//! - `status` says whether it runs (the instance lock), its heartbeat and
//!   pause, what it is doing, and each registered repository.

use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::{DaemonAction, DaemonArgs};
use crate::daemon::service::{self, ServiceOut};
use crate::daemon::state::{self, DaemonState, RepoPass};
use crate::daemon::targets::{self, Target};
use crate::daemon::{self, Step, run, spawn};
use crate::error::{ErrorCode, HallError, Result};
use crate::output::{Render, Response};
use crate::paths;
use crate::repo::Repo;
use crate::worker::Progress;

/// How long `start` waits for the first heartbeat.
const START_WAIT: Duration = Duration::from_secs(10);
/// How long `stop` waits for the loop to exit.
const STOP_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize, JsonSchema)]
pub struct DaemonOut {
    /// start, stop, status, run.
    pub action: String,
    /// Running now: a `hall daemon run` loop holds the instance lock.
    pub running: bool,
    /// start/stop: whether this call changed anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<DaemonState>,
    /// What each worker did (`run --once`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Step>,
    /// The repositories it serves (`status`), or that the pass reached
    /// (`run --once`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<RepoSummary>,
    /// The login service (`--at-login`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<ServiceOut>,
    /// status: whether the login service is installed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at_login: Option<bool>,
    /// The daemon's own log.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    /// Anything worth knowing about this reply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One registered repository, as the daemon sees it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RepoSummary {
    pub name: String,
    /// The main checkout.
    pub path: String,
    /// ok, missing, not_initialized, error.
    pub status: String,
    /// Items in checks, review, fixing or hallmarked.
    pub in_loop: u32,
    /// Tickets waiting for the assayer.
    pub queue: u32,
    /// The daemon's last pass over it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_pass: Option<RepoPass>,
}

impl DaemonOut {
    fn new(action: &str, running: bool) -> DaemonOut {
        DaemonOut {
            action: action.into(),
            running,
            changed: None,
            state: None,
            steps: vec![],
            repos: vec![],
            service: None,
            at_login: None,
            log: None,
            note: None,
        }
    }
}

fn ago(t: DateTime<Utc>) -> String {
    let s = (Utc::now() - t).num_seconds().max(0);
    match s {
        0..=59 => format!("{s}s ago"),
        60..=3599 => format!("{}m ago", s / 60),
        _ => format!("{}h ago", s / 3600),
    }
}

fn hm(t: DateTime<Utc>) -> String {
    t.with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

impl Render for DaemonOut {
    fn render(&self) -> String {
        let mut s = String::new();
        let head = if self.running {
            "running"
        } else {
            "not running"
        };
        s.push_str(&format!("daemon {head}"));
        if let Some(st) = &self.state {
            if self.running && st.pid != 0 {
                s.push_str(&format!(" · pid {}", st.pid));
            }
            if self.running
                && let Some(t) = st.started_at
            {
                s.push_str(&format!(" · since {}", hm(t)));
            }
            match (self.running, st.heartbeat) {
                (true, Some(h)) => s.push_str(&format!(" · heartbeat {}", ago(h))),
                (false, _) if st.stopped_at.is_some() => {
                    s.push_str(&format!(" · stopped {}", hm(st.stopped_at.unwrap())))
                }
                _ => {}
            }
        }
        s.push('\n');
        if let Some(c) = self.changed {
            s.push_str(if c { "  changed\n" } else { "  unchanged\n" });
        }
        if let Some(st) = &self.state {
            if let Some(p) = &st.paused {
                match p.until {
                    Some(u) => s.push_str(&format!("  paused until {}: {}\n", hm(u), p.reason)),
                    None => s.push_str(&format!("  paused: {}\n", p.reason)),
                }
            }
            if self.running
                && let Some(c) = &st.current
            {
                s.push_str(&format!("  now      {c}\n"));
            }
        }
        if let Some(n) = &self.note {
            s.push_str(&format!("  {n}\n"));
        }
        if let Some(sv) = &self.service {
            let what = match (sv.action.as_str(), sv.dry_run) {
                ("install", false) => "installed",
                ("install", true) => "would install",
                (_, false) => "removed",
                (_, true) => "would remove",
            };
            s.push_str(&format!("  login    {what} {} ({})\n", sv.label, sv.manager));
            s.push_str(&format!("           {}\n", sv.definition));
        }
        if let Some(a) = self.at_login {
            s.push_str(&format!(
                "  login    {}\n",
                if a {
                    "starts at login"
                } else {
                    "not started at login"
                }
            ));
        }
        if let Some(l) = &self.log {
            s.push_str(&format!("  log      {l}\n"));
        }
        if !self.steps.is_empty() {
            s.push('\n');
        }
        for st in &self.steps {
            let what = match &st.progress {
                Progress::Idle => st.skipped.clone().unwrap_or_else(|| "idle".into()),
                Progress::Worked { what } => what.clone(),
                Progress::Pause { reason, .. } => format!("paused: {reason}"),
            };
            s.push_str(&format!("  {:12} {:10} {what}", st.repo, st.worker));
            if let Some(e) = &st.error {
                s.push_str(&format!("  error: {e}"));
            }
            s.push('\n');
        }
        if !self.repos.is_empty() && self.steps.is_empty() {
            s.push_str("\nrepos\n");
            for r in &self.repos {
                let mut line = format!("  {:12} ", r.name);
                if r.status == "ok" {
                    line.push_str(&format!("{} in the loop · {} queued", r.in_loop, r.queue));
                } else {
                    line.push_str(&r.status.replace('_', " "));
                }
                if let Some(p) = &r.last_pass {
                    line.push_str(&format!(" · last pass {} {}", ago(p.at), p.result));
                    if let Some(n) = p.notes.first() {
                        line.push_str(&format!(" ({n})"));
                    }
                }
                s.push_str(&line);
                s.push('\n');
                s.push_str(&format!("  {:12} {}\n", "", r.path));
            }
        }
        s
    }
}

pub fn daemon(cwd: &Path, a: DaemonArgs) -> Result<Response> {
    match a.action {
        DaemonAction::Run {
            once: true,
            only,
            repo,
        } => run_once(cwd, &only, repo.as_deref()),
        DaemonAction::Run {
            once: false,
            only,
            repo,
        } => run_loop(&only, repo.as_deref()),
        DaemonAction::Start { at_login } => start(at_login),
        DaemonAction::Stop { at_login } => stop(at_login),
        DaemonAction::Status => status(),
    }
}

fn check_only(only: &[String]) -> Result<()> {
    let names = crate::worker::names();
    if let Some(bad) = only.iter().find(|o| !names.contains(&o.as_str())) {
        return Err(HallError::new(
            ErrorCode::Usage,
            format!("no worker called `{bad}`; workers: {}", names.join(", ")),
        )
        .next(["hall daemon run --once --only watchman"]));
    }
    Ok(())
}

/// The one repository `--repo` (or the current directory) names, which
/// must be set up.
fn strict_target(dir: &Path) -> Result<Target> {
    let r = Repo::discover(dir)?;
    r.require_init()?;
    Ok(Target {
        name: r.name.clone(),
        path: r.main_root.clone(),
    })
}

fn run_once(cwd: &Path, only: &[String], repo: Option<&str>) -> Result<Response> {
    check_only(only)?;
    // --repo, else the repo you're in, else every registered repo.
    let targets = match repo {
        Some(r) => {
            let t = targets::resolve(r)?;
            vec![strict_target(&t.path)?]
        }
        None => match strict_target(cwd) {
            Ok(t) => vec![t],
            Err(e) if e.code == ErrorCode::NotARepo => targets::registered(),
            Err(e) => return Err(e),
        },
    };
    let now = Utc::now();
    // Workers named with --only run even while paused: that is driving
    // them by hand.
    let in_force = state::update(|s| -> Result<_> {
        daemon::pause::settle(s, now)?;
        Ok(s.pause_at(now).cloned())
    })??;
    let paused = in_force.filter(|_| only.is_empty());
    let outcomes = daemon::pass_all(&targets, only, paused.as_ref(), &mut |_| {});
    let new_pause = state::update(|s| daemon::record_pause(s, &outcomes, Utc::now()))??;
    let mut out = DaemonOut::new("run", state::instance_running());
    out.steps = outcomes.iter().flat_map(|o| o.steps.clone()).collect();
    out.repos = outcomes
        .iter()
        .map(|o| summarize(&o.target, Some(o.summary())))
        .collect();
    let mut next = vec!["hall status".to_string(), "hall inbox".into()];
    if let Some(p) = new_pause.or(paused) {
        out.note = Some(match p.until {
            Some(u) => format!(
                "paused until {}: {}",
                u.format("%Y-%m-%d %H:%M UTC"),
                p.reason
            ),
            None => format!("paused: {}", p.reason),
        });
        next.push("hall daemon status".into());
    }
    if targets.is_empty() {
        out.note = Some("no repository is registered; run `hall init` in one".into());
        next = vec!["hall init".into()];
    }
    Ok(Response::new(out, next))
}

fn run_loop(only: &[String], repo: Option<&str>) -> Result<Response> {
    check_only(only)?;
    let repo = match repo {
        Some(r) => {
            let t = targets::resolve(r)?;
            Some(strict_target(&t.path)?)
        }
        None => None,
    };
    let st = run::run_loop(&run::Options {
        only: only.to_vec(),
        repo,
        poll: run::poll_interval(),
    })?;
    let mut out = DaemonOut::new("run", false);
    out.state = Some(without_repos(st));
    Ok(Response::new(
        out,
        vec!["hall daemon start".into(), "hall daemon status".into()],
    ))
}

fn without_repos(mut st: DaemonState) -> DaemonState {
    st.repos.clear();
    st
}

fn start(at_login: bool) -> Result<Response> {
    let mut changed = false;
    if !state::instance_running() {
        state::update(|s| s.wanted = true)?;
        if let Err(e) = launch_and_wait() {
            if !state::instance_running() {
                let _ = state::update(|s| s.wanted = false);
            }
            return Err(e);
        }
        changed = true;
    }
    let mut out = DaemonOut::new("start", true);
    if at_login {
        let sv = service::install()?;
        changed |= sv.changed;
        out.service = Some(sv);
    }
    out.changed = Some(changed);
    out.state = state::load().map(without_repos);
    out.log = Some(paths::display(&state::log_path()));
    if !changed {
        out.note = Some("already running".into());
    }
    Ok(Response::new(
        out,
        vec![
            "hall daemon status".into(),
            "hall inbox".into(),
            "hall daemon stop".into(),
        ],
    ))
}

/// Launches the loop and waits for its first heartbeat.
fn launch_and_wait() -> Result<()> {
    let mut child = spawn::launch()?;
    let pid = child.id();
    let deadline = Instant::now() + START_WAIT;
    loop {
        if let Some(s) = state::load()
            && s.pid == pid
            && s.heartbeat.is_some()
        {
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(HallError::new(
                ErrorCode::CommandFailed,
                format!(
                    "the daemon exited at once ({status}):\n{}",
                    spawn::log_tail(10)
                ),
            )
            .next(["hall daemon status", "hall daemon run"]));
        }
        if Instant::now() > deadline {
            return Err(HallError::new(
                ErrorCode::CommandFailed,
                format!(
                    "the daemon (pid {pid}) gave no heartbeat within {}s",
                    START_WAIT.as_secs()
                ),
            )
            .next(["hall daemon status", "hall daemon stop"]));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn stop(at_login: bool) -> Result<Response> {
    let was_wanted = state::update(|s| std::mem::replace(&mut s.wanted, false))?;
    let was_running = state::instance_running();
    let deadline = Instant::now() + STOP_WAIT;
    let mut running = was_running;
    while running && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        running = state::instance_running();
    }
    let mut out = DaemonOut::new("stop", running);
    let mut changed = was_wanted || was_running;
    if at_login {
        let sv = service::uninstall()?;
        changed |= sv.changed;
        out.service = Some(sv);
    }
    out.changed = Some(changed);
    out.state = state::load().map(without_repos);
    let next = if running {
        out.note = Some(format!(
            "still finishing its current pass; it exits right after (waited {}s)",
            STOP_WAIT.as_secs()
        ));
        vec!["hall daemon status".into()]
    } else {
        vec!["hall daemon start".into(), "hall daemon status".into()]
    };
    Ok(Response::new(out, next))
}

fn status() -> Result<Response> {
    let running = state::instance_running();
    let st = state::load();
    let last = |t: &Target| {
        st.as_ref().and_then(|s| {
            s.repos
                .iter()
                .find(|r| targets::same(Path::new(&r.path), &t.path))
                .cloned()
        })
    };
    let mut out = DaemonOut::new("status", running);
    out.repos = targets::registered()
        .iter()
        .map(|t| summarize(t, last(t)))
        .collect();
    out.at_login = service::installed().ok();
    out.log = Some(paths::display(&state::log_path()));
    let paused = st.as_ref().is_some_and(|s| s.paused.is_some());
    out.state = st.map(without_repos);
    let mut next = if running {
        vec!["hall inbox".to_string(), "hall daemon stop".into()]
    } else {
        vec![
            "hall daemon start".to_string(),
            "hall daemon run --once".into(),
        ]
    };
    if paused {
        next.push("hall log".into());
    }
    if out.repos.is_empty() {
        next.push("hall init".into());
    }
    Ok(Response::new(out, next))
}

fn summarize(t: &Target, last_pass: Option<RepoPass>) -> RepoSummary {
    let mut s = RepoSummary {
        name: t.name.clone(),
        path: paths::display(&t.path),
        status: "ok".into(),
        in_loop: 0,
        queue: 0,
        last_pass,
    };
    match targets::open(t) {
        Ok(r) => {
            s.in_loop = r
                .index()
                .and_then(|ix| ix.items(false))
                .map(|items| {
                    items
                        .iter()
                        .filter(|i| crate::watchman::watched(i.state))
                        .count() as u32
                })
                .unwrap_or(0);
            s.queue = r.hall.tickets().map(|t| t.len() as u32).unwrap_or(0);
        }
        Err(skip) => s.status = skip.result().into(),
    }
    s
}
