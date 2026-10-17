//! The Dispatch Dock (workstream C, ADR 0019): ships hallmarked items.
//!
//! One tick claims one hallmarked item, checks that its branch is still at
//! the hallmarked commit, writes the PR body, runs the configured `ship`
//! command in the worktree, and records `shipped` (the item closes). An
//! item that already has a PR (adopted, or reopened to rebase it: ADR 0029)
//! runs `update` instead, which replaces the PR's head under a lease. It
//! never ships a commit without a hallmark: a branch that moved past it is
//! held for you instead. Child items never ship; they merge into their
//! parent (the lease candidates skip them).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::commands::record;
use crate::error::Result;
use crate::event::{Body, Event, VerdictKind, short};
use crate::findings::Ledger;
use crate::git;
use crate::item::{HoldKind, Item, ItemId, State};
use crate::lease;
use crate::repo::Repo;
use crate::worker::{Progress, WorkerCtx};

pub const ACTOR: &str = "shipper";

/// The ship command's time limit.
const SHIP_TIMEOUT_SECS: u64 = 600;

pub fn tick(ctx: &WorkerCtx) -> Result<Progress> {
    let repo = ctx.repo;
    let ttl = chrono::Duration::seconds(SHIP_TIMEOUT_SECS as i64 + 300);
    let Some((item, _)) = lease::claim_next(repo, "shipper", &ctx.holder, ttl, None)? else {
        return Ok(Progress::Idle);
    };
    let r = ship(repo, &item, &ctx.holder);
    let _ = lease::release(&repo.hall, item.id, &ctx.holder);
    r
}

/// `items/<id>/pr-body.md`: the PR description.
pub fn body_path(repo: &Repo, id: ItemId) -> PathBuf {
    repo.hall.item_dir(id).join("pr-body.md")
}

/// `items/<id>/review.md`: the review, posted as a PR comment.
pub fn review_path(repo: &Repo, id: ItemId) -> PathBuf {
    repo.hall.item_dir(id).join("review.md")
}

