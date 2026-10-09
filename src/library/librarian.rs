//! The librarian (Haiku): distils a closed or escalated item's journal into
//! learnings, and proposes retiring the ones reviews no longer need.
//!
//! - An item is distilled once per arrival in closed or escalated (or
//!   dropped after an escalation, which is your ruling): a `distilled` event
//!   after its last state change means it's done. `hall library distill` is
//!   the same code path, so it is idempotent too.
//! - The model sees a compact digest of the journal and the existing
//!   learnings, and returns `plugin/schemas/learnings.json`. It may only
//!   update or add; Hallmark numbers new learnings, keeps one file per ID,
//!   and treats a "new" learning with an existing title as an update.
//! - Results go into a knowledge item (see `knowledge.rs`). Kinds listed in
//!   `[library] auto_accept` are written `accepted`; the rest `proposed`.
//! - Retirement: accepted learnings contradicted by a later verdict
//!   (`held: false` after the file last changed), or unused for
//!   `retire_after`, are written `retired` in the knowledge item.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use super::knowledge::{self, Knowledge};
use super::learning::{self, Kind, Learning, Status};
use super::store::{self, KNOWLEDGE_PREFIX, Library, UseStats};
use crate::config::Role;
use crate::error::{ErrorCode, HallError, Result};
use crate::event::{Body, Event};
use crate::findings::{Ledger, Status as FindingStatus};
use crate::item::{Item, ItemId, State};
use crate::output::clip;
use crate::repo::Repo;
use crate::runner::{self, Failure, Metered, Runner};
use crate::worker::{Progress, WorkerCtx};

pub const ACTOR: &str = "librarian";

/// What the model returns (`plugin/schemas/learnings.json`), read leniently.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Proposals {
    #[serde(default)]
    pub learnings: Vec<Proposal>,
    #[serde(default)]
    pub retire: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Proposal {
    /// The ID of the learning this updates, or "" for a new one.
    #[serde(default)]
    pub updates: String,
    pub kind: String,
    #[serde(default)]
    pub scope: Vec<String>,
    #[serde(default)]
    pub lens: String,
    #[serde(default)]
    pub source: Vec<String>,
    pub title: String,
    #[serde(default)]
    pub body: String,
}

