//! Recording a verdict (ADR 0015): shared by the assayer's review run and
//! `hall verdict`. Hallmark numbers the findings, applies the rulings, and
//! decides approval from the open counts, never from the model.

use std::fmt::Write as _;
use std::fs;

use chrono::Utc;

use crate::error::{ErrorCode, HallError, Result};
use crate::event::{Body, Event, VerdictKind, short};
use crate::findings::{Counts, Finding, Judged, Ledger, ModelVerdict, PriorStatus, Severity};
use crate::item::{Item, ItemId, State};
use crate::journal::ev;
use crate::repo::Repo;

use super::base::Base;

/// What [`record_verdict`] recorded (or found already recorded).
#[derive(Debug, Clone)]
pub struct Recorded {
    /// The item afterwards.
    pub item: Item,
    pub sha: String,
    pub round: u32,
    pub verdict: VerdictKind,
    pub escalated: bool,
    /// New findings, numbered.
    pub findings: Vec<Finding>,
    /// Open findings after this verdict.
    pub open: Counts,
    /// The report, relative to the item's journal directory
    /// (`reviews/r2.md`).
    pub report: Option<String>,
    /// False when this verdict was already recorded for this round and
    /// commit.
    pub changed: bool,
}

/// Judges `mv` for the item in review at `sha` and records it: numbers the
/// findings, applies the rulings, computes approve or changes from the open
/// counts, escalates a rejection that reaches `max_rejects` (read from the
/// base branch), writes `reviews/r<round>.md`, appends `verdict`, removes
/// the ticket, and strikes the hallmark on approval.
///
/// `base` is the base commit the rules came from; None resolves the item's
/// base branch now. Idempotent: the same round and commit again is
/// `changed: false`. An item that is not in review at `sha` is
/// `WRONG_STATE`.
pub fn record_verdict(
    repo: &Repo,
    id: ItemId,
    sha: &str,
    mv: &ModelVerdict,
    actor: &str,
    reviewer: Option<String>,
    base: Option<String>,
) -> Result<Recorded> {
    mv.check()?;
    let item = repo.hall.load_item(id)?;
    let base = match base {
        Some(s) => Some(Base::at(repo, &item.base, &s)),
        None => Base::resolve(repo, &item.base).ok(),
    };
    let max_rejects = base
        .as_ref()
        .map(|b| b.config.max_rejects)
        .unwrap_or(repo.config.max_rejects)
        .max(1);
    let max_nits = base
        .as_ref()
        .map(|b| b.config.max_nits)
        .unwrap_or(repo.config.max_nits);
    let base_sha = base.as_ref().map(|b| b.sha.clone());
    let base_name = item.base.clone();

    let (after, rec) = repo.hall.transact(id, |cur, events| {
        if let Some(prev) = existing(events, sha, cur.round) {
            return Ok((vec![], prev));
        }
        if cur.state != State::Review || cur.frozen_sha.as_deref() != Some(sha) {
            return Err(not_in_review(cur, sha));
        }
        let ledger = Ledger::from_events(events);
        let judged = ledger.judge(cur.round, mv, max_nits);
        let verdict = if judged.approves() {
            VerdictKind::Approve
        } else {
            VerdictKind::Changes
        };
        let escalated = verdict == VerdictKind::Changes && cur.rejects + 1 >= max_rejects;
        let since = cur.reviewed_sha.clone().filter(|s| s != sha);
        let rel = format!("reviews/r{}.md", cur.round);
        let dir = repo.hall.item_reviews(id);
        fs::create_dir_all(&dir)?;
        let report = render(&Report {
            item: cur,
            sha,
            base: &base_name,
            base_sha: base_sha.as_deref(),
            since: since.as_deref(),
            reviewer: reviewer.as_deref(),
            actor,
            mv,
            judged: &judged,
            before: &ledger,
            verdict,
            escalated,
            max_rejects,
            max_nits,
        });
        fs::write(dir.join(format!("r{}.md", cur.round)), report)?;
        let e = ev(
            actor,
            Some(id),
            Body::Verdict {
                sha: sha.to_string(),
                round: cur.round,
                verdict,
                escalated,
                findings: judged.findings.clone(),
                prior: judged.prior.clone(),
                counts: judged.counts,
                summary: Some(mv.summary.clone()),
                report: Some(rel.clone()),
                reviewer: reviewer.clone(),
                learnings: mv.learnings.clone(),
                base: base_sha.clone(),
                since,
            },
        );
        Ok((
            vec![e],
            Partial {
                round: cur.round,
                verdict,
                escalated,
                findings: judged.findings.clone(),
                open: judged.counts,
                report: Some(rel),
                reviewer: reviewer.clone(),
                actor: actor.to_string(),
                changed: true,
            },
        ))
    })?;

    repo.hall.unqueue(id)?;
    let mut item = after;
    if rec.verdict == VerdictKind::Approve
        && (rec.changed || item.hallmark_sha.as_deref() != Some(sha))
    {
        crate::stamp::strike(
            repo,
            id,
            sha,
            rec.round,
            rec.reviewer.clone(),
            &rec.actor,
            rec.open,
        )?;
        item = repo.hall.refresh_item_json(id)?;
    }
    repo.hall.live(&format!(
        "{id} review round {}: {}{} ({} blocker, {} major open)",
        rec.round,
        kind(rec.verdict),
        if rec.escalated { ", escalated" } else { "" },
        rec.open.blocker,
        rec.open.major
    ));
    Ok(Recorded {
        item,
        sha: sha.to_string(),
        round: rec.round,
        verdict: rec.verdict,
        escalated: rec.escalated,
        findings: rec.findings,
        open: rec.open,
        report: rec.report,
        changed: rec.changed,
    })
}