fn ship(repo: &Repo, item: &Item, ctx_holder: &str) -> Result<Progress> {
    let id = item.id;
    if item.state != State::Hallmarked {
        return Ok(Progress::Idle);
    }
    let wt = PathBuf::from(&item.worktree);
    if !wt.is_dir() {
        return held(
            repo,
            id,
            format!(
                "the worktree {} is missing; nothing to ship from",
                item.worktree
            ),
        );
    }
    let mut head = git::head(&wt)?;
    let Some(hallmark) = item.hallmark_sha.clone() else {
        return held(repo, id, "the item has no hallmarked commit".into());
    };
    if head != hallmark {
        return held(
            repo,
            id,
            format!(
                "the branch moved past the hallmark: HEAD is {}, the hallmark is on {}. \
                 Only reviewed commits ship: reset the branch to {}, or `hall pull-back {id}` \
                 and send it to review again",
                short(&head),
                short(&hallmark),
                short(&hallmark)
            ),
        );
    }
    if crate::stamp::read(&wt, &head).is_none() {
        return held(
            repo,
            id,
            format!(
                "{} has no hallmark note (refs/notes/hallmark); it can't ship",
                short(&head)
            ),
        );
    }
    // A follow-up stacked on a PR ships into that PR's branch, which only
    // works while the PR is open.
    let mut base = item.base.clone();
    if item.base != repo.config.base
        && let Some(url) = repo
            .index()?
            .items(true)?
            .into_iter()
            .find(|i| i.branch == item.base)
            .and_then(|i| i.pr_url)
        && let Ok(state) = crate::gh::pr_state(&repo.main_root, &url)
        && !state.is_empty()
        && state != "OPEN"
    {
        if state == "MERGED" {
            // The parent's work reached the base: ship there.
            repo.hall.live(&format!(
                "{id}: the PR {url} it is stacked on merged; shipping into {}",
                repo.config.base
            ));
            base = repo.config.base.clone();
        } else {
            return held(
                repo,
                id,
                format!(
                    "the PR {url} this item is stacked on was {} without merging, so {} is no \
                     place to ship into. Nothing can re-target an item's base: drop it \
                     (`hall drop {id}`) and redo the work with `hall new` from {}",
                    state.to_lowercase(),
                    item.base,
                    repo.config.base
                ),
            );
        }
    }
    if base != repo.config.base && !crate::commands::new::stackable_base(repo, &base) {
        return held(
            repo,
            id,
            format!(
                "the base {base} is neither {} nor the branch of an open PR still at its \
                 hallmarked commit; its commits would ship without review",
                repo.config.base
            ),
        );
    }
    // An update only makes sense while the PR is open.
    if let Some(url) = &item.pr_url
        && let Ok(state) = crate::gh::pr_state(&repo.main_root, url)
        && !state.is_empty()
        && state != "OPEN"
    {
        // Merged with this very work in it (someone merged the update by
        // hand): it is delivered, and nothing is left for you.
        if state == "MERGED" && in_base(&wt, &base, &head) {
            return merged_already(repo, item, &head, url);
        }
        return held(
            repo,
            id,
            format!(
                "the PR {url} is {}, so there is nothing to update. Nothing was pushed: \
                 continue in a new item with `hall new --follow-up {url}`",
                state.to_lowercase()
            ),
        );
    }
    // Always on the latest base before the PR opens or moves (ADR 0030).
    match freshen(ctx_holder, repo, item, &base, &wt, &head)? {
        Fresh::Ship(at) => head = at,
        Fresh::Done(p) => return Ok(p),
    }
    let item = &repo.hall.load_item(id)?;
    let events = repo.hall.read_item(id)?;
    let body = pr_body(
        item,
        &events,
        &commit_subjects(&wt, &base, &head),
        !repo.config.review_comment.trim().is_empty(),
    );
    let body_file = body_path(repo, id);
    std::fs::write(&body_file, &body)?;
    let review_file = review_path(repo, id);
    std::fs::write(
        &review_file,
        review_comment(item, &events, item.pr_url.is_some()),
    )?;
    let title = pr_title(&wt, &base, &head, &item.title);
    let issue = item
        .issue
        .as_ref()
        .map(|i| i.number.to_string())
        .unwrap_or_default();
    // An item that already has a PR (adopted, or reopened for a rebase)
    // updates it; anything else opens one.
    let lease;
    let (template, pr, pr_head) = match (&item.pr_url, &item.pr_head) {
        (Some(pr), Some(known)) => {
            lease = update_lease(repo, item, &events, &wt, known);
            (&repo.config.update, pr.as_str(), lease.as_str())
        }
        (Some(pr), None) => {
            return held(
                repo,
                id,
                format!(
                    "{pr} has no known head commit, so an update push can't be leased; \
                     run `hall adopt` in {} to record it",
                    item.worktree
                ),
            );
        }
        (None, _) => (&repo.config.ship, "", ""),
    };
    let cmd = crate::commands::new::fill_shell(
        template,
        &[
            ("branch", item.branch.as_str()),
            ("id", &id.to_string()),
            ("base", base.as_str()),
            ("title", title.as_str()),
            ("body_file", &crate::paths::display(&body_file)),
            ("review_file", &crate::paths::display(&review_file)),
            ("issue", &issue),
            ("pr", pr),
            ("pr_head", pr_head),
        ],
    );
    repo.hall.live(&format!(
        "{id} {}: {cmd}",
        if pr.is_empty() {
            "shipping"
        } else {
            "updating its PR"
        }
    ));
    // The ship command finds `gh` where Hallmark does (HALL_GH, `[tools] gh`,
    // PATH, Nix store).
    let path = crate::gh::path_with_gh().map(|p| p.to_string_lossy().into_owned());
    let mut env = vec![("HALL_ACTOR", ACTOR)];
    if let Some(p) = &path {
        env.push(("PATH", p.as_str()));
    }
    let run = crate::fixer::run_command(&cmd, &wt, SHIP_TIMEOUT_SECS, &env);
    let runs = repo.hall.item_runs(id);
    std::fs::create_dir_all(&runs)?;
    let _ = std::fs::write(
        runs.join(format!(
            "{}-ship.log",
            chrono::Utc::now().format("%Y%m%dT%H%M%S")
        )),
        format!("$ {cmd}\n{}\n{}", run.stdout, run.stderr),
    );
    if let Some(why) = run.failure() {
        let which = if item.pr_url.is_some() {
            "update"
        } else {
            "ship"
        };
        return held(repo, id, format!("the {which} command {why}"));
    }
    let pr_url = match &item.pr_url {
        Some(u) => Some(u.clone()),
        None => pr_url(&run.stdout),
    };
    let (item, shipped) = repo.hall.transact(id, |it, _| {
        if it.state != State::Hallmarked {
            return Ok((vec![], false));
        }
        Ok((
            vec![Event::new(
                ACTOR,
                Some(id),
                Body::Shipped {
                    sha: head.clone(),
                    pr_url: pr_url.clone(),
                },
            )],
            true,
        ))
    })?;
    repo.hall.unqueue(id)?;
    let (export, _) = record::write_export(repo, id, None)?;
    repo.hall.live(&format!(
        "{id} shipped{}; record at {}",
        pr_url
            .as_deref()
            .map(|u| format!(": {u}"))
            .unwrap_or_default(),
        crate::paths::display(&export)
    ));
    if !shipped {
        return Ok(Progress::worked(format!("{id} was already {}", item.state)));
    }
    // The review goes in a comment, never in the description (ADR 0039).
    let posted = match &pr_url {
        Some(url) => post_review(repo, &item, url, &base, &wt, &review_file, path.as_deref()),
        None => String::new(),
    };
    Ok(Progress::worked(format!(
        "{id} shipped{}{posted}",
        pr_url.map(|u| format!(": {u}")).unwrap_or_default()
    )))
}

/// The commit an update push replaces: the PR's head as last recorded, or
/// where the branch is on `origin` when Hallmark itself put it there. An
/// update can push and then fail (`gh pr edit` losing the network); the
/// recorded head is then behind, and leasing on it would be rejected on
/// every try. A head Hallmark hallmarked for this item is its own work, so
/// it may be replaced; anyone else's push still stops the update.
fn update_lease(repo: &Repo, item: &Item, events: &[Event], wt: &Path, known: &str) -> String {
    let Some(remote) = git::remote_head(wt, "origin", &item.branch) else {
        return known.to_string();
    };
    if remote == known {
        return remote;
    }
    let ours = events
        .iter()
        .any(|e| matches!(&e.body, Body::Hallmarked { sha, .. } if *sha == remote));
    if !ours {
        return known.to_string();
    }
    repo.hall.live(&format!(
        "{}: the PR is at {}, which Hallmark pushed for it; the update replaces that",
        item.id,
        short(&remote)
    ));
    remote
}

