//! The Repair Shop (workstream C, ADR 0019): runs the fixer in an item's
//! worktree.
//!
//! One tick claims one item in `fixing`, hands the fixer its failing checks
//! and open findings, and runs it: the `fixer` plugin agent through
//! `claude -p`, or an external engine from `.hallmark.toml`. **A script
//! decides when it's done**, never the agent's prose:
//!
//! - the item already left `fixing` (the agent ran `hall fixed`): done;
//! - else the fix report (or `{findings_file}.out`) is submitted through the
//!   same code as `hall fixed`, when HEAD moved or it only disputes;
//! - uncommitted changes, or nothing to submit, count as a failed run: the
//!   next tick retries once, the second failure holds the item for you.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::ItemView;
use crate::config::Role;
use crate::error::Result;
use crate::event::{Body, CheckRun, Event, short};
use crate::findings::{Counts, Dispute, Ledger, Tracked};
use crate::git;
use crate::item::{HoldKind, Item, ItemId, State};
use crate::lease;
use crate::repo::Repo;
use crate::runner::{self, Failure, Metered};
use crate::worker::{Progress, WorkerCtx};

/// The actor recorded for the fixer's events.
pub const ACTOR: &str = "fixer";

/// What the fixer returns (`plugin/schemas/fix.json`). An external engine
/// may write the same shape to `{findings_file}.out`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FixReport {
    #[serde(default)]
    pub summary: String,
    /// Finding IDs the committed HEAD fixes.
    #[serde(default)]
    pub fixed: Vec<String>,
    /// Findings disputed instead, each with a concrete reason.
    #[serde(default)]
    pub disputed: Vec<Dispute>,
}

/// The brief an external fix engine gets in `{findings_file}`: the same
/// fields as `hall findings`, plus the round and the request.
#[derive(Debug, Serialize, JsonSchema)]
pub struct FixBrief {
    pub item: ItemView,
    /// The review round being fixed; the submission starts the next one.
    pub round: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
    /// Open findings, most severe first.
    pub findings: Vec<Tracked>,
    pub open: Counts,
    /// The gates that failed at the reviewed commit, with log paths.
    pub failing_checks: Vec<CheckRun>,
    /// Where to write the optional fix report (`plugin/schemas/fix.json`).
    pub report_file: String,
    /// What the person said since the last run, given once (ADR 0044).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub said: Option<String>,
}

pub fn tick(ctx: &WorkerCtx) -> Result<Progress> {
    let repo = ctx.repo;
    let ttl = chrono::Duration::seconds(repo.config.budget.timeout_secs(Role::Fixer).max(60) + 600);
    let Some((item, _)) = lease::claim_next(repo, "fixer", &ctx.holder, ttl, None)? else {
        return Ok(Progress::Idle);
    };
    let r = work(ctx, &item);
    let _ = lease::release(&repo.hall, item.id, &ctx.holder);
    r
}

fn work(ctx: &WorkerCtx, item: &Item) -> Result<Progress> {
    let repo = ctx.repo;
    let id = item.id;
    let wt = PathBuf::from(&item.worktree);
    if !wt.is_dir() {
        hold(
            repo,
            id,
            HoldKind::Other,
            format!(
                "the worktree {} is missing; the fixer can't work",
                item.worktree
            ),
        )?;
        return Ok(Progress::worked(format!("{id} held: worktree missing")));
    }
    // Left-over changes from an earlier run (or a person) are not ours to
    // commit: count it as another failed attempt without spending a run.
    let dirty = git::dirty_files(&wt)?;
    if !dirty.is_empty() {
        return failed(
            repo,
            id,
            format!(
                "the worktree still has uncommitted changes ({})",
                dirty.join(", ")
            ),
        );
    }
    let events = repo.hall.read_item(id)?;
    let ledger = Ledger::from_events(&events);
    repo.hall
        .live(&format!("{id} fixer started (round {})", item.round));
    if is_claude(&repo.config.fixer) {
        run_claude(ctx, item, &events, &ledger, &wt)
    } else {
        run_external(repo, item, &events, &ledger, &wt)
    }
}

/// `claude -p` (or nothing) means the built-in fixer agent.
pub fn is_claude(cmd: &str) -> bool {
    let c = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
    c.is_empty() || c == "claude" || c == "claude -p" || c == "claude --print"
}

fn run_claude(
    ctx: &WorkerCtx,
    item: &Item,
    events: &[Event],
    ledger: &Ledger,
    wt: &Path,
) -> Result<Progress> {
    let repo = ctx.repo;
    let id = item.id;
    let mut req = runner::request(
        repo,
        Role::Fixer,
        Some("fixer"),
        prompt(repo, item, events, ledger),
        wt,
    )?;
    req.json_schema = Some(crate::plugin::file("schemas/fix.json").to_string());
    req.permission_mode = Some("acceptEdits".into());
    req.allowed_tools = tools(&repo.config.run_commands());
    req.env.extend(path_with_hall());
    if !failing_checks(events).is_empty() {
        let checks_dir = repo.hall.item_checks(id);
        if checks_dir.is_dir() {
            runner::allow_read_dir(&mut req, checks_dir);
        }
    }
    match runner::run_metered(repo, ctx.runner, Some(id), &req)? {
        Metered::Blocked(stop) => Ok(stop.progress()),
        Metered::Failed {
            failure: Failure::RateLimited { until },
            ..
        } => Ok(Progress::Pause {
            reason: "Claude's plan limit".into(),
            until,
        }),
        Metered::Failed {
            failure, held: h, ..
        } => {
            // The agent may have submitted before the run failed (say, it
            // never filled the schema). The item moving is what counts.
            let now = repo.hall.load_item(id)?;
            if now.state != State::Fixing {
                said_to_fixer(repo, id, events)?;
                return Ok(Progress::worked(format!(
                    "{id} resubmitted by the fixer at {}",
                    short(now.frozen_sha.as_deref().unwrap_or("?"))
                )));
            }
            Ok(Progress::worked(format!(
                "{id} fixer run failed{}: {}",
                if h { " (held)" } else { "" },
                failure.describe()
            )))
        }
        Metered::Done(outcome) => {
            said_to_fixer(repo, id, events)?;
            let report = outcome
                .structured()
                .and_then(|v| serde_json::from_value::<FixReport>(v.clone()).ok());
            settle(repo, id, wt, report)
        }
    }
}

