//! `hall stats`: how the review loop is doing (ADR 0056).
//!
//! Every number here is folded from the journals: reviews to approval,
//! what became of each finding, how often the reviewer was wrong
//! (noise), and how disputes went. These are the numbers that say whether
//! a change to the reviewer, its skill or its model helped.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::StatsArgs;
use crate::error::{HallError, Result};
use crate::event::{Body, Event, VerdictKind};
use crate::findings::{Ledger, Lens, Severity, Status};
use crate::item::{Item, State};
use crate::output::{Render, Response};
use crate::repo::Repo;

#[derive(Debug, Serialize, JsonSchema)]
pub struct StatsOut {
    /// The start of the window, when `--since` was given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<DateTime<Utc>>,
    /// True when live items are counted too (`--all`).
    pub all: bool,
    pub items: ItemStats,
    pub reviews: ReviewStats,
    pub findings: FindingStats,
}

#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct ItemStats {
    /// Items counted: ended in the window, plus live ones with `--all`.
    pub counted: u32,
    pub closed: u32,
    pub dropped: u32,
    /// Parallel parts merged into their parent.
    pub merged: u32,
    /// Still live (only with `--all`).
    pub live: u32,
    /// Items that reached `max_rejects` at least once.
    pub escalated: u32,
    /// Items whose journal could not be read: left out of every count, so
    /// the numbers are incomplete when this is not empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable: Vec<String>,
}

#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct ReviewStats {
    /// Items with at least one review.
    pub reviewed: u32,
    /// Items with at least one approving review.
    pub approved: u32,
    /// Reviews (verdict events).
    pub reviews: u32,
    /// Fixes the fixer submitted.
    pub fixes: u32,
    /// Gate runs that failed.
    pub checks_failed: u32,
    /// Reviews it took each approved item to pass, the first time.
    pub to_approval: Rounds,
}

/// Reviews to approval, over the items that were approved.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct Rounds {
    pub mean: f64,
    pub median: f64,
    /// Approved by the first review.
    pub one: u32,
    pub one_pct: f64,
    /// Needed three reviews or more.
    pub three_or_more: u32,
    pub three_or_more_pct: f64,
}

#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct FindingStats {
    pub raised: u32,
    pub blocker: u32,
    pub major: u32,
    pub minor: u32,
    pub nit: u32,
    pub fixed: u32,
    /// Withdrawn by a later review: the finding was wrong, or a dispute
    /// was right.
    pub withdrawn: u32,
    /// Still open when the item ended (or now, for live items).
    pub open: u32,
    /// Withdrawn findings as a share of those raised: the reviewer's noise.
    pub noise_pct: f64,
    /// Findings the fixer disputed.
    pub disputed: u32,
    /// Disputes the reviewer accepted (the finding was withdrawn).
    pub disputes_upheld: u32,
    /// Disputes the reviewer ruled on and turned down (the finding stayed
    /// open or was fixed anyway).
    pub disputes_rejected: u32,
    /// Disputes no review has ruled on yet: the item ended, or is still in
    /// flight, before the next review.
    pub disputes_pending: u32,
    pub by_lens: Vec<LensStats>,
}

#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct LensStats {
    pub lens: String,
    pub raised: u32,
    pub fixed: u32,
    pub withdrawn: u32,
    pub open: u32,
    pub noise_pct: f64,
}