struct Partial {
    round: u32,
    verdict: VerdictKind,
    escalated: bool,
    findings: Vec<Finding>,
    open: Counts,
    report: Option<String>,
    reviewer: Option<String>,
    /// Who recorded the verdict.
    actor: String,
    changed: bool,
}

/// The verdict already recorded for this round and commit.
fn existing(events: &[Event], sha: &str, round: u32) -> Option<Partial> {
    events.iter().rev().find_map(|e| match &e.body {
        Body::Verdict {
            sha: s,
            round: r,
            verdict,
            escalated,
            findings,
            counts,
            report,
            reviewer,
            ..
        } if s == sha && *r == round => Some(Partial {
            round,
            verdict: *verdict,
            escalated: *escalated,
            findings: findings.clone(),
            open: *counts,
            report: report.clone(),
            reviewer: reviewer.clone(),
            actor: e.actor.clone(),
            changed: false,
        }),
        _ => None,
    })
}

fn not_in_review(item: &Item, sha: &str) -> HallError {
    let what = match (item.state, item.frozen_sha.as_deref()) {
        (State::Review, Some(f)) => format!(
            "{} is in review at {}, not {}",
            item.id,
            short(f),
            short(sha)
        ),
        (s, _) => format!("{} is in {}, not review", item.id, s.label().to_lowercase()),
    };
    HallError::new(ErrorCode::WrongState, what).next([
        format!("hall status {}", item.id),
        format!("hall log {} --kind verdict", item.id),
    ])
}

/// Who overrode the assayer, when `actor` (who recorded a verdict or
/// struck a hallmark) isn't the assayer: `you`, or an older journal's
/// actor. None for the assayer, and for hallmarks that don't say.
pub fn override_by(actor: &str) -> Option<&str> {
    (!actor.is_empty() && actor != "assayer").then_some(actor)
}

pub fn kind(v: VerdictKind) -> &'static str {
    match v {
        VerdictKind::Approve => "approve",
        VerdictKind::Changes => "changes",
    }
}

/// Everything the report shows.
pub struct Report<'a> {
    pub item: &'a Item,
    pub sha: &'a str,
    pub base: &'a str,
    pub base_sha: Option<&'a str>,
    pub since: Option<&'a str>,
    pub reviewer: Option<&'a str>,
    /// Who recorded it: `assayer`, or `you` for a human override.
    pub actor: &'a str,
    pub mv: &'a ModelVerdict,
    pub judged: &'a Judged,
    /// The findings before this verdict.
    pub before: &'a Ledger,
    pub verdict: VerdictKind,
    pub escalated: bool,
    pub max_rejects: u32,
    pub max_nits: u32,
}

