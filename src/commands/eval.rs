//! `hall eval`: replays today's reviewer on the changes the loop got wrong
//! (ADR 0065).
//!
//! The cases are the items whose merged PR was reverted (ADR 0061): each
//! is a change the loop approved and a person had to undo. For each one
//! the reviewer runs again, as a round-1 review of the commit that was
//! first reviewed, with today's skill, rules and model, and the verdict is
//! scored against what the revert touched. Comparing with the verdict
//! recorded at the time says whether a change to the reviewer helped.
//!
//! It spends model usage, so it runs only when asked, never from the
//! daemon; `--dry-run` lists the cases without running anything.

use std::fmt::Write as _;
use std::path::Path;

use chrono::{Duration, Utc};
use schemars::JsonSchema;
use serde::Serialize;

use crate::assayer::{self, review};
use crate::cli::EvalArgs;
use crate::error::{ErrorCode, HallError, Result};
use crate::event::{Body, Event, VerdictKind, short};
use crate::findings::{ModelVerdict, Severity};
use crate::item::{Item, ItemId};
use crate::output::{Render, Response, clip};
use crate::repo::Repo;
use crate::runner::{self, Metered, Runner};

/// The run's role, for the fake runner and usage; it shares the review
/// budget (`budget::role_of`).
pub const ROLE: &str = "eval";
/// Cases replayed when `--limit` isn't given.
pub const DEFAULT_LIMIT: usize = 10;

#[derive(Debug, Serialize, JsonSchema)]
pub struct EvalOut {
    /// Nothing ran: the cases are listed as they would run.
    pub dry_run: bool,
    pub cases: Vec<EvalCase>,
    pub totals: EvalTotals,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EvalCase {
    pub item: ItemId,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    /// The commit the reviewer first saw.
    pub sha: String,
    /// The commit that undid the change on its base.
    pub reverted_by: String,
    /// Files the revert touched; a finding there is a catch.
    pub revert_files: Vec<String>,
    /// The revert's files couldn't be read (the commit isn't local), so any
    /// blocking finding counts as a catch.
    #[serde(skip_serializing_if = "is_false")]
    pub revert_files_unknown: bool,
    /// The verdict recorded at the time.
    pub then: Scored,
    /// Today's reviewer on the same commit (absent on a dry run or when
    /// the run failed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub now: Option<Scored>,
    /// Why there is no `now`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One verdict against the revert.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct Scored {
    /// Changes were requested: at least one blocker or major.
    pub rejected: bool,
    /// A blocker or major sits in a file the revert touched.
    pub caught: bool,
    pub findings: u32,
    pub blocking: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// The blocking findings' titles, with their files.
    pub titles: Vec<String>,
}

#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct EvalTotals {
    pub cases: u32,
    pub ran: u32,
    pub then_caught: u32,
    pub now_caught: u32,
    pub now_rejected: u32,
    pub cost_usd: f64,
}

impl Render for EvalOut {
    fn render(&self) -> String {
        let mut s = String::new();
        if self.cases.is_empty() {
            s.push_str(
                "No cases: no item's merged PR was reverted yet (ADR 0061), and none was named.\n",
            );
            return s;
        }
        let _ = writeln!(
            s,
            "{} case{}{}\n",
            self.cases.len(),
            if self.cases.len() == 1 { "" } else { "s" },
            if self.dry_run { " (dry run)" } else { "" }
        );
        for c in &self.cases {
            let _ = writeln!(
                s,
                "{} {}  · reviewed {} · reverted by {}",
                c.item,
                clip(&c.title, 48),
                short(&c.sha),
                short(&c.reverted_by)
            );
            let _ = writeln!(s, "  then  {}", verdict_line(&c.then));
            match (&c.now, &c.skipped) {
                (Some(n), _) => {
                    let _ = writeln!(s, "  now   {}", verdict_line(n));
                    for t in &n.titles {
                        let _ = writeln!(s, "        - {}", clip(t, 90));
                    }
                }
                (None, Some(why)) => {
                    let _ = writeln!(s, "  now   {why}");
                }
                (None, None) => {}
            }
        }
        let t = &self.totals;
        if t.ran > 0 {
            let _ = writeln!(
                s,
                "\nCaught then {} of {}, now {} of {} · now rejected {} · ${:.2}",
                t.then_caught, t.cases, t.now_caught, t.ran, t.now_rejected, t.cost_usd
            );
        }
        s
    }
}

fn verdict_line(v: &Scored) -> String {
    format!(
        "{}, {} · {} finding{}, {} blocking{}{}",
        if v.rejected { "rejected" } else { "approved" },
        if v.caught { "caught" } else { "missed" },
        v.findings,
        if v.findings == 1 { "" } else { "s" },
        v.blocking,
        v.model
            .as_deref()
            .map(|m| format!(" · {m}"))
            .unwrap_or_default(),
        v.cost_usd
            .map(|c| format!(" · ${c:.2}"))
            .unwrap_or_default()
    )
}

