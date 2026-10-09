//! The assayer's commands (workstream B): `hall verdict` and `hall findings`.

use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;

use super::{ItemView, next_for};
use crate::cli::{FindingsArgs, VerdictArgs};
use crate::error::{HallError, Result};
use crate::event::{Body, CheckRun};
use crate::findings::{Counts, Ledger, Status, Tracked};
use crate::output::{Render, Response, clip};
use crate::repo::Repo;

#[derive(Debug, Serialize, JsonSchema)]
pub struct FindingsOut {
    pub item: ItemView,
    /// Open findings (all of them with --all), in the order raised.
    pub findings: Vec<Tracked>,
    /// Open findings by severity.
    pub open: Counts,
    /// The gates that failed in the last checks run, with their log paths.
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
                "  {:4} {:7} {:16} {}:{}  {}{}\n",
                f.id,
                f.severity.as_str(),
                f.lens.as_str(),
                f.file,
                f.line,
                clip(&f.title, 60),
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
    /// approve or changes, computed from the open findings.
    pub verdict: crate::event::VerdictKind,
    pub escalated: bool,
    /// New findings, numbered.
    pub findings: Vec<crate::findings::Finding>,
    /// Open findings after this verdict.
    pub open: Counts,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    /// False when this verdict was already recorded.
    pub changed: bool,
}

impl Render for VerdictOut {
    fn render(&self) -> String {
        format!("{} · {:?}\n", self.item.id, self.verdict)
    }
}

pub fn verdict(_cwd: &Path, _a: VerdictArgs) -> Result<Response> {
    Err(HallError::not_implemented("hall verdict"))
}
