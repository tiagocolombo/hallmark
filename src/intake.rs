//! Planning a request (ADR 0024, 0068): the Product Manager writes its
//! story (a title, acceptance criteria, a priority), then the Lead decides,
//! against the board of open items, how it gets built: a new item (split
//! into parallel parts or not), folded into an open item (`merge`), a
//! follow-up to one, or a duplicate of one. Both are short read-only runs
//! with schema-checked answers, booked to the Front Desk (`intake`).
//!
//! Planning is an optimisation, never a requirement, so it **fails open**: a
//! missing `claude`, a blocked budget, a failed run, or a decision Hallmark's
//! checks refuse all mean "one new item", with the reason in the reply. A
//! failed Product Manager run only means the item has no criteria.

use std::path::Path;

use globset::{GlobBuilder, GlobMatcher};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::board::Card;
use crate::config::Role;
use crate::item::{ItemId, State};
use crate::output::clip;
use crate::repo::Repo;
use crate::runner::{self, Failure, Metered};

/// One independent part of a split request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Part {
    pub title: String,
    /// A self-contained request for whoever builds this part.
    pub request: String,
    /// Repository-relative files, directories or globs it touches.
    #[serde(default)]
    pub paths: Vec<String>,
}

/// What the Lead returned (`plugin/schemas/plan.json`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawPlan {
    #[serde(default)]
    pub decision: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub parallel: bool,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub parts: Vec<Part>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub estimate_lines: u64,
}

/// What the Product Manager returned (`plugin/schemas/story.json`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawStory {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub criteria: Vec<String>,
    #[serde(default)]
    pub priority: String,
    #[serde(default)]
    pub duplicate_of: String,
    #[serde(default)]
    pub reason: String,
}

/// The Product Manager's story, after Hallmark cleaned it up.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Story {
    pub title: String,
    /// The acceptance criteria: what done means.
    pub criteria: Vec<String>,
    /// `now`, `next` or `later`.
    pub priority: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// An open item the Product Manager thinks covers it already (a hint
    /// for the Lead).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<ItemId>,
}

impl Story {
    /// Trims, drops empty criteria, caps them at 7 and the title at 72
    /// characters; an unknown priority is `next`.
    pub fn from_raw(raw: RawStory) -> Story {
        let criteria: Vec<String> = raw
            .criteria
            .iter()
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .take(7)
            .collect();
        let priority = match raw.priority.trim() {
            p @ ("now" | "next" | "later") => p.to_string(),
            _ => "next".to_string(),
        };
        Story {
            title: clip(raw.title.trim(), 72),
            criteria,
            priority,
            reason: raw.reason.trim().to_string(),
            duplicate_of: raw.duplicate_of.trim().parse().ok(),
        }
    }
}

/// How the request is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// A new item, split into parallel parts or not.
    New,
    /// Added to an open item's scope.
    Merge,
    /// A new item that builds on an open item.
    FollowUp,
    /// Already covered by an item; nothing is created.
    Duplicate,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::New => "new",
            Decision::Merge => "merge",
            Decision::FollowUp => "follow_up",
            Decision::Duplicate => "duplicate",
        }
    }
}

/// The plan, after Hallmark checked it.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Plan {
    /// True when the Lead ran (even a run that failed).
    pub ran: bool,
    pub decision: Decision,
    /// The item a merge, follow-up or duplicate names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<ItemId>,
    /// True when the request is built as parallel parts.
    pub parallel: bool,
    /// Why this decision, or why it fell back to one new item.
    pub reason: String,
    /// The parts, when parallel.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<Part>,
    /// The Product Manager's story, when that run succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub story: Option<Story>,
}

impl Plan {
    pub fn single(ran: bool, reason: impl Into<String>) -> Plan {
        Plan {
            ran,
            decision: Decision::New,
            target: None,
            parallel: false,
            reason: reason.into(),
            parts: Vec::new(),
            story: None,
        }
    }

    /// The story's acceptance criteria, or none.
    pub fn criteria(&self) -> Vec<String> {
        self.story
            .as_ref()
            .map(|s| s.criteria.clone())
            .unwrap_or_default()
    }
}

/// A plan and the runs it came from: each run's stream-json and end line,
/// kept as `runs/<stamp>-<role>.jsonl` once an item holds the plan (the runs
/// had no item to stream into, ADR 0041).
#[derive(Debug, Clone)]
pub struct Outcome {
    pub plan: Plan,
    /// (`pm` or `intake`, the run's text), in the order they ran.
    pub raw: Vec<(String, String)>,
}

/// What the caller asks of the plan.
#[derive(Debug, Clone, Copy, Default)]
pub struct Opts {
    /// Only a new item: no board, no merge, follow-up or duplicate.
    pub no_merge: bool,
}

