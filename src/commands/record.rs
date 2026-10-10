//! The record (workstream C, ADR 0019): `hall history` and `hall export`.
//!
//! - `hall history` answers "what got rejected this week, and why": closed
//!   and dropped items, newest first, with the blocker and major findings
//!   behind each rejection.
//! - `hall export` writes an item's whole record as Markdown. The shipper
//!   writes one for every item it ships, and the PR body points to it.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;

use super::ItemView;
use crate::cli::{ExportArgs, HistoryArgs};
use crate::error::{HallError, Result};
use crate::event::{Body, Event, VerdictKind, short};
use crate::findings::{Ledger, Lens, PriorStatus, Severity, Status};
use crate::item::{Item, ItemId, State};
use crate::output::{Render, Response, clip};
use crate::repo::Repo;

// --- hall history -------------------------------------------------------------

#[derive(Debug, Serialize, JsonSchema)]
pub struct HistoryOut {
    pub items: Vec<HistoryRow>,
    /// Items that matched before `--limit`.
    pub total: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct HistoryRow {
    #[serde(flatten)]
    pub item: ItemView,
    /// closed, merged (a parallel part merged into its parent), dropped, or
    /// the live state with `--all`.
    pub outcome: String,
    /// Rejected reviews.
    pub rejected: u32,
    /// Blocker and major findings raised, as "F2 correctness: title".
    pub reasons: Vec<String>,
    /// The same findings, with where they were and what became of them.
    pub blocking: Vec<Reason>,
    /// When the item closed or was dropped (its last event while live).
    pub ended_at: DateTime<Utc>,
    /// From creation to `ended_at`, in seconds.
    pub duration_secs: i64,
    /// The same, for people: "3h 05m".
    pub duration: String,
}

/// A blocker or major finding behind a rejection.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Reason {
    pub id: String,
    pub round: u32,
    pub lens: String,
    pub severity: String,
    pub file: String,
    pub line: u32,
    pub title: String,
    /// open, fixed, withdrawn.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispute: Option<String>,
}

impl Render for HistoryOut {
    fn render(&self) -> String {
        if self.items.is_empty() {
            return "No items match.\n".into();
        }
        let mut s = String::new();
        for r in &self.items {
            let _ = writeln!(
                s,
                "{} {:8} {}  · {} round{} · {} rejected · {}",
                r.item.id,
                r.outcome,
                clip(&r.item.title, 48),
                r.item.round,
                if r.item.round == 1 { "" } else { "s" },
                r.rejected,
                r.duration
            );
            if let Some(pr) = &r.item.pr_url {
                let _ = writeln!(s, "  PR {pr}");
            }
            if let Some(why) = &r.item.drop_reason {
                let _ = writeln!(s, "  dropped: \"{}\"", clip(why, 70));
            }
            for reason in &r.reasons {
                let _ = writeln!(s, "  {}", clip(reason, 90));
            }
        }
        if self.total > self.items.len() {
            let _ = writeln!(s, "({} of {} shown)", self.items.len(), self.total);
        }
        s
    }
}

pub fn history(cwd: &Path, a: HistoryArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    repo.require_init()?;
    let lens = match a.lens.as_deref() {
        None => None,
        Some(l) => Some(Lens::parse(l).ok_or_else(|| {
            let all: Vec<&str> = Lens::ALL.iter().map(|l| l.as_str()).collect();
            HallError::usage(format!("--lens {l}: use one of {}", all.join(", ")))
        })?),
    };
    let since = match a.since.as_deref() {
        None => None,
        Some(s) => {
            let secs = crate::config::parse_duration(s).ok_or_else(|| {
                HallError::usage(format!("--since {s}: use a duration like 7d or 24h"))
            })?;
            Some(Utc::now() - chrono::Duration::seconds(secs))
        }
    };
    let ix = repo.index()?;
    let findings = ix.findings(None, lens.map(Lens::as_str), None)?;
    let mut by_item: BTreeMap<ItemId, Vec<&crate::index::FindingRow>> = BTreeMap::new();
    for f in &findings {
        by_item.entry(f.item).or_default().push(f);
    }
    let mut rows: Vec<HistoryRow> = ix
        .items(true)?
        .into_iter()
        .filter(|i| a.all || i.state.is_terminal())
        .filter(|i| !a.rejected || i.rejects > 0)
        .filter(|i| lens.is_none() || by_item.contains_key(&i.id))
        .map(|i| row(&i, by_item.get(&i.id).map(Vec::as_slice).unwrap_or(&[])))
        .filter(|r| since.is_none_or(|t| r.ended_at >= t))
        .collect();
    rows.sort_by(|x, y| y.ended_at.cmp(&x.ended_at).then(y.item.id.cmp(&x.item.id)));
    let total = rows.len();
    rows.truncate(a.limit.unwrap_or(50));
    let mut next = vec!["hall export <id>".to_string(), "hall log <id>".into()];
    if !a.rejected {
        next.push("hall history --rejected --since 7d".into());
    }
    Ok(Response::new(HistoryOut { items: rows, total }, next))
}

