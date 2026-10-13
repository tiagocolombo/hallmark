//! What an item is doing now, in any phase, and what you can do about it.
//! `hall status <id>`, `hall watch`, `hall inbox <entry>`, the hooks, the
//! HUD and the terminal app all read it. It is derived, never stored: from
//! the item, its lease, the review queue, the daemon's Live log, the
//! check log being written and the item's run streams (ADR 0039).

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;

use crate::config::Role;
use crate::event::{Body, Event};
use crate::findings::{Ledger, Severity};
use crate::item::{HoldKind, Item, ItemId, State};
use crate::repo::Repo;
use crate::trace::{self, RunFile, RunInfo, Step, StepKind, Tail};

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Now {
    /// working (development), queued, running, waiting_for_you, held,
    /// stuck, done.
    pub status: String,
    /// One line: what is happening.
    pub doing: String,
    /// Since when, and how long that is ("4m").
    pub since: DateTime<Utc>,
    pub elapsed: String,
    /// The evidence: findings, failing checks, the summary, what is ahead.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<String>,
    /// The tail of what is running (a gate's output).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub log: Vec<String>,
    /// What you can do, and what each does.
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Action {
    pub command: String,
    pub does: String,
}

fn act(command: impl Into<String>, does: impl Into<String>) -> Action {
    Action {
        command: command.into(),
        does: does.into(),
    }
}

impl Now {
    /// True while the loop is (or will be) working on it by itself.
    pub fn in_motion(&self) -> bool {
        matches!(self.status.as_str(), "queued" | "running")
    }

    /// For terminals: the line, the detail, the log tail, the actions.
    pub fn render(&self) -> String {
        let mut s = format!("  now       {} ({})\n", self.doing, self.elapsed);
        for d in &self.detail {
            s.push_str(&format!("            {d}\n"));
        }
        for l in &self.log {
            s.push_str(&format!("            │ {l}\n"));
        }
        for a in &self.actions {
            s.push_str(&format!("  → {:40} {}\n", a.command, a.does));
        }
        s
    }
}

/// Lines of the Live log about `id`, newer than `after`, oldest first.
fn live(repo: &Repo, id: &str, after: DateTime<Utc>) -> Vec<(DateTime<Utc>, String)> {
    let text = tail_text(&repo.hall.live_log(), 128 * 1024);
    let needle = format!("{id} ");
    // Live-log times are whole seconds; the lease's start isn't.
    let after = after - chrono::Duration::seconds(1);
    text.lines()
        .filter_map(|l| {
            let (t, rest) = l.split_once(' ')?;
            let t = DateTime::parse_from_rfc3339(t).ok()?.with_timezone(&Utc);
            (t >= after && rest.contains(&needle)).then(|| (t, rest.to_string()))
        })
        .collect()
}

/// The last `max` bytes of a file, from a line start.
fn tail_text(path: &std::path::Path, max: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max);
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(start)).is_err() || f.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    if start > 0 {
        text.split_once('\n')
            .map(|(_, r)| r.to_string())
            .unwrap_or_default()
    } else {
        text
    }
}

fn tail_lines(path: &std::path::Path, n: usize) -> Vec<String> {
    let text = tail_text(path, 16 * 1024);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| crate::output::clip(l, 160))
        .collect()
}

