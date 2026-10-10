//! `hall daemon` (ADR 0014, ADR 0017): one process for every registered
//! repository. Each pass runs the workers in `worker::ORDER` for each repo;
//! repositories run in parallel (a thread each), one repo's workers run
//! one after another.

pub mod pause;
pub mod run;
pub mod service;
pub mod spawn;
pub mod state;
pub mod targets;

use std::collections::BTreeMap;
use std::sync::Mutex;

use chrono::Utc;
use serde::Serialize;

use crate::error::Result;
use crate::journal::Hall;
use crate::repo::Repo;
use crate::runner::Runner;
use crate::worker::{ORDER, Progress, WorkerCtx};

use state::{Pause, RepoPass};
use targets::{Skip, Target};

/// One worker's result in a pass.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Step {
    pub repo: String,
    pub worker: String,
    #[serde(flatten)]
    pub progress: Progress,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Why the worker didn't run (a pause skips the model workers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

/// One pass over one repository. A worker error is reported and the pass
/// goes on; a pause stops the pass.
pub fn run_once(repo: &Repo, runner: &dyn Runner, only: &[String]) -> Result<Vec<Step>> {
    pass(repo, runner, only, None, &|_| {})
}

/// One pass, honoring a pause in force (the model workers are skipped and
/// reported as such). `on_tick` hears each worker's name before it runs.
pub fn pass(
    repo: &Repo,
    runner: &dyn Runner,
    only: &[String],
    paused: Option<&Pause>,
    on_tick: &dyn Fn(&str),
) -> Result<Vec<Step>> {
    let ctx = WorkerCtx {
        repo,
        runner,
        holder: format!("daemon:{}", std::process::id()),
    };
    let mut steps = Vec::new();
    for (name, tick) in ORDER {
        if !only.is_empty() && !only.iter().any(|o| o == name) {
            continue;
        }
        if let Some(p) = paused
            && pause::is_model_worker(name)
        {
            steps.push(Step {
                repo: repo.name.clone(),
                worker: name.to_string(),
                progress: Progress::Idle,
                error: None,
                skipped: Some(match p.until {
                    Some(u) => format!("paused until {}", u.format("%Y-%m-%d %H:%M UTC")),
                    None => "paused".into(),
                }),
            });
            continue;
        }
        on_tick(name);
        let (progress, error) = match tick(&ctx) {
            Ok(p) => (p, None),
            Err(e) => {
                repo.hall.live(&format!("{name} error: {e}"));
                (Progress::Idle, Some(e.to_string()))
            }
        };
        if let Progress::Worked { what } = &progress {
            repo.hall.live(&format!("{name}: {what}"));
        }
        let pause = matches!(progress, Progress::Pause { .. });
        steps.push(Step {
            repo: repo.name.clone(),
            worker: name.to_string(),
            progress,
            error,
            skipped: None,
        });
        if pause {
            break;
        }
    }
    Ok(steps)
}

/// One line for a step that did something (worked, paused, failed).
pub fn progress_line(s: &Step) -> Option<String> {
    if let Some(e) = &s.error {
        return Some(format!("error: {e}"));
    }
    match &s.progress {
        Progress::Worked { what } => Some(what.clone()),
        Progress::Pause { reason, .. } => Some(format!("pause: {reason}")),
        Progress::Idle => None,
    }
}

/// What a pass did for one target.
#[derive(Debug)]
pub struct Outcome {
    pub target: Target,
    /// The repo's journal, when it could be opened.
    pub hall: Option<Hall>,
    pub steps: Vec<Step>,
    pub skip: Option<Skip>,
}