fn row(i: &Item, findings: &[&crate::index::FindingRow]) -> HistoryRow {
    let ended_at = if i.state.is_terminal() {
        i.state_since
    } else {
        i.updated_at
    };
    let secs = (ended_at - i.created_at).num_seconds().max(0);
    let mut blocking: Vec<Reason> = findings
        .iter()
        .filter(|f| f.severity == "blocker" || f.severity == "major")
        .map(|f| Reason {
            id: f.id.clone(),
            round: f.round,
            lens: f.lens.clone(),
            severity: f.severity.clone(),
            file: f.file.clone(),
            line: f.line,
            title: f.title.clone(),
            status: f.status.clone(),
            dispute: f.dispute.clone(),
        })
        .collect();
    blocking.sort_by_key(|r| {
        (
            r.round,
            r.id.trim_start_matches('F').parse::<u32>().unwrap_or(0),
        )
    });
    let outcome = match i.state {
        State::Closed if i.parent.is_some() && i.pr_url.is_none() => "merged".to_string(),
        s => s.as_str().to_string(),
    };
    HistoryRow {
        item: ItemView::from(i),
        outcome,
        rejected: i.rejects,
        reasons: blocking
            .iter()
            .map(|r| format!("{} {} {}: {}", r.id, r.severity, r.lens, r.title))
            .collect(),
        blocking,
        ended_at,
        duration_secs: secs,
        duration: duration(secs),
    }
}

/// "45s", "12m", "3h 05m", "2d 04h".
pub fn duration(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
        s => format!("{}d {:02}h", s / 86_400, (s % 86_400) / 3600),
    }
}

// --- hall export ----------------------------------------------------------------

#[derive(Debug, Serialize, JsonSchema)]
pub struct ExportOut {
    pub id: String,
    /// Where the Markdown was written.
    pub path: String,
    pub markdown: String,
}

impl Render for ExportOut {
    fn render(&self) -> String {
        format!("{}\n(written to {})\n", self.markdown.trim_end(), self.path)
    }
}

pub fn export(cwd: &Path, a: ExportArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    let item = repo.resolve(a.id.as_deref())?;
    let out = a
        .out
        .as_deref()
        .map(|o| crate::paths::resolve(cwd, Path::new(o)));
    let (path, markdown) = write_export(&repo, item.id, out.as_deref())?;
    let id = item.id;
    Ok(Response::new(
        ExportOut {
            id: id.to_string(),
            path: crate::paths::display(&path),
            markdown,
        },
        vec![format!("hall log {id}"), "hall history".into()],
    ))
}

/// The default export path: `items/<id>/export.md` in the journal.
pub fn export_path(repo: &Repo, id: ItemId) -> PathBuf {
    repo.hall.item_dir(id).join("export.md")
}

/// Renders the item's record and writes it to `out` (default:
/// [`export_path`]). Returns the path and the Markdown.
pub fn write_export(repo: &Repo, id: ItemId, out: Option<&Path>) -> Result<(PathBuf, String)> {
    let events = repo.hall.read_item(id)?;
    let item = crate::event::fold(id, &events)?;
    let md = render_export(repo, &item, &events);
    let path = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| export_path(repo, id));
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&path, &md)?;
    Ok((path, md))
}

