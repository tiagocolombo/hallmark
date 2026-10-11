//! The Delivery Office (ADR 0030): after the Dispatch Dock ships a PR, it
//! watches the PR until it merges, and keeps it mergeable.
//!
//! One tick does at most one thing, in this order:
//!
//! 1. **Rebase.** An item in development with a rebase request (its PR
//!    conflicts with its base, or the Dispatch Dock hit a conflict) is
//!    rebased onto the latest base in its worktree. A clean rebase goes
//!    straight to review. A conflict goes to a headless dev run
//!    (`claude -p`), which resolves it and finishes the rebase. **A script
//!    decides** whether it worked: the rebase finished, the worktree is
//!    clean, and the base is an ancestor of HEAD. Then the item is sent to
//!    review (`ready`), and once hallmarked the shipper updates the PR. A
//!    failed run is retried once; the second failure holds the item
//!    (`conflict`) for you.
//! 2. **Watch.** A shipped item's PR is read at most every two minutes
//!    (`gh pr view`). Merged or closed: `delivered`, and the watch ends.
//!    Conflicting: the item is reopened to development with a rebase
//!    request, so step 1 takes it on the next tick.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::config::Role;
use crate::error::Result;
use crate::event::{Body, Event, short};
use crate::git;
use crate::item::{HoldKind, Item, ItemId, State};
use crate::journal::Ticket;
use crate::lease;
use crate::repo::Repo;
use crate::runner::{self, Failure, Metered};
use crate::worker::{Progress, WorkerCtx};

pub const ACTOR: &str = "delivery";

/// How often one PR is read.
const EVERY: i64 = 120;

/// A base branch, resolved to the ref a rebase goes onto.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    /// What git commands use: `refs/remotes/origin/main` or `refs/heads/main`.
    pub git_ref: String,
    /// What people read: `origin/main` or `main`.
    pub name: String,
}

/// Fetches `origin <base>` and returns `origin/<base>`, else the local
/// branch when the remote doesn't have it (or can't be reached). None when
/// neither exists.
pub fn base_ref(wt: &Path, base: &str) -> Option<Target> {
    if git::ok(wt, &["remote", "get-url", "origin"]) {
        let _ = git::run(
            wt,
            &[
                "fetch",
                "--quiet",
                "origin",
                &format!("+refs/heads/{base}:refs/remotes/origin/{base}"),
            ],
        );
        let remote = format!("refs/remotes/origin/{base}");
        if git::rev(wt, &remote).is_some() {
            return Some(Target {
                git_ref: remote,
                name: format!("origin/{base}"),
            });
        }
    }
    let local = format!("refs/heads/{base}");
    git::rev(wt, &local).map(|_| Target {
        git_ref: local,
        name: base.to_string(),
    })
}

/// What a rebase did.
#[derive(Debug, Clone, PartialEq)]
pub enum Rebased {
    /// The target is already in HEAD.
    UpToDate,
    /// Rebased without conflicts. `old_base` is where the commits started.
    Clean {
        from: String,
        to: String,
        old_base: String,
    },
    /// Stopped on conflicts; the rebase is left in progress.
    Conflict { files: Vec<String> },
    /// Couldn't start (say, a dirty worktree); nothing changed.
    Failed(String),
}

/// Rebases the commits HEAD has since `upstream` onto `target`.
pub fn rebase(wt: &Path, target: &str, upstream: &str) -> Result<Rebased> {
    let head = git::head(wt)?;
    if git::is_ancestor(wt, target, &head) {
        return Ok(Rebased::UpToDate);
    }
    let Some(old_base) = git::merge_base(wt, &head, upstream) else {
        return Ok(Rebased::Failed(format!(
            "{} shares no history with {upstream}",
            short(&head)
        )));
    };
    // No editor, and the repo's own pre-rebase hook doesn't decide.
    match git::run(
        wt,
        &[
            "-c",
            "core.editor=true",
            "rebase",
            "--no-verify",
            "--onto",
            target,
            &old_base,
        ],
    ) {
        Ok(_) => Ok(Rebased::Clean {
            from: head,
            to: git::head(wt)?,
            old_base,
        }),
        Err(_) if git::rebase_in_progress(wt) => Ok(Rebased::Conflict {
            files: git::conflicted(wt),
        }),
        Err(e) => Ok(Rebased::Failed(e.message)),
    }
}