/// What a distillation (or a retirement pass) changed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, schemars::JsonSchema)]
pub struct Applied {
    /// The knowledge item that carries the changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub knowledge: Option<ItemId>,
    /// Learnings added or updated.
    pub learnings: Vec<String>,
    /// Learnings proposed for retirement.
    pub retired: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// The result of [`distill`].
#[derive(Debug)]
pub enum Distill {
    /// Done now, or earlier (`fresh: false`).
    Done { applied: Applied, fresh: bool },
    /// The run failed, or a budget stopped it.
    Stopped {
        progress: Progress,
        error: HallError,
    },
}

/// Is `item` something the librarian distils?
pub fn eligible(item: &Item, events: &[Event]) -> bool {
    if item.title.starts_with(KNOWLEDGE_PREFIX) {
        return false;
    }
    match item.state {
        State::Closed | State::Escalated => true,
        State::Dropped => events.iter().any(|e| {
            matches!(
                e.body,
                Body::Verdict {
                    escalated: true,
                    ..
                }
            )
        }),
        _ => false,
    }
}

/// A `distilled` event after the item's last state change.
pub fn already_distilled(events: &[Event]) -> Option<&Event> {
    let last_move = events.iter().rposition(|e| e.body.action().is_some());
    let last = events
        .iter()
        .rposition(|e| matches!(e.body, Body::Distilled { .. }))?;
    (Some(last) > last_move).then(|| &events[last])
}

fn earlier(e: &Event) -> Applied {
    match &e.body {
        Body::Distilled { into, learnings } => Applied {
            knowledge: *into,
            learnings: learnings.clone(),
            ..Default::default()
        },
        _ => Applied::default(),
    }
}

/// Paths the item changed against its base, when its branch still exists.
fn changed_paths(repo: &Repo, item: &Item) -> Vec<String> {
    let range = format!("{}...{}", item.base, item.branch);
    crate::git::run(&repo.main_root, &["diff", "--name-only", &range])
        .map(|s| s.lines().map(String::from).collect())
        .unwrap_or_default()
}

/// A compact digest of the journal for the librarian.
pub fn digest(item: &Item, events: &[Event], changed: &[String]) -> String {
    let mut s = format!("# {} · {}\n\n", item.id, item.title);
    let mut outcome = format!(
        "Outcome: {} after {} review round(s), {} rejected",
        item.state, item.round, item.rejects
    );
    if let Some(pr) = &item.pr_url {
        outcome.push_str(&format!(", PR {pr}"));
    }
    s.push_str(&outcome);
    s.push_str(".\n");

    if let Some(Body::Created {
        request: Some(r), ..
    }) = events.first().map(|e| &e.body)
    {
        s.push_str(&format!("\n## Request\n\n{}\n", clip_block(r, 1200)));
    }
    if !changed.is_empty() {
        s.push_str("\n## Changed paths\n\n");
        for p in changed.iter().take(40) {
            s.push_str(&format!("- {p}\n"));
        }
        if changed.len() > 40 {
            s.push_str(&format!("- … and {} more\n", changed.len() - 40));
        }
    }

    let reviews: Vec<String> = events
        .iter()
        .filter_map(|e| match &e.body {
            Body::Verdict {
                round,
                verdict,
                escalated,
                summary,
                ..
            } => {
                let v = match (verdict, escalated) {
                    (crate::event::VerdictKind::Approve, _) => "approved",
                    (_, true) => "changes requested, escalated to you",
                    _ => "changes requested",
                };
                Some(format!(
                    "- Round {round}: {v}.{}",
                    summary
                        .as_deref()
                        .map(|s| format!(" {}", clip(s, 300)))
                        .unwrap_or_default()
                ))
            }
            Body::Checks {
                round,
                passed: false,
                ..
            } => Some(format!("- Round {round}: checks failed.")),
            _ => None,
        })
        .collect();
    if !reviews.is_empty() {
        s.push_str("\n## Reviews\n\n");
        s.push_str(&reviews.join("\n"));
        s.push('\n');
    }

    let ledger = Ledger::from_events(events);
    if !ledger.findings.is_empty() {
        s.push_str("\n## Findings and how they were settled\n\n");
        for t in &ledger.findings {
            let f = &t.finding;
            let place = if f.line > 0 {
                format!("{}:{}", f.file, f.line)
            } else {
                f.file.clone()
            };
            s.push_str(&format!(
                "- {} · {} · {} · `{place}` (round {}): {}\n",
                f.id,
                f.severity.as_str(),
                f.lens.as_str(),
                f.round,
                clip(&f.title, 160)
            ));
            s.push_str(&format!("  Scenario: {}\n", clip(&f.scenario, 400)));
            if let Some(d) = &t.dispute {
                s.push_str(&format!("  The fixer disputed it: {}\n", clip(d, 300)));
            }
            let outcome = match (t.status, t.settled_round) {
                (FindingStatus::Fixed, Some(r)) => format!("fixed (confirmed in round {r})"),
                (FindingStatus::Fixed, None) => "fixed".into(),
                (FindingStatus::Withdrawn, Some(r)) => {
                    format!("withdrawn by the reviewer in round {r}")
                }
                (FindingStatus::Withdrawn, None) => "withdrawn by the reviewer".into(),
                (FindingStatus::Open, _) if t.dispute.is_some() => {
                    "still open: the reviewer did not accept the dispute".into()
                }
                (FindingStatus::Open, _) => "still open".into(),
            };
            s.push_str(&format!("  Outcome: {outcome}"));
            if let Some(n) = &t.note {
                s.push_str(&format!(" ({})", clip(n, 200)));
            }
            s.push('\n');
        }
    }

    let mut decisions = Vec::new();
    let mut questions: Vec<(String, String, Option<String>)> = Vec::new();
    let mut used = Vec::new();
    for e in events {
        match &e.body {
            Body::Retried { reason } => {
                decisions.push(format!("- Allowed another round: {}", clip(reason, 300)))
            }
            Body::PulledBack { reason } => decisions.push(format!(
                "- Pulled it back to development{}",
                reason
                    .as_deref()
                    .map(|r| format!(": {}", clip(r, 300)))
                    .unwrap_or_default()
            )),
            Body::Dropped { reason, .. } => {
                decisions.push(format!("- Dropped it: {}", clip(reason, 300)))
            }
            Body::Question { qid, text, .. } => questions.push((qid.clone(), text.clone(), None)),
            Body::Answer { qid, answer, .. } => {
                if let Some(q) = questions.iter_mut().find(|q| &q.0 == qid) {
                    q.2 = Some(answer.clone());
                }
            }
            Body::Verdict { learnings, .. } => {
                for u in learnings {
                    used.push(format!(
                        "- {} {}{}",
                        u.id,
                        if u.held { "held" } else { "did NOT hold" },
                        u.note
                            .as_deref()
                            .map(|n| format!(": {}", clip(n, 200)))
                            .unwrap_or_default()
                    ));
                }
            }
            _ => {}
        }
    }
    if !decisions.is_empty() {
        s.push_str("\n## The human's decisions\n\n");
        s.push_str(&decisions.join("\n"));
        s.push('\n');
    }
    if !questions.is_empty() {
        s.push_str("\n## Questions and answers\n\n");
        for (qid, text, answer) in &questions {
            s.push_str(&format!(
                "- {qid}: {} → {}\n",
                clip(text, 200),
                answer
                    .as_deref()
                    .map(|a| clip(a, 200))
                    .unwrap_or_else(|| "(unanswered)".into())
            ));
        }
    }
    if !used.is_empty() {
        s.push_str("\n## Learnings the reviews used\n\n");
        s.push_str(&used.join("\n"));
        s.push('\n');
    }
    s
}

fn clip_block(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        t.to_string()
    } else {
        let mut c: String = t.chars().take(max).collect();
        c.push('…');
        c
    }
}