/// True when a model run can start at all: the fake runner, or a `claude`
/// binary that exists. Without one, `hall new` doesn't wait for a run that
/// can only fail.
pub fn runner_available() -> bool {
    if std::env::var("HALL_RUNNER").is_ok_and(|v| v.starts_with("fake:")) {
        return true;
    }
    let bin = crate::config::Tool::Claude.command();
    let p = Path::new(&bin);
    if p.is_absolute() || p.components().count() > 1 {
        p.is_file()
    } else {
        crate::gh::which(&bin).is_some()
    }
}

/// What one planning run gave.
enum Ran {
    Answered(serde_json::Value),
    /// It ran and failed, or answered nothing usable: the reason.
    Failed(String),
    /// It hit Claude's usage limit, and the loop is paused: the reason. The
    /// next run would hit it too.
    Limited(String),
    /// It never started (the budget, no runner): the reason. The next run
    /// wouldn't start either.
    Skipped(String),
}

/// One planning run: agent `agent`, run role `role` (the fake runner's key
/// and the usage row's role; both bill the `intake` budget), on `model`,
/// answering through `schema`.
fn run(
    repo: &Repo,
    role: &str,
    agent: &str,
    schema: &str,
    model: &str,
    prompt: String,
    raw: &mut Vec<(String, String)>,
) -> Ran {
    let who = if role == "pm" {
        "the Product Manager"
    } else {
        "the Lead"
    };
    let mut req = match runner::request(repo, Role::Intake, Some(agent), prompt, &repo.main_root) {
        Ok(r) => r,
        Err(e) => return Ran::Skipped(format!("{who} could not start: {}", e.message)),
    };
    req.role = role.to_string();
    req.model = Some(model.to_string());
    req.json_schema = Some(crate::plugin::file(schema).into());
    req.allowed_tools = ["Read", "Grep", "Glob", "StructuredOutput"]
        .map(String::from)
        .to_vec();
    let runner = runner::from_env();
    match runner::run_metered(repo, runner.as_ref(), None, &req) {
        Ok(Metered::Done(o)) => {
            raw.push((role.to_string(), o.stream_text()));
            match o.structured() {
                Some(v) => Ran::Answered(v.clone()),
                None => Ran::Failed(format!("{who} returned no answer")),
            }
        }
        Ok(Metered::Failed {
            outcome, failure, ..
        }) => {
            raw.push((role.to_string(), outcome.stream_text()));
            let mut reason = format!("{who}'s run failed: {}", failure.describe());
            let limited = matches!(failure, Failure::RateLimited { .. });
            if let Failure::RateLimited { until } = &failure {
                // Pause the loop like any other limit hit, so the inbox and
                // notifications say so; the plan isn't retried by itself.
                let now = chrono::Utc::now();
                let began = crate::daemon::state::update(|s| {
                    crate::daemon::pause::begin(
                        s,
                        &repo.name,
                        "Claude's usage limit was hit (Front Desk)",
                        *until,
                        std::slice::from_ref(&repo.hall),
                        now,
                    )
                });
                if matches!(began, Ok(Ok(_))) {
                    reason.push_str(
                        "; the request stays one item: to plan it again, `hall drop` it and \
                         run `hall new` again after the reset",
                    );
                }
            }
            if limited {
                Ran::Limited(reason)
            } else {
                Ran::Failed(reason)
            }
        }
        Ok(Metered::Blocked(stop)) => Ran::Skipped(format!("{who} did not run: {}", stop.reason)),
        Err(e) => Ran::Skipped(format!("{who} could not run: {}", e.message)),
    }
}