pub fn tick(ctx: &WorkerCtx) -> Result<Progress> {
    if let Some(p) = rebase_next(ctx)? {
        return Ok(p);
    }
    watch_next(ctx)
}

// --- 1. rebase ---------------------------------------------------------

fn rebase_next(ctx: &WorkerCtx) -> Result<Option<Progress>> {
    let repo = ctx.repo;
    let ttl = Duration::seconds(repo.config.budget.timeout_secs(Role::Fixer).max(60) + 600);
    let Some((item, _)) = lease::claim_next(repo, ACTOR, &ctx.holder, ttl, None)? else {
        return Ok(None);
    };
    let r = work(ctx, &item);
    let _ = lease::release(&repo.hall, item.id, &ctx.holder);
    r.map(Some)
}

fn work(ctx: &WorkerCtx, item: &Item) -> Result<Progress> {
    let repo = ctx.repo;
    let id = item.id;
    let Some(req) = item.rebase.clone() else {
        return Ok(Progress::Idle);
    };
    let wt = PathBuf::from(&item.worktree);
    if !wt.is_dir() {
        return held(
            repo,
            id,
            format!(
                "the worktree {} is missing; nothing to rebase",
                item.worktree
            ),
        );
    }
    if git::rebase_in_progress(&wt) {
        return held(
            repo,
            id,
            format!(
                "a rebase is already in progress in {}; finish it (`git rebase --continue`) \
                 or abort it, then `hall retry {id}`",
                item.worktree
            ),
        );
    }
    let dirty = git::dirty_files(&wt)?;
    if !dirty.is_empty() {
        return held(
            repo,
            id,
            format!(
                "the worktree has uncommitted changes ({}); they aren't the Delivery office's \
                 to commit",
                dirty.join(", ")
            ),
        );
    }
    let Some(target) = base_ref(&wt, &req.onto) else {
        return held(
            repo,
            id,
            format!("neither origin/{} nor {} exists", req.onto, req.onto),
        );
    };
    repo.hall.live(&format!(
        "{id} delivery: rebasing {} onto {}",
        item.branch, target.name
    ));
    match rebase(&wt, &target.git_ref, &target.git_ref)? {
        Rebased::UpToDate | Rebased::Clean { .. } => send_to_review(repo, id, &wt, &target),
        Rebased::Failed(why) => held(repo, id, format!("the rebase couldn't start: {why}")),
        Rebased::Conflict { files } => resolve(ctx, item, &wt, &target, &files),
    }
}

/// The headless dev run: resolve the conflicts and finish the rebase.
fn resolve(
    ctx: &WorkerCtx,
    item: &Item,
    wt: &Path,
    target: &Target,
    files: &[String],
) -> Result<Progress> {
    let repo = ctx.repo;
    let id = item.id;
    repo.hall.live(&format!(
        "{id} delivery: conflicts in {}; a dev run resolves them",
        files.join(", ")
    ));
    let events = repo.hall.read_item(id)?;
    let mut req = runner::request(
        repo,
        Role::Fixer,
        None,
        prompt(repo, item, &events, target, files),
        wt,
    )?;
    req.permission_mode = Some("acceptEdits".into());
    req.allowed_tools = tools(&repo.config.gates);
    for (k, v) in req.env.iter_mut() {
        if k == "HALL_ACTOR" {
            *v = ACTOR.into();
        }
    }
    req.env.push(("GIT_EDITOR".into(), "true".into()));
    req.env.extend(crate::fixer::path_with_hall());
    let outcome = runner::run_metered(repo, ctx.runner, Some(id), &req)?;
    match outcome {
        Metered::Blocked(stop) => {
            abort(wt);
            Ok(stop.progress())
        }
        Metered::Failed {
            failure: Failure::RateLimited { until },
            ..
        } => {
            abort(wt);
            Ok(Progress::Pause {
                reason: "Claude's plan limit".into(),
                until,
            })
        }
        // The runner counted the failure (and held the item on the second).
        Metered::Failed {
            failure, held: h, ..
        } => {
            abort(wt);
            Ok(Progress::worked(format!(
                "{id} rebase run failed{}: {}",
                if h { " (held)" } else { "" },
                failure.describe()
            )))
        }
        // Whatever the run said, the worktree decides.
        Metered::Done(_) => settle(repo, id, wt, target),
    }
}

