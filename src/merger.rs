//! The merger (ADR 0048): the Delivery office's last step. It merges each
//! shipped PR once it is ready, one at a time per base, in dependency
//! order, so nobody has to press Merge.
//!
//! **A script decides**, never a model: the check has to be cheap, frequent,
//! stable and hard to game. A PR is ready when its head is the commit
//! Hallmark shipped and that commit carries a hallmark; GitHub says it is
//! mergeable; CI passed at that head (unless `require_ci = false`); nobody
//! asked for changes; nothing it depends on is still open; it touches no
//! `lit` path; and it is fresh: it contains its base, or the commits it
//! lacks touch none of its files.
//!
//! **Order**: a PR stacked on another waits for it; a PR whose body says
//! `Depends on #N` (or `Blocked by`, `After`) waits for #N; otherwise the
//! oldest PR (lowest number) goes first.
//!
//! **The queue owns the update** (ADR 0050). A PR that is behind its base on
//! files it touches, or that GitHub says conflicts, is reopened with a
//! rebase request only at its turn: once every earlier PR that touches the
//! same files has merged. Walking the queue in order, each PR still on its
//! way to a merge claims its files, and so does each item already sent back
//! to be brought up to date; a PR that touches a claimed file waits. So a PR
//! is rebased once, onto a base that already has everything it overlaps
//! with, and PRs that share no files move in parallel lanes. A PR waiting on
//! a person claims nothing: the ones behind it go first.
//!
//! **One merge at a time**: after a merge, nothing else on that base merges
//! until the watch records `delivered`, so each next PR is judged against
//! the base as it really is.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::MergeMode;
use crate::delivery::{self, Target};
use crate::error::Result;
use crate::event::{Body, Event, short};
use crate::gh::{GhComment, GhQueue};
use crate::git;
use crate::item::{HoldKind, Item, ItemId, State as ItemState};
use crate::lease;
use crate::repo::Repo;
use crate::worker::{Progress, WorkerCtx};

pub const ACTOR: &str = "merger";

/// The merge command's time limit.
const MERGE_TIMEOUT_SECS: u64 = 300;
/// How often the tick looks at the queue: each look reads PRs from GitHub.
const EVERY: i64 = 20;

/// Where a PR stands in the merge queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Next to merge: every check holds.
    Ready,
    /// Merged by the merger; waiting for GitHub to report it.
    Merging,
    /// Waits for another PR to merge first.
    WaitsFor,
    /// Behind its base on files it touches: it is rebased before it merges.
    Behind,
    /// CI hasn't passed at the shipped head yet.
    CiPending,
    /// CI failed at the shipped head; the Delivery office fixes it.
    CiRed,
    /// GitHub says it conflicts: it is rebased at its turn.
    Conflicting,
    /// GitHub is still working out whether it conflicts.
    Unknown,
    /// Touches a `lit` path: a person merges it.
    Lit,
    /// A reviewer asked for changes.
    ChangesRequested,
    /// Held for you (a failed merge, or any other hold).
    Held,
    /// Its head isn't the commit Hallmark shipped, it has no hallmark, or
    /// its worktree is missing or dirty.
    NotOurs,
    /// Another PR on its base merges first.
    Queued,
    /// GitHub couldn't be read.
    Unreadable,
    /// Reopened through its PR (a rebase, comments or a CI fix) and not
    /// shipped again yet.
    Updating,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ready => "ready",
            Status::Merging => "merging",
            Status::WaitsFor => "waits_for",
            Status::Behind => "behind",
            Status::CiPending => "ci_pending",
            Status::CiRed => "ci_red",
            Status::Conflicting => "conflicting",
            Status::Unknown => "unknown",
            Status::Lit => "lit",
            Status::ChangesRequested => "changes_requested",
            Status::Held => "held",
            Status::NotOurs => "not_ours",
            Status::Queued => "queued",
            Status::Unreadable => "unreadable",
            Status::Updating => "updating",
        }
    }
}

/// One PR in the merge queue.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Entry {
    /// Its place on its base, from 1: the order the merger takes them in.
    pub position: u32,
    pub item: ItemId,
    pub title: String,
    pub pr_url: String,
    pub base: String,
    pub status: Status,
    /// Why, in one line.
    pub why: String,
    /// The PRs it waits for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waits_for: Vec<String>,
}

/// What the merger last decided about each item, for `hall status`, and
/// the whole plan of its last pass, for the terminal app.
#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    last: Option<DateTime<Utc>>,
    #[serde(default)]
    items: BTreeMap<String, Seen>,
    /// Every PR the last pass planned, by base and in queue order.
    #[serde(default)]
    queue: Vec<Entry>,
    /// Bases whose tip has a failed check: the merger stopped merging into
    /// them (ADR 0051). Your office shows each one.
    #[serde(default)]
    red: BTreeMap<String, Red>,
}

/// A base the merger stopped merging into: its tip's CI failed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Red {
    /// The base's tip.
    pub sha: String,
    /// The checks that failed there.
    pub failing: Vec<String>,
    /// When the merger first saw it red.
    pub since: DateTime<Utc>,
}