/// The external engine: writes the brief, runs the command in the
/// worktree with a timeout, then settles like a `claude -p` run.
fn run_external(
    repo: &Repo,
    item: &Item,
    events: &[Event],
    ledger: &Ledger,
    wt: &Path,
) -> Result<Progress> {
    let id = item.id;
    if let Err(stop) = crate::budget::check(repo, Some(id), Role::Fixer.as_str()) {
        if stop.level == crate::budget::Level::Item {
            hold(repo, id, HoldKind::Budget, stop.reason.clone())?;
        }
        return Ok(stop.progress());
    }
    let dir = repo.hall.item_dir(id);
    let file = dir.join(format!("fix-r{}.json", item.round));
    let out_file = PathBuf::from(format!("{}.out", file.display()));
    let _ = std::fs::remove_file(&out_file);
    let brief = FixBrief {
        item: ItemView::from(item),
        round: item.round,
        request: request(events),
        findings: sorted_open(ledger).into_iter().cloned().collect(),
        open: ledger.open_counts(),
        failing_checks: failing_checks(events),
        report_file: crate::paths::display(&out_file),
        said: crate::words::pending(events, "fixer").map(|w| w.section),
    };
    std::fs::write(&file, serde_json::to_string_pretty(&brief)?)?;
    let cmd = crate::commands::new::fill_shell(
        &repo.config.fixer,
        &[
            ("id", &id.to_string()),
            ("worktree", &crate::paths::display(wt)),
            ("findings_file", &crate::paths::display(&file)),
        ],
    );
    let timeout = repo.config.budget.timeout_secs(Role::Fixer).max(0) as u64;
    repo.hall.live(&format!("{id} fixer engine: {cmd}"));
    let mut env: Vec<(String, String)> = vec![("HALL_ACTOR".into(), ACTOR.into())];
    env.extend(path_with_hall());
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let run = run_command(&cmd, wt, timeout, &env);
    let runs = repo.hall.item_runs(id);
    std::fs::create_dir_all(&runs)?;
    let stamp = Utc::now().format("%Y%m%dT%H%M%S");
    let _ = std::fs::write(
        runs.join(format!("{stamp}-fixer-engine.log")),
        format!("$ {cmd}\n{}\n{}", run.stdout, run.stderr),
    );
    if let Some(why) = run.failure() {
        // The engine may still have submitted before it failed.
        if repo.hall.load_item(id)?.state != State::Fixing {
            said_to_fixer(repo, id, events)?;
            return Ok(Progress::worked(format!(
                "{id} resubmitted by the fix engine"
            )));
        }
        return failed(repo, id, format!("the fix engine {why}"));
    }
    said_to_fixer(repo, id, events)?;
    let report = std::fs::read_to_string(&out_file)
        .ok()
        .and_then(|s| serde_json::from_str::<FixReport>(&s).ok());
    settle(repo, id, wt, report)
}

/// A fix run finished, so it read what the person said; later runs don't.
fn said_to_fixer(repo: &Repo, id: ItemId, events: &[Event]) -> Result<()> {
    crate::words::delivered(
        repo,
        id,
        "fixer",
        crate::words::pending(events, "fixer").as_ref(),
        events,
    )
}

/// After a run: the script decides whether the item is resubmitted.
pub fn settle(repo: &Repo, id: ItemId, wt: &Path, report: Option<FixReport>) -> Result<Progress> {
    let item = repo.hall.load_item(id)?;
    if item.state != State::Fixing {
        return Ok(Progress::worked(format!(
            "{id} resubmitted by the fixer at {}",
            short(item.frozen_sha.as_deref().unwrap_or("?"))
        )));
    }
    let dirty = git::dirty_files(wt)?;
    let report = report.unwrap_or_default();
    let summary = report.summary.trim().to_string();
    if !dirty.is_empty() {
        return failed(
            repo,
            id,
            with_summary(
                format!(
                    "the fixer left uncommitted changes ({}); they are kept for you",
                    dirty.join(", ")
                ),
                &summary,
            ),
        );
    }
    let head = git::head(wt)?;
    let moved = item.frozen_sha.as_deref() != Some(head.as_str());
    // Claims about findings that aren't open are dropped rather than
    // failing the whole submission; the review settles the rest.
    let events = repo.hall.read_item(id)?;
    let ledger = Ledger::from_events(&events);
    let open: Vec<String> = ledger.open().iter().map(|t| t.finding.id.clone()).collect();
    let fixed: Vec<String> = crate::commands::fix::split_ids(&report.fixed)
        .into_iter()
        .filter(|f| open.contains(f))
        .collect();
    let disputed: Vec<Dispute> = report
        .disputed
        .into_iter()
        .map(|d| Dispute {
            id: d.id.trim().to_ascii_uppercase(),
            reason: d.reason,
        })
        .filter(|d| open.contains(&d.id) && !fixed.contains(&d.id) && !d.reason.trim().is_empty())
        .collect();
    let failing = failing_checks(&events);
    if !moved && disputed.is_empty() {
        return failed(repo, id, nothing_reason(&failing, &summary));
    }
    let fixed = if moved { fixed } else { Vec::new() };
    match crate::commands::fix::submit(repo, id, fixed, disputed, ACTOR) {
        Ok(out) => {
            let mut what = format!("{id} resubmitted at {}", short(&out.sha));
            if !out.fixes.is_empty() {
                what.push_str(&format!(" · fixes {}", out.fixes.join(", ")));
            }
            if !out.disputes.is_empty() {
                let ids: Vec<&str> = out.disputes.iter().map(|d| d.id.as_str()).collect();
                what.push_str(&format!(" · disputes {}", ids.join(", ")));
            }
            Ok(Progress::worked(what))
        }
        Err(e) => failed(
            repo,
            id,
            with_summary(format!("could not submit the fix: {}", e.message), &summary),
        ),
    }
}