/// After the dev run: done only if the rebase finished cleanly on top of
/// the target.
fn settle(repo: &Repo, id: ItemId, wt: &Path, target: &Target) -> Result<Progress> {
    let why = if git::rebase_in_progress(wt) {
        Some(format!(
            "the rebase is still in progress (conflicts left: {})",
            git::conflicted(wt).join(", ")
        ))
    } else if !git::dirty_files(wt)?.is_empty() {
        Some("the run left uncommitted changes".to_string())
    } else if !git::is_ancestor(wt, &target.git_ref, &git::head(wt)?) {
        Some(format!("HEAD isn't on top of {}", target.name))
    } else {
        None
    };
    match why {
        None => send_to_review(repo, id, wt, target),
        Some(why) => {
            // Back to where it was, so a retry (or you) starts clean. The
            // uncommitted case is kept for you.
            if git::rebase_in_progress(wt) {
                abort(wt);
            }
            failed(repo, id, why)
        }
    }
}

fn abort(wt: &Path) {
    if git::rebase_in_progress(wt) {
        let _ = git::run(wt, &["rebase", "--abort"]);
    }
}

/// Sends the rebased item to review, as `hall ready` would.
fn send_to_review(repo: &Repo, id: ItemId, wt: &Path, target: &Target) -> Result<Progress> {
    let head = git::head(wt)?;
    let note = format!("rebased onto {}", target.name);
    let (item, sent) = repo.hall.transact(id, |it, _| {
        if it.state != State::Development || it.rebase.is_none() {
            return Ok((vec![], false));
        }
        Ok((
            vec![Event::new(
                ACTOR,
                Some(id),
                Body::Ready {
                    sha: head.clone(),
                    round: it.round + 1,
                    note: Some(note.clone()),
                },
            )],
            true,
        ))
    })?;
    if !sent {
        return Ok(Progress::worked(format!("{id} was already {}", item.state)));
    }
    repo.hall.queue(&Ticket {
        id,
        sha: head.clone(),
        round: item.round,
        queued_at: Utc::now(),
    })?;
    let what = format!(
        "{id} {note} at {}; sent to review (round {})",
        short(&head),
        item.round
    );
    repo.hall.live(&what);
    Ok(Progress::worked(what))
}

/// A failed attempt, counted like the fixer's: the next tick retries once,
/// the second failure in a row holds the item.
fn failed(repo: &Repo, id: ItemId, reason: String) -> Result<Progress> {
    // Failed runs (counted by the runner as the fixer role's) and failed
    // results count together.
    let events = repo.hall.read_item(id)?;
    let attempt = runner::failures_in_a_row(&events, ACTOR)
        + runner::failures_in_a_row(&events, Role::Fixer.as_str())
        + 1;
    append(
        repo,
        id,
        Body::RunFailed {
            role: ACTOR.into(),
            reason: reason.clone(),
            attempt,
        },
    )?;
    repo.hall
        .live(&format!("{id} rebase failed (attempt {attempt}): {reason}"));
    if attempt >= 2 {
        return held(repo, id, reason);
    }
    Ok(Progress::worked(format!(
        "{id} rebase failed (attempt {attempt}): {reason}"
    )))
}

fn held(repo: &Repo, id: ItemId, reason: String) -> Result<Progress> {
    append(
        repo,
        id,
        Body::Held {
            kind: HoldKind::Conflict,
            reason: reason.clone(),
        },
    )?;
    repo.hall.live(&format!("{id} held (conflict): {reason}"));
    Ok(Progress::worked(format!("{id} held: {reason}")))
}

