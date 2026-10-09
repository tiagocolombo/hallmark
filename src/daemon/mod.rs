//! `hall daemon` (ADR 0014): one process for every registered repository.
//! Each pass runs the workers in `worker::ORDER` for each repo.

pub mod state;

use serde::Serialize;

use crate::error::Result;
use crate::repo::Repo;
use crate::runner::Runner;
use crate::worker::{ORDER, Progress, WorkerCtx};

/// One worker's result in a pass.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Step {
    pub repo: String,
    pub worker: String,
    #[serde(flatten)]
    pub progress: Progress,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One pass over one repository. A worker error is reported and the pass
/// goes on; a pause stops the pass.
pub fn run_once(repo: &Repo, runner: &dyn Runner, only: &[String]) -> Result<Vec<Step>> {
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
        let (progress, error) = match tick(&ctx) {
            Ok(p) => (p, None),
            Err(e) => {
                repo.hall.live(&format!("{name} error: {e}"));
                (Progress::Idle, Some(e.to_string()))
            }
        };
        let pause = matches!(progress, Progress::Pause { .. });
        steps.push(Step {
            repo: repo.name.clone(),
            worker: name.to_string(),
            progress,
            error,
        });
        if pause {
            break;
        }
    }
    Ok(steps)
}