/// The existing learnings, one line each, for the prompt.
pub fn existing_lines(lib: &Library) -> String {
    let all = lib.effective();
    if all.is_empty() {
        return "(none yet)\n".into();
    }
    let mut s = String::new();
    for l in all {
        let x = &l.learning;
        let scope = if x.scope.is_empty() {
            "anywhere".to_string()
        } else {
            x.scope.join(", ")
        };
        s.push_str(&format!(
            "- {} [{} · {}] scope: {} — {}\n",
            x.id,
            x.kind,
            x.status,
            scope,
            clip(&x.title, 160)
        ));
    }
    s
}

pub fn prompt(repo: &Repo, item: &Item, digest: &str, lib: &Library) -> String {
    format!(
        "Distil Hallmark item {id} of repository `{repo}` into learnings for its Library.\n\n\
Return the structured output:\n\
- `learnings`: what this item taught that will help future reviews here. Each is new or \
an update. Set `updates` to the ID of the existing learning it refines (keep that \
learning's point, sharpen or widen it), or \"\" for a new one. Prefer updating over adding \
a near-duplicate.\n\
- `retire`: IDs of existing learnings this item shows are wrong or obsolete.\n\
Return empty lists when the item taught nothing worth keeping; most items teach little.\n\n\
Kinds: `rule` (a finding that turned out right), `false-positive` (a finding that was \
withdrawn or a dispute the reviewer accepted: what not to flag, and why), `map` (what a \
module does, so reviewers stop re-exploring it), `decision` (the human's ruling on an \
escalation, with the reason). `source` names where it came from: `{id}/F1` for a finding, \
`{id}` for the item. The learnings live in `{dir}/` if you need to read one.\n\n\
## Existing learnings\n\n{existing}\n## The item\n\n{digest}",
        id = item.id,
        repo = repo.name,
        dir = store::dir(repo),
        existing = existing_lines(lib),
    )
}