fn append(repo: &Repo, id: ItemId, body: Body) -> Result<()> {
    repo.hall
        .append_item(id, &Event::new(ACTOR, Some(id), body))
}

/// The dev run's tools: read and edit files, stage them, continue the
/// rebase, commit, and run the repo's gates. Never push, never abort.
pub fn tools(gates: &[String]) -> Vec<String> {
    let mut t: Vec<String> = [
        "Read",
        "Edit",
        "Write",
        "Grep",
        "Glob",
        "Bash(git status:*)",
        "Bash(git diff:*)",
        "Bash(git log:*)",
        "Bash(git show:*)",
        "Bash(git add:*)",
        "Bash(git rebase --continue:*)",
        "Bash(git commit:*)",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    for g in gates {
        let g = g.trim();
        if g.is_empty() {
            continue;
        }
        let rule = format!("Bash({g}:*)");
        if !t.contains(&rule) {
            t.push(rule);
        }
    }
    t
}

fn prompt(repo: &Repo, item: &Item, events: &[Event], target: &Target, files: &[String]) -> String {
    let mut s = format!(
        "You are Hallmark's development agent for {id} \"{title}\", in its worktree {wt}.\n\n\
         Its branch {branch} is being rebased onto {onto}, and the rebase stopped on conflicts \
         in: {files}.\n\n",
        id = item.id,
        title = item.title,
        wt = item.worktree,
        branch = item.branch,
        onto = target.name,
        files = if files.is_empty() {
            "(see `git status`)".to_string()
        } else {
            files.join(", ")
        },
    );
    if let Some(r) = crate::fixer::request(events) {
        s.push_str(&format!("What the item does:\n\n{}\n\n", r.trim()));
    }
    s.push_str(
        "Finish the rebase:\n\n\
         1. For each conflicted file, read both sides and what each change meant \
         (`git log`, `git show`, `git diff`), and edit the file so it keeps both: the base's \
         changes and this item's. Remove every conflict marker.\n\
         2. `git add` each resolved file, then `git rebase --continue`. Repeat for every commit \
         that stops.\n",
    );
    if repo.config.gates.is_empty() {
        s.push_str("3. When the rebase is done, check that the code still builds.\n");
    } else {
        s.push_str(&format!(
            "3. When the rebase is done, run the gates ({}) and fix anything the rebase broke, \
             in a new commit.\n",
            repo.config.gates.join(", ")
        ));
    }
    s.push_str(&format!(
        "\nDon't push, don't run `hall ready`, don't abort the rebase, and change nothing \
         beyond what the conflicts need. A script checks the result: the rebase finished, the \
         worktree is clean, and {} is an ancestor of HEAD. Then the item goes to review.",
        target.name
    ));
    s
}

// --- 2. watch ----------------------------------------------------------

/// When each PR was last read, and what was already logged.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Watch {
    #[serde(default)]
    last: BTreeMap<String, DateTime<Utc>>,
    #[serde(default)]
    noted: BTreeSet<String>,
}

/// Shipped items whose PR hasn't merged or closed yet.
pub fn watched(items: &[Item]) -> Vec<Item> {
    items
        .iter()
        .filter(|i| {
            i.state == State::Closed
                && i.parent.is_none()
                && i.delivered.is_none()
                && i.pr_url.as_deref().is_some_and(|u| u.contains("/pull/"))
        })
        .cloned()
        .collect()
}