/// Plans a request: the Product Manager's story, then the Lead's decision.
/// Never fails: anything that goes wrong means one new item, and the reason
/// says why.
pub fn plan(repo: &Repo, request: &str, opts: Opts) -> Outcome {
    let mut raw = Vec::new();
    if !runner_available() {
        return Outcome {
            plan: Plan::single(false, "the Front Desk did not run: claude was not found"),
            raw,
        };
    }
    let open = crate::board::open(repo);
    let has_open = !open.is_empty();
    let board = if opts.no_merge { Vec::new() } else { open };
    // The fast path: nothing else is open, so there is nothing to plan
    // against. A `[budget.intake] model` the repo set still wins.
    let b = &repo.config.budget;
    let model = if has_open {
        b.model(Role::Intake)
    } else {
        b.intake
            .as_ref()
            .and_then(|t| t.model.clone())
            .unwrap_or_else(|| b.intake_fast_model.clone())
    };
    let board_text = if opts.no_merge && has_open {
        "The board is not shown: the person asked for a new item.\n".to_string()
    } else {
        crate::board::render(&board)
    };

    let story = match run(
        repo,
        "pm",
        "pm",
        "schemas/story.json",
        &model,
        pm_prompt(request, &board_text),
        &mut raw,
    ) {
        Ran::Answered(v) => serde_json::from_value::<RawStory>(v)
            .ok()
            .map(Story::from_raw),
        // Without a story the item has no criteria; the Lead still plans.
        Ran::Failed(_) => None,
        // The Lead would stop the same way.
        Ran::Limited(reason) => {
            return Outcome {
                plan: Plan::single(true, reason),
                raw,
            };
        }
        Ran::Skipped(reason) => {
            return Outcome {
                plan: Plan::single(false, reason),
                raw,
            };
        }
    };

    let max = max_parts(repo);
    let max_lines = repo.config.intake.max_item_lines as u64;
    let mut prompt = prompt(request, &repo_map(repo), max);
    prompt.push_str(&lead_context(story.as_ref(), &board_text, max_lines, opts));
    // Extensions turned on for the planner (ADR 0038).
    if let Some(x) =
        crate::extensions::prompt(&repo.config.extensions, crate::extensions::Role::Planner)
    {
        prompt.push_str(&format!("\n{x}"));
    }
    let mut plan = match run(
        repo,
        "intake",
        "intake",
        "schemas/plan.json",
        &model,
        prompt,
        &mut raw,
    ) {
        Ran::Answered(v) => match serde_json::from_value::<RawPlan>(v) {
            Ok(r) => {
                let known = |id: ItemId| repo.hall.item_events(id).is_file();
                let checks = Checks {
                    board: &board,
                    max_lines,
                    security: &repo.config.security_review,
                    no_merge: opts.no_merge,
                    exists: &known,
                };
                decide(r, max, &checks)
            }
            Err(e) => Plan::single(true, format!("the plan was unreadable ({e})")),
        },
        Ran::Failed(reason) | Ran::Limited(reason) => Plan::single(true, reason),
        Ran::Skipped(reason) => Plan::single(false, reason),
    };
    plan.story = story;
    Outcome { plan, raw }
}

fn max_parts(repo: &Repo) -> usize {
    repo.config.intake.max_parts.max(2) as usize
}

/// The Product Manager's prompt: the request and the board.
pub fn pm_prompt(request: &str, board: &str) -> String {
    format!(
        "Write the story for this request, for Hallmark's Product Manager.\n\n\
         <request>\n{}\n</request>\n\n\
         The items already open, for duplicates and context:\n\n\
         <board>\n{board}</board>\n",
        request.trim()
    )
}

/// The Lead's prompt: the request, the limits, and a compact map of the
/// repo; [`lead_context`] adds the story and the board.
pub fn prompt(request: &str, map: &str, max_parts: usize) -> String {
    format!(
        "Plan this request for Hallmark's Front Desk.\n\n\
         <request>\n{}\n</request>\n\n\
         Split it into 2 to {max_parts} parts only if the parts are truly independent: \
         disjoint files, no ordering between them, each buildable and testable alone. \
         Otherwise answer parallel: false with the reason. Paths are relative to the \
         repository root, which is your working directory.\n\n\
         <repository>\n{map}\n</repository>\n",
        request.trim()
    )
}

/// The story and the board, for the Lead's decision.
pub fn lead_context(story: Option<&Story>, board: &str, max_lines: u64, opts: Opts) -> String {
    let mut s = String::new();
    if let Some(st) = story {
        s.push_str(&format!(
            "\nThe Product Manager's story:\n\n<story>\ntitle: {}\npriority: {}\n",
            st.title, st.priority
        ));
        for c in &st.criteria {
            s.push_str(&format!("criterion: {c}\n"));
        }
        if let Some(d) = st.duplicate_of {
            s.push_str(&format!("duplicate of: {d} (a hint)\n"));
        }
        s.push_str("</story>\n");
    }
    if opts.no_merge {
        s.push_str("\nThe person asked for a new item: answer decision \"new\".\n");
        return s;
    }
    s.push_str(&format!(
        "\nThe open items:\n\n<board>\n{board}</board>\n\n\
         Decide new, merge, follow_up or duplicate. A merge needs the target in development \
         or awaiting_you, and its changed lines plus your estimate under {max_lines} lines.\n"
    ));
    s
}

/// What [`decide`] checks a decision against.
pub struct Checks<'a> {
    pub board: &'a [Card],
    pub max_lines: u64,
    /// `security_review` globs.
    pub security: &'a [String],
    pub no_merge: bool,
    /// Whether an item exists at all (a duplicate may name a closed one).
    pub exists: &'a dyn Fn(ItemId) -> bool,
}

