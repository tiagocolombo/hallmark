//! `hall trace [<id>]`: a headless run, step by step (ADR 0039). It prints
//! the item's latest run, or the one `--role` or `--run` picks; `--follow`
//! keeps printing until the run ends, `--list` lists the runs and `--open`
//! gives the `claude --resume` command for an ended run. Off a terminal
//! each step is a JSON line and the reply is the last line, as for
//! `hall watch`.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::TraceArgs;
use crate::error::{ErrorCode, HallError, Result};
use crate::item::ItemId;
use crate::output::{Mode, Render, Response};
use crate::repo::Repo;
use crate::trace::{self, RunFile, RunInfo, Step, Tail};

/// One run of an item.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RunView {
    /// Its place among the item's runs, 1 = oldest (`--run <n>`).
    pub n: usize,
    /// reviewer, fixer, intake, librarian, resume…
    pub role: String,
    pub started: DateTime<Utc>,
    /// The run's file in the item's `runs/`.
    pub path: String,
    /// A `.jsonl` stream; false for an older `.json` result.
    pub streamed: bool,
    /// The process exited. A run without it is still going, or the daemon
    /// died during it.
    pub ended: bool,
    /// No end line, and nothing is writing it: the daemon stopped mid-run.
    /// It can be opened, but has no more steps to follow.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub interrupted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The command that opens the run in Claude Code, once it has ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume: Option<String>,
}

impl RunView {
    fn new(n: usize, f: &RunFile, info: &RunInfo) -> RunView {
        RunView {
            n,
            role: f.role.clone(),
            started: f.started,
            path: crate::paths::display(&f.path),
            streamed: f.streamed,
            ended: info.ended,
            interrupted: info.interrupted,
            failure: info.failure.clone(),
            session_id: info.session_id.clone(),
            model: info.model.clone(),
            resume: info.resume_command(),
        }
    }

    fn status(&self) -> &'static str {
        match (self.ended, self.failure.is_some()) {
            (false, _) if self.interrupted => "interrupted",
            (false, _) => "running",
            (true, true) => "failed",
            (true, false) => "ended",
        }
    }
}

/// The reply of `hall trace`. The steps come before it, one per line.
#[derive(Debug, Serialize, JsonSchema)]
pub struct TraceOut {
    pub id: ItemId,
    /// The run shown, or opened; none when the item has no such run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<RunView>,
    /// How many runs the item has.
    pub total: usize,
    /// How many steps were printed before this reply.
    pub steps: usize,
    /// With `--follow`: ended, interrupted, or timeout.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped: Option<String>,
    /// With `--list`: every run, oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<RunView>,
    #[serde(skip)]
    #[schemars(skip)]
    view: View,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum View {
    #[default]
    Steps,
    List,
    Open,
}

impl Render for TraceOut {
    fn render(&self) -> String {
        if self.view == View::List {
            if self.runs.is_empty() {
                return format!("{} has no runs.", self.id);
            }
            let mut s = format!("Runs of {}, oldest first:\n", self.id);
            for r in &self.runs {
                s.push_str(&format!(
                    "  {:>3}  {}  {:10} {}\n",
                    r.n,
                    r.started
                        .with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M:%S"),
                    r.role,
                    r.status()
                ));
            }
            return s;
        }
        let Some(r) = &self.run else {
            return format!("{} has no runs yet.", self.id);
        };
        if self.view == View::Open {
            return format!(
                "Open the {} run ({} of {}) in Claude Code:\n  {}",
                r.role,
                r.n,
                self.total,
                r.resume.as_deref().unwrap_or("")
            );
        }
        let what = match self.stopped.as_deref() {
            Some("timeout") => "Stopped following (timeout); it is still going.".to_string(),
            _ if r.interrupted => {
                "It stopped without ending: the daemon was interrupted.".to_string()
            }
            _ if !r.ended => "It is still going.".to_string(),
            _ => match &r.failure {
                Some(f) => format!("It failed: {f}"),
                None => "It ended.".to_string(),
            },
        };
        format!("{} run {} of {}. {what}", r.role, r.n, self.total)
    }
}

pub fn trace(cwd: &Path, a: TraceArgs, mode: Mode) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    let id = repo.resolve(a.id.as_deref())?.id;
    let limit = crate::config::parse_duration(&a.timeout)
        .map(|s| Duration::from_secs(s.max(0) as u64))
        .ok_or_else(|| {
            HallError::usage(format!("--timeout {}: use a duration like 30m", a.timeout))
        })?;
    let all = trace::runs(&repo, id)?;
    let total = all.len();
    // Every run with its place, 1 = oldest; `--role` narrows them.
    let numbered: Vec<(usize, &RunFile)> = all
        .iter()
        .enumerate()
        .map(|(i, f)| (i + 1, f))
        .filter(|(_, f)| a.role.as_deref().is_none_or(|r| f.role == r))
        .collect();