/// Reason recorded when a fixer run neither moved HEAD nor disputed anything:
/// names any failing gates and keeps the fixer's own summary so `hall status`
/// and `hall inbox` explain what blocked it.
fn nothing_reason(failing: &[CheckRun], summary: &str) -> String {
    let mut base = String::from("the fixer committed nothing and disputed nothing");
    if !failing.is_empty() {
        let cmds: Vec<String> = failing
            .iter()
            .map(|c| format!("`{}` (exit {})", c.command, c.exit_code))
            .collect();
        let label = if failing.len() == 1 {
            "failing check"
        } else {
            "failing checks"
        };
        base.push_str(&format!(" ({label}: {})", cmds.join(", ")));
    }
    with_summary(base, summary)
}

fn with_summary(mut reason: String, summary: &str) -> String {
    let s = summary.trim();
    if !s.is_empty() {
        reason.push_str(" — ");
        reason.push_str(&crate::output::clip(s, 240));
    }
    reason
}

/// Records a failed fixer attempt the way `run_metered` does: the first
/// failure in a row is retried on the next tick, the second holds the item.
fn failed(repo: &Repo, id: ItemId, reason: String) -> Result<Progress> {
    let attempt = runner::failures_in_a_row(&repo.hall.read_item(id)?, "fixer") + 1;
    append(
        repo,
        id,
        Body::RunFailed {
            role: "fixer".into(),
            reason: reason.clone(),
            attempt,
        },
    )?;
    repo.hall
        .live(&format!("{id} fixer failed (attempt {attempt}): {reason}"));
    if attempt >= 2 {
        hold(
            repo,
            id,
            HoldKind::RunFailed,
            format!("the fixer failed twice: {reason}"),
        )?;
        return Ok(Progress::worked(format!("{id} held: {reason}")));
    }
    Ok(Progress::worked(format!("{id} fixer failed: {reason}")))
}

fn hold(repo: &Repo, id: ItemId, kind: HoldKind, reason: String) -> Result<()> {
    append(repo, id, Body::Held { kind, reason })
}

fn append(repo: &Repo, id: ItemId, body: Body) -> Result<()> {
    repo.hall
        .append_item(id, &Event::new(ACTOR, Some(id), body))
}

/// `PATH` with this binary's directory first, so the fixer (and an external
/// engine) can run `hall fixed` even when `hall` isn't installed on PATH.
pub fn path_with_hall() -> Option<(String, String)> {
    let exe = std::env::current_exe().ok()?;
    let mut dirs = vec![exe.parent()?.to_path_buf()];
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    let joined = std::env::join_paths(dirs).ok()?;
    Some(("PATH".into(), joined.to_string_lossy().into_owned()))
}