/// Runs the librarian on `id` and records the result.
pub fn distill(repo: &Repo, runner: &dyn Runner, id: ItemId) -> Result<Distill> {
    let events = repo.hall.read_item(id)?;
    let item = crate::event::fold(id, &events)?;
    if item.title.starts_with(KNOWLEDGE_PREFIX) {
        return Err(HallError::new(
            ErrorCode::WrongState,
            format!("{id} is a knowledge item; its learnings are already in the Library"),
        )
        .next(["hall library review"]));
    }
    if !eligible(&item, &events) {
        return Err(HallError::new(
            ErrorCode::WrongState,
            format!(
                "{id} is {}; the librarian distils closed or escalated items",
                item.state
            ),
        )
        .next([format!("hall status {id}")]));
    }
    if let Some(e) = already_distilled(&events) {
        return Ok(Distill::Done {
            applied: earlier(e),
            fresh: false,
        });
    }
    let lib = Library::load(repo, &repo.config.base)?;
    let changed = changed_paths(repo, &item);
    let text = prompt(repo, &item, &digest(&item, &events, &changed), &lib);
    let mut req = runner::request(
        repo,
        Role::Librarian,
        Some("librarian"),
        text,
        &repo.main_root,
    )?;
    req.json_schema = Some(crate::plugin::file("schemas/learnings.json").to_string());
    req.allowed_tools = vec!["Read".into(), "Grep".into(), "Glob".into()];
    req.env.push(("HALL_NO_HOOKS".into(), "1".into()));
    let outcome = match runner::run_metered(repo, runner, Some(id), &req)? {
        Metered::Done(o) => o,
        Metered::Failed { failure, .. } => {
            let progress = match &failure {
                Failure::RateLimited { until } => Progress::Pause {
                    reason: failure.describe(),
                    until: *until,
                },
                f => Progress::worked(format!("librarian run for {id} failed: {}", f.describe())),
            };
            return Ok(Distill::Stopped {
                progress,
                error: runner::run_error("librarian", &failure),
            });
        }
        Metered::Blocked(stop) => {
            return Ok(Distill::Stopped {
                progress: stop.progress(),
                error: HallError::new(ErrorCode::BudgetExceeded, stop.reason.clone())
                    .next(["hall budget", "hall usage"]),
            });
        }
    };
    let proposals: Proposals = outcome
        .structured()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let applied = knowledge::with_lock(repo, || {
        apply(repo, Some(&item), &proposals, ACTOR, Utc::now())
    })?;
    let e = Event::new(
        ACTOR,
        Some(id),
        Body::Distilled {
            into: applied.knowledge,
            learnings: applied.learnings.clone(),
        },
    );
    repo.hall.append_item(id, &e)?;
    repo.hall.live(&format!(
        "{id} distilled: {} learnings, {} retirements{}",
        applied.learnings.len(),
        applied.retired.len(),
        applied
            .knowledge
            .map(|k| format!(" in {k}"))
            .unwrap_or_default()
    ));
    Ok(Distill::Done {
        applied,
        fresh: true,
    })
}

fn date(t: Option<DateTime<Utc>>) -> Option<chrono::NaiveDate> {
    t.map(|t| t.date_naive())
}

/// Brings `used` and `last_used` up to date from the journals (never
/// lowering what the file already says: other clones have other journals).
pub fn refresh_usage(l: &mut Learning, stats: &HashMap<String, UseStats>) {
    if let Some(s) = stats.get(&l.id) {
        l.used = l.used.max(s.used);
        l.last_used = l.last_used.max(date(s.last_used));
    }
}

fn status_for(repo: &Repo, kind: Kind) -> Status {
    let auto = repo
        .config
        .library
        .auto_accept
        .iter()
        .any(|k| Kind::parse(k) == Some(kind));
    if auto {
        Status::Accepted
    } else {
        Status::Proposed
    }
}

