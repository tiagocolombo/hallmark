//! The Dispatch Dock (workstream C, ADR 0019): ships hallmarked items.
//!
//! One tick claims one hallmarked item, checks that its branch is still at
//! the hallmarked commit, writes the PR body, runs the configured `ship`
//! command in the worktree, and records `shipped` (the item closes). It
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
    let r = ship(repo, &item);
    let _ = lease::release(&repo.hall, item.id, &ctx.holder);
    r
}

/// `items/<id>/pr-body.md`.
pub fn body_path(repo: &Repo, id: ItemId) -> PathBuf {
    repo.hall.item_dir(id).join("pr-body.md")
}

fn ship(repo: &Repo, item: &Item) -> Result<Progress> {
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
    let head = git::head(&wt)?;
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
    let events = repo.hall.read_item(id)?;
    let body = pr_body(repo, item, &events);
    let body_file = body_path(repo, id);
    std::fs::write(&body_file, &body)?;
    let issue = item
        .issue
        .as_ref()
        .map(|i| i.number.to_string())
        .unwrap_or_default();
    let cmd = crate::commands::new::fill_shell(
        &repo.config.ship,
        &[
            ("branch", item.branch.as_str()),
            ("id", &id.to_string()),
            ("base", item.base.as_str()),
            ("title", item.title.as_str()),
            ("body_file", &crate::paths::display(&body_file)),
            ("issue", &issue),
        ],
    );
    repo.hall.live(&format!("{id} shipping: {cmd}"));
    let run = crate::fixer::run_command(&cmd, &wt, SHIP_TIMEOUT_SECS, &[("HALL_ACTOR", ACTOR)]);
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
        return held(repo, id, format!("the ship command {why}"));
    }
    let pr_url = pr_url(&run.stdout);
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
pub fn pr_body(repo: &Repo, item: &Item, events: &[Event]) -> String {
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
        let by: Vec<String> = roles.iter().map(|(r, c)| format!("{r} ${c:.2}")).collect();
        let _ = writeln!(
            s,
            "\n### Usage\n\n{} tokens · ${cost:.2} at list price ({})",
            crate::usage::fmt_tokens(tokens),
            by.join(", ")
        );
    }
    if let Some(i) = &item.issue {
        let _ = writeln!(s, "\nCloses #{}", i.number);
    }
    let _ = writeln!(
        s,
        "\n---\n\nThe full record: `hall export {id}` ({}).",
        crate::paths::display(&record::export_path(repo, id))
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