/// Runs `review_comment` on the PR. A failure doesn't undo the ship: it is
/// logged and said in the step, and the review stays in review.md.
fn post_review(
    repo: &Repo,
    item: &Item,
    pr: &str,
    base: &str,
    wt: &Path,
    review_file: &Path,
    path: Option<&str>,
) -> String {
    let template = repo.config.review_comment.trim();
    if template.is_empty() {
        return String::new();
    }
    let id = item.id;
    let shown = crate::paths::display(review_file);
    // The gh Hallmark is configured with (HALL_GH, `[tools] gh`), never
    // another one that happens to be on PATH.
    if crate::gate::words(template).first().map(String::as_str) == Some("gh")
        && crate::gh::find().is_none()
    {
        repo.hall
            .live(&format!("{id}: gh wasn't found; the review is in {shown}"));
        return format!("; gh wasn't found, so the review is in {shown}");
    }
    let cmd = crate::commands::new::fill_shell(
        template,
        &[
            ("pr", pr),
            ("review_file", &crate::paths::display(review_file)),
            ("branch", item.branch.as_str()),
            ("id", &id.to_string()),
            ("base", base),
        ],
    );
    let mut env = vec![("HALL_ACTOR", ACTOR)];
    if let Some(p) = path {
        env.push(("PATH", p));
    }
    let run = crate::fixer::run_command(&cmd, wt, SHIP_TIMEOUT_SECS, &env);
    let _ = std::fs::write(
        repo.hall.item_runs(id).join(format!(
            "{}-review-comment.log",
            chrono::Utc::now().format("%Y%m%dT%H%M%S")
        )),
        format!("$ {cmd}\n{}\n{}", run.stdout, run.stderr),
    );
    match run.failure() {
        None => {
            repo.hall.live(&format!("{id}: the review is on {pr}"));
            "; the review is in a comment".into()
        }
        Some(why) => {
            repo.hall.live(&format!(
                "{id}: the review comment command {why}; the review is in {shown}"
            ));
            format!("; the review comment command {why} (the review is in {shown})")
        }
    }
}

/// What the rebase before shipping decided.
enum Fresh {
    /// Ship this commit: the hallmarked one, or its rebase carrying the
    /// hallmark.
    Ship(String),
    /// Not now: the item went back through the loop, or was held.
    Done(Progress),
}