/// The tools the fixer may use: commit, read its findings, record the fix,
/// and run the repo's gates and `dev_commands`. Edits need no rule:
/// `acceptEdits` allows them inside the worktree only, where a bare `Edit`
/// rule would allow any path (ADR 0053).
pub fn tools(gates: &[String]) -> Vec<String> {
    let mut t: Vec<String> = [
        "Read",
        "Grep",
        "Glob",
        "Bash(git add:*)",
        "Bash(git commit:*)",
        "Bash(git status:*)",
        "Bash(git diff:*)",
        "Bash(git log:*)",
        "Bash(git show:*)",
        "Bash(hall findings:*)",
        "Bash(hall fixed:*)",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for g in gates {
        let g = g.trim();
        if !g.is_empty() {
            let rule = format!("Bash({g}:*)");
            if !t.contains(&rule) {
                t.push(rule);
            }
        }
    }
    t
}

/// All gate runs from the last checks event when that run failed and is still
/// the latest word on the item.
fn last_failed_runs(events: &[Event]) -> Vec<CheckRun> {
    events
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
}

/// The gates that failed in the last checks run, while that run is still
/// the latest word on the item (the same rule as `hall findings`).
pub fn failing_checks(events: &[Event]) -> Vec<CheckRun> {
    last_failed_runs(events)
        .into_iter()
        .filter(|r| r.exit_code != 0)
        .collect()
}

/// The latest fixer `run_failed` event since the last state transition or
/// release, so a retry attempt knows why the prior attempt failed.
fn last_fixer_failure(events: &[Event]) -> Option<(u32, &str)> {
    let mut last = None;
    for e in events {
        match &e.body {
            Body::RunFailed {
                role,
                reason,
                attempt,
            } if role == ACTOR => last = Some((*attempt, reason.as_str())),
            Body::Released { .. } => last = None,
            b if b.action().is_some() => last = None,
            _ => {}
        }
    }
    last
}

/// Open findings, most severe first, then in the order raised; the
/// pre-existing ones last, whatever their severity (ADR 0059).
pub fn sorted_open(ledger: &Ledger) -> Vec<&Tracked> {
    let mut open = ledger.open();
    open.sort_by_key(|t| (t.finding.pre_existing, t.finding.severity));
    open
}

/// Open findings that still need fixing: never claimed yet, or still open
/// after a review saw the claiming commit.
fn active_open(ledger: &Ledger) -> Vec<&Tracked> {
    sorted_open(ledger)
        .into_iter()
        .filter(|t| t.claimed_by.is_none() || t.claimed_reviewed)
        .collect()
}

/// Open findings already claimed fixed by a commit that hasn't been reviewed
/// yet (for instance, because checks failed before review).
fn pending_review(ledger: &Ledger) -> Vec<&Tracked> {
    sorted_open(ledger)
        .into_iter()
        .filter(|t| t.claimed_by.is_some() && !t.claimed_reviewed)
        .collect()
}

/// The request as typed when the item was created, with any scope the Lead
/// added since (ADR 0068).
pub fn request(events: &[Event]) -> Option<String> {
    crate::event::request_with_criteria(events)
}

/// A check log's path: absolute, or relative to the item's journal
/// directory (`checks/r2.log`), or to the journal root.
pub fn log_path(repo: &Repo, id: ItemId, log: &str) -> PathBuf {
    let p = Path::new(log);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    let in_item = repo.hall.item_dir(id).join(p);
    if in_item.exists() {
        in_item
    } else {
        repo.hall.root.join(p)
    }
}

/// The last `lines` lines of `text`, at most `max` bytes.
fn tail_str(text: &str, lines: usize, max: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    let mut t = all[all.len().saturating_sub(lines)..].join("\n");
    if t.len() > max {
        let mut cut = t.len() - max;
        while !t.is_char_boundary(cut) {
            cut += 1;
        }
        t = format!("…{}", &t[cut..]);
    }
    t
}

/// The last `lines` lines of a file, at most `max` bytes.
pub fn tail(path: &Path, lines: usize, max: usize) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(tail_str(&text, lines, max))
}

/// Extracts `command`'s own section from a combined `checks/r<round>.log`,
/// where `assayer::checks` writes each gate starting with `$ <command>`.
/// Returns `None` when the log has no `$ <command>` header (such as a
/// standalone single-gate log).
pub fn gate_section<'a>(text: &'a str, command: &str, all_commands: &[&str]) -> Option<&'a str> {
    let header = format!("$ {command}");
    let mut offset = 0usize;
    let mut spans: Vec<(usize, usize, &str)> = Vec::new();
    for raw in text.split_inclusive('\n') {
        let line = raw.trim_end_matches(['\r', '\n']);
        let end = offset + raw.len();
        spans.push((offset, end, line));
        offset = end;
    }
    let start_idx = spans.iter().rposition(|(_, _, l)| *l == header)?;
    let mut end_byte = text.len();
    for i in (start_idx + 1)..spans.len() {
        let line = spans[i].2;
        let prev = spans[i - 1].2;
        let after_break = i == start_idx + 1
            || prev.is_empty()
            || prev.starts_with("[exit ")
            || prev.starts_with("[cached: ")
            || prev.starts_with("[timed out ");
        if !after_break {
            continue;
        }
        let is_next_gate = if !all_commands.is_empty() {
            line.strip_prefix("$ ")
                .is_some_and(|cmd| all_commands.contains(&cmd))
        } else {
            line.starts_with("$ ")
        };
        if is_next_gate {
            end_byte = spans[i].0;
            break;
        }
    }
    Some(text[spans[start_idx].0..end_byte].trim_end())
}

/// The tail of `command`'s own section in `path`, falling back to the tail of
/// the whole file when `path` doesn't use `$ <command>` sections.
pub fn gate_tail(
    path: &Path,
    command: &str,
    all_commands: &[&str],
    lines: usize,
    max: usize,
) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let slice = gate_section(&text, command, all_commands).unwrap_or(&text);
    Some(tail_str(slice, lines, max))
}

/// The fixer's prompt: everything it needs, since the agent replaces the
/// default system prompt (docs/spikes.md, spike 3).
pub fn prompt(repo: &Repo, item: &Item, events: &[Event], ledger: &Ledger) -> String {
    let id = item.id;
    let mut s = format!(
        "Fix Hallmark item {id} \"{}\", review round {} (your fix starts round {}).\n\n\
         - Worktree (your only workspace; you are already in it): {}\n\
         - Branch: {} · base: {}\n\
         - Commit under review: {}\n",
        item.title,
        item.round,
        item.round + 1,
        item.worktree,
        item.branch,
        item.base,
        item.frozen_sha.as_deref().unwrap_or("?"),
    );
    if let Some(r) = request(events) {
        s.push_str(&format!("\n## The request\n\n{}\n", r.trim()));
    }
    if let Some(sum) = &item.summary {
        s.push_str(&format!("\nThe developer's summary: {}\n", sum.trim()));
    }
    if let Some(w) = crate::words::pending(events, "fixer") {
        s.push_str(&format!("\n{}", w.section));
    }
    if let Some((attempt, reason)) = last_fixer_failure(events) {
        s.push_str(&format!(
            "\n## Previous fix attempt failed\n\n\
             Attempt {attempt} failed: {reason}\n\
             Don't repeat that failure: address the root cause, commit your changes, and resubmit.\n"
        ));
    }
    let runs = last_failed_runs(events);
    let all_cmds: Vec<&str> = runs.iter().map(|r| r.command.as_str()).collect();
    let failing: Vec<&CheckRun> = runs.iter().filter(|r| r.exit_code != 0).collect();
    if !failing.is_empty() {
        s.push_str("\n## Failing checks (fix these first)\n");
        for c in &failing {
            s.push_str(&format!("\n- `{}` exited {}", c.command, c.exit_code));
            match &c.log {
                Some(log) => {
                    let path = log_path(repo, id, log);
                    s.push_str(&format!(" · log: {}\n", crate::paths::display(&path)));
                    if let Some(t) = gate_tail(&path, &c.command, &all_cmds, 200, 20000) {
                        s.push_str(&format!("\n```text\n{t}\n```\n"));
                    }
                }
                None => s.push('\n'),
            }
        }
    }
    s.push_str(&open_findings_section(ledger, id, !failing.is_empty()));
    prompt_end(repo, item, ledger, !failing.is_empty(), s)
}

