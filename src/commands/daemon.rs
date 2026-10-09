//! `hall daemon start|stop|status|run` (workstream A). The foundation
//! provides `run --once` for the current repo, so every worker can be
//! driven from tests.

use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::{DaemonAction, DaemonArgs};
use crate::daemon::state::DaemonState;
use crate::daemon::{self, Step};
use crate::error::{HallError, Result};
use crate::output::{Render, Response};
use crate::repo::Repo;

#[derive(Debug, Serialize, JsonSchema)]
pub struct DaemonOut {
    /// start, stop, status, run.
    pub action: String,
    /// Running now (a fresh heartbeat).
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<DaemonState>,
    /// What each worker did (`run`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Step>,
}

impl Render for DaemonOut {
    fn render(&self) -> String {
        let mut s = format!(
            "daemon {}\n",
            if self.running {
                "running"
            } else {
                "not running"
            }
        );
        for st in &self.steps {
            let what = match &st.progress {
                crate::worker::Progress::Idle => "idle".to_string(),
                crate::worker::Progress::Worked { what } => what.clone(),
                crate::worker::Progress::Pause { reason, .. } => format!("paused: {reason}"),
            };
            s.push_str(&format!("  {:10} {:10} {what}", st.repo, st.worker));
            if let Some(e) = &st.error {
                s.push_str(&format!("  error: {e}"));
            }
            s.push('\n');
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
        } => {
            let dir = repo
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| cwd.to_path_buf());
            let repo = Repo::discover(&dir)?;
            repo.require_init()?;
            let runner = crate::runner::from_env();
            let steps = daemon::run_once(&repo, runner.as_ref(), &only)?;
            let out = DaemonOut {
                action: "run".into(),
                running: false,
                state: None,
                steps,
            };
            Ok(Response::new(
                out,
                vec!["hall status".into(), "hall inbox".into()],
            ))
        }
        _ => Err(HallError::not_implemented(
            "hall daemon start/stop/status and the loop",
        )),
    }
}