/// Rebases a hallmarked branch onto the latest `base` before it ships.
///
/// - Already on top of it: ship as is.
/// - A clean rebase with the same patch (`git patch-id --stable`) whose
///   gates pass at the new commit: the hallmark is carried to it, and it
///   ships. No review is spent on a change the reviewer already approved.
/// - A clean rebase whose patch changed, or whose gates fail: the rebased
///   commit goes back through the loop (`pulled_back`, then `ready`).
/// - Conflicts: the rebase is undone and the item goes back to
///   development with a rebase request; the Delivery office resolves it
///   with a headless dev run and sends it to review.
fn freshen(
    holder: &str,
    repo: &Repo,
    item: &Item,
    base: &str,
    wt: &std::path::Path,
    head: &str,
) -> Result<Fresh> {
    let id = item.id;
    let Some(target) = crate::delivery::base_ref(wt, base) else {
        // No base to rebase onto here; the ship command finds out.
        return Ok(Fresh::Ship(head.to_string()));
    };
    // A stack whose lower PR merged ships into the configured base: replay
    // only this item's commits, the ones since the old base branch.
    let upstream =
        if base != item.base && git::rev(wt, &format!("refs/heads/{}", item.base)).is_some() {
            format!("refs/heads/{}", item.base)
        } else {
            target.git_ref.clone()
        };
    let rebased = crate::delivery::rebase(wt, &target.git_ref, &upstream)?;
    let (from, to, old_base) = match rebased {
        crate::delivery::Rebased::UpToDate => return Ok(Fresh::Ship(head.to_string())),
        crate::delivery::Rebased::Failed(why) => {
            return held(
                repo,
                id,
                format!("the rebase onto {} failed: {why}", target.name),
            )
            .map(Fresh::Done);
        }
        crate::delivery::Rebased::Conflict { files } => {
            let _ = git::run(wt, &["rebase", "--abort"]);
            let reason = format!(
                "{} conflicts with {} in {}",
                item.branch,
                target.name,
                if files.is_empty() {
                    "some files".to_string()
                } else {
                    files.join(", ")
                }
            );
            back(
                repo,
                id,
                vec![
                    Body::PulledBack {
                        reason: Some(reason.clone()),
                    },
                    Body::RebaseRequested {
                        onto: base.to_string(),
                        reason: reason.clone(),
                        files,
                    },
                ],
            )?;
            let what = format!("{id} not shipped: {reason}; the Delivery office rebases it");
            repo.hall.live(&what);
            return Ok(Fresh::Done(Progress::worked(what)));
        }
        crate::delivery::Rebased::Clean { from, to, old_base } => (from, to, old_base),
    };
    let same = match (
        git::patch_id(wt, &old_base, &from),
        git::patch_id(wt, &target.git_ref, &to),
    ) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    };
    repo.hall.live(&format!(
        "{id} rebased onto {}: {} → {}{}",
        target.name,
        short(&from),
        short(&to),
        if same {
            ", same patch; running the gates"
        } else {
            ", the patch changed"
        }
    ));
    let mut checks = Vec::new();
    if same {
        let renew = || {
            let _ = lease::claim(
                &repo.hall,
                id,
                "shipper",
                holder,
                chrono::Duration::seconds(SHIP_TIMEOUT_SECS as i64 + 900),
            );
        };
        match crate::assayer::checks::gates_at(
            repo,
            id,
            &to,
            &format!("rebase-{}", short(&to)),
            &renew,
        ) {
            Ok(runs) => checks = runs,
            Err(e) if e.code == crate::error::ErrorCode::Claimed => {
                // Every check slot is busy: put the branch back and try on
                // a later pass.
                let _ = git::run(wt, &["reset", "--hard", &from]);
                return Ok(Fresh::Done(Progress::Idle));
            }
            Err(e) => {
                let _ = git::run(wt, &["reset", "--hard", &from]);
                return Err(e);
            }
        }
    }
    let passed = checks.iter().all(|r| r.exit_code == 0);
    let carried = same && passed;
    let reason = (!carried).then(|| {
        if same {
            let failed: Vec<&str> = checks
                .iter()
                .filter(|r| r.exit_code != 0)
                .map(|r| r.command.as_str())
                .collect();
            format!("the gates failed after the rebase: {}", failed.join(", "))
        } else {
            "the patch changed in the rebase, so it is reviewed again".to_string()
        }
    });
    let rebased_ev = Body::Rebased {
        from: from.clone(),
        to: to.clone(),
        onto: target.name.clone(),
        carried,
        checks,
        reason: reason.clone(),
    };
    if carried {
        repo.hall
            .append_item(id, &Event::new(ACTOR, Some(id), rebased_ev))?;
        crate::stamp::carry(repo, id, &from, &to, ACTOR)?;
        return Ok(Fresh::Ship(to));
    }
    let round = item.round + 1;
    back(
        repo,
        id,
        vec![
            rebased_ev,
            Body::PulledBack {
                reason: reason.clone(),
            },
            Body::Ready {
                sha: to.clone(),
                round,
                note: Some(format!("rebased onto {}", target.name)),
            },
        ],
    )?;
    repo.hall.queue(&crate::journal::Ticket {
        id,
        sha: to.clone(),
        round,
        queued_at: chrono::Utc::now(),
    })?;
    let what = format!(
        "{id} not shipped: rebased onto {} at {}; {}; back to review",
        target.name,
        short(&to),
        reason.unwrap_or_default()
    );
    repo.hall.live(&what);
    Ok(Fresh::Done(Progress::worked(what)))
}

/// Appends `bodies` while the item is still hallmarked.
fn back(repo: &Repo, id: ItemId, bodies: Vec<Body>) -> Result<()> {
    repo.hall.transact(id, |it, _| {
        if it.state != State::Hallmarked {
            return Ok((vec![], ()));
        }
        Ok((
            bodies
                .into_iter()
                .map(|b| Event::new(ACTOR, Some(id), b))
                .collect(),
            (),
        ))
    })?;
    Ok(())
}

/// `head`'s work is already in the latest `base`: merging it would change
/// nothing (`git merge-tree`), however the PR was merged (merge, squash or
/// rebase).
fn in_base(wt: &std::path::Path, base: &str, head: &str) -> bool {
    let Some(target) = crate::delivery::base_ref(wt, base) else {
        return false;
    };
    let Ok(tree) = git::run(
        wt,
        &[
            "rev-parse",
            "--verify",
            &format!("{}^{{tree}}", target.git_ref),
        ],
    ) else {
        return false;
    };
    git::run(wt, &["merge-tree", "--write-tree", &target.git_ref, head])
        .is_ok_and(|out| out.lines().next().map(str::trim) == Some(tree.trim()))
}

/// The hallmarked update reached the base through its merged PR: record it
/// as shipped (the item closes), with nothing pushed.
fn merged_already(repo: &Repo, item: &Item, head: &str, url: &str) -> Result<Progress> {
    let id = item.id;
    let (_, shipped) = repo.hall.transact(id, |it, _| {
        if it.state != State::Hallmarked {
            return Ok((vec![], false));
        }
        Ok((
            vec![Event::new(
                ACTOR,
                Some(id),
                Body::Shipped {
                    sha: head.to_string(),
                    pr_url: None,
                },
            )],
            true,
        ))
    })?;
    repo.hall.unqueue(id)?;
    if !shipped {
        return Ok(Progress::Idle);
    }
    let _ = record::write_export(repo, id, None);
    let what = format!("{id} closed: {url} merged with its hallmarked work already in it");
    repo.hall.live(&what);
    Ok(Progress::worked(what))
}