/// Checks the Lead's decision (ADR 0068): Hallmark checks the plan, not the
/// model. A merge, follow-up or duplicate that doesn't hold up becomes one
/// new item with the reason; a new item's split goes through [`validate`].
pub fn decide(raw: RawPlan, max_parts: usize, c: &Checks) -> Plan {
    let fallback = |why: String| Plan::single(true, format!("{why}; planned as one new item"));
    let reason = raw.reason.trim().to_string();
    let decision = match raw.decision.trim() {
        "" | "new" => Decision::New,
        "merge" => Decision::Merge,
        "follow_up" => Decision::FollowUp,
        "duplicate" => Decision::Duplicate,
        other => return fallback(format!("the Lead's decision \"{other}\" is unknown")),
    };
    if decision == Decision::New {
        return validate(raw, max_parts);
    }
    if c.no_merge {
        return fallback(format!(
            "the Lead chose {} but a new item was asked for",
            decision.as_str()
        ));
    }
    let Ok(target) = raw.target.trim().parse::<ItemId>() else {
        return fallback(format!(
            "the Lead chose {} without a usable target (\"{}\")",
            decision.as_str(),
            raw.target.trim()
        ));
    };
    let card = c.board.iter().find(|k| k.id == target);
    match decision {
        Decision::Duplicate => {
            if card.is_none() && !(c.exists)(target) {
                return fallback(format!("the duplicate's target {target} doesn't exist"));
            }
        }
        Decision::FollowUp => {
            if card.is_none() {
                return fallback(format!("the follow-up's target {target} is not open"));
            }
        }
        Decision::Merge => {
            let Some(card) = card else {
                return fallback(format!("the merge's target {target} is not open"));
            };
            if let Err(why) = mergeable(card, &raw, c) {
                return fallback(format!("{target} can't take the request: {why}"));
            }
        }
        Decision::New => unreachable!(),
    }
    Plan {
        ran: true,
        decision,
        target: Some(target),
        parallel: false,
        reason: if reason.is_empty() {
            format!("{} {target}", decision.as_str())
        } else {
            reason
        },
        parts: Vec::new(),
        story: None,
    }
}

/// Whether `card` may take the request as added scope.
fn mergeable(card: &Card, raw: &RawPlan, c: &Checks) -> Result<(), String> {
    if !matches!(card.state, State::Development | State::AwaitingYou) {
        return Err(format!(
            "it is in {}, past development; a follow-up fits instead",
            card.state.as_str()
        ));
    }
    if card.parent.is_some() {
        return Err("it is a parallel part, which merges into its parent".into());
    }
    if ["Records: ", "Library: "]
        .iter()
        .any(|p| card.title.starts_with(p))
    {
        return Err("it is a records item".into());
    }
    let total = card.lines + raw.estimate_lines;
    if total > c.max_lines {
        return Err(format!(
            "its {} changed lines plus an estimated {} would pass intake.max_item_lines ({})",
            card.lines, raw.estimate_lines, c.max_lines
        ));
    }
    if !c.security.is_empty() {
        let set = crate::assayer::base::globset(c.security);
        let item_sec = card.changed.iter().any(|x| set.is_match(&x.path));
        let paths: Vec<String> = raw.paths.iter().filter_map(|p| clean_path(p)).collect();
        // Unknown paths can't be shown to stay on one side.
        // A directory or a broad glob is security when anything under it
        // can match a security glob.
        let req_sec = paths.is_empty()
            || paths
                .iter()
                .any(|p| set.is_match(p) || c.security.iter().any(|g| overlap(g, p)));
        if item_sec != req_sec {
            return Err("it would mix a security_review path with paths outside them".into());
        }
    }
    Ok(())
}

/// Top-level entries with file counts, the first tracked files, and the
/// headings of the base branch's CLAUDE.md / AGENTS.md.
pub fn repo_map(repo: &Repo) -> String {
    let root = &repo.main_root;
    let files: Vec<String> = crate::git::run(root, &["ls-files"])
        .map(|s| s.lines().map(String::from).collect())
        .unwrap_or_default();
    let mut s = String::new();
    let mut top: Vec<(String, usize)> = Vec::new();
    for f in &files {
        let (name, dir) = match f.split_once('/') {
            Some((d, _)) => (format!("{d}/"), true),
            None => (f.clone(), false),
        };
        match top.iter_mut().find(|(n, _)| *n == name) {
            Some((_, c)) => *c += 1,
            None => top.push((name, usize::from(dir))),
        }
    }
    s.push_str("Top level:\n");
    for (name, count) in top.iter().take(60) {
        if name.ends_with('/') {
            s.push_str(&format!("  {name} ({count} files)\n"));
        } else {
            s.push_str(&format!("  {name}\n"));
        }
    }
    const LIST: usize = 150;
    s.push_str(&format!(
        "\nTracked files ({} in all{}):\n",
        files.len(),
        if files.len() > LIST {
            format!(", first {LIST}")
        } else {
            String::new()
        }
    ));
    for f in files.iter().take(LIST) {
        s.push_str(&format!("  {}\n", clip(f, 120)));
    }
    for doc in ["CLAUDE.md", "AGENTS.md"] {
        let text = crate::git::run(root, &["show", &format!("{}:{doc}", repo.config.base)])
            .ok()
            .or_else(|| std::fs::read_to_string(root.join(doc)).ok());
        let Some(text) = text else { continue };
        let heads: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with('#'))
            .take(40)
            .collect();
        if !heads.is_empty() {
            s.push_str(&format!("\n{doc} headings:\n"));
            for h in heads {
                s.push_str(&format!("  {}\n", clip(h, 100)));
            }
        }
    }
    clip(&s, 12_000)
}