fn norm_title(t: &str) -> String {
    t.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Accepted learnings on the base that should go: contradicted by a verdict
/// after the file last changed, or unused for `retire_after`. Learnings
/// already being changed in an open knowledge item, or proposed for
/// retirement recently, are left alone.
pub fn retirement_candidates(
    repo: &Repo,
    lib: &Library,
    stats: &HashMap<String, UseStats>,
    now: DateTime<Utc>,
) -> Vec<(String, String)> {
    let after =
        crate::config::parse_duration(&repo.config.library.retire_after).unwrap_or(90 * 86_400);
    let dates = store::file_dates(repo, &repo.config.base);
    let recent = Retirements::load(repo);
    let mut out = Vec::new();
    for l in &lib.base {
        let x = &l.learning;
        if x.status != Status::Accepted
            || lib.pending_get(&x.id).is_some()
            || recent.blocks(repo, &x.id, now, after)
        {
            continue;
        }
        let s = stats.get(&x.id).cloned().unwrap_or_default();
        let d = dates.get(&l.path);
        if let Some(c) = s.last_contradicted
            && d.is_none_or(|d| c > d.modified)
        {
            let why = s.notes.last().cloned().unwrap_or_else(|| "no note".into());
            out.push((
                x.id.clone(),
                format!("a review found it no longer holds ({})", clip(&why, 200)),
            ));
            continue;
        }
        let file_last_used = x
            .last_used
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|t| t.and_utc());
        let reference = [s.last_used, file_last_used, d.map(|d| d.created)]
            .into_iter()
            .flatten()
            .max();
        if let Some(r) = reference
            && (now - r).num_seconds() > after
        {
            out.push((
                x.id.clone(),
                format!(
                    "not used by any review in {}",
                    repo.config.library.retire_after
                ),
            ));
        }
    }
    out
}

/// Retirements the librarian proposed, so it doesn't propose the same one
/// again while its knowledge item is on its way to the base.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Retirements {
    #[serde(default)]
    proposed: HashMap<String, (ItemId, DateTime<Utc>)>,
}

impl Retirements {
    fn path(repo: &Repo) -> std::path::PathBuf {
        repo.hall.root.join("library").join("retirements.json")
    }

    fn load(repo: &Repo) -> Retirements {
        std::fs::read_to_string(Self::path(repo))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, repo: &Repo) -> Result<()> {
        let p = Self::path(repo);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(p, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// A proposal within `after` whose knowledge item wasn't dropped.
    fn blocks(&self, repo: &Repo, id: &str, now: DateTime<Utc>, after: i64) -> bool {
        let Some((item, t)) = self.proposed.get(id) else {
            return false;
        };
        if (now - *t).num_seconds() > after {
            return false;
        }
        repo.hall
            .load_item(*item)
            .map(|i| i.state != State::Dropped)
            .unwrap_or(false)
    }
}

/// Remembers that `ids` were proposed for retirement in `item`, so the
/// librarian doesn't propose them again while that item is on its way.
pub fn note_retired(repo: &Repo, ids: &[String], item: ItemId, now: DateTime<Utc>) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut r = Retirements::load(repo);
    for id in ids {
        r.proposed.insert(id.clone(), (item, now));
    }
    r.save(repo)
}

/// Writes proposals and retirements into a knowledge item, commits, and
/// proposes it ready. Call under [`knowledge::with_lock`]. `source` is the
/// item being distilled (none for a retirement-only pass).
pub fn apply(
    repo: &Repo,
    source: Option<&Item>,
    props: &Proposals,
    actor: &str,
    now: DateTime<Utc>,
) -> Result<Applied> {
    let lib = Library::load(repo, &repo.config.base)?;
    let stats = store::usage(repo)?;
    let mut next = store::next_id(repo, &lib)?;
    // (learning, the path the base keeps it at)
    let mut changes: Vec<(Learning, Option<String>)> = Vec::new();
    let mut lines: Vec<String> = Vec::new();

    for p in &props.learnings {
        let (Some(kind), title) = (Kind::parse(&p.kind), p.title.trim()) else {
            continue;
        };
        if title.is_empty() {
            continue;
        }
        let nt = norm_title(title);
        let target_id = learning::normalize_id(&p.updates)
            .filter(|id| lib.get(id).is_some() || changes.iter().any(|(l, _)| &l.id == id))
            .or_else(|| {
                changes
                    .iter()
                    .find(|(l, _)| norm_title(&l.title) == nt)
                    .map(|(l, _)| l.id.clone())
            })
            .or_else(|| {
                lib.effective()
                    .into_iter()
                    .find(|l| {
                        l.learning.status != Status::Retired && norm_title(&l.learning.title) == nt
                    })
                    .map(|l| l.learning.id.clone())
            });
        let lens = crate::findings::Lens::parse(&p.lens)
            .map(|l| l.as_str().to_string())
            .or_else(|| Some(p.lens.trim().to_string()).filter(|s| !s.is_empty()));
        let mut sources: Vec<String> = p
            .source
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| s.starts_with("H-"))
            .collect();
        if let Some(src) = source {
            let sid = src.id.to_string();
            if !sources.iter().any(|s| s.starts_with(&sid)) {
                sources.push(sid);
            }
        }
        let scope: Vec<String> = p
            .scope
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let (mut l, base_path, verb) = match target_id {
            Some(id) => {
                let pos = changes.iter().position(|(l, _)| l.id == id);
                let (prev, base_path) = match pos {
                    Some(i) => changes.remove(i),
                    None => {
                        let found = lib.get(&id).expect("target exists");
                        (
                            found.learning.clone(),
                            lib.base_get(&id).map(|b| b.path.clone()),
                        )
                    }
                };
                let mut l = prev;
                for s in sources {
                    if !l.source.contains(&s) {
                        l.source.push(s);
                    }
                }
                (l, base_path, "updated")
            }
            None => {
                let id = learning::format_id(next);
                next += 1;
                (
                    Learning {
                        id,
                        kind,
                        scope: Vec::new(),
                        lens: None,
                        source: sources,
                        status: Status::Proposed,
                        used: 0,
                        last_used: None,
                        title: String::new(),
                        body: String::new(),
                        extra: Vec::new(),
                    },
                    None,
                    "new",
                )
            }
        };
        l.kind = kind;
        if !scope.is_empty() || verb == "new" {
            l.scope = scope;
        }
        if lens.is_some() {
            l.lens = lens;
        }
        l.title = title.to_string();
        if !p.body.trim().is_empty() {
            l.body = p.body.trim().to_string();
        }
        l.status = status_for(repo, kind);
        refresh_usage(&mut l, &stats);
        lines.push(format!(
            "- {} {verb} {}: {}",
            l.id,
            l.kind,
            clip(&l.title, 80)
        ));
        changes.push((l, base_path));
    }