fn held(repo: &Repo, id: ItemId, reason: String) -> Result<Progress> {
    repo.hall.append_item(
        id,
        &Event::new(
            ACTOR,
            Some(id),
            Body::Held {
                kind: HoldKind::ShipFailed,
                reason: reason.clone(),
            },
        ),
    )?;
    repo.hall.live(&format!("{id} not shipped: {reason}"));
    Ok(Progress::worked(format!("{id} held: {reason}")))
}

/// The PR URL in the ship command's output: the last `…/pull/<n>` URL, else
/// the last URL.
pub fn pr_url(out: &str) -> Option<String> {
    let urls: Vec<&str> = out
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| "\"'<>()[],;.".contains(c)))
        .filter(|w| w.starts_with("https://") || w.starts_with("http://"))
        .collect();
    let is_pull = |u: &&str| {
        u.split_once("/pull/")
            .is_some_and(|(_, n)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    };
    urls.iter()
        .rev()
        .find(|u| is_pull(u))
        .or_else(|| urls.last())
        .map(|u| u.to_string())
}

/// The item's title as a conventional commit, typed by the commits it
/// ships (ADR 0035): the squash merge of the PR is what release tooling
/// reads. `freshen` just fetched the base, so this reads it locally.
fn pr_title(wt: &Path, base: &str, head: &str, title: &str) -> String {
    let log = git::run(
        wt,
        &[
            "log",
            "--format=%B%x00",
            &format!("{}..{head}", base_ref(wt, base)),
        ],
    )
    .unwrap_or_default();
    crate::conventional::pr_title(title, log.split('\0').map(str::trim))
}

/// The base as this checkout knows it: `origin/<base>` when fetched.
fn base_ref(wt: &Path, base: &str) -> String {
    let remote = format!("refs/remotes/origin/{base}");
    if git::rev(wt, &remote).is_some() {
        remote
    } else {
        format!("refs/heads/{base}")
    }
}

/// The subjects of the commits the PR ships, oldest first, merges left out.
fn commit_subjects(wt: &Path, base: &str, head: &str) -> Vec<String> {
    git::run(
        wt,
        &[
            "log",
            "--reverse",
            "--no-merges",
            "--format=%s",
            &format!("{}..{head}", base_ref(wt, base)),
        ],
    )
    .unwrap_or_default()
    .lines()
    .map(str::trim)
    .filter(|l| !l.is_empty())
    .map(String::from)
    .collect()
}

/// Marks Hallmark's review comment on a PR: the Delivery office never reads
/// it as feedback.
pub const REVIEW_MARK: &str = "<!-- hallmark:review -->";

/// The PR description (ADR 0039): What, Why and How, and the issue it
/// closes. No review: that goes in a comment ([`review_comment`]).
///
/// - What: the author's summary (`propose-ready --summary`), else the title.
/// - Why: `propose-ready --why`, else the request, else the issue.
/// - How: `propose-ready --how`, else the commits.
///
/// `review_posted` is whether a review comment will be posted; when it isn't,
/// the footer says the review stays local.
pub fn pr_body(item: &Item, events: &[Event], commits: &[String], review_posted: bool) -> String {
    let proposed = events.iter().rev().find_map(|e| match &e.body {
        Body::ProposedReady { why, how, .. } => Some((why.clone(), how.clone())),
        _ => None,
    });
    let (why, how) = proposed.unwrap_or_default();
    // A follow-up's request is generated (PR feedback, CI logs): never the
    // Why. It gets a one-line pointer to the PR instead.
    let request = events.iter().find_map(|e| match &e.body {
        Body::Created {
            follow_up: Some(url),
            ..
        } => Some(format!("Follow-up to {url}.")),
        Body::Created { request, .. } => request.clone(),
        _ => None,
    });
    let mut s = String::new();
    let what = item
        .summary
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or(&item.title);
    let _ = writeln!(s, "## What\n\n{}\n", what.trim());

    let why = why
        .or(request)
        .filter(|w| !w.trim().is_empty())
        .or_else(|| item.issue.as_ref().map(|i| format!("Issue #{}.", i.number)))
        .unwrap_or_else(|| item.title.clone());
    let _ = writeln!(s, "## Why\n\n{}\n", why.trim());

    let _ = writeln!(s, "## How\n");
    match how.filter(|h| !h.trim().is_empty()) {
        Some(h) => {
            let _ = writeln!(s, "{}", h.trim());
        }
        None if !commits.is_empty() => {
            for c in commits {
                let _ = writeln!(s, "- {c}");
            }
        }
        None => {
            let _ = writeln!(s, "See the commits.");
        }
    }
    if let Some(i) = &item.issue {
        let _ = writeln!(s, "\nCloses #{}", i.number);
    }
    if review_posted {
        let _ = writeln!(
            s,
            "\n---\n\nReviewed and hallmarked as {}: the review is in the comments.",
            item.id
        );
    } else {
        let _ = writeln!(
            s,
            "\n---\n\nReviewed and hallmarked as {0}: the review stays local (`hall export {0}`).",
            item.id
        );
    }
    s
}