/// Checks a plan: 2 to `max` parts, each with a title, a request and paths,
/// and no path shared between parts. Anything else is one item.
pub fn validate(raw: RawPlan, max: usize) -> Plan {
    let reason = raw.reason.trim().to_string();
    if !raw.parallel {
        return Plan::single(
            true,
            if reason.is_empty() {
                "the Front Desk kept it as one item".into()
            } else {
                reason
            },
        );
    }
    let reject = |why: String| Plan::single(true, format!("{why}; kept as one item"));
    if raw.parts.len() < 2 {
        return reject("the plan had fewer than 2 parts".into());
    }
    if raw.parts.len() > max {
        return reject(format!(
            "the plan had {} parts, more than intake.max_parts ({max})",
            raw.parts.len()
        ));
    }
    let mut parts = Vec::new();
    for p in raw.parts {
        let title = p.title.trim().to_string();
        let request = p.request.trim().to_string();
        if title.is_empty() || request.is_empty() {
            return reject("a part had no title or request".into());
        }
        if p.paths.is_empty() {
            return reject(format!("part \"{title}\" listed no paths"));
        }
        let mut paths = Vec::new();
        for path in &p.paths {
            match clean_path(path) {
                Some(c) => paths.push(c),
                None => return reject(format!("part \"{title}\" has an unusable path \"{path}\"")),
            }
        }
        parts.push(Part {
            title,
            request,
            paths,
        });
    }
    for (i, a) in parts.iter().enumerate() {
        for b in &parts[i + 1..] {
            for pa in &a.paths {
                for pb in &b.paths {
                    if overlap(pa, pb) {
                        return reject(format!(
                            "parts \"{}\" and \"{}\" overlap ({pa} and {pb})",
                            a.title, b.title
                        ));
                    }
                }
            }
        }
    }
    Plan {
        ran: true,
        decision: Decision::New,
        target: None,
        parallel: true,
        reason: if reason.is_empty() {
            "the parts touch disjoint paths".into()
        } else {
            reason
        },
        parts,
        story: None,
    }
}

/// `./src/api/` → `src/api`. Absolute paths and `..` are refused; `.` or
/// an empty path means the whole repository (it overlaps everything).
fn clean_path(p: &str) -> Option<String> {
    let mut s = p.trim().replace('\\', "/");
    if s.starts_with('/') || s.get(1..2) == Some(":") {
        return None;
    }
    while let Some(rest) = s.strip_prefix("./") {
        s = rest.to_string();
    }
    let s = s.trim_end_matches('/').to_string();
    if s.split('/').any(|c| c == "..") {
        return None;
    }
    Some(if s == "." { String::new() } else { s })
}

fn is_glob(component: &str) -> bool {
    component.contains(['*', '?', '[', '{'])
}

fn has_glob(p: &str) -> bool {
    p.split('/').any(is_glob)
}

/// The components before the first one with a glob character.
fn literal(p: &str) -> Vec<&str> {
    p.split('/')
        .filter(|c| !c.is_empty())
        .take_while(|c| !is_glob(c))
        .collect()
}

fn matcher(p: &str) -> Option<GlobMatcher> {
    GlobBuilder::new(p)
        .literal_separator(true)
        .build()
        .ok()
        .map(|g| g.compile_matcher())
}

/// Could two path patterns name a common file? Plain paths overlap when one
/// contains the other. A glob is compared through its literal prefix,
/// conservatively: when unsure, they overlap.
pub fn overlap(a: &str, b: &str) -> bool {
    let (la, lb) = (literal(a), literal(b));
    let nested = la.starts_with(&lb) || lb.starts_with(&la);
    if !nested {
        return false;
    }
    match (has_glob(a), has_glob(b)) {
        (false, false) => true,
        (true, true) => true,
        (true, false) => glob_reaches(a, b),
        (false, true) => glob_reaches(b, a),
    }
}