impl Render for StatsOut {
    fn render(&self) -> String {
        let mut s = String::new();
        let i = &self.items;
        let mut window = match self.since {
            Some(t) => format!("since {}", t.format("%Y-%m-%d %H:%M UTC")),
            None => "all time".into(),
        };
        if self.all {
            window.push_str(", live items included");
        }
        let _ = writeln!(s, "Hallmark stats ({window})\n");
        if i.counted == 0 {
            let _ = writeln!(s, "No items yet.");
            return s;
        }
        let mut parts = vec![format!("{} closed", i.closed)];
        if i.merged > 0 {
            parts.push(format!("{} merged", i.merged));
        }
        if i.dropped > 0 {
            parts.push(format!("{} dropped", i.dropped));
        }
        if i.live > 0 {
            parts.push(format!("{} live", i.live));
        }
        let _ = writeln!(
            s,
            "Items      {} ({}) · {} escalated",
            i.counted,
            parts.join(", "),
            i.escalated
        );
        let r = &self.reviews;
        let _ = writeln!(
            s,
            "Reviews    {} reviews on {} items, {} approved · {} fixes · {} failed checks",
            r.reviews, r.reviewed, r.approved, r.fixes, r.checks_failed
        );
        if r.approved > 0 {
            let t = &r.to_approval;
            let _ = writeln!(
                s,
                "To approve mean {:.2}, median {} · one review {:.0}% ({}) · three or more {:.0}% ({})",
                t.mean,
                fmt_median(t.median),
                t.one_pct,
                t.one,
                t.three_or_more_pct,
                t.three_or_more
            );
        }
        let f = &self.findings;
        let _ = writeln!(
            s,
            "Findings   {} raised ({} blocker, {} major, {} minor, {} nit) · {} fixed, {} withdrawn, {} open · noise {:.0}%",
            f.raised, f.blocker, f.major, f.minor, f.nit, f.fixed, f.withdrawn, f.open, f.noise_pct
        );
        if f.disputed > 0 {
            let _ = write!(
                s,
                "Disputes   {} · {} upheld, {} rejected",
                f.disputed, f.disputes_upheld, f.disputes_rejected
            );
            if f.disputes_pending > 0 {
                let _ = write!(s, ", {} not yet ruled on", f.disputes_pending);
            }
            s.push('\n');
        }
        if !i.unreadable.is_empty() {
            let _ = writeln!(
                s,
                "\nIncomplete: {} unreadable journal(s) left out: {}",
                i.unreadable.len(),
                i.unreadable.join(", ")
            );
        }
        if !f.by_lens.is_empty() {
            let _ = writeln!(s, "\nBy lens");
            for l in &f.by_lens {
                let _ = writeln!(
                    s,
                    "  {:<18} {:>3} raised · {:>3} fixed · {:>3} withdrawn · {:>3} open · noise {:.0}%",
                    l.lens, l.raised, l.fixed, l.withdrawn, l.open, l.noise_pct
                );
            }
        }
        s
    }
}

fn fmt_median(m: f64) -> String {
    if m.fract() == 0.0 {
        format!("{m:.0}")
    } else {
        format!("{m:.1}")
    }
}

