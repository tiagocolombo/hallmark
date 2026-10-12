//! `hall watch <id>`: follow an item live. A line each time what it is
//! doing changes; it stops when the item leaves the loop, needs you, or the
//! timeout passes. Off a terminal each line is a JSON object, and the reply
//! is the last line, as for every command. With `--trace`, the steps of the
//! item's run while one is going print under the changes (ADR 0039).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::WatchArgs;
use crate::error::{HallError, Result};
use crate::item::{ItemId, State};
use crate::now::{self, Now};
use crate::output::{Mode, Render, Response};
use crate::repo::Repo;
use crate::trace::{self, Step, Tail};

#[derive(Debug, Serialize, JsonSchema)]
pub struct WatchOut {
    pub id: ItemId,
    pub state: State,
    pub now: Now,
    /// left_loop, needs_you, timeout.
    pub stopped: String,
    /// How many changes it printed.
    pub changes: usize,
}

impl Render for WatchOut {
    fn render(&self) -> String {
        let why = match self.stopped.as_str() {
            "needs_you" => "It needs you.",
            "timeout" => "Stopped watching (timeout).",
            _ => "It left the review loop.",
        };
        format!("{why}\n{}", self.now.render())
    }
}

/// One progress line.
#[derive(Serialize, JsonSchema)]
pub struct Change<'a> {
    pub t: chrono::DateTime<Utc>,
    pub id: ItemId,
    pub state: State,
    /// working, queued, running, waiting_for_you, held, stuck, done.
    pub status: &'a str,
    /// One line: what is happening.
    pub doing: &'a str,
    /// The tail of what is running (a gate's output).
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    pub log: &'a [String],
    /// With `--trace`: the new steps of the run that is going.
    #[serde(skip_serializing_if = "<[Step]>::is_empty")]
    pub steps: &'a [Step],
}

/// Follows the item's latest run for `--trace`. A run that had already
/// ended when watching began is not shown.
#[derive(Default)]
struct Tracer {
    run: Option<(PathBuf, Tail)>,
    started: bool,
}

impl Tracer {
    fn poll(&mut self, repo: &Repo, id: ItemId) -> Result<Vec<Step>> {
        let first = !std::mem::replace(&mut self.started, true);
        let Some(latest) = trace::runs(repo, id)?.pop() else {
            return Ok(Vec::new());
        };
        if self.run.as_ref().is_none_or(|(p, _)| *p != latest.path) {
            let mut tail = Tail::default();
            let steps = tail.poll(&latest.path)?;
            let old = first && tail.info.ended;
            self.run = Some((latest.path, tail));
            return Ok(if old { Vec::new() } else { steps });
        }
        match &mut self.run {
            Some((p, tail)) if !tail.info.ended => tail.poll(p),
            _ => Ok(Vec::new()),
        }
    }
}

pub fn watch(cwd: &Path, a: WatchArgs, mode: Mode) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    let id = repo.resolve(a.id.as_deref())?.id;
    let limit = crate::config::parse_duration(&a.timeout)
        .map(|s| Duration::from_secs(s.max(0) as u64))
        .ok_or_else(|| {
            HallError::usage(format!("--timeout {}: use a duration like 30m", a.timeout))
        })?;
    let interval = std::env::var("HALL_WATCH_INTERVAL")
        .ok()
        .and_then(|s| crate::daemon::run::parse_poll(&s))
        .unwrap_or(Duration::from_secs(2));
    let started = Instant::now();
    let mut last = String::new();
    let mut changes = 0;
    let mut seen_log: Vec<String> = Vec::new();
    let mut tracer = a.trace.then(Tracer::default);
    loop {
        let item = repo.hall.load_item(id)?;
        let events = repo.hall.read_item(id)?;
        let live = repo.index()?.items(false)?;
        let now = now::now(&repo, &item, &live, &events);
        let key = format!("{}|{}|{}", item.state, now.status, now.doing);
        let fresh: Vec<String> = now
            .log
            .iter()
            .filter(|l| !seen_log.contains(l))
            .cloned()
            .collect();
        let steps = match &mut tracer {
            Some(t) => t.poll(&repo, id)?,
            None => Vec::new(),
        };
        if key != last || !fresh.is_empty() || !steps.is_empty() {
            let mut out = std::io::stdout().lock();
            if mode == Mode::Json {
                let line = Change {
                    t: Utc::now(),
                    id,
                    state: item.state,
                    status: &now.status,
                    doing: &now.doing,
                    log: &fresh,
                    steps: &steps,
                };
                let _ = writeln!(out, "{}", serde_json::to_string(&line)?);
            } else {
                if key != last {
                    let _ = writeln!(
                        out,
                        "{} {:12} {}",
                        Utc::now().format("%H:%M:%S"),
                        item.state.as_str(),
                        now.doing
                    );
                }
                for l in &fresh {
                    let _ = writeln!(out, "             │ {l}");
                }
                for st in &steps {
                    let _ = writeln!(out, "             ┊ {}", st.render());
                }
            }
            let _ = out.flush();
            seen_log = now.log.clone();
            if key != last {
                changes += 1;
            }
            last = key;
        }
        let stopped = if !now.in_motion() {
            Some(
                if now.status == "done" || item.state == State::Development {
                    "left_loop"
                } else {
                    "needs_you"
                },
            )
        } else if started.elapsed() >= limit {
            Some("timeout")
        } else {
            None
        };
        if let Some(why) = stopped {
            let next = now.actions.iter().map(|a| a.command.clone()).collect();
            return Ok(Response::new(
                WatchOut {
                    id,
                    state: item.state,
                    now,
                    stopped: why.into(),
                    changes,
                },
                next,
            ));
        }
        crate::daemon::nap(interval);
    }
}