/// One table cell: no pipes, no line breaks.
fn cell(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

fn time(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

/// Usage by (role, model): tokens and list-price cost.
pub fn usage_by_role(events: &[Event]) -> Vec<(String, String, u64, f64)> {
    let mut m: BTreeMap<(String, String), (u64, f64)> = BTreeMap::new();
    for e in events {
        if let Body::Usage(u) = &e.body {
            let k = (u.role.clone(), u.model.clone());
            let v = m.entry(k).or_default();
            v.0 += u.tokens();
            v.1 += u.cost_usd;
        }
    }
    m.into_iter()
        .map(|((r, mo), (t, c))| (r, mo, t, c))
        .collect()
}

/// The item's whole record as Markdown.
pub fn render_export(repo: &Repo, item: &Item, events: &[Event]) -> String {
    let id = item.id;
    let mut s = String::new();
    let _ = writeln!(s, "# {id} · {}\n", item.title);

    // Header.
    let mut rows: Vec<(String, String)> = vec![
        (
            "State".into(),
            format!("{} ({})", item.state, item.state.room()),
        ),
        ("Branch".into(), format!("`{}`", item.branch)),
        ("Base".into(), format!("`{}`", item.base)),
        ("Worktree".into(), format!("`{}`", item.worktree)),
        ("Created".into(), time(item.created_at)),
        (
            "Rounds".into(),
            format!("{} · {} rejected", item.round, item.rejects),
        ),
    ];
    if let Some(i) = &item.issue {
        rows.push((
            "Issue".into(),
            match &i.url {
                Some(u) => format!("[#{}]({u})", i.number),
                None => format!("#{}", i.number),
            },
        ));
    }
    if let Some(pr) = &item.pr_url {
        rows.push(("PR".into(), pr.clone()));
    }
    if let Some(p) = item.parent {
        rows.push(("Parent".into(), p.to_string()));
    }
    let children = children(repo, item, events);
    if !children.is_empty() {
        let c: Vec<String> = children.iter().map(ItemId::to_string).collect();
        rows.push(("Children".into(), c.join(", ")));
    }
    if let Some(f) = &item.follow_up {
        rows.push(("Follow-up of".into(), f.clone()));
    }
    if let Some(h) = &item.hallmark_sha {
        rows.push(("Hallmark".into(), format!("`{}`", short(h))));
    }
    if let Some(d) = &item.drop_reason {
        rows.push(("Dropped".into(), format!("\"{d}\"")));
    }
    if item.state.is_terminal() {
        rows.push(("Ended".into(), time(item.state_since)));
    }
    s.push_str("| | |\n| --- | --- |\n");
    for (k, v) in rows {
        let _ = writeln!(s, "| {k} | {} |", cell(&v));
    }

    if let Some(r) = crate::fixer::request(events) {
        let _ = writeln!(s, "\n## Request\n");
        for l in r.trim().lines() {
            let _ = writeln!(s, "> {l}");
        }
    }
    if let Some(sum) = &item.summary {
        let _ = writeln!(s, "\n**Developer's summary:** {}", sum.trim());
    }

    // Timeline.
    let _ = writeln!(s, "\n## Timeline\n");
    s.push_str("| Time | Actor | Event |\n| --- | --- | --- |\n");
    for e in events {
        let _ = writeln!(
            s,
            "| {} | {} | {} |",
            e.t.format("%Y-%m-%d %H:%M:%S"),
            cell(&e.actor),
            cell(&e.body.summary())
        );
    }

    // Checks.
    let checks: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e.body, Body::Checks { .. }))
        .collect();
    if !checks.is_empty() {
        let _ = writeln!(s, "\n## Checks");
        for e in checks {
            let Body::Checks {
                sha,
                round,
                passed,
                runs,
            } = &e.body
            else {
                continue;
            };
            let _ = writeln!(
                s,
                "\n### Round {round} · {} at `{}`\n",
                if *passed { "passed" } else { "failed" },
                short(sha)
            );
            for r in runs {
                let _ = write!(
                    s,
                    "- `{}` · exit {} · {:.1}s",
                    r.command,
                    r.exit_code,
                    r.duration_ms as f64 / 1000.0
                );
                if let Some(log) = &r.log {
                    let _ = write!(s, " · log `{log}`");
                }
                s.push('\n');
            }
        }
    }

    // Reviews and fixes, in order.
    let mut ledger = Ledger::default();
    let mut any_review = false;
    for e in events {
        match &e.body {
            Body::Verdict {
                sha,
                round,
                verdict,
                escalated,
                findings,
                prior,
                counts,
                summary,
                report,
                reviewer,
                ..
            } => {
                if !any_review {
                    let _ = writeln!(s, "\n## Reviews and fixes");
                    any_review = true;
                }
                let v = match (verdict, escalated) {
                    (VerdictKind::Approve, _) => "approved",
                    (_, true) => "changes requested, escalated",
                    _ => "changes requested",
                };
                let _ = writeln!(s, "\n### Review, round {round} · {v} · `{}`\n", short(sha));
                if let Some(r) = reviewer {
                    let _ = writeln!(s, "Reviewer: {r} · {}\n", time(e.t));
                }
                if let Some(sum) = summary {
                    let _ = writeln!(s, "{}\n", sum.trim());
                }
                if !prior.is_empty() {
                    s.push_str("Rulings on earlier findings:\n\n");
                    for p in prior {
                        let st = match p.status {
                            PriorStatus::Fixed => "fixed",
                            PriorStatus::Open => "still open",
                            PriorStatus::Withdrawn => "withdrawn",
                        };
                        let _ = write!(s, "- {} {st}", p.id);
                        if let Some(n) = &p.note {
                            let _ = write!(s, ": {}", n.trim());
                        }
                        s.push('\n');
                    }
                    s.push('\n');
                }
                if findings.is_empty() {
                    s.push_str("No new findings.\n");
                } else {
                    for f in findings {
                        let _ = writeln!(
                            s,
                            "- **{}** {} · {} · `{}:{}` · {}\n  - Scenario: {}\n  - Fix: {}",
                            f.id,
                            f.severity.as_str(),
                            f.lens.as_str(),
                            f.file,
                            f.line,
                            f.title.trim(),
                            clip(&f.scenario, 600),
                            clip(&f.fix, 400)
                        );
                    }
                }
                let _ = writeln!(
                    s,
                    "\nOpen after this review: {} blocker · {} major · {} minor · {} nit",
                    counts.blocker, counts.major, counts.minor, counts.nit
                );
                if let Some(r) = report {
                    let _ = writeln!(
                        s,
                        "Full report: `{}`",
                        crate::paths::display(&repo.hall.item_dir(id).join(r))
                    );
                }
            }
            Body::FixSubmitted {
                sha,
                fixes,
                disputes,
            } => {
                if !any_review {
                    let _ = writeln!(s, "\n## Reviews and fixes");
                    any_review = true;
                }
                let _ = writeln!(s, "\n### Fix by {} · `{}`\n", e.actor, short(sha));
                if fixes.is_empty() && disputes.is_empty() {
                    s.push_str("- fixed the failing checks\n");
                }
                if !fixes.is_empty() {
                    let _ = writeln!(s, "- claims fixed: {}", fixes.join(", "));
                }
                for d in disputes {
                    let _ = writeln!(s, "- disputes {}: {}", d.id, d.reason.trim());
                }
            }
            _ => {}
        }
        ledger.apply(&e.body);
    }

    // The ledger as it stands.
    if !ledger.findings.is_empty() {
        let _ = writeln!(s, "\n## Findings\n");
        s.push_str("| ID | Round | Severity | Lens | Where | Title | Status |\n");
        s.push_str("| --- | --- | --- | --- | --- | --- | --- |\n");
        for t in &ledger.findings {
            let f = &t.finding;
            let mut st = match t.status {
                Status::Open => "open".to_string(),
                Status::Fixed => "fixed".into(),
                Status::Withdrawn => "withdrawn".into(),
            };
            if let Some(r) = t.settled_round {
                let _ = write!(st, " (round {r})");
            }
            if t.dispute.is_some() {
                st.push_str(", disputed");
            }
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | `{}:{}` | {} | {} |",
                f.id,
                f.round,
                f.severity.as_str(),
                f.lens.as_str(),
                cell(&f.file),
                f.line,
                cell(&f.title),
                st
            );
        }
        let open_minor: Vec<_> = ledger
            .open()
            .into_iter()
            .filter(|t| matches!(t.finding.severity, Severity::Minor | Severity::Nit))
            .collect();
        if !open_minor.is_empty() && item.state != State::Dropped {
            let ids: Vec<&str> = open_minor.iter().map(|t| t.finding.id.as_str()).collect();
            let _ = writeln!(
                s,
                "\nOpen minors and nits ride along into the PR: {}.",
                ids.join(", ")
            );
        }
    }

    // Questions.
    let mut answers: BTreeMap<&str, (&str, bool)> = BTreeMap::new();
    for e in events {
        if let Body::Answer {
            qid,
            answer,
            defaulted,
        } = &e.body
        {
            answers.insert(qid, (answer, *defaulted));
        }
    }
    let questions: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e.body, Body::Question { .. }))
        .collect();
    if !questions.is_empty() {
        let _ = writeln!(s, "\n## Questions\n");
        for e in questions {
            let Body::Question { qid, text, .. } = &e.body else {
                continue;
            };
            let _ = writeln!(s, "- **{qid}** ({}): {}", e.actor, text.trim());
            match answers.get(qid.as_str()) {
                Some((a, true)) => {
                    let _ = writeln!(s, "  - Answer (default, no reply): {a}");
                }
                Some((a, false)) => {
                    let _ = writeln!(s, "  - Answer: {a}");
                }
                None => s.push_str("  - Not answered\n"),
            }
        }
    }

    // Usage.
    let usage = usage_by_role(events);
    if !usage.is_empty() {
        let _ = writeln!(s, "\n## Usage\n");
        s.push_str("| Role | Model | Tokens | Cost (list price) |\n| --- | --- | --- | --- |\n");
        let (mut tt, mut tc) = (0u64, 0f64);
        for (role, model, tokens, cost) in &usage {
            tt += tokens;
            tc += cost;
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} |",
                cell(role),
                cell(model),
                crate::usage::fmt_tokens(*tokens),
                crate::usage::fmt_usd(*cost)
            );
        }
        let _ = writeln!(
            s,
            "| **Total** | | **{}** | **{}** |",
            crate::usage::fmt_tokens(tt),
            crate::usage::fmt_usd(tc)
        );
    }

    // The hallmark.
    if let Some(h) = &item.hallmark_sha {
        let _ = writeln!(s, "\n## Hallmark\n");
        let _ = writeln!(s, "Struck on `{h}`.");
        if let Some(note) = crate::stamp::read(&repo.main_root, h) {
            let _ = writeln!(
                s,
                "\nRound {} · reviewer {} · open then: {} minor, {} nit · {}",
                note.round,
                note.reviewer.as_deref().unwrap_or("unknown"),
                note.findings.minor,
                note.findings.nit,
                time(note.t)
            );
        }
    }
    s
}

/// Child items: from the plan, merges, and the index.
fn children(repo: &Repo, item: &Item, events: &[Event]) -> Vec<ItemId> {
    let mut out: Vec<ItemId> = Vec::new();
    for e in events {
        match &e.body {
            Body::Planned { children, .. } => out.extend(children.iter().copied()),
            Body::ChildMerged { child, .. } => out.push(*child),
            _ => {}
        }
    }
    if let Ok(ix) = repo.index()
        && let Ok(items) = ix.items(true)
    {
        out.extend(
            items
                .iter()
                .filter(|i| i.parent == Some(item.id))
                .map(|i| i.id),
        );
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(duration(5), "5s");
        assert_eq!(duration(720), "12m");
        assert_eq!(duration(3 * 3600 + 300), "3h 05m");
        assert_eq!(duration(2 * 86_400 + 4 * 3600), "2d 04h");
    }

    #[test]
    fn cells_are_safe() {
        assert_eq!(cell("a | b\nc"), "a \\| b c");
    }
}