/// "2nd", "3rd".
fn ordinal(n: usize) -> String {
    let suffix = match (n % 10, n % 100) {
        (1, 11) | (2, 12) | (3, 13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// What `item` is doing now. `items` are the repo's live items (for the
/// queue), `events` the item's journal.
pub fn now(repo: &Repo, item: &Item, items: &[Item], events: &[Event]) -> Now {
    now_with(repo, item, items, events, true)
}

/// Like `now`, without the git-derived detail (last commit, dirty files):
/// for callers that only show the line and must stay cheap, like the HUD.
pub fn now_cheap(repo: &Repo, item: &Item, items: &[Item], events: &[Event]) -> Now {
    now_with(repo, item, items, events, false)
}

fn now_with(repo: &Repo, item: &Item, items: &[Item], events: &[Event], git: bool) -> Now {
    let id = item.id.to_string();
    let mut n = Now {
        status: String::new(),
        doing: String::new(),
        since: item.state_since,
        elapsed: String::new(),
        detail: Vec::new(),
        log: Vec::new(),
        actions: Vec::new(),
    };
    let log_cmd = act(format!("hall log {id}"), "read its whole journal");

    if let Some(h) = &item.hold {
        n.status = "held".into();
        n.doing = format!("Held in {} ({}): {}", item.state, h.kind.as_str(), h.reason);
        n.since = h.since;
        n.actions = vec![
            act(
                format!("hall retry {id} --reason \"<why>\""),
                "release the hold; the loop picks it up again",
            ),
            act(
                format!("hall pull-back {id}"),
                "take it back to development",
            ),
        ];
        if let Some(any) = after_failed_runs(h.kind, events) {
            n.actions.extend(failed_run(repo, item, any));
        }
        n.actions.push(log_cmd);
        return finish(n);
    }
    if let Some(since) = item.stuck_since {
        n.status = "stuck".into();
        n.doing = format!(
            "No progress in {} since {}",
            item.state,
            item.updated_at.format("%H:%M")
        );
        n.since = since;
        n.actions = vec![
            act(
                "hall daemon status",
                "see whether the daemon runs, and what it is doing",
            ),
            log_cmd,
        ];
        return finish(n);
    }

    let lease = crate::lease::current(&repo.hall, item.id);
    match item.state {
        // The Delivery office rebases it, then sends it to review (ADR 0030).
        State::Development if item.rebase.is_some() => {
            let r = item.rebase.as_ref().expect("guarded");
            n.since = r.since;
            match &lease {
                Some(l) => {
                    n.status = "running".into();
                    n.doing = format!("The Delivery office is rebasing it onto {}", r.onto);
                    n.since = l.acquired;
                }
                None => {
                    n.status = "queued".into();
                    n.doing = format!(
                        "Waiting for the Delivery office to rebase it onto {}",
                        r.onto
                    );
                }
            }
            n.detail.push(r.reason.clone());
            if !r.files.is_empty() {
                n.detail
                    .push(format!("conflicts in {}", r.files.join(", ")));
            }
            n.actions = vec![
                act(
                    "hall daemon status",
                    "see whether the daemon runs, and what it is doing",
                ),
                act(
                    format!("hall ready {id}"),
                    "rebased it yourself: send it to review",
                ),
            ];
        }
        // The Delivery office addresses the PR's new comments (ADR 0030).
        State::Development if item.feedback.is_some() => {
            let f = item.feedback.as_ref().expect("guarded");
            n.since = f.since;
            match &lease {
                Some(l) => {
                    n.status = "running".into();
                    n.doing = "The Delivery office is addressing its PR's comments".into();
                    n.since = l.acquired;
                }
                None => {
                    n.status = "queued".into();
                    n.doing = "Waiting for the Delivery office to address its PR's comments".into();
                }
            }
            for c in &f.comments {
                n.detail.push(format!(
                    "{}: {}",
                    c.author,
                    crate::output::clip(c.body.lines().next().unwrap_or_default(), 80)
                ));
            }
            n.actions = vec![
                act(
                    "hall daemon status",
                    "see whether the daemon runs, and what it is doing",
                ),
                act(
                    format!("hall ready {id}"),
                    "addressed them yourself: send it to review",
                ),
            ];
        }
        // The Delivery office fixes its PR's failing CI (ADR 0031).
        State::Development if item.ci_fix.is_some() => {
            let c = item.ci_fix.as_ref().expect("guarded");
            n.since = c.since;
            match &lease {
                Some(l) => {
                    n.status = "running".into();
                    n.doing = "The Delivery office is fixing its PR's failing CI".into();
                    n.since = l.acquired;
                }
                None => {
                    n.status = "queued".into();
                    n.doing = "Waiting for the Delivery office to fix its PR's failing CI".into();
                }
            }
            n.detail.push(format!(
                "failing: {}",
                c.failing
                    .iter()
                    .map(|f| f.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            n.actions = vec![
                act(
                    "hall daemon status",
                    "see whether the daemon runs, and what it is doing",
                ),
                act(
                    format!("hall ready {id}"),
                    "fixed it yourself: send it to review",
                ),
            ];
        }
        State::Development => {
            n.status = "working".into();
            n.doing = format!("In development in {}", item.worktree);
            let wt = std::path::Path::new(&item.worktree);
            if git && let Ok(last) = crate::git::run(wt, &["log", "-1", "--format=%h %s"]) {
                n.detail.push(format!("last commit {last}"));
            }
            if git
                && let Ok(dirty) = crate::git::dirty_files(wt)
                && !dirty.is_empty()
            {
                n.detail
                    .push(format!("{} files with uncommitted changes", dirty.len()));
            }
            n.actions = if item.parent.is_some() {
                vec![act(
                    format!("hall merge {id}"),
                    "merge this part into its parent and close it",
                )]
            } else {
                vec![
                    act(
                        format!(
                            "hall propose-ready {id} --summary \"<what was done>\" --why \"<why>\" --how \"<how>\""
                        ),
                        "say it's done: straight to review when nothing is left to confirm",
                    ),
                    act(
                        format!("hall ready {id}"),
                        "freeze HEAD and send it to checks and review",
                    ),
                ]
            };
            n.actions.push(act(
                format!("hall drop {id} --reason \"<why>\""),
                "abandon it; the branch and journal stay",
            ));
        }
        State::AwaitingYou => {
            n.status = "waiting_for_you".into();
            n.doing = "The agent says it looks done; it waits for your call".into();
            if let Some(s) = &item.summary {
                n.detail.push(format!("summary: {s}"));
            }
            n.actions = vec![
                if item.parent.is_some() {
                    // Parts merge into their parent; they never go to review.
                    act(
                        format!("hall merge {id}"),
                        "merge this part into its parent and close it",
                    )
                } else {
                    act(format!("hall ready {id}"), "send it to checks and review")
                },
                act(
                    format!("hall pull-back {id}"),
                    "not yet: back to development",
                ),
                act(format!("hall drop {id} --reason \"<why>\""), "abandon it"),
            ];
        }
        State::Checks | State::Review | State::Fixing | State::Hallmarked => {
            let role = match item.state {
                State::Fixing => "fixer",
                State::Hallmarked => "shipper",
                _ => "assayer",
            };
            match &lease {
                Some(l) if l.role == role => running(repo, item, &mut n, l.acquired, &id, git),
                _ => queued(repo, item, items, role, &mut n),
            }
            if item.state == State::Fixing {
                let ledger = Ledger::from_events(events);
                for t in ledger.open() {
                    if t.finding.severity.blocks() {
                        n.detail.push(format!(
                            "{} {} {}:{}: {}",
                            t.finding.id,
                            t.finding.severity.as_str(),
                            t.finding.file,
                            t.finding.line,
                            t.finding.title
                        ));
                    }
                }
            }
            n.actions
                .push(act(format!("hall watch {id}"), "follow it live"));
            n.actions.push(act(
                format!("hall pull-back {id}"),
                "stop the loop and take it back to development",
            ));
        }
        State::Escalated => {
            n.status = "waiting_for_you".into();
            n.doing = format!(
                "Rejected {} times; it waits for your decision",
                item.rejects
            );
            let ledger = Ledger::from_events(events);
            for t in ledger.open() {
                if matches!(t.finding.severity, Severity::Blocker | Severity::Major) {
                    let dispute = t
                        .dispute
                        .as_deref()
                        .map(|d| format!(" (disputed: {d})"))
                        .unwrap_or_default();
                    n.detail.push(format!(
                        "{} {} {}:{}: {}{dispute}",
                        t.finding.id,
                        t.finding.severity.as_str(),
                        t.finding.file,
                        t.finding.line,
                        t.finding.title
                    ));
                }
            }
            n.actions = vec![
                act(
                    format!("hall findings {id}"),
                    "read the open findings in full",
                ),
                act(
                    format!("hall retry {id} --reason \"<why>\""),
                    "allow one more fix round",
                ),
                act(
                    format!("hall pull-back {id}"),
                    "take it back to development and fix it yourself",
                ),
                act(format!("hall drop {id} --reason \"<why>\""), "abandon it"),
            ];
        }
        State::Closed => {
            n.status = "done".into();
            n.doing = match (&item.pr_url, &item.delivered) {
                (Some(u), Some(how)) => format!("Delivered: {u} {how}"),
                (Some(u), None) if u.contains("/pull/") && item.parent.is_none() => {
                    format!("Shipped: {u}; the Delivery office watches it until it merges")
                }
                (Some(u), None) => format!("Shipped: {u}"),
                (None, _) => "Shipped".into(),
            };
            n.actions = vec![act(
                format!("hall export {id}"),
                "the whole record as Markdown",
            )];
            // Its PR can still need a rebase or a conflict fix (ADR 0029).
            if let Some(u) = &item.pr_url
                && item.parent.is_none()
                && item.delivered.is_none()
            {
                n.actions.push(act(
                    format!("hall adopt --pr {u}"),
                    "reopen it while its PR is open: rebase, resolve conflicts or change it",
                ));
            }
        }
        State::Dropped => {
            n.status = "done".into();
            n.doing = format!("Dropped: {}", item.drop_reason.clone().unwrap_or_default());
            n.actions = vec![log_cmd.clone()];
        }
    }
    if n.status == "running"
        && let Some(l) = &lease
    {
        following(repo, item, l.acquired, &mut n);
    }
    if !matches!(item.state, State::Closed | State::Dropped) {
        n.actions.push(log_cmd);
    }
    finish(n)
}

/// How much of a run's stream the now block reads from each end. A stream
/// holds the whole conversation; the now block needs only how it began
/// (the session, where it ran) and where it stands.
const WINDOW: u64 = 64 * 1024;

/// The item's latest run, when it started at or after `since`.
fn latest_run(
    repo: &Repo,
    id: ItemId,
    since: DateTime<Utc>,
) -> Option<(RunFile, Vec<Step>, RunInfo)> {
    // Run files are stamped to the second.
    let since = since - chrono::Duration::seconds(1);
    let run = trace::runs(repo, id)
        .ok()?
        .pop()
        .filter(|r| r.started >= since)?;
    let (steps, info) = glance(&run.path)?;
    Some((run, steps, info))
}

/// A run's info and its last steps, without reading all of a long stream:
/// whole lines from its first and last `WINDOW` bytes.
fn glance(path: &std::path::Path) -> Option<(Vec<Step>, RunInfo)> {
    use std::io::Read;
    let len = std::fs::metadata(path).ok()?.len();
    let streamed = path.extension().and_then(|e| e.to_str()) == Some("jsonl");
    if !streamed || len <= 2 * WINDOW {
        return trace::read(path).ok();
    }
    let mut head = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(WINDOW)
        .read_to_end(&mut head)
        .ok()?;
    let upto = head.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let mut info = RunInfo::default();
    for line in String::from_utf8_lossy(&head[..upto]).lines() {
        if let Ok(v) = serde_json::from_str(line.trim()) {
            info.observe(&v);
        }
    }
    // From the middle of a line: the partial first line doesn't parse and
    // is skipped.
    let mut tail = Tail {
        offset: len - WINDOW,
        info,
    };
    let steps = tail.poll(path).ok()?;
    Some((steps, tail.info))
}

/// How wide a step is in the log.
pub const STEP_CLIP: usize = 160;

/// What the agent is doing: its last call or words, and the result that
/// came back, if one did. At most two lines.
fn last_steps(steps: &[Step]) -> Vec<String> {
    let Some(i) = steps
        .iter()
        .rposition(|s| matches!(s.kind, StepKind::Start | StepKind::Text | StepKind::Tool))
    else {
        return Vec::new();
    };
    steps[i..]
        .iter()
        .take(2)
        .map(|s| crate::output::clip(&s.render(), STEP_CLIP))
        .collect()
}

/// A model run is going: its last step, and how to follow it. The lease
/// says the loop holds the item now; a stream that began under the lease
/// and has no end line says the run hasn't exited. A stream alone isn't
/// enough: a daemon that died mid-run never writes the end line.
fn following(repo: &Repo, item: &Item, acquired: DateTime<Utc>, n: &mut Now) {
    let Some((run, steps, info)) = latest_run(repo, item.id, acquired) else {
        return;
    };
    if info.ended {
        return;
    }
    n.log.extend(last_steps(&steps));
    n.actions.insert(
        0,
        act(
            format!("hall trace {} --follow", item.id),
            format!("watch the {} work, live", run.role),
        ),
    );
}

/// Whether the hold came from failed model runs: a `run_failed` hold
/// (`Some(true)`), or a Delivery-office hold recorded right after its
/// second failed run (`Some(false)`: only when the run itself failed).
fn after_failed_runs(kind: HoldKind, events: &[Event]) -> Option<bool> {
    if kind == HoldKind::RunFailed {
        return Some(true);
    }
    let held = events
        .iter()
        .rposition(|e| matches!(e.body, Body::Held { .. }))?;
    let before = events.get(held.checked_sub(1)?)?;
    matches!(before.body, Body::RunFailed { .. }).then_some(false)
}

/// For a hold after failed runs: open the last one in Claude Code, and
/// read it. Only once it has ended, so nobody resumes a session the
/// daemon is still writing. `any`: the run counts even if its process
/// exited cleanly (its output was what failed).
fn failed_run(repo: &Repo, item: &Item, any: bool) -> Vec<Action> {
    let Some((run, _, info)) = latest_run(repo, item.id, item.state_since) else {
        return Vec::new();
    };
    if !info.ended || !(any || info.failure.is_some()) {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Some(cmd) = info.resume_command() {
        out.push(act(cmd, "open the failed run in Claude Code"));
    }
    out.push(act(
        format!("hall trace {}", item.id),
        format!("read what the {} run did, step by step", run.role),
    ));
    out
}

fn finish(mut n: Now) -> Now {
    n.elapsed = crate::commands::age(n.since);
    n
}

/// The loop is working on it: which gate, which run, since when.
fn running(repo: &Repo, item: &Item, n: &mut Now, acquired: DateTime<Utc>, id: &str, git: bool) {
    n.status = "running".into();
    n.since = acquired;
    let lines = live(repo, id, acquired);
    let b = &repo.config.budget;
    match item.state {
        State::Checks => {
            let gate = lines
                .iter()
                .rev()
                .find(|(_, l)| l.contains(" checks ") && l.contains('/'));
            match gate {
                Some((t, l)) => {
                    let what = l.split_once(" checks ").map(|(_, r)| r).unwrap_or(l);
                    n.doing = format!("Checks, round {}: running gate {what}", item.round);
                    n.since = *t;
                }
                None => {
                    n.doing = format!("Checks, round {}: preparing the assay worktree", item.round)
                }
            }
            n.log = tail_lines(
                &repo
                    .hall
                    .item_checks(item.id)
                    .join(format!("r{}.log", item.round)),
                12,
            );
        }
        State::Review => {
            let started = lines
                .iter()
                .rev()
                .find(|(_, l)| l.contains("reviewer run started"));
            let effort = lines
                .iter()
                .rev()
                .find_map(|(_, l)| l.split_once("effort ").map(|(_, e)| e.trim().to_string()));
            let model = b.model(Role::Reviewer);
            n.doing = match started {
                Some((t, _)) => {
                    n.since = *t;
                    format!(
                        "Review, round {}: {model}{} is reading the change",
                        item.round,
                        effort
                            .map(|e| format!(" at effort {e}"))
                            .unwrap_or_default()
                    )
                }
                None => format!("Review, round {}: preparing the prompt", item.round),
            };
            n.detail.push(format!(
                "time limit {}{}",
                crate::commands::age_secs(b.timeout_secs(Role::Reviewer)),
                b.max_usd(Role::Reviewer)
                    .map(|u| format!(", budget ${u:.2} per run"))
                    .unwrap_or_default()
            ));
        }
        State::Fixing => {
            let model = b.model(Role::Fixer);
            n.doing = format!(
                "Fixing, round {}: {model} is working in {}",
                item.round, item.worktree
            );
            if git
                && let Ok(last) = crate::git::run(
                    std::path::Path::new(&item.worktree),
                    &["log", "-1", "--format=%h %s"],
                )
            {
                n.detail.push(format!("last commit {last}"));
            }
        }
        _ => {
            n.doing = "Shipping: pushing the branch and opening the PR".into();
        }
    }
    if let Some((_, last)) = lines.last() {
        n.detail
            .push(format!("latest: {}", crate::output::clip(last, 120)));
    }
}

/// Waiting for a worker: its place in line and what is ahead.
fn queued(repo: &Repo, item: &Item, items: &[Item], role: &str, n: &mut Now) {
    n.status = "queued".into();
    let daemon = crate::daemon::state::instance_running();
    let paused = crate::daemon::state::load().and_then(|s| s.paused);
    let line: Vec<&Item> = if role == "assayer" {
        let tickets = repo.hall.tickets().unwrap_or_default();
        let mut l: Vec<&Item> = tickets
            .iter()
            .filter_map(|t| items.iter().find(|i| i.id == t.id))
            .filter(|i| {
                matches!(i.state, State::Checks | State::Review)
                    && i.parent.is_none()
                    // Held items are skipped, as `lease::candidates` does.
                    && (i.hold.is_none() || i.id == item.id)
            })
            .collect();
        if !l.iter().any(|i| i.id == item.id) {
            l.push(item);
        }
        l
    } else {
        vec![item]
    };
    let pos = line.iter().position(|i| i.id == item.id).unwrap_or(0);
    let what = match item.state {
        State::Checks => "the checks",
        State::Review => "the review",
        State::Fixing => "the fixer",
        _ => "the shipper",
    };
    n.doing = if !daemon {
        format!("Waiting for {what}, but the daemon isn't running")
    } else if let Some(p) = &paused {
        format!("Waiting for {what}; the daemon is paused: {}", p.reason)
    } else if pos == 0 {
        format!("Next for {what}; the daemon picks it up on its next pass")
    } else {
        format!("Waiting for {what}: {} in line", ordinal(pos + 1))
    };
    for ahead in &line[..pos] {
        let busy = crate::lease::current(&repo.hall, ahead.id)
            .map(|l| format!(", running for {}", crate::commands::age(l.acquired)))
            .unwrap_or_default();
        n.detail.push(format!(
            "ahead: {} ({}{busy}) {}",
            ahead.id,
            ahead.state,
            crate::output::clip(&ahead.title, 50)
        ));
    }
    if let Some(t) = repo.hall.ticket(item.id) {
        n.since = t.queued_at;
    }
    if !daemon {
        n.actions
            .push(act("hall daemon start", "start the review loop"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream() -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/claude/stream.jsonl",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn a_long_stream_is_read_from_both_ends() {
        // The fixture's run began here; a resume command cds only into a
        // directory that exists.
        std::fs::create_dir_all("/tmp/spike").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("20261012T215146-reviewer.jsonl");
        // The run's init, then a long conversation: more than both windows.
        let text = stream();
        let (init, rest) = text.split_once('\n').unwrap();
        let mut long = format!("{init}\n");
        while (long.len() as u64) < 3 * WINDOW {
            long.push_str(rest);
        }
        std::fs::write(&p, &long).unwrap();
        let (steps, info) = glance(&p).unwrap();
        let (all, whole) = trace::read(&p).unwrap();
        assert!(steps.len() < all.len(), "only the tail's steps");
        assert_eq!(steps.last(), all.last());
        assert_eq!(info, whole, "the head knows where it ran");
        assert!(!info.ended);

        let end = trace::RunEnd::new(4_000, None, Some("timed out".into())).line();
        std::fs::write(&p, format!("{long}{end}")).unwrap();
        let (steps, info) = glance(&p).unwrap();
        assert!(info.ended);
        assert_eq!(info.failure.as_deref(), Some("timed out"));
        assert_eq!(steps.last().map(|s| s.kind), Some(StepKind::End));
        assert_eq!(
            info.resume_command().as_deref(),
            Some("cd /tmp/spike && claude --resume 91aedf44-f4cd-4efa-8e74-768d46a9dc75")
        );
    }

    #[test]
    fn the_last_steps_are_a_call_and_its_result() {
        let steps: Vec<Step> = stream().lines().flat_map(trace::steps).collect();
        // Up to the Bash call's result: the call and what came back.
        let lines = last_steps(&steps[..5]);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].ends_with("▸ Bash Count bytes in note.txt"),
            "{lines:?}"
        );
        assert_eq!(lines[1], "↳ 6 note.txt");
        // Words and the result: the words, and how it ended.
        let lines = last_steps(&steps);
        assert!(lines[0].contains("» `note.txt` contains"), "{lines:?}");
        assert!(lines[1].starts_with("■ done"), "{lines:?}");
        assert!(lines.iter().all(|l| l.chars().count() <= 160));
        assert!(last_steps(&[]).is_empty());
    }

    #[test]
    fn ordinals() {
        assert_eq!(super::ordinal(1), "1st");
        assert_eq!(super::ordinal(2), "2nd");
        assert_eq!(super::ordinal(3), "3rd");
        assert_eq!(super::ordinal(4), "4th");
        assert_eq!(super::ordinal(11), "11th");
        assert_eq!(super::ordinal(22), "22nd");
    }
}