pub fn eval(cwd: &Path, a: EvalArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    repo.require_init()?;
    let since = match a.since.as_deref() {
        None => None,
        Some(s) => {
            let secs = crate::config::parse_duration(s)
                .ok_or_else(|| HallError::usage(format!("--since {s}: use a duration like 90d")))?;
            Some(Utc::now() - Duration::seconds(secs))
        }
    };
    let items = repo.index()?.items(true)?;
    let mut picked: Vec<Item> = if a.ids.is_empty() {
        let mut v: Vec<Item> = items
            .into_iter()
            .filter(|i| {
                i.reverted
                    .as_ref()
                    .is_some_and(|r| since.is_none_or(|t| r.at >= t))
            })
            .collect();
        v.sort_by(|x, y| {
            y.reverted
                .as_ref()
                .map(|r| r.at)
                .cmp(&x.reverted.as_ref().map(|r| r.at))
        });
        v
    } else {
        let mut v = Vec::new();
        for id in &a.ids {
            let id: ItemId = id.parse()?;
            let item = items.iter().find(|i| i.id == id).cloned().ok_or_else(|| {
                HallError::new(ErrorCode::ItemNotFound, format!("{id} not found"))
            })?;
            if item.reverted.is_none() {
                return Err(HallError::new(
                    ErrorCode::WrongState,
                    format!(
                        "{id}: its PR wasn't reverted, so there is nothing to score it against"
                    ),
                )
                .next([format!("hall log {id}")]));
            }
            v.push(item);
        }
        v
    };
    let has_review = |i: &Item| -> Result<bool> {
        Ok(repo
            .hall
            .read_item(i.id)?
            .iter()
            .any(|e| matches!(e.body, Body::Verdict { .. })))
    };
    if a.ids.is_empty() {
        let mut v = Vec::new();
        for i in picked {
            if has_review(&i)? {
                v.push(i);
            }
        }
        picked = v;
    } else {
        for i in &picked {
            if !has_review(i)? {
                return Err(HallError::new(
                    ErrorCode::WrongState,
                    format!("{}: it has no review to replay", i.id),
                )
                .next([format!("hall log {}", i.id)]));
            }
        }
    }
    picked.truncate(a.limit.unwrap_or(DEFAULT_LIMIT));

    let runner_ = runner::from_env();
    let mut out = EvalOut {
        dry_run: a.dry_run,
        cases: Vec::new(),
        totals: EvalTotals::default(),
    };
    for item in &picked {
        let events = repo.hall.read_item(item.id)?;
        let Some(case) = case(&repo, item, &events) else {
            continue;
        };
        let mut case = case;
        if !a.dry_run {
            replay(&repo, runner_.as_ref(), item, &events, &mut case);
        }
        out.totals.cases += 1;
        out.totals.then_caught += u32::from(case.then.caught);
        if let Some(n) = &case.now {
            out.totals.ran += 1;
            out.totals.now_caught += u32::from(n.caught);
            out.totals.now_rejected += u32::from(n.rejected);
            out.totals.cost_usd += n.cost_usd.unwrap_or(0.0);
        }
        out.cases.push(case);
    }
    let next = vec![
        "hall stats --since 90d".to_string(),
        "hall eval --dry-run".into(),
    ];
    Ok(Response::new(out, next))
}

/// The case for a reverted item: the commit its first review saw, that
/// verdict scored, and the files the revert touched. None when the item
/// has no review to replay.
fn case(repo: &Repo, item: &Item, events: &[Event]) -> Option<EvalCase> {
    let r = item.reverted.as_ref()?;
    let first = events.iter().find_map(|e| match &e.body {
        Body::Verdict {
            sha,
            verdict,
            findings,
            reviewer,
            ..
        } => Some((sha.clone(), *verdict, findings.clone(), reviewer.clone())),
        _ => None,
    })?;
    let (sha, verdict, findings, reviewer) = first;
    let revert_files = revert_files(repo, &r.revert_sha);
    let then = score(
        verdict == VerdictKind::Changes,
        findings
            .iter()
            .map(|f| (f.severity, f.file.as_str(), f.title.as_str())),
        &revert_files,
        reviewer,
        None,
    );
    Some(EvalCase {
        item: item.id,
        title: item.title.clone(),
        pr_url: item.pr_url.clone(),
        sha,
        reverted_by: r.revert_sha.clone(),
        revert_files_unknown: revert_files.is_empty(),
        revert_files,
        then,
        now: None,
        skipped: None,
    })
}

/// Files the revert commit touched, when it is known locally; empty when
/// it isn't (the case says so in `revert_files_unknown`).
fn revert_files(repo: &Repo, revert_sha: &str) -> Vec<String> {
    crate::git::run(
        &repo.main_root,
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "--no-renames",
            revert_sha,
        ],
    )
    .map(|s| {
        s.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect()
    })
    .unwrap_or_default()
}

