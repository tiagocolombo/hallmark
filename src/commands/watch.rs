//! `hall watch <id>`: follow an item live. A line each time what it is
//! doing changes; it stops when the item leaves the loop, needs you, or the
//! timeout passes. Off a terminal each line is a JSON object, and the reply
//! is the last line, as for every command.

use std::io::Write;
use std::path::Path;
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
        if key != last || !fresh.is_empty() {
            let mut out = std::io::stdout().lock();
            if mode == Mode::Json {
                let line = Change {
                    t: Utc::now(),
                    id,
                    state: item.state,
                    status: &now.status,
                    doing: &now.doing,
                    log: &fresh,
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
        std::thread::sleep(interval);
    }
}
