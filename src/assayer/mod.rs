//! The Assay Office (ADR 0015, ADR 0018): runs the gates at the frozen
//! commit in the assayer's own worktree, then the principal engineer's
//! review. Up to `[parallel] checks` and `review` tickets at once per
//! repository (ADR 0026); each tick moves one item one step.
//!
//! - **Checks** (`checks`): the configured gates, in order, at the frozen
//!   commit. Red checks go to the fixer and never reach a model.
//! - **Review** (`review`): one `claude -p` run of the principal engineer,
//!   whose rules come from the base branch. Hallmark judges its verdict
//!   ([`record_verdict`]) and strikes the hallmark on approval.

pub mod base;
pub mod cache;
pub mod checks;
pub mod prompt;
pub mod rebased;
pub mod review;
pub mod security;
pub mod verdict;
pub mod verify;

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

/// Takes one ticket (within the phase's `[parallel]` limit) and moves it one step:
/// checks, or review. The lease is released when the step ends.
pub fn tick(ctx: &WorkerCtx) -> Result<Progress> {
    let repo = ctx.repo;
    let Some((item, _)) = lease::claim_next(repo, ROLE, &ctx.holder, lease_ttl(repo), None)? else {
        return Ok(Progress::Idle);
    };
    let out = match item.state {
        State::Checks => match checks::run(ctx, &item) {
            // Every check slot is taken: wait for one, that is not an error.
            Err(e) if e.code == ErrorCode::Claimed => Ok(Progress::Idle),
            r => r,
        },
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
    lease_ttl_for(&repo.config)
}

/// [`lease_ttl`] for a config: outlasts the reviewer and the slowest gate.
pub fn lease_ttl_for(config: &crate::config::Config) -> Duration {
    let mut run = config.budget.timeout_secs(Role::Reviewer).max(0);
    // The verifier runs after the reviewer, under the same lease.
    let v = config.verify.trim();
    if !v.is_empty() && !v.eq_ignore_ascii_case(verify::OFF) {
        run += verify::TIMEOUT_SECS as i64;
    }
    let gate = config.longest_gate_secs() as i64;
    Duration::seconds(run.max(gate).max(600) + 600)
}

/// The assayer's own worktree: a sibling of the main checkout,
/// `../<repo>-assay`, created once and reused so ignored build caches
/// survive between rounds.
pub fn worktree_path(repo: &Repo) -> PathBuf {
    let parent = repo.main_root.parent().unwrap_or(&repo.main_root);
    parent.join(format!("{}-assay", repo.name))
}

/// A checkout the assayer works in, at one commit. A check slot is locked
/// for as long as this lives, so two checks never share a worktree.
pub struct Checkout {
    pub path: PathBuf,
    _lock: Option<std::fs::File>,
}

/// Checks out `sha` for the checks in a free slot of the assay pool:
/// `../<repo>-assay`, then `../<repo>-assay-2`, … up to `[parallel] checks`.
/// Slots are reused, so ignored build caches stay warm; tracked changes and
/// untracked files from an earlier run are discarded.
pub fn checkout_checks(repo: &Repo, sha: &str) -> Result<Checkout> {
    use fs4::FileExt;
    let slots = repo.config.parallel.checks.max(1);
    let dir = repo.hall.root.join("slots");
    std::fs::create_dir_all(&dir)?;
    for k in 1..=slots {
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!("checks-{k}.lock")))?;
        if FileExt::try_lock(&f).is_err() {
            continue;
        }
        let path = if k == 1 {
            worktree_path(repo)
        } else {
            let p = worktree_path(repo);
            p.with_file_name(format!(
                "{}-{k}",
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ))
        };
        let path = checkout_at(repo, &path, sha)?;
        return Ok(Checkout {
            path,
            _lock: Some(f),
        });
    }
    Err(HallError::new(
        ErrorCode::Claimed,
        format!("every check slot ({slots}) is busy"),
    )
    .next(["hall daemon status"]))
}

/// Checks out `sha` for an item's review: its own worktree,
/// `../<repo>-review-<id>`, so reviews run side by side. The item's lease
/// keeps it to one review at a time.
pub fn checkout_review(repo: &Repo, item: crate::item::ItemId, sha: &str) -> Result<Checkout> {
    let p = worktree_path(repo);
    let path = p.with_file_name(format!("{}-review-{item}", repo.name));
    Ok(Checkout {
        path: checkout_at(repo, &path, sha)?,
        _lock: None,
    })
}

/// Removes an item's review checkout once its verdict is recorded.
pub fn remove_review_checkout(repo: &Repo, item: crate::item::ItemId) {
    let p = worktree_path(repo);
    let path = p.with_file_name(format!("{}-review-{item}", repo.name));
    if path.exists() {
        let _ = git::run(
            &repo.main_root,
            &["worktree", "remove", "--force", &path.to_string_lossy()],
        );
    }
}

/// Checks out `sha` (detached) in the worktree at `path`, creating it when
/// needed. Tracked changes and untracked files from an earlier run are
/// discarded; ignored files (build caches) are kept.
fn checkout_at(repo: &Repo, path: &Path, sha: &str) -> Result<PathBuf> {
    let norm = paths::normalize(path);
    let known = git::worktrees(&repo.main_root)?
        .iter()
        .any(|w| w.path == norm);
    if known && path.join(".git").exists() {
        git::run(path, &["checkout", "-q", "--detach", "-f", sha])?;
        git::run(path, &["clean", "-fdq"])?;
        return Ok(norm);
    }
    // A worktree whose directory is gone, or a stale registration.
    let _ = git::run(&repo.main_root, &["worktree", "prune"]);
    if path.exists() && !is_empty_dir(path) {
        return Err(HallError::new(
            ErrorCode::CommandFailed,
            format!(
                "{} exists and is not one of this repo's assay worktrees",
                paths::display(path)
            ),
        )
        .next([format!(
            "move {} away, then run the assayer again",
            paths::display(path)
        )]));
    }
    let p = path.to_string_lossy().into_owned();
    git::run(
        &repo.main_root,
        &["worktree", "add", "-q", "--detach", &p, sha],
    )?;
    Ok(paths::normalize(path))
}

fn is_empty_dir(p: &Path) -> bool {
    std::fs::read_dir(p)
        .map(|mut d| d.next().is_none())
        .unwrap_or(false)
}

#[cfg(test)]
mod lease_tests {
    use super::*;

    #[test]
    fn a_slow_gate_gets_a_lease_longer_than_itself() {
        let c = crate::config::Config::parse("gates = [{ cmd = 'cargo test', timeout = '2h' }]\n")
            .unwrap();
        assert!(lease_ttl_for(&c).num_seconds() > 7200);
        assert!(lease_ttl_for(&crate::config::Config::default()).num_seconds() < 7200);
    }
}