/// The review as readable Markdown: `reviews/r<round>.md`.
pub fn render(r: &Report) -> String {
    let it = r.item;
    let c = r.judged.counts;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "# {} · review round {} · {}\n",
        it.id,
        it.round,
        kind(r.verdict)
    );
    let _ = writeln!(s, "{}\n", it.title);
    let mut meta = vec![format!("Commit `{}`", short(r.sha))];
    match r.base_sha {
        Some(b) => meta.push(format!("base `{}` at `{}`", r.base, short(b))),
        None => meta.push(format!("base `{}`", r.base)),
    }
    if let Some(since) = r.since {
        meta.push(format!("incremental since `{}`", short(since)));
    }
    if let Some(m) = r.reviewer {
        meta.push(format!("reviewer {m}"));
    }
    meta.push(Utc::now().format("%Y-%m-%d %H:%M UTC").to_string());
    let _ = writeln!(s, "{}\n", meta.join(" · "));
    if let Some(who) = override_by(r.actor) {
        if who == "you" {
            let _ = writeln!(
                s,
                "**A human override**: {who} recorded this verdict with `hall verdict`; \
                 the assayer did not review it.\n"
            );
        } else {
            let _ = writeln!(
                s,
                "Recorded by {who} with `hall verdict`, not the assayer.\n"
            );
        }
    }
    let open = format!(
        "{} blocker, {} major, {} minor, {} nit open",
        c.blocker, c.major, c.minor, c.nit
    );
    let _ = match (r.verdict, r.escalated) {
        (VerdictKind::Approve, _) => writeln!(
            s,
            "**Approved**: no blocker or major finding is open ({open})."
        ),
        (VerdictKind::Changes, false) => writeln!(s, "**Changes requested**: {open}."),
        (VerdictKind::Changes, true) => writeln!(
            s,
            "**Changes requested, escalated**: rejection {} of {}; the item waits for you ({open}).",
            it.rejects + 1,
            r.max_rejects
        ),
    };
    let _ = writeln!(s, "\n## Summary\n\n{}", r.mv.summary.trim());
    if r.judged.dropped_nits > 0 {
        let _ = writeln!(
            s,
            "\nThe reviewer reported {} nits beyond the cap of {} (`max_nits`); they were not recorded.",
            r.judged.dropped_nits, r.max_nits
        );
    }

    if !r.judged.findings.is_empty() {
        let _ = writeln!(s, "\n## New findings");
        for sev in Severity::ALL {
            let fs: Vec<&Finding> = r
                .judged
                .findings
                .iter()
                .filter(|f| f.severity == sev)
                .collect();
            if fs.is_empty() {
                continue;
            }
            let _ = writeln!(s, "\n### {}", plural(sev));
            for f in fs {
                let _ = writeln!(
                    s,
                    "\n#### {} · {}{}\n\n`{}:{}` · {}\n\n**Scenario.** {}\n\n**Fix.** {}",
                    f.id,
                    f.title.trim(),
                    if f.pre_existing {
                        " (pre-existing: not introduced by this change; never blocks)"
                    } else {
                        ""
                    },
                    f.file,
                    f.line,
                    f.lens.label(),
                    f.scenario.trim(),
                    f.fix.trim()
                );
            }
        }
    }

    let open_before = r.before.open();
    if !open_before.is_empty() {
        let _ = writeln!(s, "\n## Earlier findings\n");
        for t in open_before {
            let f = &t.finding;
            let ruling = r.judged.prior.iter().find(|p| p.id == f.id);
            let status = match ruling.map(|p| p.status) {
                Some(PriorStatus::Fixed) => "fixed",
                Some(PriorStatus::Withdrawn) => "withdrawn",
                Some(PriorStatus::Open) => "still open",
                None => "still open (no ruling)",
            };
            let _ = write!(
                s,
                "- **{}** {} · `{}:{}` · {}: **{status}**",
                f.id,
                f.severity.as_str(),
                f.file,
                f.line,
                f.title.trim()
            );
            if let Some(n) = ruling.and_then(|p| p.note.as_deref()) {
                let _ = write!(s, ". {}", n.trim());
            }
            let _ = writeln!(s);
        }
    }

    if !r.mv.learnings.is_empty() {
        let _ = writeln!(s, "\n## Learnings used\n");
        for l in &r.mv.learnings {
            let _ = write!(
                s,
                "- {} {}",
                l.id,
                if l.held { "held" } else { "did not hold" }
            );
            if let Some(n) = &l.note {
                let _ = write!(s, ": {}", n.trim());
            }
            let _ = writeln!(s);
        }
    }
    s
}

fn plural(s: Severity) -> &'static str {
    match s {
        Severity::Blocker => "Blockers",
        Severity::Major => "Majors",
        Severity::Minor => "Minors",
        Severity::Nit => "Nits",
    }
}

#[cfg(test)]
mod override_tests {
    use super::override_by;

    #[test]
    fn only_a_non_assayer_actor_is_an_override() {
        assert_eq!(override_by("assayer"), None);
        assert_eq!(override_by(""), None);
        assert_eq!(override_by("you"), Some("you"));
        assert_eq!(override_by("agent"), Some("agent"));
    }
}