    if a.list {
        let mut runs = Vec::new();
        for (n, f) in &numbered {
            let (_, mut info) = trace::read(&f.path)?;
            trace::mark_interrupted(&repo.hall,id, &all, n - 1, &mut info);
            runs.push(RunView::new(*n, f, &info));
        }
        let next = match runs.last() {
            Some(r) => vec![
                format!("hall trace {id} --run {}", r.n),
                format!("hall status {id}"),
            ],
            None => vec![format!("hall status {id}")],
        };
        return Ok(Response::new(
            TraceOut {
                id,
                run: None,
                total,
                steps: 0,
                stopped: None,
                runs,
                view: View::List,
            },
            next,
        ));
    }

    let chosen = match a.run {
        Some(n) => Some(
            numbered
                .iter()
                .find(|(i, _)| *i == n)
                .copied()
                .ok_or_else(|| no_such_run(id, n, total))?,
        ),
        None => numbered.last().copied(),
    };
    let Some((n, file)) = chosen else {
        // Nothing has run for it yet: a reply that says so, not an error.
        if a.open {
            let what = a
                .role
                .as_deref()
                .map(|r| format!("{r} "))
                .unwrap_or_default();
            return Err(HallError::new(
                ErrorCode::ItemNotFound,
                format!("{id} has no {what}run to open"),
            )
            .next([
                format!("hall trace {id} --list"),
                format!("hall status {id}"),
            ]));
        }
        return Ok(Response::new(
            TraceOut {
                id,
                run: None,
                total,
                steps: 0,
                stopped: None,
                runs: vec![],
                view: View::Steps,
            },
            vec![format!("hall status {id}"), format!("hall watch {id}")],
        ));
    };

    if a.open {
        let (_, mut info) = trace::read(&file.path)?;
        trace::mark_interrupted(&repo.hall,id, &all, n - 1, &mut info);
        let run = RunView::new(n, file, &info);
        let Some(cmd) = run.resume.clone() else {
            let (why, next) = if !run.ended {
                (
                    format!(
                        "the {} run of {id} is still going; it can be opened once it ends",
                        run.role
                    ),
                    vec![format!("hall trace {id} --run {n} --follow")],
                )
            } else {
                (
                    format!("the {} run of {id} has no session to resume", run.role),
                    vec![format!("hall trace {id} --run {n}")],
                )
            };
            return Err(HallError::new(ErrorCode::WrongState, why).next(next));
        };
        return Ok(Response::new(
            TraceOut {
                id,
                run: Some(run),
                total,
                steps: 0,
                stopped: None,
                runs: vec![],
                view: View::Open,
            },
            vec![cmd, format!("hall trace {id} --run {n}")],
        ));
    }

    let interval = std::env::var("HALL_WATCH_INTERVAL")
        .ok()
        .and_then(|s| crate::daemon::run::parse_poll(&s))
        .unwrap_or(Duration::from_secs(1));
    let started = Instant::now();
    let mut tail = Tail::default();
    let mut printed = 0;
    if mode == Mode::Text {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(
            out,
            "{id} · {} run {n} of {total}, started {}",
            file.role,
            file.started
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
        );
    }
    let stopped = loop {
        let steps = tail.poll(&file.path)?;
        printed += steps.len();
        print_steps(&steps, mode, "")?;
        if !a.follow {
            break None;
        }
        if tail.info.ended {
            break Some("ended");
        }
        trace::mark_interrupted(&repo.hall,id, &all, n - 1, &mut tail.info);
        if tail.info.interrupted {
            break Some("interrupted");
        }
        if started.elapsed() >= limit {
            break Some("timeout");
        }
        crate::daemon::nap(interval);
    };
    trace::mark_interrupted(&repo.hall,id, &all, n - 1, &mut tail.info);
    let run = RunView::new(n, file, &tail.info);
    let mut next = Vec::new();
    if run.ended || run.interrupted {
        if run.resume.is_some() {
            next.push(format!("hall trace {id} --run {n} --open"));
        }
    } else {
        next.push(format!("hall trace {id} --run {n} --follow"));
    }
    next.push(format!("hall trace {id} --list"));
    next.push(format!("hall status {id}"));
    Ok(Response::new(
        TraceOut {
            id,
            run: Some(run),
            total,
            steps: printed,
            stopped: stopped.map(String::from),
            runs: vec![],
            view: View::Steps,
        },
        next,
    ))
}

/// Prints steps as they arrive: a JSON line each off a terminal, else
/// `Step::render` lines after `indent`.
pub fn print_steps(steps: &[Step], mode: Mode, indent: &str) -> Result<()> {
    if steps.is_empty() {
        return Ok(());
    }
    let mut out = std::io::stdout().lock();
    for s in steps {
        if mode == Mode::Json {
            let _ = writeln!(out, "{}", serde_json::to_string(s)?);
        } else {
            let _ = writeln!(out, "{indent}{}", s.render());
        }
    }
    let _ = out.flush();
    Ok(())
}

fn no_such_run(id: ItemId, n: usize, total: usize) -> HallError {
    let what = match total {
        0 => format!("{id} has no runs"),
        1 => format!("{id} has 1 run; there is no run {n}"),
        _ => format!("{id} has {total} runs; there is no run {n}"),
    };
    HallError::new(ErrorCode::Usage, what).next([format!("hall trace {id} --list")])
}