/// Could glob `g` match `plain` or a file under it?
fn glob_reaches(g: &str, plain: &str) -> bool {
    if g.contains("**") {
        return true;
    }
    let Some(m) = matcher(g) else { return true };
    // A glob without `**` matches at a fixed depth: deeper than `plain`
    // means it may match inside it.
    m.is_match(plain) || g.split('/').count() > plain.split('/').count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(title: &str, paths: &[&str]) -> Part {
        Part {
            title: title.into(),
            request: format!("do {title}"),
            paths: paths.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn raw(parts: Vec<Part>) -> RawPlan {
        RawPlan {
            parallel: true,
            reason: "independent".into(),
            parts,
            ..RawPlan::default()
        }
    }

    fn card(id: &str, state: State, lines: u64, paths: &[&str]) -> Card {
        Card {
            id: id.parse().unwrap(),
            title: format!("work {id}"),
            state,
            parent: None,
            request: Some("something".into()),
            criteria: vec![],
            summary: None,
            pr_url: None,
            changed: paths
                .iter()
                .map(|p| crate::board::Changed {
                    path: p.to_string(),
                    lines: lines / paths.len().max(1) as u64,
                })
                .collect(),
            lines,
        }
    }

    fn lead(decision: &str, target: &str, estimate: u64, paths: &[&str]) -> RawPlan {
        RawPlan {
            decision: decision.into(),
            target: target.into(),
            reason: "same module".into(),
            paths: paths.iter().map(|s| s.to_string()).collect(),
            estimate_lines: estimate,
            ..RawPlan::default()
        }
    }

    fn checks<'a>(board: &'a [Card], security: &'a [String], no_merge: bool) -> Checks<'a> {
        Checks {
            board,
            max_lines: 400,
            security,
            no_merge,
            exists: &|id: ItemId| id.to_string() == "H-0001",
        }
    }

    #[test]
    fn a_merge_into_an_item_in_development_stands() {
        let board = [card("H-0042", State::Development, 120, &["src/hooks.rs"])];
        let p = decide(
            lead("merge", "H-0042", 60, &["src/hooks.rs"]),
            4,
            &checks(&board, &[], false),
        );
        assert_eq!(p.decision, Decision::Merge, "{p:?}");
        assert_eq!(p.target.map(|t| t.to_string()).as_deref(), Some("H-0042"));
        assert_eq!(p.reason, "same module");
        let waiting = [card("H-0042", State::AwaitingYou, 0, &[])];
        let p = decide(
            lead("merge", "H-0042", 10, &["a"]),
            4,
            &checks(&waiting, &[], false),
        );
        assert_eq!(p.decision, Decision::Merge, "{p:?}");
    }

    #[test]
    fn a_merge_that_breaks_a_rule_is_one_new_item() {
        let new_item = |p: Plan, why: &str| {
            assert_eq!(p.decision, Decision::New, "{p:?}");
            assert!(p.target.is_none());
            assert!(p.reason.contains(why), "{}", p.reason);
            assert!(
                p.reason.ends_with("planned as one new item"),
                "{}",
                p.reason
            );
        };
        let none: Vec<String> = vec![];
        // Past development: its commit is frozen in review.
        let review = [card("H-0042", State::Review, 10, &["a"])];
        new_item(
            decide(
                lead("merge", "H-0042", 10, &["a"]),
                4,
                &checks(&review, &none, false),
            ),
            "past development",
        );
        // Too big together.
        let big = [card("H-0042", State::Development, 350, &["a"])];
        new_item(
            decide(
                lead("merge", "H-0042", 60, &["a"]),
                4,
                &checks(&big, &none, false),
            ),
            "intake.max_item_lines (400)",
        );
        // Exactly at the limit is still fine.
        let p = decide(
            lead("merge", "H-0042", 50, &["a"]),
            4,
            &checks(&big, &none, false),
        );
        assert_eq!(p.decision, Decision::Merge, "{p:?}");
        // Not on the board, or no usable target.
        new_item(
            decide(
                lead("merge", "H-0099", 1, &["a"]),
                4,
                &checks(&big, &none, false),
            ),
            "not open",
        );
        new_item(
            decide(
                lead("merge", "soon", 1, &["a"]),
                4,
                &checks(&big, &none, false),
            ),
            "usable target",
        );
        // A parallel part, or a records item.
        let mut part = card("H-0042", State::Development, 10, &["a"]);
        part.parent = Some("H-0041".parse().unwrap());
        new_item(
            decide(
                lead("merge", "H-0042", 1, &["a"]),
                4,
                &checks(&[part], &none, false),
            ),
            "parallel part",
        );
        let mut records = card("H-0042", State::Development, 10, &["a"]);
        records.title = "Records: H-0040".into();
        new_item(
            decide(
                lead("merge", "H-0042", 1, &["a"]),
                4,
                &checks(&[records], &none, false),
            ),
            "records item",
        );
        // Asked for a new item.
        let dev = [card("H-0042", State::Development, 10, &["a"])];
        new_item(
            decide(
                lead("merge", "H-0042", 1, &["a"]),
                4,
                &checks(&dev, &none, true),
            ),
            "a new item was asked for",
        );
        new_item(
            decide(
                lead("ship it", "H-0042", 1, &["a"]),
                4,
                &checks(&dev, &none, false),
            ),
            "unknown",
        );
    }

    #[test]
    fn a_merge_never_mixes_security_paths_with_others() {
        let sec = vec!["src/auth/**".to_string()];
        let auth = [card(
            "H-0042",
            State::Development,
            10,
            &["src/auth/token.rs"],
        )];
        let plain = [card("H-0042", State::Development, 10, &["docs/a.md"])];
        let mixes = |p: Plan| {
            assert_eq!(p.decision, Decision::New, "{p:?}");
            assert!(p.reason.contains("security_review"), "{}", p.reason);
        };
        mixes(decide(
            lead("merge", "H-0042", 5, &["docs/b.md"]),
            4,
            &checks(&auth, &sec, false),
        ));
        mixes(decide(
            lead("merge", "H-0042", 5, &["src/auth/x.rs"]),
            4,
            &checks(&plain, &sec, false),
        ));
        // Paths it can't name might land anywhere.
        mixes(decide(
            lead("merge", "H-0042", 5, &[]),
            4,
            &checks(&plain, &sec, false),
        ));
        // A directory or a broad glob over a security glob is security.
        mixes(decide(
            lead("merge", "H-0042", 5, &["src/auth/"]),
            4,
            &checks(&plain, &sec, false),
        ));
        mixes(decide(
            lead("merge", "H-0042", 5, &["src/**"]),
            4,
            &checks(&plain, &sec, false),
        ));
        let same = decide(
            lead("merge", "H-0042", 5, &["./src/auth/y.rs"]),
            4,
            &checks(&auth, &sec, false),
        );
        assert_eq!(same.decision, Decision::Merge, "{same:?}");
    }

    #[test]
    fn follow_ups_and_duplicates_need_a_real_target() {
        let none: Vec<String> = vec![];
        let board = [card("H-0042", State::Review, 10, &["a"])];
        let f = decide(
            lead("follow_up", "H-0042", 30, &["a"]),
            4,
            &checks(&board, &none, false),
        );
        assert_eq!(f.decision, Decision::FollowUp, "{f:?}");
        let f = decide(
            lead("follow_up", "H-0007", 30, &["a"]),
            4,
            &checks(&board, &none, false),
        );
        assert_eq!(f.decision, Decision::New, "{f:?}");
        // A duplicate may name an item that is no longer open.
        let d = decide(
            lead("duplicate", "H-0001", 0, &[]),
            4,
            &checks(&board, &none, false),
        );
        assert_eq!(d.decision, Decision::Duplicate, "{d:?}");
        let d = decide(
            lead("duplicate", "H-0042", 0, &[]),
            4,
            &checks(&board, &none, false),
        );
        assert_eq!(d.decision, Decision::Duplicate, "{d:?}");
        let d = decide(
            lead("duplicate", "H-0003", 0, &[]),
            4,
            &checks(&board, &none, false),
        );
        assert_eq!(d.decision, Decision::New, "{d:?}");
        assert!(d.reason.contains("doesn't exist"), "{}", d.reason);
    }

    #[test]
    fn a_new_decision_goes_through_the_split_checks() {
        let none: Vec<String> = vec![];
        let mut r = raw(vec![part("API", &["api/"]), part("Web", &["web/**"])]);
        r.decision = "new".into();
        let p = decide(r, 4, &checks(&[], &none, false));
        assert!(p.parallel && p.decision == Decision::New, "{p:?}");
        // An older Lead that gives no decision means new.
        let p = decide(
            raw(vec![part("API", &["api"])]),
            4,
            &checks(&[], &none, false),
        );
        assert!(!p.parallel && p.decision == Decision::New, "{p:?}");
    }

    #[test]
    fn stories_are_cleaned_up() {
        let s = Story::from_raw(RawStory {
            title: "  Retry webhooks ".into(),
            criteria: (0..9)
                .map(|i| {
                    if i == 1 {
                        "  ".into()
                    } else {
                        format!(" c{i} ")
                    }
                })
                .collect(),
            priority: "urgent".into(),
            duplicate_of: "H-0042".into(),
            reason: " normal work ".into(),
        });
        assert_eq!(s.title, "Retry webhooks");
        assert_eq!(s.criteria, vec!["c0", "c2", "c3", "c4", "c5", "c6", "c7"]);
        assert_eq!(s.priority, "next");
        assert_eq!(
            s.duplicate_of.map(|d| d.to_string()).as_deref(),
            Some("H-0042")
        );
        assert_eq!(s.reason, "normal work");
        let s = Story::from_raw(RawStory {
            priority: "now".into(),
            duplicate_of: "".into(),
            ..RawStory::default()
        });
        assert_eq!(s.priority, "now");
        assert!(s.duplicate_of.is_none());
    }

    #[test]
    fn the_lead_is_given_the_story_and_the_board() {
        let story = Story {
            title: "Retry webhooks".into(),
            criteria: vec!["stops after 6 attempts".into()],
            priority: "next".into(),
            reason: String::new(),
            duplicate_of: Some("H-0042".parse().unwrap()),
        };
        let s = lead_context(
            Some(&story),
            "- H-0042 [development]\n",
            400,
            Opts::default(),
        );
        assert!(s.contains("criterion: stops after 6 attempts"), "{s}");
        assert!(s.contains("duplicate of: H-0042 (a hint)"), "{s}");
        assert!(
            s.contains("<board>\n- H-0042 [development]\n</board>"),
            "{s}"
        );
        assert!(s.contains("under 400 lines"), "{s}");
        let s = lead_context(None, "- H-0042\n", 400, Opts { no_merge: true });
        assert!(s.contains("answer decision \"new\""), "{s}");
        assert!(!s.contains("<board>"), "{s}");
        let pm = pm_prompt(" retry webhooks ", "No open items.\n");
        assert!(pm.contains("<request>\nretry webhooks\n</request>"), "{pm}");
        assert!(pm.contains("<board>\nNo open items.\n</board>"), "{pm}");
    }

    #[test]
    fn overlapping_paths() {
        assert!(overlap("src/api", "src/api/users.rs"));
        assert!(overlap("src/api/users.rs", "src/api"));
        assert!(overlap("Cargo.toml", "Cargo.toml"));
        assert!(overlap("", "web"));
        assert!(overlap("**/*.rs", "web/x.rs"));
        assert!(overlap("src/**", "src/a.rs"));
        assert!(overlap("src/*/mod.rs", "src/web"));
        assert!(overlap("src/*.rs", "src/lib.rs"));
        assert!(!overlap("src/api", "src/web"));
        assert!(!overlap("src/a.rs", "src/ab.rs"));
        assert!(!overlap("src/*.rs", "src/web/x.ts"));
        assert!(!overlap("api/**", "web/**"));
        assert!(!overlap("docs/*.md", "src/**"));
    }

    #[test]
    fn cleans_paths() {
        assert_eq!(clean_path("./src/api/").as_deref(), Some("src/api"));
        assert_eq!(clean_path("web\\x.ts").as_deref(), Some("web/x.ts"));
        assert_eq!(clean_path(".").as_deref(), Some(""));
        assert_eq!(clean_path("/etc/passwd"), None);
        assert_eq!(clean_path("C:/x"), None);
        assert_eq!(clean_path("../other"), None);
    }

    #[test]
    fn a_good_plan_splits() {
        let p = validate(
            raw(vec![part("API", &["api/"]), part("Web", &["web/**"])]),
            4,
        );
        assert!(p.parallel, "{p:?}");
        assert_eq!(p.parts.len(), 2);
        assert_eq!(p.parts[0].paths, vec!["api"]);
    }

    #[test]
    fn bad_plans_stay_one_item() {
        let one = validate(raw(vec![part("API", &["api"])]), 4);
        assert!(!one.parallel && one.reason.contains("fewer than 2"));
        let many = validate(
            raw(vec![
                part("a", &["a"]),
                part("b", &["b"]),
                part("c", &["c"]),
            ]),
            2,
        );
        assert!(!many.parallel && many.reason.contains("max_parts"));
        let shared = validate(
            raw(vec![
                part("API", &["api", "Cargo.toml"]),
                part("Web", &["web", "Cargo.toml"]),
            ]),
            4,
        );
        assert!(
            !shared.parallel && shared.reason.contains("overlap"),
            "{shared:?}"
        );
        let pathless = validate(raw(vec![part("API", &["api"]), part("Web", &[])]), 4);
        assert!(!pathless.parallel && pathless.reason.contains("no paths"));
        let escape = validate(raw(vec![part("a", &["a"]), part("b", &["../b"])]), 4);
        assert!(!escape.parallel && escape.reason.contains("unusable"));
        let no = validate(
            RawPlan {
                parallel: false,
                reason: "one feature".into(),
                ..RawPlan::default()
            },
            4,
        );
        assert!(!no.parallel && no.ran);
        assert_eq!(no.reason, "one feature");
    }

    #[test]
    fn the_prompt_carries_the_request_and_limits() {
        let p = prompt("  fix api and web  ", "Top level:\n  api/ (3 files)\n", 3);
        assert!(p.contains("<request>\nfix api and web\n</request>"));
        assert!(p.contains("2 to 3 parts"));
        assert!(p.contains("api/ (3 files)"));
    }

    #[test]
    fn the_schema_is_embedded() {
        let v: serde_json::Value =
            serde_json::from_str(crate::plugin::file("schemas/plan.json")).unwrap();
        assert_eq!(
            v["required"],
            serde_json::json!([
                "decision",
                "target",
                "parallel",
                "parts",
                "paths",
                "estimate_lines",
                "reason"
            ])
        );
        let story: serde_json::Value =
            serde_json::from_str(crate::plugin::file("schemas/story.json")).unwrap();
        assert!(story.get("$schema").is_none());
        assert_eq!(
            story["required"],
            serde_json::json!(["title", "criteria", "priority", "duplicate_of", "reason"])
        );
        // Claude Code's --json-schema validator rejects a draft 2020-12
        // `$schema` key ("no schema with key or ref"), and the run fails.
        assert!(v.get("$schema").is_none());
        // An agent with a `tools:` list must allow StructuredOutput, or a
        // --json-schema run answers in prose and has no structured_output.
        for a in ["agents/intake.md", "agents/pm.md"] {
            let agent = crate::plugin::file(a);
            let tools = agent.lines().find(|l| l.starts_with("tools:")).unwrap();
            assert!(tools.contains("StructuredOutput"), "{a}: {tools}");
        }
    }
}