    // Retirements: the model's, then Hallmark's own.
    let mut retire: Vec<(String, String)> = Vec::new();
    for r in &props.retire {
        if let Some(id) = learning::normalize_id(r) {
            retire.push((id, "the librarian found it wrong or obsolete".into()));
        }
    }
    retire.extend(retirement_candidates(repo, &lib, &stats, now));
    let mut retired_ids = Vec::new();
    let mut seen = HashSet::new();
    for (id, why) in retire {
        if changes.iter().any(|(l, _)| l.id == id) || !seen.insert(id.clone()) {
            continue;
        }
        let Some(found) = lib.get(&id) else {
            continue;
        };
        if found.learning.status == Status::Retired {
            continue;
        }
        let mut l = found.learning.clone();
        l.status = Status::Retired;
        l.body = format!(
            "{}\n\nRetired {}: {why}.",
            l.body.trim(),
            now.format("%Y-%m-%d")
        )
        .trim()
        .to_string();
        refresh_usage(&mut l, &stats);
        lines.push(format!("- {id} retired: {why}"));
        retired_ids.push(id.clone());
        changes.push((l, lib.base_get(&id).map(|b| b.path.clone())));
    }

    if changes.is_empty() {
        return Ok(Applied::default());
    }
    let what = match source {
        Some(s) => format!("learnings from {}", s.id),
        None => "retire unused learnings".to_string(),
    };
    let k = knowledge::get_or_create(repo, actor, &what)?;
    write_all(repo, &k, &changes)?;
    let subject = match source {
        Some(s) => format!("docs(library): learnings from {}", s.id),
        None => "docs(library): propose retiring learnings".into(),
    };
    let message = format!("{subject}\n\n{}", lines.join("\n"));
    let commit = knowledge::commit(repo, &k, actor, &message)?;
    let learnings: Vec<String> = changes
        .iter()
        .filter(|(l, _)| !retired_ids.contains(&l.id))
        .map(|(l, _)| l.id.clone())
        .collect();
    let summary = format!(
        "Library: {} learning(s) added or updated, {} proposed for retirement. Review them with `hall library review`.",
        learnings.len(),
        retired_ids.len()
    );
    knowledge::propose_ready(repo, &k, actor, &summary)?;
    note_retired(repo, &retired_ids, k.item.id, now)?;
    Ok(Applied {
        knowledge: Some(k.item.id),
        learnings,
        retired: retired_ids,
        commit,
    })
}