/// The review as a PR comment (ADR 0039): the verdict, every finding and
/// how it was settled, the checks and the usage. On an update of a PR
/// already shipped (`update: true`), it covers what happened since: why
/// the item came back, and the findings raised or settled since.
pub fn review_comment(item: &Item, events: &[Event], update: bool) -> String {
    let id = item.id;
    let mut s = String::new();
    // Where the previous ship was, for an update.
    let last_ship = update
        .then(|| {
            events
                .iter()
                .rposition(|e| matches!(e.body, Body::Shipped { .. }))
        })
        .flatten();
    let shipped_round = last_ship.map_or(0, |i| {
        events[..i]
            .iter()
            .filter_map(|e| match &e.body {
                Body::Verdict { round, .. } => Some(*round),
                _ => None,
            })
            .max()
            .unwrap_or(0)
    });
    let approval = events.iter().rev().find_map(|e| match &e.body {
        Body::Verdict {
            verdict: VerdictKind::Approve,
            summary,
            reviewer,
            round,
            ..
        } => Some((summary.clone(), reviewer.clone(), *round)),
        _ => None,
    });

    let _ = writeln!(s, "{REVIEW_MARK}");
    let _ = writeln!(
        s,
        "## Hallmark review{}: approved\n",
        if last_ship.is_some() {
            " of this update"
        } else {
            ""
        }
    );
    match &approval {
        Some((summary, reviewer, round)) => {
            if let Some(t) = summary {
                let _ = writeln!(s, "{}\n", t.trim());
            }
            let _ = writeln!(
                s,
                "Hallmarked at `{}` in round {round} after {} rejected review{}{}.",
                short(item.hallmark_sha.as_deref().unwrap_or("?")),
                item.rejects,
                if item.rejects == 1 { "" } else { "s" },
                reviewer
                    .as_deref()
                    .map(|r| format!(", reviewed by {r}"))
                    .unwrap_or_default()
            );
        }
        None => {
            let _ = writeln!(s, "Hallmarked after {} rounds.", item.round);
        }
    }
    // The approval carried across a rebase onto the latest base.
    let carried = events.iter().rev().find_map(|e| match &e.body {
        Body::Rebased {
            from,
            onto,
            carried: true,
            to,
            ..
        } if item.hallmark_sha.as_deref() == Some(to.as_str()) => {
            Some((from.clone(), onto.clone()))
        }
        _ => None,
    });
    // After such a rebase the commits a fix claimed were never pushed.
    let rebased_onto = carried.as_ref().map(|(_, onto)| onto.clone());
    if let Some((from, onto)) = carried {
        let _ = writeln!(
            s,
            "\nRebased onto `{onto}` before shipping: the patch is identical to the approved `{}` \
             and the gates passed again.",
            short(&from)
        );
    }

    if let Some(i) = last_ship {
        let why = reasons(&events[i + 1..]);
        if !why.is_empty() {
            let _ = writeln!(s, "\n### Why it was reviewed again\n");
            for w in why {
                let _ = writeln!(s, "{w}");
            }
        }
    }

    let ledger = Ledger::from_events(events);
    let mut tracked: Vec<_> = ledger
        .findings
        .iter()
        .filter(|t| {
            t.finding.round > shipped_round
                || t.settled_round.is_some_and(|r| r > shipped_round)
                || t.status == crate::findings::Status::Open
        })
        .collect();
    tracked.sort_by_key(|t| {
        (
            t.status != crate::findings::Status::Open,
            t.finding.severity,
        )
    });
    let _ = writeln!(s, "\n### Findings\n");
    if tracked.is_empty() {
        let _ = writeln!(s, "None.");
    }
    for t in tracked {
        let f = &t.finding;
        let _ = writeln!(
            s,
            "- **{}** {} · {} · `{}:{}` · {}",
            f.id,
            f.severity.as_str(),
            f.lens.as_str(),
            f.file,
            f.line,
            f.title.trim()
        );
        let note = t
            .note
            .as_deref()
            .filter(|n| !n.trim().is_empty())
            .map(|n| format!(" The reviewer: {}", crate::output::clip(n, 200)))
            .unwrap_or_default();
        let outcome = match t.status {
            crate::findings::Status::Fixed => format!(
                "Fixed{}{}.{note}",
                match (&rebased_onto, t.claimed_by.as_deref()) {
                    (Some(onto), _) => format!(" before the rebase onto `{onto}`"),
                    (None, Some(c)) => format!(" in `{}`", short(c)),
                    (None, None) => String::new(),
                },
                t.settled_round
                    .map(|r| format!(", confirmed in round {r}"))
                    .unwrap_or_default()
            ),
            crate::findings::Status::Withdrawn => format!(
                "Withdrawn{}.{note}",
                t.settled_round
                    .map(|r| format!(" in round {r}"))
                    .unwrap_or_default()
            ),
            crate::findings::Status::Open => {
                let fix = if f.fix.trim().is_empty() {
                    String::new()
                } else {
                    format!(" Suggested: {}", crate::output::clip(&f.fix, 200))
                };
                format!("Still open ({} rides along).{fix}", f.severity.as_str())
            }
        };
        let _ = writeln!(s, "  - {outcome}");
        if let Some(d) = &t.dispute {
            let _ = writeln!(
                s,
                "  - Disputed by the fixer: {}",
                crate::output::clip(d, 200)
            );
        }
    }

    let checks = events.iter().rev().find_map(|e| match &e.body {
        Body::Checks {
            passed: true, runs, ..
        } => Some(runs.clone()),
        _ => None,
    });
    let _ = writeln!(s, "\n### Checks\n");
    match checks {
        Some(runs) if !runs.is_empty() => {
            for r in runs {
                let _ = writeln!(
                    s,
                    "- `{}` passed ({:.1}s{})",
                    r.command,
                    r.duration_ms as f64 / 1000.0,
                    if r.cached { ", cached" } else { "" }
                );
            }
        }
        _ => {
            let _ = writeln!(s, "No gates are configured.");
        }
    }
    let usage = record::usage_by_role(events);
    if !usage.is_empty() {
        let tokens: u64 = usage.iter().map(|u| u.2).sum();
        let cost: f64 = usage.iter().map(|u| u.3).sum();
        let mut roles: Vec<(String, f64)> = Vec::new();
        for (role, _, _, c) in &usage {
            match roles.iter_mut().find(|(r, _)| r == role) {
                Some(x) => x.1 += c,
                None => roles.push((role.clone(), *c)),
            }
        }
        let by: Vec<String> = roles
            .iter()
            .map(|(r, c)| format!("{r} {}", crate::usage::fmt_usd(*c)))
            .collect();
        let _ = writeln!(
            s,
            "\n### Usage\n\n{} tokens · {} at list price ({})",
            crate::usage::fmt_tokens(tokens),
            crate::usage::fmt_usd(cost),
            by.join(", ")
        );
    }
    let _ = writeln!(
        s,
        "\n---\n\nThe full record stays local: `hall export {id}`."
    );
    s
}

