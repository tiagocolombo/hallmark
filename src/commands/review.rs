//! The assayer's commands (workstream B): `hall verdict` and `hall findings`.

use std::io::Read as _;
use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;

use super::{ItemView, next_for};
use crate::cli::{FindingsArgs, VerdictArgs};
use crate::error::{ErrorCode, HallError, Result};
use crate::event::{Body, CheckRun, VerdictKind};
use crate::findings::{Counts, Ledger, ModelVerdict, Status, Tracked};
use crate::git;
use crate::output::{Render, Response, clip};
use crate::repo::{Repo, actor};

#[derive(Debug, Serialize, JsonSchema)]
pub struct FindingsOut {
    pub item: ItemView,
    /// Open findings (all of them with --all), in the order raised.
    pub findings: Vec<Tracked>,
    /// Open findings by severity.
    pub open: Counts,
    /// The gates that failed in the last checks run, each with its log
    /// path relative to the item's journal directory (`checks/r1.log`).
    pub failing_checks: Vec<CheckRun>,
}

impl Render for FindingsOut {
    fn render(&self) -> String {
        let mut s = format!(
            "{} · {} blocker · {} major · {} minor · {} nit open\n",
            self.item.id, self.open.blocker, self.open.major, self.open.minor, self.open.nit
        );
        for c in &self.failing_checks {
            s.push_str(&format!(
                "  check failed: {} (exit {})\n",
                c.command, c.exit_code
            ));
        }
        for t in &self.findings {
            let f = &t.finding;
            s.push_str(&format!(
                "  {:4} {:7} {:16} {}:{}  {}{}{}\n",
                f.id,
                f.severity.as_str(),
                f.lens.as_str(),
                f.file,
                f.line,
                clip(&f.title, 60),
                if f.pre_existing {
                    " (pre-existing)"
                } else {
                    ""
                },
                match t.status {
                    Status::Open => String::new(),
                    Status::Fixed => " (fixed)".into(),
                    Status::Withdrawn => " (withdrawn)".into(),
                }
            ));
        }
        s
    }
}

pub fn findings(cwd: &Path, a: FindingsArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    let item = repo.resolve(a.id.as_deref())?;
    let events = repo.hall.read_item(item.id)?;
    let ledger = Ledger::from_events(&events);
    let findings: Vec<Tracked> = ledger
        .findings
        .iter()
        .filter(|t| a.all || t.status == Status::Open)
        .cloned()
        .collect();
    // Failing checks matter only while the last checks run is the latest
    // word on the item.
    let failing_checks = events
        .iter()
        .rev()
        .find_map(|e| match &e.body {
            Body::Checks {
                passed: false,
                runs,
                ..
            } => Some(Some(runs.clone())),
            Body::Checks { passed: true, .. }
            | Body::Verdict { .. }
            | Body::FixSubmitted { .. } => Some(None),
            _ => None,
        })
        .flatten()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.exit_code != 0)
        .collect();
    let out = FindingsOut {
        item: ItemView::from(&item),
        open: ledger.open_counts(),
        findings,
        failing_checks,
    };
    Ok(Response::new(out, next_for(&item)))
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct VerdictOut {
    #[serde(flatten)]
    pub item: ItemView,
    /// The reviewed commit.
    pub reviewed: String,
    /// approve or changes, computed from the open findings.
    pub verdict: crate::event::VerdictKind,
    /// True when this rejection reached `max_rejects`: the item waits for
    /// you.
    pub escalated: bool,
    /// New findings, numbered.
    pub findings: Vec<crate::findings::Finding>,
    /// Open findings after this verdict.
    pub open: Counts,
    /// The full review as Markdown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    /// False when this verdict was already recorded.
    pub changed: bool,
}

impl Render for VerdictOut {
    fn render(&self) -> String {
        let mut s = format!(
            "{} · round {} · {}{}{}\n",
            self.item.id,
            self.item.round,
            crate::assayer::verdict::kind(self.verdict),
            if self.escalated { " · escalated" } else { "" },
            if self.changed {
                ""
            } else {
                " (already recorded)"
            }
        );
        s.push_str(&format!(
            "  {} blocker · {} major · {} minor · {} nit open\n",
            self.open.blocker, self.open.major, self.open.minor, self.open.nit
        ));
        for f in &self.findings {
            s.push_str(&format!(
                "  {:4} {:7} {}:{}  {}\n",
                f.id,
                f.severity.as_str(),
                f.file,
                f.line,
                clip(&f.title, 60)
            ));
        }
        if let Some(r) = &self.report {
            s.push_str(&format!("  report {r}\n"));
        }
        s
    }
}

/// Who may record a verdict: the assayer, or you on a terminal, whose
/// verdict is a human override. An agent may not approve its own work, and
/// a headless run other than the assayer's may not either.
fn verdict_actor(id: crate::item::ItemId) -> Result<String> {
    let actor = actor();
    let allowed = match actor.as_str() {
        "assayer" => true,
        "you" => !crate::repo::headless(),
        _ => false,
    };
    if !allowed {
        return Err(HallError::new(
            ErrorCode::GateDenied,
            format!(
                "`hall verdict` is for the assayer and for you on a terminal, not for {actor}: \
                 an agent doesn't approve work"
            ),
        )
        .next([format!("hall status {id}")]));
    }
    Ok(actor)
}

/// `hall verdict [ID] --file <json|->`: records a schema-checked verdict
/// for an item in review, exactly as the assayer's own run would.
pub fn verdict(cwd: &Path, a: VerdictArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    let item = repo.resolve(a.id.as_deref())?;
    let actor = verdict_actor(item.id)?;
    let text = if a.file == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| HallError::usage(format!("could not read the verdict from stdin: {e}")))?;
        s
    } else {
        let p = crate::paths::resolve(cwd, Path::new(&a.file));
        std::fs::read_to_string(&p).map_err(|e| {
            HallError::usage(format!("could not read {}: {e}", crate::paths::display(&p)))
        })?
    };
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        HallError::new(ErrorCode::InvalidVerdict, format!("verdict: not JSON: {e}"))
            .next(["hall schema verdict-input"])
    })?;
    let mv = ModelVerdict::parse(&value)?;
    let sha = match a.sha.as_deref().or(item.frozen_sha.as_deref()) {
        Some(s) => git::run(
            &repo.main_root,
            &["rev-parse", "--verify", "-q", &format!("{s}^{{commit}}")],
        )
        .map_err(|_| HallError::usage(format!("--sha {s} is not a commit")))?,
        None => {
            return Err(HallError::new(
                ErrorCode::WrongState,
                format!("{} has no frozen commit to review", item.id),
            )
            .next([format!("hall status {}", item.id)]));
        }
    };
    let rec = crate::assayer::record_verdict(&repo, item.id, &sha, &mv, &actor, None, None)?;
    let mut next = Vec::new();
    if rec.verdict == VerdictKind::Changes && !rec.escalated {
        next.push(format!("hall findings {}", item.id));
    }
    next.extend(next_for(&rec.item));
    let out = VerdictOut {
        item: ItemView::from(&rec.item),
        reviewed: rec.sha.clone(),
        verdict: rec.verdict,
        escalated: rec.escalated,
        findings: rec.findings,
        open: rec.open,
        report: rec
            .report
            .map(|r| crate::paths::display(&repo.hall.item_dir(item.id).join(r))),
        changed: rec.changed,
    };
    Ok(Response::new(out, next))
}