fn write_all(repo: &Repo, k: &Knowledge, changes: &[(Learning, Option<String>)]) -> Result<()> {
    for (l, base_path) in changes {
        knowledge::write(repo, k, l, base_path.as_deref())?;
    }
    Ok(())
}

/// How often a tick with nothing to distil checks for retirements.
const RETIRE_CHECK_EVERY: i64 = 24 * 3600;

#[derive(Debug, Default, Serialize, Deserialize)]
struct TickState {
    #[serde(default)]
    last_retire_check: Option<DateTime<Utc>>,
}

fn tick_state_path(repo: &Repo) -> std::path::PathBuf {
    repo.hall.root.join("library").join("state.json")
}

/// The next item to distil: eligible, not yet distilled, not held, oldest
/// first.
pub fn next_candidate(repo: &Repo) -> Result<Option<Item>> {
    let mut items: Vec<Item> = repo
        .index()?
        .items(true)?
        .into_iter()
        .filter(|i| {
            matches!(i.state, State::Closed | State::Escalated | State::Dropped)
                && !i.title.starts_with(KNOWLEDGE_PREFIX)
        })
        .collect();
    items.sort_by_key(|i| (i.state_since, i.id));
    for i in items {
        let events = repo.hall.read_item(i.id)?;
        let item = crate::event::fold(i.id, &events)?;
        if item.hold.is_none() && eligible(&item, &events) && already_distilled(&events).is_none() {
            return Ok(Some(item));
        }
    }
    Ok(None)
}