/// Why a shipped item came back through review, from the events since it
/// shipped: the PR comments it addressed, red CI, a conflict, or, when none
/// of those, a person reopening it.
fn reasons(events: &[Event]) -> Vec<String> {
    let mut out = Vec::new();
    let mut reopened = false;
    for e in events {
        match &e.body {
            Body::FeedbackRequested { comments, .. } => {
                out.push(format!(
                    "- It addressed {} PR comment{}:",
                    comments.len(),
                    if comments.len() == 1 { "" } else { "s" }
                ));
                for c in comments {
                    let at = match (&c.path, c.line) {
                        (Some(p), Some(l)) => format!(" on `{p}:{l}`"),
                        (Some(p), None) => format!(" on `{p}`"),
                        _ => String::new(),
                    };
                    out.push(format!(
                        "  - @{}{at}: {}",
                        c.author,
                        crate::output::clip(&c.body, 160)
                    ));
                }
            }
            Body::CiFixRequested { failing, .. } => {
                let names: Vec<String> = failing.iter().map(|f| format!("`{}`", f.name)).collect();
                out.push(format!("- It fixed failing CI: {}.", names.join(", ")));
            }
            Body::RebaseRequested { onto, reason, .. } => {
                out.push(format!(
                    "- It was rebased onto `{onto}`: {}.",
                    reason.trim().trim_end_matches('.')
                ));
            }
            Body::Reopened { .. } => reopened = true,
            // Back on its PR unchanged (ADR 0047): what came before was
            // never shipped.
            Body::Restored { .. } => {
                out.clear();
                reopened = false;
            }
            _ => {}
        }
    }
    if out.is_empty() && reopened {
        out.push("- It was reopened for more work.".into());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn item(follow_up: Option<&str>) -> Item {
        let now = chrono::Utc::now();
        let mut v = json!({
            "id": "H-0042", "title": "Retry webhooks", "state": "hallmarked",
            "branch": "hm/H-0042-retry", "worktree": "/w", "base": "main",
            "created_at": now, "updated_at": now, "state_since": now,
            "round": 4, "rejects": 1, "hallmark_sha": "def5678def5678",
            "adopted": false, "events": 0,
            "pr_url": "https://github.com/acme/app/pull/12",
        });
        if let Some(u) = follow_up {
            v["follow_up"] = json!(u);
        }
        serde_json::from_value(v).unwrap()
    }

    fn ev(body: Value) -> Event {
        Event::new(
            "test",
            Some(ItemId(42)),
            serde_json::from_value(body).unwrap(),
        )
    }

    fn finding(id: &str, round: u32) -> Value {
        json!({"id": id, "round": round, "lens": "correctness", "severity": "major",
            "file": "a.rs", "line": 1, "title": format!("Title {id}"),
            "scenario": "s", "fix": "f"})
    }

    fn verdict(round: u32, kind: &str, found: Vec<Value>, prior: Value) -> Event {
        ev(
            json!({"event": "verdict", "sha": "abc1234", "round": round, "verdict": kind,
            "findings": found, "prior": prior,
            "counts": {"blocker": 0, "major": 0, "minor": 0, "nit": 0}}),
        )
    }

    /// F1 raised and fixed, shipped, then F2 raised and fixed after `since`.
    fn updated(since: Value) -> Vec<Event> {
        vec![
            verdict(1, "changes", vec![finding("F1", 1)], json!([])),
            verdict(
                2,
                "approve",
                vec![],
                json!([{"id": "F1", "status": "fixed"}]),
            ),
            ev(
                json!({"event": "shipped", "sha": "abc1234", "pr_url": "https://github.com/acme/app/pull/12"}),
            ),
            ev(since),
            verdict(3, "changes", vec![finding("F2", 3)], json!([])),
            verdict(
                4,
                "approve",
                vec![],
                json!([{"id": "F2", "status": "fixed"}]),
            ),
        ]
    }

    #[test]
    fn a_restored_ci_fix_is_not_claimed_by_the_next_update() {
        let events = vec![
            ev(json!({"event": "ci_fix_requested",
                "pr_url": "https://github.com/acme/app/pull/12",
                "failing": [{"name": "test (windows-latest)"}]})),
            ev(json!({"event": "restored",
                "pr_url": "https://github.com/acme/app/pull/12",
                "sha": "abc1234", "reason": "CI passed on a re-run"})),
            ev(json!({"event": "reopened", "pr_url": "u", "pr_head": "h",
                "base": "main", "worktree": "/w"})),
        ];
        let why = reasons(&events);
        assert_eq!(why, vec!["- It was reopened for more work.".to_string()]);
    }

    #[test]
    fn an_update_lists_only_the_findings_since_the_last_ship() {
        let events = updated(json!({"event": "ci_fix_requested",
            "pr_url": "https://github.com/acme/app/pull/12",
            "failing": [{"name": "test"}]}));
        let c = review_comment(&item(None), &events, true);
        assert!(c.contains("- It fixed failing CI: `test`."), "{c}");
        assert!(c.contains("**F2**"), "{c}");
        assert!(!c.contains("**F1**"), "settled before the last ship:\n{c}");
        // A first review lists everything.
        let first = review_comment(&item(None), &events, false);
        assert!(
            first.contains("**F1**") && first.contains("**F2**"),
            "{first}"
        );
    }

    #[test]
    fn a_cached_check_says_so_in_the_comment() {
        let mut events = updated(json!({"event": "rebase_requested", "onto": "main",
            "reason": "r"}));
        events.push(ev(json!({"event": "checks", "sha": "abc1234", "round": 4,
        "passed": true, "runs": [
            {"command": "cargo test", "exit_code": 0, "duration_ms": 2500, "cached": true},
            {"command": "cargo fmt", "exit_code": 0, "duration_ms": 1000}
        ]})));
        let c = review_comment(&item(None), &events, false);
        assert!(c.contains("- `cargo test` passed (2.5s, cached)"), "{c}");
        assert!(c.contains("- `cargo fmt` passed (1.0s)"), "{c}");
    }

    #[test]
    fn an_update_after_a_rebase_says_so() {
        let events = updated(json!({"event": "rebase_requested", "onto": "main",
            "reason": "the branch conflicts with main."}));
        let c = review_comment(&item(None), &events, true);
        assert!(
            c.contains("- It was rebased onto `main`: the branch conflicts with main."),
            "{c}"
        );
    }

    #[test]
    fn a_fix_before_a_carried_rebase_isnt_named_by_its_old_sha() {
        let mut events = updated(json!({"event": "reopened", "pr_url": "u", "pr_head": "h",
            "base": "main", "worktree": "/w"}));
        events.push(ev(
            json!({"event": "fix_submitted", "sha": "abc1234", "fixes": ["F1"]}),
        ));
        events.push(ev(
            json!({"event": "rebased", "from": "abc1234", "to": "def5678def5678",
            "onto": "origin/main", "carried": true}),
        ));
        let c = review_comment(&item(None), &events, false);
        assert!(
            c.contains("Rebased onto `origin/main` before shipping"),
            "{c}"
        );
        assert!(
            c.contains("Fixed before the rebase onto `origin/main`"),
            "{c}"
        );
        assert!(!c.contains("Fixed in `"), "{c}");
    }

    #[test]
    fn a_follow_ups_why_is_a_pointer_not_the_generated_request() {
        let events = vec![ev(json!({"event": "created", "title": "t", "branch": "b",
            "worktree": "/w", "base": "main",
            "request": "Follow-up to PR #12\nReview feedback:\n- @me: Hallmark review",
            "follow_up": "https://github.com/acme/app/pull/12"}))];
        let u = "https://github.com/acme/app/pull/12";
        let b = pr_body(&item(Some(u)), &events, &[], true);
        assert!(b.contains(&format!("## Why\n\nFollow-up to {u}.\n")), "{b}");
        assert!(
            !b.contains("Review feedback") && !b.contains("Hallmark review"),
            "{b}"
        );
    }

    #[test]
    fn the_footer_says_when_no_review_is_posted() {
        let b = pr_body(&item(None), &[], &[], false);
        assert!(
            b.contains("the review stays local (`hall export H-0042`)"),
            "{b}"
        );
        assert!(!b.contains("in the comments"), "{b}");
        let b = pr_body(&item(None), &[], &[], true);
        assert!(b.contains("the review is in the comments"), "{b}");
    }

    #[test]
    fn pr_urls() {
        assert_eq!(
            pr_url("Creating pull request…\nhttps://github.com/acme/app/pull/42\n").as_deref(),
            Some("https://github.com/acme/app/pull/42")
        );
        // A real PR beats a later "create a PR" link; trailing dots go.
        assert_eq!(
            pr_url("opened https://github.com/acme/app/pull/7. see https://github.com/acme/app/pull/new/x")
                .as_deref(),
            Some("https://github.com/acme/app/pull/7")
        );
        assert_eq!(
            pr_url("done: <https://git.example.com/merge_requests/3>").as_deref(),
            Some("https://git.example.com/merge_requests/3")
        );
        assert_eq!(pr_url("Everything up-to-date"), None);
    }
}