fn watch_next(ctx: &WorkerCtx) -> Result<Progress> {
    let repo = ctx.repo;
    let now = Utc::now();
    let candidates = watched(&repo.index()?.items(true)?);
    if candidates.is_empty() {
        return Ok(Progress::Idle);
    }
    let path = repo.hall.root.join("delivery-watch.json");
    let mut watch: Watch = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let due = candidates
        .into_iter()
        .filter(|i| {
            watch
                .last
                .get(&i.id.to_string())
                .is_none_or(|t| now - *t >= Duration::seconds(EVERY))
        })
        .min_by_key(|i| watch.last.get(&i.id.to_string()).copied());
    let Some(item) = due else {
        return Ok(Progress::Idle);
    };
    let id = item.id;
    let url = item.pr_url.clone().unwrap_or_default();
    watch.last.insert(id.to_string(), now);
    let save = |w: &Watch| {
        let _ = std::fs::write(&path, serde_json::to_string(w).unwrap_or_default());
    };
    save(&watch);
    // Says a thing once, not every two minutes.
    let mut note_once = |key: String, what: String| {
        if watch.noted.insert(key) {
            repo.hall.live(&what);
            save(&watch);
        }
    };
    let pr = match crate::gh::pr_merge(&repo.main_root, &url) {
        Ok(pr) => pr,
        Err(e) => {
            note_once(
                format!("error-{id}"),
                format!("{id} delivery: {}", e.message),
            );
            return Ok(Progress::Idle);
        }
    };
    match pr.state.as_str() {
        "MERGED" | "CLOSED" => {
            let outcome = pr.state.to_lowercase();
            append(
                repo,
                id,
                Body::Delivered {
                    pr_url: url.clone(),
                    outcome: outcome.clone(),
                },
            )?;
            let what = format!("{id} delivered: {url} {outcome}");
            repo.hall.live(&what);
            Ok(Progress::worked(what))
        }
        "OPEN" if pr.mergeable == "CONFLICTING" => {
            let wt = PathBuf::from(&item.worktree);
            if item.pr_head.as_deref() != Some(pr.head_oid.as_str()) {
                note_once(
                    format!("pushed-{id}-{}", pr.head_oid),
                    format!(
                        "{id} delivery: {url} conflicts with {}, but someone else pushed to it \
                         ({}); take it over with `hall adopt --pr {url}`",
                        item.base,
                        short(&pr.head_oid)
                    ),
                );
                return Ok(Progress::Idle);
            }
            if !wt.is_dir() || !git::dirty_files(&wt).map(|d| d.is_empty()).unwrap_or(false) {
                note_once(
                    format!("worktree-{id}"),
                    format!(
                        "{id} delivery: {url} conflicts with {}, but its worktree {} is missing \
                         or has uncommitted changes; `hall adopt --pr {url}` to fix it by hand",
                        item.base, item.worktree
                    ),
                );
                return Ok(Progress::Idle);
            }
            reopen(repo, &item, &url, &pr.head_oid)
        }
        _ => Ok(Progress::Idle),
    }
}

/// Back to development through the open PR, with a rebase request: the
/// next tick rebases it.
fn reopen(repo: &Repo, item: &Item, url: &str, head: &str) -> Result<Progress> {
    let id = item.id;
    let base = item.base.clone();
    let (_, reopened) = repo.hall.transact(id, |it, _| {
        if it.state != State::Closed {
            return Ok((vec![], false));
        }
        Ok((
            vec![
                Event::new(
                    ACTOR,
                    Some(id),
                    Body::Reopened {
                        pr_url: url.to_string(),
                        pr_head: head.to_string(),
                        base: base.clone(),
                        worktree: it.worktree.clone(),
                    },
                ),
                Event::new(
                    ACTOR,
                    Some(id),
                    Body::RebaseRequested {
                        onto: base.clone(),
                        reason: format!("{url} conflicts with {base}"),
                        files: vec![],
                    },
                ),
            ],
            true,
        ))
    })?;
    if !reopened {
        return Ok(Progress::Idle);
    }
    let what = format!("{id} reopened: {url} conflicts with {base}; rebasing it");
    repo.hall.live(&what);
    Ok(Progress::worked(what))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_never_push_or_abort() {
        let t = tools(&["cargo test".into(), " ".into()]);
        assert!(t.contains(&"Bash(git rebase --continue:*)".to_string()));
        assert!(t.contains(&"Bash(cargo test:*)".to_string()));
        assert!(!t.iter().any(|x| x.contains("push") || x.contains("abort")));
    }
}
