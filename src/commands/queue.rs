//! `hall queue`: the merge queue (ADR 0048). Read-only: it judges every
//! open shipped PR the way the merger would, and says why each one waits.

use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;

use crate::error::Result;
use crate::merger::{Entry, Status};
use crate::output::{Render, Response};
use crate::repo::Repo;

#[derive(Debug, Serialize, JsonSchema)]
pub struct QueueOut {
    /// `[merge] mode`: `auto` when the merger merges, `off` when you do.
    pub mode: String,
    /// `[merge] require_ci`.
    pub require_ci: bool,
    /// Every open shipped PR, by base and in the order the merger takes them.
    pub queue: Vec<Entry>,
}

impl Render for QueueOut {
    fn render(&self) -> String {
        let mut s = format!(
            "Merge queue (merger {}{}):\n",
            if self.mode == "auto" { "on" } else { "off" },
            if self.require_ci {
                ""
            } else {
                ", CI not required"
            }
        );
        if self.queue.is_empty() {
            s.push_str("  no open PRs\n");
        }
        let mut base = "";
        for e in &self.queue {
            if e.base != base {
                base = &e.base;
                s.push_str(&format!("  into {base}:\n"));
            }
            let mark = match e.status {
                Status::Ready => "→",
                Status::Merging => "✓",
                Status::Updating => "↻",
                _ => " ",
            };
            s.push_str(&format!(
                "  {mark} {:>2}. {} {} {}\n       {}: {}\n",
                e.position,
                e.item,
                pr_ref(&e.pr_url),
                e.title,
                e.status.as_str().replace('_', " "),
                e.why
            ));
        }
        s
    }
}

fn pr_ref(url: &str) -> String {
    crate::merger::number(url)
        .map(|n| format!("#{n}"))
        .unwrap_or_else(|| url.to_string())
}

pub fn queue(cwd: &Path) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    repo.require_init()?;
    let queue = crate::merger::queue(&repo)?;
    let m = &repo.config.merge;
    let mut next = Vec::new();
    if m.mode == crate::config::MergeMode::Off {
        next.push("hall config set merge.mode auto".to_string());
    }
    if let Some(e) = queue.iter().find(|e| e.status != Status::Ready) {
        next.push(format!("hall status {}", e.item));
    }
    next.push("hall daemon status".into());
    Ok(Response::new(
        QueueOut {
            mode: m.mode.as_str().into(),
            require_ci: m.require_ci,
            queue,
        },
        next,
    ))
}