/// The prompt's open findings, most severe first, pre-existing ones marked.
/// Findings already claimed fixed by an unreviewed commit (e.g. checks failed
/// before the next review) are noted as pending review rather than shown as
/// rejected by the reviewer.
fn open_findings_section(ledger: &Ledger, id: ItemId, gate_failed: bool) -> String {
    let mut s = String::new();
    let active = active_open(ledger);
    let pending = pending_review(ledger);
    if active.is_empty() && pending.is_empty() {
        s.push_str("\n## Open findings\n\nNone.\n");
    } else {
        if active.is_empty() {
            s.push_str(
                "\n## Open findings\n\nNone left to fix right now. The findings below are \
                 already claimed fixed and wait for checks to pass before the reviewer \
                 checks them.\n",
            );
            if gate_failed {
                s.push_str(&format!(
                    "Focus on fixing the failing checks above, commit, and resubmit \
                     (`hall fixed --item {id}`).\n"
                ));
            }
        } else {
            s.push_str("\n## Open findings (most severe first)\n");
            for t in &active {
                s.push_str(&finding_block(t));
            }
        }
        if !pending.is_empty() {
            let ids: Vec<&str> = pending.iter().map(|t| t.finding.id.as_str()).collect();
            s.push_str(&format!(
                "\n## Claimed fixed, pending review once checks pass: {}\n\n\
                 Change these only when a failing check comes from their fix; don't undo \
                 their fix or their tests to turn a check green.\n",
                ids.join(", ")
            ));
            for t in &pending {
                s.push_str(&finding_block(t));
            }
        }
    }
    s
}

/// One finding as the prompt shows it.
fn finding_block(t: &Tracked) -> String {
    let f = &t.finding;
    let mut s = format!(
        "\n### {} · {}{} · {} · {}:{}\n\n{}\n\n- Scenario: {}\n- Suggested fix: {}\n",
        f.id,
        f.severity.as_str(),
        if f.pre_existing {
            " (pre-existing)"
        } else {
            ""
        },
        f.lens.as_str(),
        f.file,
        f.line,
        f.title.trim(),
        f.scenario.trim(),
        f.fix.trim()
    );
    if f.pre_existing {
        s.push_str(
            "- Pre-existing: this change didn't introduce it, and it never blocks \
             approval. Fix it only when that is cheap and safe; otherwise leave it, \
             and say so in your summary.\n",
        );
    }
    if let Some(d) = &t.dispute {
        s.push_str(&format!("- You disputed it before: \"{}\"\n", d.trim()));
    }
    if let Some(n) = &t.note {
        s.push_str(&format!("- The reviewer's note: \"{}\"\n", n.trim()));
    }
    if let Some(c) = &t.claimed_by
        && t.claimed_reviewed
    {
        s.push_str(&format!(
            "- Claimed fixed by {} before; the reviewer still sees it open.\n",
            short(c)
        ));
    }
    s
}

/// The rest of the prompt: the repo's skill, extensions, and the closing steps.
fn prompt_end(
    repo: &Repo,
    item: &Item,
    ledger: &Ledger,
    gate_failed: bool,
    mut s: String,
) -> String {
    let id = item.id;
    // The repo's own hallmark-fix skill (ADR 0037), from the base: the
    // branch being fixed can't change how it is fixed.
    if let Ok(base) = crate::assayer::base::Base::resolve(repo, &item.base)
        && let Some(skill) = crate::skills::read(&repo.main_root, &base.sha, "hallmark-fix")
    {
        let origin = format!("{} at {}", base.name, short(&base.sha));
        s.push_str(&format!("\n{}\n", crate::skills::section(&skill, &origin)));
    }
    // Extensions turned on for the fixer (ADR 0038), from the main
    // checkout's config like every other setting (ADR 0012), each skill
    // only when this round calls for it: a red gate, an open finding's lens.
    let round = crate::extensions::Context {
        gate_failed,
        lenses: active_open(ledger).iter().map(|t| t.finding.lens).collect(),
    };
    if let Some(x) = crate::extensions::prompt_for(
        &repo.config.extensions,
        crate::extensions::Role::Fixer,
        &round,
    ) {
        s.push_str(&format!("\n{x}"));
    }
    let gates: Vec<String> = repo.config.gates.iter().map(|g| format!("`{g}`")).collect();
    s.push_str("\n## When you are done\n\n");
    s.push_str(
        "1. Commit each fix with git in this worktree; name the finding IDs in the message \
         (\"fix F1: …\").\n",
    );
    if gates.is_empty() {
        s.push_str("2. This repo has no configured gates; run its tests if you can.\n");
    } else {
        s.push_str(&format!(
            "2. Run the gates and make them pass: {}.\n",
            gates.join(", ")
        ));
    }
    s.push_str(&format!(
        "3. Record the fix: `hall fixed <ids you fixed> --item {id}` (or `hall fixed --item {id}` \
         if you only fixed failing checks). Dispute a finding only with \
         a concrete reason: `hall fixed --dispute <id> --reason \"…\" --item {id}`.\n\
         4. Return the fix report: summary, fixed, disputed.\n\n\
         Never push, never open a PR, never run `hall ready`: you can't approve, only the next \
         review can. Hallmark notes may say this item is in the review loop and not to edit it: \
         that is for development sessions. You are the loop's fixer.\n"
    ));
    s
}