/// Scores a verdict against the revert: rejected when a blocker or major
/// is open, caught when one of them sits in a file the revert touched.
/// With no known revert files, any blocking finding counts as a catch.
pub fn score<'a>(
    rejected: bool,
    findings: impl Iterator<Item = (Severity, &'a str, &'a str)>,
    revert_files: &[String],
    model: Option<String>,
    cost_usd: Option<f64>,
) -> Scored {
    let mut s = Scored {
        rejected,
        model,
        cost_usd,
        ..Default::default()
    };
    for (sev, file, title) in findings {
        s.findings += 1;
        if sev.blocks() {
            s.blocking += 1;
            s.titles
                .push(format!("{} · {file} · {title}", sev.as_str()));
            if revert_files.is_empty() || revert_files.iter().any(|f| f == file) {
                s.caught = true;
            }
        }
    }
    s
}

/// Runs today's reviewer on the case's commit as a round-1 review and
/// scores it.
fn replay(repo: &Repo, runner_: &dyn Runner, item: &Item, events: &[Event], case: &mut EvalCase) {
    // The item as the first review saw it: nothing reviewed yet, no
    // findings, and only the events up to that review.
    let mut then = item.clone();
    then.reviewed_sha = None;
    // Not approved yet: a hallmark of a later fix would make this a rebase
    // review of the fixes only.
    then.hallmark_sha = None;
    then.rejects = 0;
    then.round = 1;
    then.frozen_sha = Some(case.sha.clone());
    then.state = crate::item::State::Review;
    let before: Vec<Event> = events
        .iter()
        .take_while(|e| !matches!(e.body, Body::Verdict { .. }))
        .cloned()
        .collect();
    struct Cleanup<'a>(&'a Repo, ItemId);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            assayer::remove_review_checkout(self.0, self.1);
        }
    }
    // A merge-commit merge leaves the commit in the base: there is no
    // diff left to review.
    if crate::git::ok(
        &repo.main_root,
        &["merge-base", "--is-ancestor", &case.sha, &item.base],
    ) {
        case.skipped = Some(format!(
            "{} is already in {}: the merge kept it, so there is no diff to replay",
            short(&case.sha),
            item.base
        ));
        return;
    }
    let _cleanup = Cleanup(repo, item.id);
    let (mut req, _) = match review::request_for(repo, &then, &before, &case.sha, true) {
        Ok(r) => r,
        Err(e) => {
            case.skipped = Some(format!("couldn't build the review: {}", e.message));
            return;
        }
    };
    req.role = ROLE.into();
    let outcome = match runner::run_metered(repo, runner_, None, &req) {
        Ok(Metered::Done(o)) => o,
        Ok(Metered::Blocked(stop)) => {
            case.skipped = Some(format!("didn't run: {}", stop.reason));
            return;
        }
        Ok(Metered::Failed { failure, .. }) => {
            case.skipped = Some(format!("the run failed: {}", failure.describe()));
            return;
        }
        Err(e) => {
            case.skipped = Some(format!("the run failed: {}", e.message));
            return;
        }
    };
    let model = outcome
        .result
        .as_ref()
        .and_then(|r| {
            r.model_usage
                .iter()
                .max_by(|a, b| a.1.cost_usd.total_cmp(&b.1.cost_usd))
                .map(|(m, _)| m.clone())
        })
        .or(req.model.clone());
    let cost = outcome.result.as_ref().and_then(|r| r.total_cost_usd);
    let mv = match ModelVerdict::parse(&outcome.structured().cloned().unwrap_or_default()) {
        Ok(mv) => mv,
        Err(e) => {
            case.skipped = Some(format!("the verdict didn't parse: {}", e.message));
            return;
        }
    };
    let rejected = mv.findings.iter().any(|f| f.severity.blocks());
    case.now = Some(score(
        rejected,
        mv.findings
            .iter()
            .map(|f| (f.severity, f.file.as_str(), f.title.as_str())),
        &case.revert_files,
        model,
        cost,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blocking_finding_in_a_reverted_file_is_a_catch() {
        let files = vec!["src/auth.rs".to_string()];
        let s = score(
            true,
            [
                (Severity::Minor, "src/auth.rs", "naming"),
                (Severity::Major, "src/other.rs", "race"),
            ]
            .into_iter(),
            &files,
            None,
            None,
        );
        assert!(s.rejected && !s.caught);
        assert_eq!((s.findings, s.blocking), (2, 1));
        assert_eq!(s.titles, ["major · src/other.rs · race"]);
        let s = score(
            true,
            [(Severity::Blocker, "src/auth.rs", "token")].into_iter(),
            &files,
            Some("opus".into()),
            Some(1.5),
        );
        assert!(s.caught);
        // No known revert files: any blocking finding counts.
        let s = score(
            true,
            [(Severity::Major, "x.rs", "t")].into_iter(),
            &[],
            None,
            None,
        );
        assert!(s.caught);
        let s = score(false, std::iter::empty(), &files, None, None);
        assert!(!s.rejected && !s.caught && s.findings == 0);
    }
}