fn pct(part: u32, whole: u32) -> f64 {
    if whole == 0 {
        0.0
    } else {
        round2(100.0 * f64::from(part) / f64::from(whole))
    }
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

pub fn stats(cwd: &Path, a: StatsArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    repo.require_init()?;
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
    let mut out = StatsOut {
        since,
        all: a.all,
        items: ItemStats::default(),
        reviews: ReviewStats::default(),
        findings: FindingStats::default(),
    };
    let mut to_approval: Vec<u32> = Vec::new();
    let mut by_lens: BTreeMap<Lens, LensStats> = BTreeMap::new();
    for item in ix.items(true)? {
        if !item.state.is_terminal() && !a.all {
            continue;
        }
        if !in_window(&item, since) {
            continue;
        }
        let events = match repo.hall.read_item(item.id) {
            Ok(e) => e,
            Err(_) => {
                out.items.unreadable.push(item.id.to_string());
                continue;
            }
        };
        count_item(&mut out, &mut to_approval, &mut by_lens, &item, &events);
    }
    finish(&mut out, &to_approval, by_lens);
    let mut next = vec!["hall history --rejected".to_string()];
    if since.is_none() {
        next.push("hall stats --since 30d".into());
    }
    next.push("hall usage --since 30d".into());
    Ok(Response::new(out, next))
}

/// Whether the item falls in the `--since` window: a finished item by when
/// it ended, a live one (only counted with `--all`) by its last update.
fn in_window(item: &Item, since: Option<DateTime<Utc>>) -> bool {
    let ended_at = if item.state.is_terminal() {
        item.state_since
    } else {
        item.updated_at
    };
    !since.is_some_and(|t| ended_at < t)
}

fn count_item(
    out: &mut StatsOut,
    to_approval: &mut Vec<u32>,
    by_lens: &mut BTreeMap<Lens, LensStats>,
    item: &Item,
    events: &[Event],
) {
    let i = &mut out.items;
    i.counted += 1;
    match item.state {
        State::Closed if item.parent.is_some() && item.pr_url.is_none() => i.merged += 1,
        State::Closed => i.closed += 1,
        State::Dropped => i.dropped += 1,
        _ => i.live += 1,
    }
    let r = &mut out.reviews;
    let mut reviews = 0u32;
    let mut first_approval: Option<u32> = None;
    let mut escalated = false;
    // Disputed findings no verdict has ruled on since the dispute.
    let mut unruled: BTreeSet<&str> = BTreeSet::new();
    for e in events {
        match &e.body {
            Body::Verdict {
                verdict,
                escalated: esc,
                prior,
                ..
            } => {
                for p in prior {
                    unruled.remove(p.id.as_str());
                }
                reviews += 1;
                if *esc {
                    escalated = true;
                }
                if *verdict == VerdictKind::Approve && first_approval.is_none() {
                    first_approval = Some(reviews);
                }
            }
            Body::FixSubmitted { disputes, .. } => {
                r.fixes += 1;
                unruled.extend(disputes.iter().map(|d| d.id.as_str()));
            }
            Body::Checks { passed: false, .. } => r.checks_failed += 1,
            _ => {}
        }
    }
    if escalated {
        i.escalated += 1;
    }
    if reviews > 0 {
        r.reviewed += 1;
        r.reviews += reviews;
    }
    if let Some(n) = first_approval {
        r.approved += 1;
        to_approval.push(n);
    }
    let f = &mut out.findings;
    for t in &Ledger::from_events(events).findings {
        f.raised += 1;
        match t.finding.severity {
            Severity::Blocker => f.blocker += 1,
            Severity::Major => f.major += 1,
            Severity::Minor => f.minor += 1,
            Severity::Nit => f.nit += 1,
        }
        let l = by_lens.entry(t.finding.lens).or_insert_with(|| LensStats {
            lens: t.finding.lens.as_str().into(),
            ..Default::default()
        });
        l.raised += 1;
        match t.status {
            Status::Fixed => {
                f.fixed += 1;
                l.fixed += 1;
            }
            Status::Withdrawn => {
                f.withdrawn += 1;
                l.withdrawn += 1;
            }
            Status::Open => {
                f.open += 1;
                l.open += 1;
            }
        }
        if t.dispute.is_some() {
            f.disputed += 1;
            if unruled.contains(t.finding.id.as_str()) {
                f.disputes_pending += 1;
            } else if t.status == Status::Withdrawn {
                f.disputes_upheld += 1;
            } else {
                f.disputes_rejected += 1;
            }
        }
    }
}

fn finish(out: &mut StatsOut, to_approval: &[u32], by_lens: BTreeMap<Lens, LensStats>) {
    let t = &mut out.reviews.to_approval;
    if !to_approval.is_empty() {
        let n = to_approval.len() as u32;
        let sum: u32 = to_approval.iter().sum();
        t.mean = round2(f64::from(sum) / f64::from(n));
        let mut sorted = to_approval.to_vec();
        sorted.sort_unstable();
        let mid = sorted.len() / 2;
        t.median = if sorted.len().is_multiple_of(2) {
            f64::from(sorted[mid - 1] + sorted[mid]) / 2.0
        } else {
            f64::from(sorted[mid])
        };
        t.one = sorted.iter().filter(|&&r| r == 1).count() as u32;
        t.three_or_more = sorted.iter().filter(|&&r| r >= 3).count() as u32;
        t.one_pct = pct(t.one, n);
        t.three_or_more_pct = pct(t.three_or_more, n);
    }
    let f = &mut out.findings;
    f.noise_pct = pct(f.withdrawn, f.raised);
    f.by_lens = by_lens
        .into_values()
        .map(|mut l| {
            l.noise_pct = pct(l.withdrawn, l.raised);
            l
        })
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_of_even_and_odd_sets() {
        let mut out = empty();
        finish(&mut out, &[1, 2, 4, 1], BTreeMap::new());
        let t = &out.reviews.to_approval;
        assert_eq!(t.mean, 2.0);
        assert_eq!(t.median, 1.5);
        assert_eq!((t.one, t.three_or_more), (2, 1));
        assert_eq!((t.one_pct, t.three_or_more_pct), (50.0, 25.0));

        let mut out = empty();
        finish(&mut out, &[3, 1, 2], BTreeMap::new());
        assert_eq!(out.reviews.to_approval.median, 2.0);
    }

    #[test]
    fn text_has_one_line_per_block_with_the_numbers() {
        let mut out = empty();
        out.items = ItemStats {
            counted: 2,
            closed: 1,
            dropped: 1,
            ..Default::default()
        };
        out.reviews = ReviewStats {
            reviewed: 1,
            approved: 1,
            reviews: 2,
            fixes: 1,
            checks_failed: 0,
            to_approval: Rounds::default(),
        };
        out.findings = FindingStats {
            raised: 3,
            major: 1,
            minor: 1,
            nit: 1,
            fixed: 1,
            withdrawn: 1,
            open: 1,
            disputed: 1,
            disputes_upheld: 1,
            ..Default::default()
        };
        let mut by_lens = BTreeMap::new();
        by_lens.insert(
            Lens::RepoRules,
            LensStats {
                lens: "repo_rules".into(),
                raised: 1,
                withdrawn: 1,
                ..Default::default()
            },
        );
        finish(&mut out, &[2], by_lens);
        let text = out.render();
        for s in [
            "Hallmark stats (all time)",
            "Items      2 (1 closed, 1 dropped) · 0 escalated",
            "Reviews    2 reviews on 1 items, 1 approved · 1 fixes · 0 failed checks",
            "To approve mean 2.00, median 2 · one review 0% (0) · three or more 0% (0)",
            "Findings   3 raised (0 blocker, 1 major, 1 minor, 1 nit) · 1 fixed, 1 withdrawn, 1 open · noise 33%",
            "Disputes   1 · 1 upheld, 0 rejected",
            "repo_rules           1 raised ·   0 fixed ·   1 withdrawn ·   0 open · noise 100%",
        ] {
            assert!(text.contains(s), "{s:?} in\n{text}");
        }
    }

    #[test]
    fn no_approvals_means_zeros_not_a_panic() {
        let mut out = empty();
        finish(&mut out, &[], BTreeMap::new());
        assert_eq!(out.reviews.to_approval.mean, 0.0);
        assert_eq!(out.findings.noise_pct, 0.0);
        assert!(out.render().contains("No items yet."));
    }

    fn item(state: &str, parent: bool, ended: DateTime<Utc>) -> Item {
        let id = serde_json::to_value(crate::item::ItemId(1)).unwrap();
        let parent = if parent {
            serde_json::to_value(crate::item::ItemId(2)).unwrap()
        } else {
            serde_json::Value::Null
        };
        serde_json::from_value(serde_json::json!({
            "id": id, "title": "t", "state": state, "branch": "b",
            "worktree": "w", "base": "main",
            "created_at": ended, "updated_at": ended, "state_since": ended,
            "round": 0, "rejects": 0, "adopted": false, "events": 0,
            "parent": parent,
        }))
        .unwrap()
    }

    #[test]
    fn the_window_keeps_what_ended_inside_it() {
        let now = Utc::now();
        let day = chrono::Duration::days(1);
        let since = Some(now - day * 7);
        let old = item("closed", false, now - day * 30);
        let recent = item("closed", false, now - day);
        assert!(!in_window(&old, since));
        assert!(in_window(&recent, since));
        assert!(in_window(&old, None));
        // A live item (counted with --all) is windowed by its last update.
        let stale = item("review", false, now - day * 30);
        let busy = item("review", false, now - day);
        assert!(!in_window(&stale, since));
        assert!(in_window(&busy, since));
    }

    #[test]
    fn a_merged_part_is_not_a_closed_item() {
        let now = Utc::now();
        let mut out = empty();
        let mut t = Vec::new();
        let mut l = BTreeMap::new();
        count_item(&mut out, &mut t, &mut l, &item("closed", true, now), &[]);
        assert_eq!((out.items.merged, out.items.closed), (1, 0));
        count_item(&mut out, &mut t, &mut l, &item("closed", false, now), &[]);
        assert_eq!((out.items.merged, out.items.closed), (1, 1));
        count_item(&mut out, &mut t, &mut l, &item("dropped", true, now), &[]);
        assert_eq!(out.items.dropped, 1);
        assert_eq!(out.items.counted, 3);
    }

    #[test]
    fn unreadable_journals_are_named_in_the_text() {
        let mut out = empty();
        out.items.counted = 1;
        out.items.closed = 1;
        out.items.unreadable = vec!["H-0007".into()];
        let text = out.render();
        assert!(text.contains("Incomplete: 1 unreadable journal(s) left out: H-0007"));
    }

    fn empty() -> StatsOut {
        StatsOut {
            since: None,
            all: false,
            items: ItemStats::default(),
            reviews: ReviewStats::default(),
            findings: FindingStats::default(),
        }
    }
}