/// A configured command's result.
pub struct CommandRun {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub spawn_error: Option<String>,
}

impl CommandRun {
    /// Why it failed, if it did.
    pub fn failure(&self) -> Option<String> {
        if let Some(e) = &self.spawn_error {
            return Some(format!("could not start: {e}"));
        }
        if self.timed_out {
            return Some("timed out".into());
        }
        match self.code {
            Some(0) => None,
            Some(c) => Some(format!(
                "exited {c}: {}",
                crate::output::clip(&last_lines(&self.stderr, &self.stdout), 300)
            )),
            None => Some("was killed by a signal".into()),
        }
    }
}

fn last_lines(stderr: &str, stdout: &str) -> String {
    let src = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    let lines: Vec<&str> = src.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(5)..].join(" / ")
}

/// Runs a configured command through the platform shell in `dir`, with a
/// timeout in seconds (0: none) and extra environment.
pub fn run_command(cmd: &str, dir: &Path, timeout: u64, env: &[(&str, &str)]) -> CommandRun {
    use std::io::Read;
    use std::process::Stdio;
    use wait_timeout::ChildExt;
    let mut c = crate::commands::new::shell(cmd);
    c.current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("CLAUDECODE");
    for (k, v) in env {
        c.env(k, v);
    }
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => {
            return CommandRun {
                code: None,
                stdout: String::new(),
                stderr: String::new(),
                timed_out: false,
                spawn_error: Some(e.to_string()),
            };
        }
    };
    let reader = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = Vec::new();
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut s);
            }
            String::from_utf8_lossy(&s).into_owned()
        })
    };
    let out = reader(
        child
            .stdout
            .take()
            .map(|o| Box::new(o) as Box<dyn Read + Send>),
    );
    let err = reader(
        child
            .stderr
            .take()
            .map(|o| Box::new(o) as Box<dyn Read + Send>),
    );
    let (code, timed_out) = if timeout == 0 {
        (child.wait().ok().and_then(|s| s.code()), false)
    } else {
        match child.wait_timeout(Duration::from_secs(timeout)) {
            Ok(Some(s)) => (s.code(), false),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                (None, true)
            }
        }
    };
    CommandRun {
        code,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
        timed_out,
        spawn_error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::findings::{Finding, Lens, Severity, Status};

    #[test]
    fn the_plugin_schema_matches_the_report() {
        let schema: serde_json::Value =
            serde_json::from_str(crate::plugin::file("schemas/fix.json")).unwrap();
        let mut req: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        req.sort();
        assert_eq!(req, ["disputed", "fixed", "summary"]);
        // `claude -p --json-schema` rejects a draft 2020-12 `$schema` key
        // ("no schema with key or ref"), checked live with Claude Code 2.1.296.
        assert!(schema.get("$schema").is_none());
        let r: FixReport = serde_json::from_value(serde_json::json!({
            "summary": "s", "fixed": ["F1"], "disputed": [{"id": "F2", "reason": "r"}]
        }))
        .unwrap();
        assert_eq!(r.disputed[0].id, "F2");
    }

    #[test]
    fn the_fixer_reads_the_added_scope_and_criteria() {
        use crate::event::{Body, Event};
        let id = crate::item::ItemId(1);
        let events = [Event::new(
            "you",
            Some(id),
            Body::ScopeAdded {
                request: "Cap retries at 6".into(),
                criteria: vec!["stops after 6 attempts".into()],
                reason: None,
            },
        )];
        let r = request(&events).unwrap();
        assert!(r.contains("Added scope: Cap retries at 6"), "{r}");
        assert!(r.contains("- stops after 6 attempts"), "{r}");
    }

    #[test]
    fn claude_or_an_engine() {
        assert!(is_claude("claude -p"));
        assert!(is_claude("  claude   -p "));
        assert!(is_claude(""));
        assert!(!is_claude("ralph run --prompt {findings_file}"));
    }

    #[test]
    fn tools_include_the_gates() {
        let t = tools(&["cargo test".into(), " ".into(), "cargo test".into()]);
        assert!(t.contains(&"Bash(hall fixed:*)".to_string()));
        assert!(t.contains(&"Bash(cargo test:*)".to_string()));
        assert_eq!(t.iter().filter(|x| x.contains("cargo test")).count(), 1);
        assert!(!t.iter().any(|x| x.contains("push")));
    }

    #[test]
    fn findings_sort_by_severity() {
        let f = |id: &str, sev| Tracked {
            finding: Finding {
                id: id.into(),
                round: 1,
                lens: Lens::Correctness,
                severity: sev,
                file: "a.rs".into(),
                line: 1,
                title: "t".into(),
                scenario: "s".into(),
                fix: "f".into(),
                pre_existing: false,
            },
            status: Status::Open,
            claimed_by: None,
            claimed_reviewed: false,
            dispute: None,
            settled_round: None,
            note: None,
        };
        let l = Ledger {
            findings: vec![
                f("F1", Severity::Nit),
                f("F2", Severity::Blocker),
                f("F3", Severity::Major),
                f("F4", Severity::Blocker),
            ],
        };
        let ids: Vec<&str> = sorted_open(&l)
            .iter()
            .map(|t| t.finding.id.as_str())
            .collect();
        assert_eq!(ids, ["F2", "F4", "F3", "F1"]);
    }

    #[test]
    fn a_pre_existing_blocker_sorts_after_a_normal_minor_and_is_marked() {
        let f = |id: &str, sev, pre| Tracked {
            finding: Finding {
                id: id.into(),
                round: 1,
                lens: Lens::Correctness,
                severity: sev,
                file: "a.rs".into(),
                line: 1,
                title: "t".into(),
                scenario: "s".into(),
                fix: "f".into(),
                pre_existing: pre,
            },
            status: Status::Open,
            claimed_by: None,
            claimed_reviewed: false,
            dispute: None,
            settled_round: None,
            note: None,
        };
        let l = Ledger {
            findings: vec![
                f("F1", Severity::Blocker, true),
                f("F2", Severity::Minor, false),
            ],
        };
        let ids: Vec<&str> = sorted_open(&l)
            .iter()
            .map(|t| t.finding.id.as_str())
            .collect();
        assert_eq!(ids, ["F2", "F1"]);
        let text = open_findings_section(&l, ItemId(1), false);
        assert!(text.contains("### F1 · blocker (pre-existing)"), "{text}");
        assert!(!text.contains("### F2 · minor (pre-existing)"), "{text}");
        assert!(text.contains("Fix it only when that is cheap and safe"));
        assert_eq!(text.matches("Pre-existing: this change").count(), 1);
    }

    #[test]
    fn gate_section_extracts_earlier_failing_gate_from_combined_log() {
        let log = "\
# H-0109 round 2: checks at 084fea1
# in /tmp/assay

$ cargo fmt --check
Diff in src/hooks.rs:10:
-foo();
+bar();

[exit 1 after 0.4s]

$ cargo clippy --all-targets -- -D warnings
Finished `dev` profile

[exit 0 after 2.1s]

$ cargo test --all-targets
test a ... ok
test b ... ok

[exit 0 after 12.0s]
";
        let cmds = [
            "cargo fmt --check",
            "cargo clippy --all-targets -- -D warnings",
            "cargo test --all-targets",
        ];
        let sec = gate_section(log, "cargo fmt --check", &cmds).unwrap();
        assert!(sec.contains("Diff in src/hooks.rs:10:"), "{sec}");
        assert!(sec.contains("[exit 1 after 0.4s]"), "{sec}");
        assert!(!sec.contains("cargo clippy"), "{sec}");
        assert!(!sec.contains("test a ... ok"), "{sec}");

        // A standalone log without `$ <cmd>` headers falls back to the tail.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("r1.log");
        std::fs::write(&p, "line 1\nline 2\nFAILED\n").unwrap();
        let t = gate_tail(&p, "cargo test", &["cargo test"], 2, 1000).unwrap();
        assert_eq!(t, "line 2\nFAILED");
    }

    #[test]
    fn unreviewed_claimed_findings_are_not_shown_as_reviewer_rejections() {
        let mut l = Ledger::default();
        l.apply(&Body::Verdict {
            sha: "aaaa111".into(),
            round: 1,
            verdict: crate::event::VerdictKind::Changes,
            escalated: false,
            findings: vec![Finding {
                id: "F1".into(),
                round: 1,
                lens: Lens::Correctness,
                severity: Severity::Major,
                file: "src/a.rs".into(),
                line: 10,
                title: "missing lock".into(),
                scenario: "race".into(),
                fix: "lock it".into(),
                pre_existing: false,
            }],
            prior: vec![],
            counts: Counts::default(),
            summary: None,
            report: None,
            reviewer: None,
            learnings: vec![],
            base: None,
            since: None,
            needs_person: None,
        });
        l.apply(&Body::FixSubmitted {
            sha: "bbbb222".into(),
            fixes: vec!["F1".into()],
            disputes: vec![],
        });
        // Before any round-2 verdict (e.g. checks failed on bbbb222):
        let before = open_findings_section(&l, ItemId(9), true);
        assert!(before.contains("None left to fix right now"), "{before}");
        assert!(before.contains("F1"), "{before}");
        assert!(
            !before.contains("the reviewer still sees it open"),
            "{before}"
        );
        assert!(before.contains("hall fixed --item H-0009"), "{before}");

        // Once a round-2 verdict runs and leaves F1 open:
        l.apply(&Body::Verdict {
            sha: "bbbb222".into(),
            round: 2,
            verdict: crate::event::VerdictKind::Changes,
            escalated: false,
            findings: vec![],
            prior: vec![crate::findings::PriorRuling {
                id: "F1".into(),
                status: crate::findings::PriorStatus::Open,
                note: Some("still racy".into()),
            }],
            counts: Counts::default(),
            summary: None,
            report: None,
            reviewer: None,
            learnings: vec![],
            base: None,
            since: None,
            needs_person: None,
        });
        let after = open_findings_section(&l, ItemId(9), false);
        assert!(after.contains("### F1 · major"), "{after}");
        assert!(
            after.contains("Claimed fixed by bbbb222 before; the reviewer still sees it open."),
            "{after}"
        );
    }

    fn test_finding(id: &str, title: &str) -> Finding {
        Finding {
            id: id.into(),
            round: 1,
            lens: Lens::Correctness,
            severity: Severity::Major,
            file: "src/a.rs".into(),
            line: 10,
            title: title.into(),
            scenario: format!("scenario of {id}"),
            fix: format!("fix of {id}"),
            pre_existing: false,
        }
    }

    fn test_verdict(round: u32, findings: Vec<Finding>, open: &[&str]) -> Body {
        Body::Verdict {
            sha: format!("sha{round}"),
            round,
            verdict: crate::event::VerdictKind::Changes,
            escalated: false,
            findings,
            prior: open
                .iter()
                .map(|id| crate::findings::PriorRuling {
                    id: (*id).into(),
                    status: crate::findings::PriorStatus::Open,
                    note: None,
                })
                .collect(),
            counts: Counts::default(),
            summary: None,
            report: None,
            reviewer: None,
            learnings: vec![],
            base: None,
            since: None,
            needs_person: None,
        }
    }

    fn test_claim(sha: &str, ids: &[&str]) -> Body {
        Body::FixSubmitted {
            sha: sha.into(),
            fixes: ids.iter().map(|s| (*s).into()).collect(),
            disputes: vec![],
        }
    }

    #[test]
    fn reclaiming_after_a_review_makes_the_finding_pending_again() {
        let mut l = Ledger::default();
        l.apply(&test_verdict(1, vec![test_finding("F1", "t1")], &[]));
        l.apply(&test_claim("sha2", &["F1"]));
        l.apply(&test_verdict(2, vec![], &["F1"]));
        assert!(l.get("F1").unwrap().claimed_reviewed);
        l.apply(&test_claim("sha3", &["F1"]));
        let t = l.get("F1").unwrap();
        assert_eq!(t.claimed_by.as_deref(), Some("sha3"));
        assert!(!t.claimed_reviewed);
        let text = open_findings_section(&l, ItemId(9), true);
        assert!(text.contains("None left to fix right now"), "{text}");
        assert!(!text.contains("reviewer still sees it open"), "{text}");
    }

    #[test]
    fn a_claimed_finding_left_out_of_prior_is_active() {
        let mut l = Ledger::default();
        l.apply(&test_verdict(1, vec![test_finding("F1", "t1")], &[]));
        l.apply(&test_claim("sha2", &["F1"]));
        l.apply(&test_verdict(2, vec![], &[]));
        assert!(l.get("F1").unwrap().claimed_reviewed);
        let text = open_findings_section(&l, ItemId(9), false);
        assert!(text.contains("### F1 · major"), "{text}");
        assert!(text.contains("the reviewer still sees it open"), "{text}");
    }

    #[test]
    fn mixed_active_and_pending_findings_are_both_in_full() {
        let mut l = Ledger::default();
        l.apply(&test_verdict(
            1,
            vec![test_finding("F1", "t1"), test_finding("F2", "t2")],
            &[],
        ));
        l.apply(&test_claim("sha2", &["F2"]));
        let text = open_findings_section(&l, ItemId(9), true);
        assert!(
            text.contains("## Open findings (most severe first)"),
            "{text}"
        );
        assert!(text.contains("### F1 · major"), "{text}");
        assert!(
            text.contains("pending review once checks pass: F2"),
            "{text}"
        );
        // The pending finding keeps its scenario and suggested fix.
        assert!(text.contains("### F2 · major"), "{text}");
        assert!(text.contains("scenario of F2"), "{text}");
        assert!(text.contains("fix of F2"), "{text}");
        assert!(!text.contains("reviewer still sees it open"), "{text}");
    }

    #[test]
    fn a_fixer_failure_is_forgotten_once_the_item_moves_on() {
        let ev = |body| Event::new("hall", Some(ItemId(9)), body);
        let failed = |attempt, reason: &str| {
            ev(Body::RunFailed {
                role: ACTOR.into(),
                reason: reason.into(),
                attempt,
            })
        };
        let mut events = vec![failed(1, "Sandbox blocked cargo fmt")];
        assert_eq!(
            last_fixer_failure(&events),
            Some((1, "Sandbox blocked cargo fmt"))
        );
        // A release (e.g. after `hall retry` lifts the hold) clears it.
        events.push(ev(Body::Released {
            reason: None,
            granted_tokens: None,
        }));
        assert_eq!(last_fixer_failure(&events), None);
        // A later failure shows again; a failed review of the next fix clears it.
        events.push(failed(1, "second"));
        assert_eq!(last_fixer_failure(&events), Some((1, "second")));
        events.push(ev(test_verdict(3, vec![], &[])));
        assert_eq!(last_fixer_failure(&events), None);
        // So does a checks result.
        events.push(failed(1, "third"));
        events.push(ev(Body::Retried {
            reason: "again".into(),
        }));
        assert_eq!(last_fixer_failure(&events), None);
    }

    #[test]
    fn tails_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.log");
        let body: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&p, body).unwrap();
        let t = tail(&p, 3, 1000).unwrap();
        assert_eq!(t, "line 98\nline 99\nline 100");
        let t = tail(&p, 100, 10).unwrap();
        assert!(t.starts_with('…') && t.ends_with("line 100"));
        assert!(tail(&dir.path().join("none"), 3, 10).is_none());
    }

    #[test]
    fn commands_report_failures() {
        let dir = tempfile::tempdir().unwrap();
        let ok = run_command("echo hi", dir.path(), 30, &[]);
        assert_eq!(ok.failure(), None);
        assert!(ok.stdout.contains("hi"));
        let bad = run_command("exit 3", dir.path(), 30, &[]);
        assert!(bad.failure().unwrap().starts_with("exited 3"));
    }

    #[cfg(unix)]
    #[test]
    fn commands_time_out() {
        let dir = tempfile::tempdir().unwrap();
        let slow = run_command("sleep 5", dir.path(), 1, &[]);
        assert!(slow.timed_out);
        assert_eq!(slow.failure().as_deref(), Some("timed out"));
    }
}