/// The bases the merger stopped merging into, from `merger.json` in the
/// hallmark dir `root`: what your office shows.
pub fn red_bases(root: &Path) -> BTreeMap<String, Red> {
    std::fs::read_to_string(root.join("merger.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<State>(&s).ok())
        .map(|s| s.red)
        .unwrap_or_default()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Seen {
    pub status: Status,
    pub why: String,
    pub at: DateTime<Utc>,
}

fn state_path(repo: &Repo) -> PathBuf {
    repo.hall.root.join("merger.json")
}

fn load(repo: &Repo) -> State {
    load_from(&state_path(repo))
}

fn load_from(path: &Path) -> State {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// The merge queue as the merger's last pass planned it, and when: what
/// the terminal app shows. Reads one local file; never calls GitHub.
pub fn last_plan(hall: &crate::journal::Hall) -> (Option<DateTime<Utc>>, Vec<Entry>) {
    let s = load_from(&hall.root.join("merger.json"));
    (s.last, s.queue)
}

fn save(repo: &Repo, s: &State) {
    if let Ok(text) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(state_path(repo), text);
    }
}

/// What the merger last said about an item, if it looked at it.
pub fn last_seen(repo: &Repo, id: ItemId) -> Option<Seen> {
    load(repo).items.get(&id.to_string()).cloned()
}

/// The PR number in a PR URL.
pub fn number(url: &str) -> Option<u64> {
    url.rsplit_once("/pull/")?
        .1
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// The PRs a text says it depends on: `Depends on #12`, `Blocked by #12`,
/// `After #12`, or the same with a PR URL. Case doesn't matter; several
/// may follow one phrase, separated by commas or "and".
pub fn depends_on(text: &str) -> BTreeSet<u64> {
    let lower = text.to_lowercase();
    let mut out = BTreeSet::new();
    for phrase in ["depends on", "blocked by", "after"] {
        let mut from = 0;
        while let Some(i) = lower[from..].find(phrase) {
            let start = from + i;
            from = start + phrase.len();
            // A whole word: "thereafter" or "dafter" isn't "after".
            let before = lower[..start].chars().next_back();
            if before.is_some_and(|c| c.is_alphanumeric()) {
                continue;
            }
            let mut rest = lower[from..].trim_start();
            if let Some(r) = rest.strip_prefix(':') {
                rest = r.trim_start();
            }
            loop {
                let (n, len) = if let Some(r) = rest.strip_prefix('#') {
                    let digits: String = r.chars().take_while(|c| c.is_ascii_digit()).collect();
                    (digits.parse::<u64>().ok(), 1 + digits.len())
                } else if rest.starts_with("http") {
                    let word: &str = rest
                        .split(|c: char| c.is_whitespace() || c == ',' || c == ')')
                        .next()
                        .unwrap_or("");
                    (
                        word.contains("/pull/").then(|| number(word)).flatten(),
                        word.len(),
                    )
                } else {
                    (None, 0)
                };
                let Some(n) = n else { break };
                out.insert(n);
                rest = rest[len..].trim_start();
                if let Some(r) = rest.strip_prefix(',') {
                    rest = r.trim_start();
                } else if let Some(r) = rest.strip_prefix("and ") {
                    rest = r.trim_start();
                } else {
                    break;
                }
            }
        }
    }
    out
}

/// Whether a person (not a bot) asked for changes and hasn't approved since.
pub fn changes_requested(reviews: &[GhComment]) -> Option<String> {
    let mut latest: BTreeMap<String, &str> = BTreeMap::new();
    for r in reviews {
        let who = r
            .author
            .as_ref()
            .map(|a| a.login.clone())
            .unwrap_or_default();
        if who.ends_with("[bot]") || who == "github-actions" {
            continue;
        }
        if let Some(s @ ("APPROVED" | "CHANGES_REQUESTED" | "DISMISSED")) = r.state.as_deref() {
            latest.insert(who, s);
        }
    }
    latest
        .into_iter()
        .find(|(_, s)| *s == "CHANGES_REQUESTED")
        .map(|(who, _)| who)
}

/// How a PR's head relates to its base.
#[derive(Debug, Clone, PartialEq)]
pub enum Fresh {
    /// It contains the base, or what it lacks touches none of its files.
    Yes,
    /// The base moved on files it touches, too.
    Behind(Vec<String>),
}

/// The files `head` changes since it left `target`, and whether it is fresh.
pub fn freshness(wt: &Path, target: &str, head: &str) -> Result<(Fresh, Vec<String>)> {
    let mb = git::run(wt, &["merge-base", head, target])?;
    let files = |from: &str, to: &str| -> Result<Vec<String>> {
        // No rename detection and NUL-separated, so a file moved out of a
        // path still lists its old name and nothing is C-quoted.
        let out = git::run(wt, &["diff", "--name-only", "--no-renames", "-z", from, to])?;
        Ok(out
            .split('\0')
            .map(str::to_string)
            .filter(|l| !l.is_empty())
            .collect())
    };
    let mine = files(&mb, head)?;
    let base = git::run(wt, &["rev-parse", target])?;
    if mb == base {
        return Ok((Fresh::Yes, mine));
    }
    let theirs: BTreeSet<String> = files(&mb, &base)?.into_iter().collect();
    let both: Vec<String> = mine
        .iter()
        .filter(|f| theirs.contains(*f))
        .cloned()
        .collect();
    Ok(if both.is_empty() {
        (Fresh::Yes, mine)
    } else {
        (Fresh::Behind(both), mine)
    })
}

/// The time of the item's last ship (a ship or an update; a restore puts the
/// same head back with its own checks, so it doesn't count).
fn last_shipped(events: &[Event]) -> Option<DateTime<Utc>> {
    events
        .iter()
        .filter(|e| matches!(e.body, Body::Shipped { .. }))
        .map(|e| e.t)
        .max()
}

/// Whether the CI watch gave up on the PR's last ship: checks that never
/// report (a `paths-ignore` that skips every file it changes) leave it
/// pending for good.
fn ci_gave_up(events: &[Event]) -> bool {
    last_shipped(events)
        .is_some_and(|t| Utc::now() - t >= Duration::hours(crate::ci::GIVE_UP_HOURS))
}

/// Whether CI passed on the PR after its last ship, and if not, why.
fn ci_status(item: &Item, events: &[Event]) -> Option<(Status, String)> {
    let shipped = last_shipped(events);
    let checks = events
        .iter()
        .rev()
        .find(|e| matches!(e.body, Body::PrChecks { .. }));
    let fresh = checks.is_some_and(|c| shipped.is_none_or(|s| c.t >= s));
    match (&item.ci, fresh) {
        (Some(ci), true) if ci.passed => None,
        (Some(ci), true) => Some((
            Status::CiRed,
            format!("CI failed: {}", ci.failing.join(", ")),
        )),
        _ => Some((
            Status::CiPending,
            "CI hasn't passed at the shipped head yet".into(),
        )),
    }
}

/// What the merger knows while it walks one base's queue.
struct Look<'a> {
    repo: &'a Repo,
    /// Every shipped PR still open, by number.
    open: BTreeMap<u64, &'a Item>,
    /// Bases fetched this look.
    targets: BTreeMap<String, Option<Target>>,
    /// The files each item changes, once known.
    files: BTreeMap<ItemId, Vec<String>>,
    /// Resolve bases with the capped fetch (the CLI), not the daemon's.
    capped: bool,
    /// What each base's tip CI said this look: its sha and the checks that
    /// failed there. A base GitHub couldn't tell about isn't here.
    tips: BTreeMap<String, (String, Vec<String>)>,
}

impl<'a> Look<'a> {
    /// The checks that failed on `base`'s tip, read once per look. Empty
    /// when it is green or still running; `None` when GitHub can't say, and
    /// then nothing stops: the guard never blocks on an outage.
    fn tip_failures(&mut self, base: &str, line: &[Item]) -> Option<Vec<String>> {
        if let Some((_, f)) = self.tips.get(base) {
            return Some(f.clone());
        }
        let target = self.target(base)?;
        let pr = line.iter().find_map(|i| i.pr_url.clone())?;
        let sha = git::run(&self.repo.main_root, &["rev-parse", &target.git_ref]).ok()?;
        let failed = crate::gh::commit_failures(&self.repo.main_root, &pr, &sha).ok()?;
        self.tips.insert(base.to_string(), (sha, failed.clone()));
        Some(failed)
    }

    fn target(&mut self, base: &str) -> Option<Target> {
        self.targets
            .entry(base.to_string())
            .or_insert_with(|| {
                if self.capped {
                    delivery::base_ref_capped(&self.repo.main_root, base)
                } else {
                    delivery::base_ref(&self.repo.main_root, base)
                }
            })
            .clone()
    }

    /// The URL of PR `n`, next to `like`.
    fn url(like: &str, n: u64) -> String {
        match like.rsplit_once("/pull/") {
            Some((head, _)) => format!("{head}/pull/{n}"),
            None => format!("#{n}"),
        }
    }

    /// The open PRs `item` waits for: the one it is stacked on, and those
    /// its body or commits name.
    fn waits_for(&self, item: &Item, url: &str, text: &str) -> Vec<String> {
        let mut out = Vec::new();
        if item.base != self.repo.config.base {
            match self.open.values().find(|o| o.branch == item.base) {
                Some(o) => out.push(o.pr_url.clone().unwrap_or_default()),
                // Its parent isn't open: if that PR merged, a person
                // retargets this one onto the base.
                None => out.push(format!(
                    "{} (its base branch; if that PR already merged, retarget this one onto {})",
                    item.base, self.repo.config.base
                )),
            }
        }
        let me = number(url);
        for n in depends_on(text) {
            if Some(n) == me {
                continue;
            }
            if let Some(o) = self.open.get(&n) {
                out.push(o.pr_url.clone().unwrap_or_default());
                continue;
            }
            // Not one of ours that is still open: ask GitHub.
            let dep = Self::url(url, n);
            match crate::gh::pr_state(&self.repo.main_root, &dep) {
                Ok(s) if s == "MERGED" => {}
                _ => out.push(dep),
            }
        }
        out.dedup();
        out
    }

    /// Judges one PR. Cheap, local checks come first, so a PR that can't
    /// merge yet costs no GitHub call; one whose CI hasn't reported costs
    /// one, to see whether it conflicts.
    fn judge(&mut self, item: &Item) -> Judged {
        let url = item.pr_url.clone().unwrap_or_default();
        let none = Judged::wait;
        if let Some(h) = &item.hold {
            return none(
                if h.kind == HoldKind::Lit {
                    Status::Lit
                } else {
                    Status::Held
                },
                format!("held ({}): {}", h.kind.as_str(), h.reason),
            );
        }
        if let Some(sha) = &item.merged {
            let waiting = format!("merged at {}; waiting for GitHub to say so", short(sha));
            // A command that exits 0 without merging (a merge queue, `--auto`)
            // can leave the PR open for good: say so once it has been a while.
            let events = self.repo.hall.read_item(item.id).unwrap_or_default();
            let at = events
                .iter()
                .rev()
                .find(|e| matches!(e.body, Body::PrMerged { .. }))
                .map(|e| e.t);
            if at.is_some_and(|t| Utc::now() - t > Duration::seconds(STUCK_SECS))
                && crate::gh::pr_state(&self.repo.main_root, &url).is_ok_and(|s| s == "OPEN")
            {
                let why = format!(
                    "the merge command succeeded, but the PR is still open after {} minutes",
                    STUCK_SECS / 60
                );
                return Judged {
                    verdict: Some(Verdict::Stuck(why.clone())),
                    ..none(Status::Merging, why)
                };
            }
            return none(Status::Merging, waiting);
        }
        let Some(head) = item.pr_head.clone() else {
            return none(Status::NotOurs, "no shipped head is recorded".into());
        };
        let wt = PathBuf::from(&item.worktree);
        if !wt.is_dir() {
            return none(
                Status::NotOurs,
                format!("its worktree {} is missing", item.worktree),
            );
        }
        if crate::stamp::read(&wt, &head).is_none() {
            return none(
                Status::NotOurs,
                format!("{} has no hallmark note", short(&head)),
            );
        }
        let events = self.repo.hall.read_item(item.id).unwrap_or_default();
        // CI that hasn't reported still needs GitHub's word on a conflict:
        // GitHub runs no pull_request workflows on a conflicting PR, so its
        // CI may never report until it is brought up to date.
        let mut ci_pending = None;
        if self.repo.config.merge.require_ci {
            match ci_status(item, &events) {
                Some((Status::CiPending, why)) => {
                    ci_pending = Some(match ci_gave_up(&events) {
                        true => (
                            format!(
                                "CI never reported at the shipped head in {}h; \
                                 the CI watch gave up",
                                crate::ci::GIVE_UP_HOURS
                            ),
                            true,
                        ),
                        false => (why, false),
                    })
                }
                Some((s, why)) => return none(s, why),
                None => {}
            }
        }
        let pr: GhQueue = match crate::gh::pr_queue(&self.repo.main_root, &url) {
            Ok(p) => p,
            Err(e) => return none(Status::Unreadable, e.message),
        };
        if pr.state != "OPEN" {
            return none(
                Status::Merging,
                format!(
                    "GitHub says {}; the watch records it",
                    pr.state.to_lowercase()
                ),
            );
        }
        if pr.head_oid != head {
            return none(
                Status::NotOurs,
                format!(
                    "its head is {}, not the {} Hallmark shipped",
                    short(&pr.head_oid),
                    short(&head)
                ),
            );
        }
        let Some(target) = self.target(&item.base) else {
            return none(
                Status::Unreadable,
                format!("no {} branch to compare with", item.base),
            );
        };
        // Only the PR's own commits, not the base's history.
        let own = format!("{}..{head}", target.git_ref);
        let text = format!(
            "{}\n{}",
            pr.body,
            git::run(&wt, &["log", "--format=%B", &own]).unwrap_or_default()
        );
        let waits = self.waits_for(item, &url, &text);
        if !waits.is_empty() {
            return Judged {
                waits: waits.clone(),
                ..none(Status::WaitsFor, format!("waits for {}", waits.join(", ")))
            };
        }
        if let Some(who) = changes_requested(&pr.reviews) {
            return none(Status::ChangesRequested, format!("{who} asked for changes"));
        }
        // A conflict is brought up to date at its turn, like a PR that is
        // behind: GitHub's word only says when to look closer.
        let conflicting = match (pr.mergeable.as_str(), ci_pending) {
            ("CONFLICTING", _) => true,
            (_, Some((why, gave_up))) => {
                return Judged {
                    stalled: gave_up,
                    ..none(Status::CiPending, why)
                };
            }
            ("MERGEABLE", None) => false,
            _ => {
                return none(
                    Status::Unknown,
                    "GitHub is still working out whether it conflicts".into(),
                );
            }
        };
        if !git::dirty_files(&wt).map(|d| d.is_empty()).unwrap_or(false) {
            // The merge doesn't need the worktree, but a dirty one means
            // someone is working in it; a rebase would only hold it.
            return none(
                Status::NotOurs,
                format!("its worktree {} has uncommitted changes", item.worktree),
            );
        }
        let (fresh, files) = match freshness(&wt, &target.git_ref, &head) {
            Ok(f) => f,
            Err(e) => return none(Status::Unreadable, e.message),
        };
        self.files.insert(item.id, files.clone());
        let judged = |status, why: String, verdict| Judged {
            status,
            why,
            waits: vec![],
            verdict: Some(verdict),
            stalled: false,
        };
        if conflicting {
            let both = match fresh {
                Fresh::Behind(both) => both,
                Fresh::Yes => vec![],
            };
            return judged(
                Status::Conflicting,
                format!("conflicts with {}; bringing it up to date", target.name),
                Verdict::Rebase(both, head),
            );
        }
        // With `update = "conflicts"`, a PR GitHub merges cleanly goes in as
        // it is: the base's own CI catches a bad combination (ADR 0051).
        let as_it_is = match fresh {
            Fresh::Behind(both)
                if self.repo.config.merge.update == crate::config::MergeUpdate::Files =>
            {
                return judged(
                    Status::Behind,
                    format!(
                        "behind {} on files it touches ({}); bringing it up to date",
                        target.name,
                        both.join(", ")
                    ),
                    Verdict::Rebase(both, head),
                );
            }
            Fresh::Behind(both) => Some(format!(
                "behind {} on {}, and merges cleanly as it is",
                target.name,
                both.join(", ")
            )),
            Fresh::Yes => None,
        };
        // A person is asked only about a head that can merge.
        let lit = crate::assayer::base::globset(&self.repo.config.merge.lit);
        let touched: Vec<String> = files.iter().filter(|f| lit.is_match(f)).cloned().collect();
        if !touched.is_empty() && item.merge_approved.as_deref() != Some(head.as_str()) {
            let why = format!(
                "ready to merge, waiting for you: touches {}",
                touched.join(", ")
            );
            return judged(Status::Lit, why.clone(), Verdict::Lit(why));
        }
        judged(
            Status::Ready,
            match as_it_is {
                Some(why) => format!("ready: hallmarked at {}; {why}", short(&head)),
                None => format!(
                    "ready: hallmarked at {}, mergeable, up to date",
                    short(&head)
                ),
            },
            Verdict::Merge(head),
        )
    }

    /// The files `item` changes since it left its base: its shipped head, or
    /// for an item being brought up to date, its branch as it is now.
    fn files_of(&mut self, item: &Item) -> Vec<String> {
        if let Some(f) = self.files.get(&item.id) {
            return f.clone();
        }
        let head = if item.state == ItemState::Closed {
            item.pr_head.clone()
        } else {
            Some(format!("refs/heads/{}", item.branch))
        };
        let files = match (head, self.target(&item.base)) {
            (Some(head), Some(target)) => freshness(&self.repo.main_root, &target.git_ref, &head)
                .map(|(_, f)| f)
                .unwrap_or_default(),
            _ => vec![],
        };
        self.files.insert(item.id, files.clone());
        files
    }

    /// Plans one base: what each PR in `line` waits for or what the merger
    /// does with it, in queue order. `hall queue` shows this plan; the
    /// worker carries it out.
    fn plan(&mut self, base: &str, line: &[Item], items: &[Item]) -> Vec<Planned> {
        // Files a PR may not touch yet, and the PR that claims each one.
        let mut claimed: BTreeMap<String, Vec<Claim>> = BTreeMap::new();
        let mut out = Vec::new();
        // Reopened items the loop is bringing back: a wait for one of them
        // moves, like a wait for a PR in line.
        let mut in_flight: Vec<String> = Vec::new();
        // Items already sent back claim their files first: whatever overlaps
        // them would be stale again once they merge. One held, or in a
        // person's hands, claims nothing.
        for item in reopened(items, base) {
            let (status, why) = if let Some(h) = &item.hold {
                (
                    Status::Held,
                    format!("held ({}): {}", h.kind.as_str(), h.reason),
                )
            } else if is_in_flight(item) {
                let url = item.pr_url.clone().unwrap_or_default();
                in_flight.push(url.clone());
                for f in self.files_of(item) {
                    claimed.entry(f).or_default().push(Claim {
                        url: url.clone(),
                        waits: vec![],
                        reopened: true,
                    });
                }
                (
                    Status::Updating,
                    format!(
                        "being brought up to date ({}); it ships again, then merges",
                        item.state
                    ),
                )
            } else {
                (
                    Status::Updating,
                    format!(
                        "reopened, with you ({}); it ships again, then merges",
                        item.state
                    ),
                )
            };
            out.push(Planned {
                entry: entry(item, status, why, vec![]),
                verdict: None,
            });
        }
        let busy = line.iter().any(|i| i.merged.is_some());
        let mut merging = false;
        let mut unknown_ahead = false;
        // Walk dependency-first: a PR comes after every PR in this line it
        // waits for, otherwise oldest first. A cycle falls back to number
        // order.
        let mut pending: Vec<(&Item, Judged)> = line.iter().map(|i| (i, self.judge(i))).collect();
        let urls: Vec<String> = line
            .iter()
            .map(|i| i.pr_url.clone().unwrap_or_default())
            .collect();
        // A wait moves when it is on a PR of this line that claimed (the walk
        // is dependency-first, so it comes before the PRs waiting for it), on
        // an item being brought back, or on an open PR of another base (the
        // PR a stacked one sits on). A wait on a PR that waits on a person
        // doesn't move.
        let mut moving: BTreeSet<String> = in_flight.iter().cloned().collect();
        moving.extend(
            self.open
                .values()
                .filter_map(|o| o.pr_url.clone())
                .filter(|u| !urls.contains(u)),
        );
        let mut walk: Vec<(&Item, Judged)> = Vec::new();
        while !pending.is_empty() {
            let at = pending
                .iter()
                .position(|(_, j)| {
                    j.waits.iter().all(|w| {
                        !urls.contains(w)
                            || walk.iter().any(|(i, _)| i.pr_url.as_deref() == Some(w))
                            || !pending.iter().any(|(i, _)| i.pr_url.as_deref() == Some(w))
                    })
                })
                .unwrap_or(0);
            walk.push(pending.remove(at));
        }
        for (item, judged) in walk {
            let Judged {
                mut status,
                mut why,
                mut waits,
                mut verdict,
                stalled,
            } = judged;
            // An older PR GitHub hasn't finished checking goes first.
            if unknown_ahead && matches!(verdict, Some(Verdict::Merge(_))) {
                status = Status::Queued;
                why = "ready; waits for an older PR GitHub is still checking".into();
                verdict = None;
            }
            unknown_ahead |= status == Status::Unknown;
            // A person is asked about a lit PR only once nothing ahead of it
            // claims its files, so the head they approve can merge.
            if matches!(
                verdict,
                Some(Verdict::Merge(_) | Verdict::Rebase(..) | Verdict::Lit(_))
            ) {
                let mut by: BTreeMap<&str, Vec<String>> = BTreeMap::new();
                let me = item.pr_url.clone().unwrap_or_default();
                for f in self.files_of(item) {
                    // A claim by a PR that waits for this one, or by a
                    // reopened PR newer than this one, doesn't count:
                    // honoring it would deadlock, or make this PR wait for a
                    // newer one that must be rebased onto it anyway.
                    let first = claimed.get(&f).and_then(|v| {
                        v.iter().find(|c| {
                            let newer_reopened = c.reopened && number(&c.url) > number(&me);
                            !c.waits.contains(&me) && !newer_reopened
                        })
                    });
                    if let Some(c) = first {
                        by.entry(c.url.as_str()).or_default().push(f);
                    }
                }
                if !by.is_empty() {
                    let ahead: Vec<String> = by
                        .iter()
                        .map(|(u, fs)| format!("{} ({})", pr_name(u), fs.join(", ")))
                        .collect();
                    why = match status {
                        Status::Ready | Status::Lit => "ready".to_string(),
                        _ => why.replace("; bringing it up to date", ""),
                    };
                    why = format!("{why}; first waits for {}", ahead.join(", "));
                    waits = by.keys().map(|u| u.to_string()).collect();
                    status = Status::WaitsFor;
                    verdict = None;
                }
            }
            if matches!(verdict, Some(Verdict::Merge(_))) {
                if busy {
                    status = Status::Queued;
                    why = "ready; merges once the PR being merged is delivered".into();
                    verdict = None;
                } else if merging {
                    status = Status::Queued;
                    why = "ready; merges after the PRs ahead of it".into();
                    verdict = None;
                }
                merging = true;
            }
            if !stalled && claims(status, &waits, &moving) {
                let url = item.pr_url.clone().unwrap_or_default();
                moving.insert(url.clone());
                for f in self.files_of(item) {
                    claimed.entry(f).or_default().push(Claim {
                        url: url.clone(),
                        waits: waits.clone(),
                        reopened: false,
                    });
                }
            }
            out.push(Planned {
                entry: entry(item, status, why, waits),
                verdict,
            });
        }
        // Stop the line: nothing merges into, or is rebased onto, a base
        // whose own CI failed, until it is green again (ADR 0051).
        let failed = if out.iter().any(|p| {
            matches!(
                p.verdict,
                Some(Verdict::Merge(_) | Verdict::Rebase(..) | Verdict::Lit(_))
            )
        }) {
            self.tip_failures(base, line).unwrap_or_default()
        } else {
            vec![]
        };
        if !failed.is_empty() {
            let stop = format!(
                "{base} is red ({} failed); the merger waits for it to be green",
                failed.join(", ")
            );
            for p in &mut out {
                if let Some(Verdict::Merge(_) | Verdict::Rebase(..) | Verdict::Lit(_)) = p.verdict {
                    p.verdict = None;
                    if matches!(p.entry.status, Status::Ready | Status::Lit) {
                        p.entry.status = Status::Queued;
                    }
                    p.entry.why = format!("{}; {stop}", p.entry.why);
                }
            }
        }
        out.sort_by_key(|p| (number(&p.entry.pr_url), p.entry.item));
        for (n, p) in out.iter_mut().enumerate() {
            p.entry.position = n as u32 + 1;
        }
        out
    }
}

/// Whether a PR in this status is on its way to a merge on its own, so the
/// PRs behind it that touch its files wait for it. One waiting on a person,
/// or broken, claims nothing; so does one waiting for it.
fn claims(status: Status, waits: &[String], moving: &BTreeSet<String>) -> bool {
    match status {
        Status::Ready
        | Status::Merging
        | Status::Behind
        | Status::Conflicting
        | Status::CiPending
        | Status::Unknown
        | Status::Queued
        | Status::Updating => true,
        // Waiting for PRs that move, moves; waiting for one outside the
        // queue, or one waiting on a person, may not.
        Status::WaitsFor => waits.iter().all(|w| moving.contains(w)),
        Status::CiRed
        | Status::Lit
        | Status::ChangesRequested
        | Status::Held
        | Status::NotOurs
        | Status::Unreadable => false,
    }
}

/// A PR's hold on a file while the plan walks the queue.
struct Claim {
    url: String,
    /// The PRs it waits for.
    waits: Vec<String>,
    /// An item reopened and being brought back, not a PR in line.
    reopened: bool,
}

/// One PR, judged.
struct Judged {
    status: Status,
    why: String,
    waits: Vec<String>,
    verdict: Option<Verdict>,
    /// Pending for good, so it claims nothing: CI the watch gave up on.
    stalled: bool,
}

impl Judged {
    /// Nothing to do yet.
    fn wait(status: Status, why: String) -> Self {
        Judged {
            status,
            why,
            waits: vec![],
            verdict: None,
            stalled: false,
        }
    }
}

/// One PR in the plan, and what the merger does with it.
struct Planned {
    entry: Entry,
    verdict: Option<Verdict>,
}

fn entry(item: &Item, status: Status, why: String, waits_for: Vec<String>) -> Entry {
    Entry {
        position: 0,
        item: item.id,
        title: item.title.clone(),
        pr_url: item.pr_url.clone().unwrap_or_default(),
        base: item.base.clone(),
        status,
        why,
        waits_for,
    }
}

/// `#64` for a PR URL.
fn pr_name(url: &str) -> String {
    number(url).map_or_else(|| url.to_string(), |n| format!("#{n}"))
}

/// Items of `base` reopened through their open PR and not shipped again
/// yet.
fn reopened<'i>(items: &'i [Item], base: &str) -> impl Iterator<Item = &'i Item> {
    items.iter().filter(move |i| {
        i.base == base
            && i.parent.is_none()
            && i.delivered.is_none()
            && !i.state.is_terminal()
            && i.pr_url.as_deref().is_some_and(|u| u.contains("/pull/"))
    })
}

/// A reopened item the loop, not a person, is bringing back: a rebase,
/// comments or a CI fix, with no hold.
fn is_in_flight(i: &Item) -> bool {
    i.hold.is_none()
        && match i.state {
            ItemState::Checks | ItemState::Review | ItemState::Fixing | ItemState::Hallmarked => {
                true
            }
            ItemState::Development => {
                i.rebase.is_some() || i.feedback.is_some() || i.ci_fix.is_some()
            }
            _ => false,
        }
}

/// What the merger does with a PR it judged.
#[derive(Debug, Clone)]
enum Verdict {
    Merge(String),
    Rebase(Vec<String>, String),
    Lit(String),
    /// Merged by the command, yet the PR is still open.
    Stuck(String),
}

/// How long a PR may stay open after a merge command succeeded.
const STUCK_SECS: i64 = 600;

/// The open shipped PRs, grouped by base and in merge order, with every
/// base that has an item on its way back, too.
fn lines(items: &[Item]) -> BTreeMap<String, Vec<Item>> {
    let mut by_base: BTreeMap<String, Vec<Item>> = BTreeMap::new();
    for i in delivery::watched(items) {
        by_base.entry(i.base.clone()).or_default().push(i);
    }
    for v in by_base.values_mut() {
        v.sort_by_key(|i| (i.pr_url.as_deref().and_then(number), i.id));
    }
    for i in items {
        if reopened(std::slice::from_ref(i), &i.base).next().is_some() {
            by_base.entry(i.base.clone()).or_default();
        }
    }
    by_base
}

fn look<'a>(repo: &'a Repo, watched: &'a [Item], capped: bool) -> Look<'a> {
    Look {
        repo,
        open: watched
            .iter()
            .filter_map(|i| Some((number(i.pr_url.as_deref()?)?, i)))
            .collect(),
        targets: BTreeMap::new(),
        files: BTreeMap::new(),
        capped,
        tips: BTreeMap::new(),
    }
}

/// The whole queue, every PR judged (`hall queue`). Reads GitHub once per
/// PR that passes the local checks; never acts.
pub fn queue(repo: &Repo) -> Result<Vec<Entry>> {
    let items = repo.index()?.items(true)?;
    let watched = delivery::watched(&items);
    let mut look = look(repo, &watched, true);
    let mut out = Vec::new();
    for (base, line) in lines(&items) {
        out.extend(look.plan(&base, &line, &items).into_iter().map(|p| p.entry));
    }
    Ok(out)
}

/// The `merger` worker: carries out each base's plan, at most one merge
/// per base, and a rebase for each PR whose turn has come.
pub fn tick(ctx: &WorkerCtx) -> Result<Progress> {
    let repo = ctx.repo;
    if repo.config.merge.mode != MergeMode::Auto {
        // Turned off: nothing is stopped any more.
        let mut state = load(repo);
        if !state.red.is_empty() {
            state.red.clear();
            save(repo, &state);
        }
        return Ok(Progress::Idle);
    }
    let now = Utc::now();
    let mut state = load(repo);
    if state
        .last
        .is_some_and(|t| now - t < Duration::seconds(EVERY))
    {
        return Ok(Progress::Idle);
    }
    let items = repo.index()?.items(true)?;
    let watched = delivery::watched(&items);
    if watched.is_empty() {
        // The last PR left: the app shows an empty queue, not the last plan,
        // and nothing is stopped.
        if !state.queue.is_empty() || !state.items.is_empty() || !state.red.is_empty() {
            state.last = Some(now);
            state.queue.clear();
            state.items.clear();
            state.red.clear();
            save(repo, &state);
        }
        return Ok(Progress::Idle);
    }
    state.last = Some(now);
    let mut look = look(repo, &watched, false);
    let mut done = Vec::new();
    let mut queue = Vec::new();
    let lines = lines(&items);
    // A base with nothing left in its queue stops nothing.
    state.red.retain(|b, _| lines.contains_key(b));
    for (base, line) in lines {
        // A base it stopped for is read again every pass, so it starts
        // again once the base is green, even with nothing ready.
        if state.red.contains_key(&base) {
            look.tip_failures(&base, &line);
        }
        for Planned { entry, verdict } in look.plan(&base, &line, &items) {
            queue.push(entry.clone());
            state.items.insert(
                entry.item.to_string(),
                Seen {
                    status: entry.status,
                    why: entry.why.clone(),
                    at: now,
                },
            );
            let Some(item) = line.iter().find(|i| i.id == entry.item) else {
                continue;
            };
            let acted = match verdict {
                None => continue,
                Some(Verdict::Lit(why)) => hold(repo, item.id, HoldKind::Lit, why)?,
                Some(Verdict::Stuck(why)) => hold(repo, item.id, HoldKind::MergeFailed, why)?,
                Some(Verdict::Rebase(files, head)) => rebase(repo, item, &head, files, &entry.why)?,
                Some(Verdict::Merge(head)) => merge(repo, item, &head, &ctx.holder)?,
            };
            match acted {
                Progress::Worked { what } => done.push(what),
                p @ Progress::Pause { .. } => {
                    save(repo, &state);
                    return Ok(p);
                }
                _ => {}
            }
        }
    }
    // Forget items that left the queue.
    let open: BTreeSet<String> = watched.iter().map(|i| i.id.to_string()).collect();
    state.items.retain(|k, _| open.contains(k));
    state.queue = queue;
    // What each base's tip said: red ones wait in your office, green ones
    // leave it. A base GitHub couldn't tell about keeps what it had.
    for (base, (sha, failing)) in std::mem::take(&mut look.tips) {
        if failing.is_empty() {
            if state.red.remove(&base).is_some() {
                repo.hall
                    .live(&format!("merger: {base} is green again; merging resumes"));
            }
            continue;
        }
        let since = match state.red.get(&base) {
            Some(r) if r.sha == sha => r.since,
            _ => {
                repo.hall.live(&format!(
                    "merger: {base} is red at {} ({}); stopped merging into it",
                    short(&sha),
                    failing.join(", ")
                ));
                now
            }
        };
        state.red.insert(
            base,
            Red {
                sha,
                failing,
                since,
            },
        );
    }
    save(repo, &state);
    Ok(if done.is_empty() {
        Progress::Idle
    } else {
        Progress::worked(done.join("; "))
    })
}

fn hold(repo: &Repo, id: ItemId, kind: HoldKind, reason: String) -> Result<Progress> {
    let (_, held) = repo.hall.transact(id, |it, _| {
        if it.hold.is_some() || it.delivered.is_some() {
            return Ok((vec![], false));
        }
        Ok((
            vec![Event::new(
                ACTOR,
                Some(id),
                Body::Held {
                    kind,
                    reason: reason.clone(),
                },
            )],
            true,
        ))
    })?;
    if !held {
        return Ok(Progress::Idle);
    }
    let what = format!("{id} held ({}): {reason}", kind.as_str());
    repo.hall.live(&what);
    Ok(Progress::worked(what))
}

fn rebase(repo: &Repo, item: &Item, head: &str, files: Vec<String>, why: &str) -> Result<Progress> {
    let url = item.pr_url.clone().unwrap_or_default();
    delivery::reopen_by(
        repo,
        ACTOR,
        item,
        &url,
        head,
        Body::RebaseRequested {
            onto: item.base.clone(),
            reason: why.to_string(),
            files,
        },
        format!("is up next on its files: {why}"),
    )
}

fn merge(repo: &Repo, item: &Item, head: &str, holder: &str) -> Result<Progress> {
    let id = item.id;
    let url = item.pr_url.clone().unwrap_or_default();
    let ttl = Duration::seconds(MERGE_TIMEOUT_SECS as i64 + 60);
    if lease::claim(&repo.hall, id, ACTOR, holder, ttl).is_err() {
        return Ok(Progress::Idle);
    }
    // The delivery-watch takes no lease: look again, under ours.
    let (_, still) = repo.hall.transact(id, |it, _| {
        let ok = it.state == ItemState::Closed
            && it.hold.is_none()
            && it.merged.is_none()
            && it.delivered.is_none()
            && it.rebase.is_none()
            && it.feedback.is_none()
            && it.pr_head.as_deref() == Some(head);
        Ok((vec![], ok))
    })?;
    if !still {
        let _ = lease::release(&repo.hall, id, holder);
        return Ok(Progress::Idle);
    }
    let cmd = crate::commands::new::fill_shell(
        &repo.config.merge.command,
        &[
            ("pr", url.as_str()),
            ("pr_head", head),
            ("branch", item.branch.as_str()),
            ("base", item.base.as_str()),
            ("id", &id.to_string()),
            ("title", item.title.as_str()),
        ],
    );
    repo.hall.live(&format!("{id} merging: {cmd}"));
    let path = crate::gh::path_with_gh().map(|p| p.to_string_lossy().into_owned());
    let mut env = vec![("HALL_ACTOR", ACTOR)];
    if let Some(p) = &path {
        env.push(("PATH", p.as_str()));
    }
    let run = crate::fixer::run_command(&cmd, &repo.main_root, MERGE_TIMEOUT_SECS, &env);
    let runs = repo.hall.item_runs(id);
    let _ = std::fs::create_dir_all(&runs);
    let _ = std::fs::write(
        runs.join(format!("{}-merge.log", Utc::now().format("%Y%m%dT%H%M%S"))),
        format!("$ {cmd}\n{}\n{}", run.stdout, run.stderr),
    );
    let r = match run.failure() {
        Some(why) => hold(
            repo,
            id,
            HoldKind::MergeFailed,
            format!("the merge command {why}"),
        ),
        None => {
            let (_, merged) = repo.hall.transact(id, |it, _| {
                if it.merged.is_some() || it.delivered.is_some() {
                    return Ok((vec![], false));
                }
                Ok((
                    vec![Event::new(
                        ACTOR,
                        Some(id),
                        Body::PrMerged {
                            pr_url: url.clone(),
                            sha: head.to_string(),
                        },
                    )],
                    true,
                ))
            })?;
            if merged {
                let what = format!("{id} merged: {url} at {}", short(head));
                repo.hall.live(&what);
                Ok(Progress::worked(what))
            } else {
                Ok(Progress::Idle)
            }
        }
    };
    let _ = lease::release(&repo.hall, id, holder);
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gh::GhAuthor;

    #[test]
    fn dependencies_are_read_from_plain_words() {
        let d = |t: &str| depends_on(t).into_iter().collect::<Vec<_>>();
        assert_eq!(d("Depends on #12"), vec![12]);
        assert_eq!(d("blocked by: #3, #4 and #5."), vec![3, 4, 5]);
        assert_eq!(
            d("After https://github.com/acme/app/pull/77 lands"),
            vec![77]
        );
        assert_eq!(d("DEPENDS ON #9\nafter #2"), vec![2, 9]);
        // Not a dependency.
        assert!(d("thereafter #4; fixes #5; after lunch").is_empty());
        assert!(d("after https://example.com/x").is_empty());
    }

    #[test]
    fn numbers_come_from_pr_urls() {
        assert_eq!(number("https://github.com/a/b/pull/64"), Some(64));
        assert_eq!(
            number("https://github.com/a/b/pull/64#issuecomment-1"),
            Some(64)
        );
        assert_eq!(number("https://github.com/a/b/issues/64"), None);
    }

    fn review(who: &str, state: &str) -> GhComment {
        GhComment {
            author: Some(GhAuthor { login: who.into() }),
            state: Some(state.into()),
            ..Default::default()
        }
    }

    #[test]
    fn only_a_persons_standing_request_for_changes_counts() {
        assert_eq!(changes_requested(&[]), None);
        assert_eq!(
            changes_requested(&[review("ana", "CHANGES_REQUESTED")]),
            Some("ana".into())
        );
        // Approved since.
        assert_eq!(
            changes_requested(&[
                review("ana", "CHANGES_REQUESTED"),
                review("ana", "COMMENTED"),
                review("ana", "APPROVED"),
            ]),
            None
        );
        // Bots don't count.
        assert_eq!(
            changes_requested(&[review("lint[bot]", "CHANGES_REQUESTED")]),
            None
        );
    }
}
