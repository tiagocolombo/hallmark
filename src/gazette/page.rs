//! The facts on a Gazette page, from the journal, in words a person reads.
//! Nothing here runs a model: the librarian's prose, when there is some,
//! replaces the headline, the story and each finding's words.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use super::{Diagram, Prose, attached, image_type, is_url};
use crate::event::{Body, Event, VerdictKind, short};
use crate::findings::{Counts, Ledger, Severity, Status as FindingStatus, Tracked};
use crate::item::{Item, State};
use crate::library::store::Store;
use crate::output::clip;
use crate::repo::Repo;

/// Everything the page shows, already in words.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Page {
    pub id: String,
    pub repo: String,
    pub base: String,
    pub date: String,
    pub outcome: String,
    pub took: Option<String>,
    pub rounds: u32,
    pub cost: f64,
    pub kicker: String,
    pub headline: String,
    pub deck: String,
    pub story: Vec<String>,
    /// The hallmarked commit, short.
    pub stamp: Option<String>,
    pub byline: String,
    pub plates: Vec<Plate>,
    pub links: Vec<(String, Option<String>)>,
    pub diagram: Option<Diagram>,
    pub verdicts: Vec<VerdictRow>,
    pub checks: Vec<CheckRow>,
    /// The PR's CI: passed or not, when it reported.
    pub ci: Option<bool>,
    pub brief: Vec<(String, String)>,
    pub caught: Vec<Desk>,
    pub fixed: usize,
    pub corrections: Vec<Correction>,
    pub lessons: Vec<Lesson>,
    pub ledger: Vec<LedgerRow>,
    pub dispatch: Vec<(String, String, String)>,
    pub events: usize,
    pub by_librarian: bool,
}