impl Outcome {
    /// The summary kept in the state file and shown by `hall daemon status`.
    pub fn summary(&self) -> RepoPass {
        let (result, notes) = match &self.skip {
            Some(s) => (s.result(), vec![s.note()]),
            None => {
                let mut notes = Vec::new();
                let mut result = "idle";
                for s in &self.steps {
                    if let Some(e) = &s.error {
                        notes.push(format!("{}: {e}", s.worker));
                        result = "error";
                    }
                    match &s.progress {
                        Progress::Worked { what } => {
                            notes.push(format!("{}: {what}", s.worker));
                            if result == "idle" {
                                result = "worked";
                            }
                        }
                        Progress::Pause { reason, .. } => {
                            notes.push(format!("{}: pause: {reason}", s.worker));
                            result = "paused";
                        }
                        Progress::Idle => {}
                    }
                }
                (result, notes)
            }
        };
        RepoPass {
            name: self.target.name.clone(),
            path: crate::paths::display(&self.target.path),
            at: Utc::now(),
            result: result.into(),
            notes,
        }
    }
}

/// Runs one pass over every target, a thread per repository. While the
/// threads work, `wait` is called every 200ms with what each repo is doing
/// now (`app: assayer · api: fixer`), so the caller can keep its heartbeat.
pub fn pass_all(
    targets: &[Target],
    only: &[String],
    paused: Option<&Pause>,
    wait: &mut dyn FnMut(Option<String>),
) -> Vec<Outcome> {
    let current: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());
    let now_doing = |current: &Mutex<BTreeMap<String, String>>| {
        let m = current.lock().map(|m| m.clone()).unwrap_or_default();
        (!m.is_empty()).then(|| {
            m.iter()
                .map(|(r, w)| format!("{r}: {w}"))
                .collect::<Vec<_>>()
                .join(" · ")
        })
    };
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    std::thread::scope(|sc| {
        let handles: Vec<_> = targets
            .iter()
            .map(|t| {
                let current = &current;
                let done = done_tx.clone();
                sc.spawn(move || {
                    let o = one(t, only, paused, current);
                    let _ = done.send(());
                    o
                })
            })
            .collect();
        drop(done_tx);
        let mut finished = 0;
        while finished < handles.len() {
            match done_rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(()) => finished += 1,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                // A thread panicked before it could say so.
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
            wait(now_doing(&current));
        }
        handles
            .into_iter()
            .zip(targets)
            .map(|(h, t)| {
                h.join().unwrap_or_else(|_| Outcome {
                    target: t.clone(),
                    hall: None,
                    steps: vec![],
                    skip: Some(Skip::Error("the pass panicked".into())),
                })
            })
            .collect()
    })
}

/// One pass over one target; what each repo's own schedule runs.
pub(crate) fn one(
    t: &Target,
    only: &[String],
    paused: Option<&Pause>,
    current: &Mutex<BTreeMap<String, String>>,
) -> Outcome {
    let repo = match targets::open(t) {
        Ok(r) => r,
        Err(skip) => {
            return Outcome {
                target: t.clone(),
                hall: None,
                steps: vec![],
                skip: Some(skip),
            };
        }
    };
    let runner = crate::runner::from_env();
    let key = t.name.clone();
    let on_tick = |w: &str| {
        if let Ok(mut m) = current.lock() {
            m.insert(key.clone(), w.to_string());
        }
    };
    let result = pass(&repo, runner.as_ref(), only, paused, &on_tick);
    if let Ok(mut m) = current.lock() {
        m.remove(&key);
    }
    match result {
        Ok(steps) => Outcome {
            target: t.clone(),
            hall: Some(repo.hall.clone()),
            steps,
            skip: None,
        },
        Err(e) => Outcome {
            target: t.clone(),
            hall: Some(repo.hall.clone()),
            steps: vec![],
            skip: Some(Skip::Error(e.to_string())),
        },
    }
}

/// After a pass: start a pause if a worker asked for one. Every journal the
/// pass reached records it. Returns the new pause.
pub fn record_pause(
    st: &mut state::DaemonState,
    outcomes: &[Outcome],
    now: chrono::DateTime<Utc>,
) -> Result<Option<Pause>> {
    let steps: Vec<Step> = outcomes.iter().flat_map(|o| o.steps.clone()).collect();
    let Some((repo, reason, until)) = pause::requested(&steps) else {
        return Ok(None);
    };
    let journals: Vec<Hall> = outcomes.iter().filter_map(|o| o.hall.clone()).collect();
    pause::begin(st, &repo, &reason, until, &journals, now)
}
