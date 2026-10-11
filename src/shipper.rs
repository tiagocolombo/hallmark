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
use std::path::PathBuf;

use crate::commands::record;
use crate::error::Result;
use crate::event::{Body, Event, VerdictKind, short};
use crate::findings::{Ledger, Severity};
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

/// `items/<id>/pr-body.md`.
pub fn body_path(repo: &Repo, id: ItemId) -> PathBuf {
    repo.hall.item_dir(id).join("pr-body.md")
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
    let body = pr_body(repo, item, &events);
    let body_file = body_path(repo, id);
    std::fs::write(&body_file, &body)?;
    let issue = item
        .issue
        .as_ref()
        .map(|i| i.number.to_string())
        .unwrap_or_default();
    // An item that already has a PR (adopted, or reopened for a rebase)
    // updates it; anything else opens one.
    let (template, pr, pr_head) = match (&item.pr_url, &item.pr_head) {
        (Some(pr), Some(head)) => (&repo.config.update, pr.as_str(), head.as_str()),
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
            ("title", item.title.as_str()),
            ("body_file", &crate::paths::display(&body_file)),
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
    // The ship command finds `gh` where Hallmark does (HALL_GH, PATH, Nix store).
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
    Ok(Progress::worked(format!(
        "{id} shipped{}",
        pr_url.map(|u| format!(": {u}")).unwrap_or_default()
    )))
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

/// The PR body: the review summary, rounds, the open minors and nits that
/// ride along, the checks, usage, the issue it closes, and where the full
/// record is.
pub fn pr_body(_repo: &Repo, item: &Item, events: &[Event]) -> String {
    let id = item.id;
    let mut s = String::new();
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
    if let Some(sum) = &item.summary {
        let _ = writeln!(s, "{}\n", sum.trim());
    }
    let _ = writeln!(s, "## Review\n");
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
    if let Some((from, onto)) = events.iter().rev().find_map(|e| match &e.body {
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
    }) {
        let _ = writeln!(
            s,
            "Rebased onto `{onto}` before shipping: the patch is identical to the approved `{}` \
             and the gates passed again.",
            short(&from)
        );
    }
    let ledger = Ledger::from_events(events);
    let settled: Vec<String> = ledger
        .findings
        .iter()
        .filter(|t| t.status != crate::findings::Status::Open)
        .map(|t| t.finding.id.clone())
        .collect();
    if !settled.is_empty() {
        let _ = writeln!(s, "Findings settled on the way: {}.", settled.join(", "));
    }
    let riders: Vec<_> = crate::fixer::sorted_open(&ledger)
        .into_iter()
        .filter(|t| matches!(t.finding.severity, Severity::Minor | Severity::Nit))
        .collect();
    if !riders.is_empty() {
        let _ = writeln!(s, "\n### Still open (minor and nit)\n");
        for t in riders {
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
            if !f.fix.trim().is_empty() {
                let _ = writeln!(s, "  - Suggested: {}", crate::output::clip(&f.fix, 200));
            }
            if let Some(d) = &t.dispute {
                let _ = writeln!(s, "  - Disputed: {}", crate::output::clip(d, 200));
            }
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
                    "- `{}` passed ({:.1}s)",
                    r.command,
                    r.duration_ms as f64 / 1000.0
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
    if let Some(i) = &item.issue {
        let _ = writeln!(s, "\nCloses #{}", i.number);
    }
    let _ = writeln!(
        s,
        "\n---\n\nThe full record stays local: `hall export {id}`."
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

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