/// An attached image (inlined) or file (linked).
#[derive(Debug, Clone, PartialEq)]
pub struct Plate {
    /// A `data:` URI for an image, or a path relative to the page.
    pub src: String,
    pub image: bool,
    pub caption: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerdictRow {
    pub round: u32,
    /// Open at this verdict.
    pub counts: Counts,
    pub line: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CheckRow {
    pub name: String,
    pub passed: u32,
    pub ran: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Desk {
    pub name: String,
    pub stories: Vec<Story>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Story {
    pub headline: String,
    pub text: String,
    pub severity: Severity,
    pub outcome: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Correction {
    pub text: String,
    pub id: String,
    pub severity: Severity,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Lesson {
    pub id: String,
    pub title: String,
    /// "A rule, waiting for you", "A map of the code, kept".
    pub note: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LedgerRow {
    pub who: String,
    pub model: String,
    pub did: String,
    pub cost: f64,
}

/// Builds the page for `item`.
pub fn build(repo: &Repo, item: &Item, events: &[Event], prose: Option<&Prose>) -> Page {
    let ledger = Ledger::from_events(events);
    let ended = end_time(item, events);
    let rounds = events
        .iter()
        .filter(|e| matches!(e.body, Body::Verdict { .. }))
        .count() as u32;
    let usage = crate::commands::record::usage_by_role(events);
    let cost: f64 = usage.iter().map(|u| u.3).sum();
    let p = prose.cloned().unwrap_or_default();

    let mut page = Page {
        id: item.id.to_string(),
        repo: repo.name.clone(),
        base: item.base.clone(),
        date: ended.format("%A, %-d %B %Y").to_string(),
        outcome: outcome(item).into(),
        took: took(item, ended),
        rounds,
        cost,
        stamp: item.hallmark_sha.as_deref().map(|s| short(s).to_string()),
        byline: byline(events),
        events: events.len(),
        by_librarian: prose.is_some(),
        ..Page::default()
    };

    // The words: the librarian's, else the item's own.
    let (why, how) = proposal(events);
    page.kicker = non_empty(&p.kicker).unwrap_or_else(|| kicker(item));
    page.headline = non_empty(&p.headline).unwrap_or_else(|| item.title.clone());
    page.deck = non_empty(&p.deck)
        .or_else(|| item.summary.clone())
        .unwrap_or_default();
    page.story = p
        .story
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if page.story.is_empty() {
        page.story = [crate::fixer::request(events), why, how]
            .into_iter()
            .flatten()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    page.diagram = Some(p.diagram.clone()).filter(|d| d.nodes.len() >= 2);

    // Plates and links.
    let dir = repo.hall.item_dir(item.id);
    for (path, caption) in attached(events) {
        if is_url(&path) {
            page.links.push((path, caption));
            continue;
        }
        let image = image_type(&path);
        let src = match image.zip(std::fs::read(dir.join(&path)).ok()) {
            Some((mime, bytes)) => format!("data:{mime};base64,{}", super::html::base64(&bytes)),
            None => path.clone(),
        };
        page.plates.push(Plate {
            src,
            image: image.is_some(),
            caption,
        });
    }
    if let Some(r) = crate::fixer::request(events) {
        for url in urls(&r) {
            if !page.links.iter().any(|(u, _)| *u == url) {
                page.links
                    .push((url, Some("linked from the request".into())));
            }
        }
    }

    // The verdicts, round by round.
    let line_for = |round: u32| {
        p.rounds
            .iter()
            .find(|r| r.round == round)
            .and_then(|r| non_empty(&r.line))
    };
    for e in events {
        if let Body::Verdict {
            round,
            verdict,
            escalated,
            counts,
            ..
        } = &e.body
        {
            let line =
                line_for(*round).unwrap_or_else(|| verdict_line(*verdict, *escalated, counts));
            page.verdicts.push(VerdictRow {
                round: *round,
                counts: *counts,
                line,
            });
        }
    }

    // Checks: how often each gate passed.
    let mut checks: Vec<CheckRow> = Vec::new();
    for e in events {
        match &e.body {
            Body::Checks { runs, .. } => {
                for r in runs {
                    let name = gate_name(&r.command);
                    let row = match checks.iter_mut().position(|c| c.name == name) {
                        Some(i) => &mut checks[i],
                        None => {
                            checks.push(CheckRow {
                                name,
                                passed: 0,
                                ran: 0,
                            });
                            checks.last_mut().expect("just pushed")
                        }
                    };
                    row.ran += 1;
                    if r.exit_code == 0 {
                        row.passed += 1;
                    }
                }
            }
            Body::PrChecks { passed, .. } => page.ci = Some(*passed),
            _ => {}
        }
    }
    page.checks = checks;
    page.brief = brief(item, events);

    // What the reviewer caught, by desk; what shipped open.
    let words = |t: &Tracked| -> (String, String) {
        let f = &t.finding;
        match p.findings.iter().find(|x| x.id == f.id) {
            Some(x) if !x.headline.trim().is_empty() => {
                (x.headline.trim().to_string(), x.text.trim().to_string())
            }
            _ => (f.title.clone(), clip(&f.scenario, 320)),
        }
    };
    let mut desks: BTreeMap<usize, Desk> = BTreeMap::new();
    for t in &ledger.findings {
        let (headline, text) = words(t);
        let id = t.finding.id.clone();
        let severity = t.finding.severity;
        let outcome = match (t.status, t.settled_round) {
            (FindingStatus::Fixed, Some(r)) => format!("Fixed in round {r}"),
            (FindingStatus::Fixed, None) => "Fixed".into(),
            (FindingStatus::Withdrawn, Some(r)) => format!("Withdrawn in round {r}"),
            (FindingStatus::Withdrawn, None) => "Withdrawn".into(),
            (FindingStatus::Open, _) => {
                let text = if headline.ends_with('.') {
                    headline
                } else {
                    format!("{headline}.")
                };
                page.corrections.push(Correction { text, id, severity });
                continue;
            }
        };
        if t.status == FindingStatus::Fixed {
            page.fixed += 1;
        }
        let (order, name) = desk(t.finding.lens.as_str());
        desks
            .entry(order)
            .or_insert_with(|| Desk {
                name: name.into(),
                stories: Vec::new(),
            })
            .stories
            .push(Story {
                headline,
                text,
                severity,
                outcome,
                id,
            });
    }
    page.caught = desks.into_values().collect();

    // The Library.
    let store = Store::of(repo);
    let mut ids: Vec<String> = Vec::new();
    for e in events {
        if let Body::Distilled { learnings, .. } = &e.body {
            for id in learnings {
                if !ids.contains(id) {
                    ids.push(id.clone());
                }
            }
        }
    }
    for id in ids {
        if let Some(l) = store.get(&id) {
            let l = l.learning;
            page.lessons.push(Lesson {
                note: format!(
                    "{}, {}",
                    kind_words(l.kind.as_str()),
                    status_words(l.status.as_str())
                ),
                id,
                title: l.title,
            });
        }
    }

    // The ledger.
    let mut by_role: BTreeMap<(usize, String), LedgerRow> = BTreeMap::new();
    for (role, model, _, c) in &usage {
        let (order, who) = role_name(role);
        let row = by_role
            .entry((order, role.clone()))
            .or_insert_with(|| LedgerRow {
                who: who.into(),
                model: String::new(),
                did: did(role, events),
                cost: 0.0,
            });
        let m = model_name(model);
        if !row.model.split(", ").any(|x| x == m) {
            if !row.model.is_empty() {
                row.model.push_str(", ");
            }
            row.model.push_str(&m);
        }
        row.cost += c;
    }
    page.ledger = by_role.into_values().collect();

    // Dispatch.
    if let Some(url) = &item.pr_url {
        let n = url.rsplit('/').next().unwrap_or(url);
        let state = match item.delivered.as_deref() {
            Some("merged") => "merged".to_string(),
            Some(o) => o.to_string(),
            None => "open".into(),
        };
        page.dispatch
            .push(("Pull request".into(), format!("#{n}"), state));
    }
    let commits = events
        .iter()
        .filter(|e| matches!(e.body, Body::Commit { .. }))
        .count();
    let fixer = events
        .iter()
        .filter(|e| matches!(e.body, Body::FixSubmitted { .. }))
        .count();
    if commits > 0 {
        page.dispatch.push((
            "Commits".into(),
            commits.to_string(),
            if fixer > 0 {
                format!("{fixer} by the fixer")
            } else {
                "by the dev agent".into()
            },
        ));
    }
    page.dispatch.push((
        "Review".into(),
        format!("{rounds} round{}", plural(rounds as usize)),
        format!("{} rejected", item.rejects),
    ));
    let total = ledger.findings.len();
    page.dispatch.push((
        "Findings".into(),
        total.to_string(),
        format!("{} fixed, {} open", page.fixed, page.corrections.len()),
    ));
    page
}

fn non_empty(s: &str) -> Option<String> {
    Some(s.trim().to_string()).filter(|s| !s.is_empty())
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// When the item's story ends: its PR merged or closed, else its state.
fn end_time(item: &Item, events: &[Event]) -> DateTime<Utc> {
    events
        .iter()
        .rev()
        .find(|e| matches!(e.body, Body::Delivered { .. }))
        .map(|e| e.t)
        .unwrap_or(item.state_since)
}

fn outcome(item: &Item) -> &'static str {
    match (item.state, item.delivered.as_deref()) {
        (State::Closed, Some("merged")) => "Merged",
        (State::Closed, Some(_)) => "Closed without merging",
        (State::Closed, None) if item.pr_url.is_some() => "Pull request open",
        (State::Closed, None) => "Closed",
        (State::Escalated, _) => "Waiting for your call",
        (State::Dropped, _) => "Dropped",
        _ => "In progress",
    }
}

fn kicker(item: &Item) -> String {
    match item.state {
        State::Escalated => "Escalated".into(),
        State::Dropped => "Dropped".into(),
        _ => "Shipped".into(),
    }
}

fn took(item: &Item, ended: DateTime<Utc>) -> Option<String> {
    let secs = (ended - item.created_at).num_seconds();
    if secs <= 0 {
        return None;
    }
    let to = match (item.state, item.delivered.as_deref()) {
        (State::Closed, Some("merged")) => "from request to merge",
        (State::Closed, _) => "from request to pull request",
        (State::Escalated, _) => "until it reached you",
        _ => "until it was dropped",
    };
    Some(format!("{} {to}", duration(secs)))
}

/// "36 minutes", "1 h 58 m", "2 d 14 h".
pub fn duration(secs: i64) -> String {
    match secs.max(0) {
        s if s < 90 => "a minute".into(),
        s if s < 3600 => format!("{} minutes", s / 60),
        s if s < 86_400 => format!("{} h {:02} m", s / 3600, (s % 3600) / 60),
        s => format!("{} d {} h", s / 86_400, (s % 86_400) / 3600),
    }
}

fn byline(events: &[Event]) -> String {
    let did = |role: &str| {
        events
            .iter()
            .any(|e| matches!(&e.body, Body::Usage(u) if u.role == role))
    };
    let any = |f: fn(&Body) -> bool| events.iter().any(|e| f(&e.body));
    let mut parts = Vec::new();
    if did("dev") || any(|b| matches!(b, Body::Commit { .. })) {
        parts.push("Built by the dev agent.");
    }
    if any(|b| matches!(b, Body::Verdict { .. })) {
        parts.push("Reviewed by the principal engineer.");
    }
    if any(|b| matches!(b, Body::FixSubmitted { .. })) {
        parts.push("Fixed by the fixer.");
    }
    if events.iter().any(|e| {
        e.actor == "you"
            && matches!(
                e.body,
                Body::Retried { .. }
                    | Body::PulledBack { .. }
                    | Body::Dropped { .. }
                    | Body::Answer { .. }
            )
    }) {
        parts.push("Steered by you.");
    }
    parts.push("Written up by the librarian.");
    parts.join(" ")
}

/// The dev agent's why and how from its last proposal.
fn proposal(events: &[Event]) -> (Option<String>, Option<String>) {
    events
        .iter()
        .rev()
        .find_map(|e| match &e.body {
            Body::ProposedReady { why, how, .. } => Some((why.clone(), how.clone())),
            _ => None,
        })
        .unwrap_or_default()
}

/// `https://…` links in `text`, trailing punctuation removed.
pub fn urls(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for w in text.split_whitespace() {
        let Some(at) = w.find("https://").or_else(|| w.find("http://")) else {
            continue;
        };
        let u = w[at..].trim_end_matches(['.', ',', ';', ':', ')', ']', '\'', '"', '`']);
        if u.len() > 10 && !out.iter().any(|x| x == u) {
            out.push(u.to_string());
        }
    }
    out
}

fn verdict_line(v: VerdictKind, escalated: bool, c: &Counts) -> String {
    let open = describe(c);
    match (v, escalated) {
        (VerdictKind::Approve, _) if c.total() == 0 => "Approved.".into(),
        (VerdictKind::Approve, _) => format!("Approved, with {open} left open."),
        (_, true) => format!("Escalated to you: {open}."),
        _ => format!("Sent back: {open}."),
    }
}

/// "3 major, 6 minor".
fn describe(c: &Counts) -> String {
    let parts: Vec<String> = [
        (c.blocker, "blocker"),
        (c.major, "major"),
        (c.minor, "minor"),
        (c.nit, "nit"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, w)| format!("{n} {w}"))
    .collect();
    if parts.is_empty() {
        "nothing open".into()
    } else {
        parts.join(", ")
    }
}

/// A gate's short name: what runs inside a `--command` wrapper.
fn gate_name(command: &str) -> String {
    let c = command
        .rsplit_once("--command ")
        .map(|(_, c)| c)
        .unwrap_or(command)
        .trim();
    clip(c, 40)
}

fn desk(lens: &str) -> (usize, &'static str) {
    match lens {
        "security_tenancy" => (0, "Safety"),
        "correctness" => (1, "Correctness"),
        "data_migrations" => (2, "Data"),
        "evidence" => (3, "Tests and evidence"),
        "operability" => (4, "Running it"),
        "first_principles" => (5, "Design"),
        "repo_rules" => (6, "House rules"),
        _ => (7, "Other"),
    }
}

fn kind_words(kind: &str) -> &'static str {
    match kind {
        "rule" => "A rule",
        "false-positive" => "What not to flag",
        "map" => "A map of the code",
        "decision" => "Your decision",
        _ => "A lesson",
    }
}

fn status_words(status: &str) -> &'static str {
    match status {
        "accepted" => "kept",
        "proposed" => "waiting for you",
        "retired" => "retired since",
        _ => "kept",
    }
}

fn role_name(role: &str) -> (usize, &'static str) {
    match role {
        "intake" => (0, "Front desk"),
        "dev" => (1, "Dev agent"),
        "reviewer" => (2, "Principal engineer"),
        "fixer" => (3, "Fixer"),
        "delivery" => (4, "Delivery"),
        "librarian" => (5, "Librarian"),
        _ => (6, "Other runs"),
    }
}

fn model_name(model: &str) -> String {
    let m = model.to_ascii_lowercase();
    for (k, name) in [
        ("fable", "Fable"),
        ("opus", "Opus"),
        ("sonnet", "Sonnet"),
        ("haiku", "Haiku"),
    ] {
        if m.contains(k) {
            return name.into();
        }
    }
    model.to_string()
}

fn did(role: &str, events: &[Event]) -> String {
    let count = |f: fn(&Body) -> bool| events.iter().filter(|e| f(&e.body)).count();
    match role {
        "dev" => {
            let n = count(|b| matches!(b, Body::Commit { .. }));
            if n > 0 {
                format!("Built it: {n} commit{}", plural(n))
            } else {
                "Built it".into()
            }
        }
        "reviewer" => {
            let n = count(|b| matches!(b, Body::Verdict { .. }));
            format!("{n} review{}", plural(n))
        }
        "fixer" => {
            let n = count(|b| matches!(b, Body::FixSubmitted { .. }));
            let mut fixed: Vec<&String> = events
                .iter()
                .flat_map(|e| match &e.body {
                    Body::FixSubmitted { fixes, .. } => fixes.iter().collect(),
                    _ => Vec::new(),
                })
                .collect();
            fixed.sort();
            fixed.dedup();
            format!(
                "{n} round{} of fixes, {} finding{}",
                plural(n),
                fixed.len(),
                plural(fixed.len())
            )
        }
        "librarian" => "Lessons, and this page".into(),
        "intake" => "Planned the work".into(),
        "delivery" => "Kept the pull request current".into(),
        other => other.to_string(),
    }
}

/// The day in brief: what happened, in order, one line each.
fn brief(item: &Item, events: &[Event]) -> Vec<(String, String)> {
    let first = events.first().map(|e| e.t).unwrap_or(item.created_at);
    let last = events.last().map(|e| e.t).unwrap_or(first);
    let days = first.date_naive() != last.date_naive();
    let when = |t: DateTime<Utc>| {
        if days {
            t.format("%-d %b %H:%M").to_string()
        } else {
            t.format("%H:%M").to_string()
        }
    };
    let mut out: Vec<(String, String)> = Vec::new();
    let mut reviewed = false;
    let mut shipped: Option<String> = None;
    let mut rebased: Option<&str> = None;
    for e in events {
        // A rebase is one line, however many events record it: the pull
        // back it causes and the review of the resolution are part of it.
        match &e.body {
            Body::Rebased { to, .. } if rebased == Some(to.as_str()) => continue,
            Body::Rebased { to, .. } => rebased = Some(to),
            Body::PulledBack { .. } if rebased.is_some() && e.actor != "you" => continue,
            Body::Ready { .. } | Body::Verdict { .. } | Body::Checks { .. } => {}
            _ if e.body.action().is_some() => rebased = None,
            _ => {}
        }
        let text = match &e.body {
            Body::Created { adopted: true, .. } => Some("Adopted from an existing branch.".into()),
            Body::Created { .. } => Some("The request came in.".into()),
            Body::Ready { round, note, .. } => {
                if note.as_deref().is_some_and(|n| n.contains("rebase")) {
                    None
                } else if !reviewed {
                    reviewed = true;
                    Some(format!(
                        "Sent to review after {} in the workshop.",
                        duration((e.t - item.created_at).num_seconds())
                    ))
                } else {
                    Some(format!("Sent to review again (round {round})."))
                }
            }
            Body::Checks {
                passed: false,
                round,
                ..
            } => Some(format!("Checks failed in round {round}.")),
            Body::Verdict {
                round,
                verdict,
                escalated,
                findings,
                counts,
                ..
            } => Some(match (verdict, escalated) {
                (VerdictKind::Approve, _) => format!("Approved in round {round}."),
                (_, true) => format!("Escalated to you after round {round}."),
                _ if !findings.is_empty() => format!(
                    "The reviewer sent it back with {} finding{}.",
                    findings.len(),
                    plural(findings.len())
                ),
                _ => format!("The reviewer sent it back: {}.", describe(counts)),
            }),
            Body::FixSubmitted {
                fixes, disputes, ..
            } => Some(match (fixes.len(), disputes.len()) {
                (f, 0) => format!("The fixer fixed {f}."),
                (0, d) => format!("The fixer disputed {d}."),
                (f, d) => format!("The fixer fixed {f} and disputed {d}."),
            }),
            Body::Rebased { onto, carried, .. } => Some(if *carried {
                format!("Rebased onto {onto}; the approval carried over.")
            } else {
                format!("Rebased onto {onto}, and reviewed again.")
            }),
            Body::Shipped { pr_url, .. } => match (pr_url, &shipped) {
                (Some(u), Some(s)) if u == s => Some("The pull request was updated.".into()),
                (Some(u), _) => {
                    shipped = Some(u.clone());
                    Some(format!(
                        "Pull request #{} opened.",
                        u.rsplit('/').next().unwrap_or(u)
                    ))
                }
                (None, _) => Some("Shipped.".into()),
            },
            Body::Reopened { .. } => Some("Reopened through its pull request.".into()),
            Body::PulledBack { reason } => Some(match reason {
                Some(r) => format!("Pulled back: {}.", clip(r, 90).trim_end_matches('.')),
                None => "Pulled back to development.".into(),
            }),
            Body::Retried { reason } => Some(format!(
                "You allowed another round: {}.",
                clip(reason, 90).trim_end_matches('.')
            )),
            Body::Dropped { reason, .. } => Some(format!(
                "Dropped: {}.",
                clip(reason, 90).trim_end_matches('.')
            )),
            Body::Question { text, .. } => Some(format!("A question for you: {}", clip(text, 90))),
            Body::PrChecks { passed: true, .. } => Some("CI passed on the pull request.".into()),
            Body::PrChecks { passed: false, .. } => Some("CI failed on the pull request.".into()),
            Body::Delivered { outcome, .. } if outcome == "merged" => {
                Some("The pull request merged.".into())
            }
            Body::Delivered { .. } => Some("The pull request was closed without merging.".into()),
            _ => None,
        };
        if let Some(t) = text {
            out.push((when(e.t), t));
        }
    }
    // A long life keeps its start and its end.
    if out.len() > 18 {
        let tail = out.split_off(out.len() - 9);
        out.truncate(8);
        out.push((String::new(), "…".into()));
        out.extend(tail);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_like_a_person_wrote_them() {
        assert_eq!(duration(40), "a minute");
        assert_eq!(duration(36 * 60), "36 minutes");
        assert_eq!(duration(7080), "1 h 58 m");
        assert_eq!(duration(2 * 86_400 + 14 * 3600 + 5), "2 d 14 h");
    }

    #[test]
    fn urls_come_out_of_the_request() {
        assert_eq!(
            urls("Designed in https://claude.ai/artifact/Fw. Ships (see https://x.dev/a)."),
            vec!["https://claude.ai/artifact/Fw", "https://x.dev/a"]
        );
    }

    #[test]
    fn gate_names_drop_the_wrapper() {
        assert_eq!(
            gate_name("nix develop --command cargo test --all-targets"),
            "cargo test --all-targets"
        );
        assert_eq!(gate_name("npm test"), "npm test");
    }

    #[test]
    fn verdict_lines_say_what_is_open() {
        let c = Counts {
            major: 3,
            minor: 6,
            ..Counts::default()
        };
        assert_eq!(
            verdict_line(VerdictKind::Changes, false, &c),
            "Sent back: 3 major, 6 minor."
        );
        assert_eq!(
            verdict_line(VerdictKind::Approve, false, &Counts::default()),
            "Approved."
        );
    }
}