/// The daemon's librarian: distil one item, else (at most daily) propose
/// retirements.
pub fn tick(ctx: &WorkerCtx) -> Result<Progress> {
    let repo = ctx.repo;
    if let Some(item) = next_candidate(repo)? {
        let ttl = Duration::seconds(repo.config.budget.timeout_secs(Role::Librarian) + 300);
        if crate::lease::claim(&repo.hall, item.id, "librarian", &ctx.holder, ttl).is_err() {
            return Ok(Progress::Idle);
        }
        let r = distill(repo, ctx.runner, item.id);
        let _ = crate::lease::release(&repo.hall, item.id, &ctx.holder);
        return Ok(match r? {
            Distill::Done { applied, .. } => Progress::worked(match applied.knowledge {
                Some(k) => format!(
                    "distilled {} into {k}: {} learning(s), {} retirement(s)",
                    item.id,
                    applied.learnings.len(),
                    applied.retired.len()
                ),
                None => format!("distilled {}: nothing worth keeping", item.id),
            }),
            Distill::Stopped { progress, .. } => progress,
        });
    }
    let now = Utc::now();
    let path = tick_state_path(repo);
    let mut st: TickState = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if st
        .last_retire_check
        .is_some_and(|t| (now - t).num_seconds() < RETIRE_CHECK_EVERY)
    {
        return Ok(Progress::Idle);
    }
    let applied = knowledge::with_lock(repo, || {
        apply(repo, None, &Proposals::default(), ACTOR, now)
    })?;
    st.last_retire_check = Some(now);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&path, serde_json::to_string(&st)?)?;
    Ok(match applied.knowledge {
        Some(k) => Progress::worked(format!(
            "proposed retiring {} in {k}",
            applied.retired.join(", ")
        )),
        None => Progress::Idle,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::VerdictKind;
    use crate::findings::{
        Counts, Dispute, Finding, LearningUse, Lens, PriorRuling, PriorStatus, Severity,
    };

    fn ev(body: Body) -> Event {
        Event::new("you", Some(ItemId(42)), body)
    }

    fn journal() -> Vec<Event> {
        let finding = |id: &str, sev: Severity, title: &str| Finding {
            id: id.into(),
            round: 1,
            lens: Lens::Correctness,
            severity: sev,
            file: "apps/api/src/webhooks/retry.rs".into(),
            line: 42,
            title: title.into(),
            scenario: "A deleted endpoint keeps its deliveries.".into(),
            fix: "Check first.".into(),
        };
        let verdict = |round, findings, prior, escalated, learnings| Body::Verdict {
            sha: "abc".into(),
            round,
            verdict: VerdictKind::Changes,
            escalated,
            findings,
            prior,
            counts: Counts::default(),
            summary: Some(format!("round {round}")),
            report: None,
            reviewer: None,
            learnings,
            base: None,
            since: None,
        };
        vec![
            ev(Body::Created {
                title: "Retry webhooks".into(),
                branch: "hm/H-0042-retry".into(),
                worktree: "/tmp/x".into(),
                base: "main".into(),
                issue: None,
                request: Some("Retry failed webhook deliveries".into()),
                adopted: false,
                parent: None,
                follow_up: None,
            }),
            ev(Body::Ready {
                sha: "abc".into(),
                round: 1,
                note: None,
            }),
            ev(Body::Checks {
                sha: "abc".into(),
                round: 1,
                passed: true,
                runs: vec![],
            }),
            ev(verdict(
                1,
                vec![
                    finding("F1", Severity::Major, "Retry ignores deleted endpoints"),
                    finding("F2", Severity::Minor, "Backoff has no jitter"),
                ],
                vec![],
                false,
                vec![LearningUse {
                    id: "L-0003".into(),
                    held: false,
                    note: Some("jitter now lives in the queue".into()),
                }],
            )),
            ev(Body::FixSubmitted {
                sha: "def".into(),
                fixes: vec!["F1".into()],
                disputes: vec![Dispute {
                    id: "F2".into(),
                    reason: "the queue adds jitter".into(),
                }],
            }),
            ev(Body::Checks {
                sha: "def".into(),
                round: 2,
                passed: true,
                runs: vec![],
            }),
            ev(verdict(
                2,
                vec![],
                vec![
                    PriorRuling {
                        id: "F1".into(),
                        status: PriorStatus::Open,
                        note: None,
                    },
                    PriorRuling {
                        id: "F2".into(),
                        status: PriorStatus::Withdrawn,
                        note: Some("dispute accepted".into()),
                    },
                ],
                true,
                vec![],
            )),
            ev(Body::Retried {
                reason: "deleted endpoints are rare; ship with a follow-up".into(),
            }),
        ]
    }

    #[test]
    fn the_digest_tells_the_story() {
        let events = journal();
        let item = crate::event::fold(ItemId(42), &events).unwrap();
        let d = digest(&item, &events, &["apps/api/src/webhooks/retry.rs".into()]);
        for want in [
            "# H-0042 · Retry webhooks",
            "## Request\n\nRetry failed webhook deliveries",
            "- apps/api/src/webhooks/retry.rs",
            "Round 2: changes requested, escalated to you.",
            "- F1 · major · correctness · `apps/api/src/webhooks/retry.rs:42` (round 1)",
            "The fixer disputed it: the queue adds jitter",
            "withdrawn by the reviewer in round 2 (dispute accepted)",
            "Outcome: still open",
            "Allowed another round: deleted endpoints are rare",
            "- L-0003 did NOT hold: jitter now lives in the queue",
        ] {
            assert!(d.contains(want), "missing {want:?} in\n{d}");
        }
    }

    #[test]
    fn distilled_once_per_arrival() {
        let mut events = journal();
        assert!(already_distilled(&events).is_none());
        events.push(ev(Body::Distilled {
            into: Some(ItemId(50)),
            learnings: vec!["L-0001".into()],
        }));
        assert!(already_distilled(&events).is_some());
        // Moving again (here: a pull-back) makes it due once more.
        events.push(ev(Body::PulledBack { reason: None }));
        assert!(already_distilled(&events).is_none());
    }

    #[test]
    fn who_is_eligible() {
        let events = journal();
        let mut item = crate::event::fold(ItemId(42), &events).unwrap();
        assert_eq!(item.state, State::Fixing, "retried after escalation");
        assert!(!eligible(&item, &events));
        item.state = State::Dropped;
        assert!(eligible(&item, &events), "dropped after an escalation");
        item.state = State::Closed;
        assert!(eligible(&item, &events));
        item.title = "Library: learnings from H-0001".into();
        assert!(
            !eligible(&item, &events),
            "knowledge items are not distilled"
        );
    }

    #[test]
    fn titles_compare_loosely() {
        assert_eq!(
            norm_title("Retry: check the endpoint!"),
            norm_title("retry   check the ENDPOINT")
        );
    }
}
