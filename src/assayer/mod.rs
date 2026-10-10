//! The Assay Office (ADR 0015, ADR 0018): runs the gates at the frozen
//! commit in the assayer's own worktree, then the principal engineer's
//! review. One ticket at a time per repository; each tick moves one item
//! one step.
//!
//! - **Checks** (`checks`): the configured gates, in order, at the frozen
//!   commit. Red checks go to the fixer and never reach a model.
//! - **Review** (`review`): one `claude -p` run of the principal engineer,
//!   whose rules come from the base branch. Hallmark judges its verdict
//!   ([`record_verdict`]) and strikes the hallmark on approval.

pub mod base;
pub mod checks;
pub mod prompt;
pub mod review;
pub mod verdict;

use std::path::{Path, PathBuf};

use chrono::Duration;

use crate::config::Role;
use crate::error::{ErrorCode, HallError, Result};
use crate::git;
use crate::item::State;
use crate::lease;
use crate::paths;
use crate::repo::Repo;
use crate::worker::{Progress, WorkerCtx};

pub use verdict::{Recorded, record_verdict};

/// The lease role.
pub const ROLE: &str = "assayer";

/// Takes one ticket (one at a time per repo) and moves it one step:
/// checks, or review. The lease is released when the step ends.
pub fn tick(ctx: &WorkerCtx) -> Result<Progress> {
    let repo = ctx.repo;
    let Some((item, _)) = lease::claim_next(repo, ROLE, &ctx.holder, lease_ttl(repo), None)? else {
        return Ok(Progress::Idle);
    };
    let out = match item.state {
        State::Checks => checks::run(ctx, &item),
        State::Review => review::run(ctx, &item),
        _ => Ok(Progress::Idle),
    };
    if let Err(e) = lease::release(&repo.hall, item.id, &ctx.holder) {
        repo.hall
            .live(&format!("{} assayer lease not released: {e}", item.id));
    }
    out
}

/// Long enough for one gate or one review run; renewed between gates.
pub fn lease_ttl(repo: &Repo) -> Duration {
    let run = repo.config.budget.timeout_secs(Role::Reviewer).max(0);
    Duration::seconds(run.max(600) + 600)
}

/// The assayer's own worktree: a sibling of the main checkout,
/// `../<repo>-assay`, created once and reused so ignored build caches
/// survive between rounds.
pub fn worktree_path(repo: &Repo) -> PathBuf {
    let parent = repo.main_root.parent().unwrap_or(&repo.main_root);
    parent.join(format!("{}-assay", repo.name))
}

/// Checks out `sha` (detached) in the assay worktree, creating it when
/// needed. Tracked changes and untracked files from an earlier run are
/// discarded; ignored files (build caches) are kept.
pub fn checkout(repo: &Repo, sha: &str) -> Result<PathBuf> {
    let path = worktree_path(repo);
    let norm = paths::normalize(&path);
    let known = git::worktrees(&repo.main_root)?
        .iter()
        .any(|w| w.path == norm);
    if known && path.join(".git").exists() {
        git::run(&path, &["checkout", "-q", "--detach", "-f", sha])?;
        git::run(&path, &["clean", "-fdq"])?;
        return Ok(norm);
    }
    // A worktree whose directory is gone, or a stale registration.
    let _ = git::run(&repo.main_root, &["worktree", "prune"]);
    if path.exists() && !is_empty_dir(&path) {
        return Err(HallError::new(
            ErrorCode::CommandFailed,
            format!(
                "{} exists and is not this repo's assay worktree",
                paths::display(&path)
            ),
        )
        .next([format!(
            "move {} away, then run the assayer again",
            paths::display(&path)
        )]));
    }
    let p = path.to_string_lossy().into_owned();
    git::run(
        &repo.main_root,
        &["worktree", "add", "-q", "--detach", &p, sha],
    )?;
    Ok(paths::normalize(&path))
}

fn is_empty_dir(p: &Path) -> bool {
    std::fs::read_dir(p)
        .map(|mut d| d.next().is_none())
        .unwrap_or(false)
}
