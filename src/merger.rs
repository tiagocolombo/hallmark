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
//! oldest PR (lowest number) goes first. The first PR in line that is ready
//! merges. The first one that is only behind its base, on files it touches,
//! is reopened with a rebase request, and the Delivery office brings it up
//! to date (rebase, gates, carried hallmark, update, CI).
//!
//! **One at a time**: after a merge, nothing else on that base merges until
//! the watch records `delivered`, so each next PR is judged against the
//! base as it really is.

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
use crate::item::{HoldKind, Item, ItemId};
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
    /// GitHub says it conflicts; the Delivery office rebases it.
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
        }
    }
}

/// One PR in the merge queue.
#[derive(Debug, Clone, Serialize, JsonSchema)]
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub waits_for: Vec<String>,
}

/// What the merger last decided about each item, for `hall status`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    last: Option<DateTime<Utc>>,
    #[serde(default)]
    items: BTreeMap<String, Seen>,
    /// The item the merger sent back to be brought up to date, per base.
    /// Until it is shipped again (or dropped), no other PR on that base is
    /// sent back: each merge would make them stale again.
    #[serde(default)]
    rebasing: BTreeMap<String, String>,
}

impl State {
    /// The item a rebase the merger asked for is still going on, on `base`.
    /// Forgets one that is shipped again or dropped.
    fn rebasing(&mut self, base: &str, items: &[Item]) -> Option<ItemId> {
        let id = self.rebasing.get(base)?;
        let live = items
            .iter()
            .find(|i| i.id.to_string() == *id)
            .filter(|i| !i.state.is_terminal())
            .map(|i| i.id);
        if live.is_none() {
            self.rebasing.remove(base);
        }
        live
    }
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
    std::fs::read_to_string(state_path(repo))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
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
        Ok(git::run(wt, &["diff", "--name-only", from, to])?
            .lines()
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

/// The time of the item's last ship (a ship, an update, or a restore).
fn last_shipped(events: &[Event]) -> Option<DateTime<Utc>> {
    events
        .iter()
        .filter(|e| matches!(e.body, Body::Shipped { .. } | Body::Restored { .. }))
        .map(|e| e.t)
        .max()
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
}

impl<'a> Look<'a> {
    fn target(&mut self, base: &str) -> Option<Target> {
        self.targets
            .entry(base.to_string())
            .or_insert_with(|| delivery::base_ref(&self.repo.main_root, base))
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
                None => out.push(format!("{} (its base branch)", item.base)),
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
    /// merge yet costs no GitHub call.
    fn judge(&mut self, item: &Item) -> (Status, String, Vec<String>, Option<Verdict>) {
        let url = item.pr_url.clone().unwrap_or_default();
        let none = |s: Status, why: String| (s, why, vec![], None);
        if let Some(sha) = &item.merged {
            return none(
                Status::Merging,
                format!("merged at {}; waiting for GitHub to say so", short(sha)),
            );
        }
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
        if self.repo.config.merge.require_ci
            && let Some((s, why)) = ci_status(item, &events)
        {
            return none(s, why);
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
        let text = format!(
            "{}\n{}",
            pr.body,
            git::run(&wt, &["log", "--format=%B", "-n", "50", &head]).unwrap_or_default()
        );
        let waits = self.waits_for(item, &url, &text);
        if !waits.is_empty() {
            return (
                Status::WaitsFor,
                format!("waits for {}", waits.join(", ")),
                waits,
                None,
            );
        }
        if let Some(who) = changes_requested(&pr.reviews) {
            return none(Status::ChangesRequested, format!("{who} asked for changes"));
        }
        match pr.mergeable.as_str() {
            "MERGEABLE" => {}
            "CONFLICTING" => {
                return none(
                    Status::Conflicting,
                    "conflicts with its base; the Delivery office rebases it".into(),
                );
            }
            _ => {
                return none(
                    Status::Unknown,
                    "GitHub is still working out whether it conflicts".into(),
                );
            }
        }
        let Some(target) = self.target(&item.base) else {
            return none(
                Status::Unreadable,
                format!("no {} branch to compare with", item.base),
            );
        };
        let (fresh, files) = match freshness(&wt, &target.git_ref, &head) {
            Ok(f) => f,
            Err(e) => return none(Status::Unreadable, e.message),
        };
        let lit = crate::assayer::base::globset(&self.repo.config.merge.lit);
        let touched: Vec<String> = files.iter().filter(|f| lit.is_match(f)).cloned().collect();
        if !touched.is_empty() && item.merge_approved.as_deref() != Some(head.as_str()) {
            let why = format!(
                "ready to merge, waiting for you: touches {}",
                touched.join(", ")
            );
            return (Status::Lit, why.clone(), vec![], Some(Verdict::Lit(why)));
        }
        if let Fresh::Behind(both) = fresh {
            return (
                Status::Behind,
                format!(
                    "behind {} on files it touches ({}); bringing it up to date first",
                    target.name,
                    both.join(", ")
                ),
                vec![],
                Some(Verdict::Rebase(both, head)),
            );
        }
        if !git::dirty_files(&wt).map(|d| d.is_empty()).unwrap_or(false) {
            // The merge doesn't need the worktree, but a dirty one means
            // someone is working in it.
            return none(
                Status::NotOurs,
                format!("its worktree {} has uncommitted changes", item.worktree),
            );
        }
        (
            Status::Ready,
            format!(
                "ready: hallmarked at {}, mergeable, up to date",
                short(&head)
            ),
            vec![],
            Some(Verdict::Merge(head)),
        )
    }
}

/// What the merger does with a PR it judged.
#[derive(Debug, Clone)]
enum Verdict {
    Merge(String),
    Rebase(Vec<String>, String),
    Lit(String),
}

/// The open shipped PRs, grouped by base and in merge order.
fn lines(items: &[Item]) -> BTreeMap<String, Vec<Item>> {
    let mut by_base: BTreeMap<String, Vec<Item>> = BTreeMap::new();
    for i in delivery::watched(items) {
        by_base.entry(i.base.clone()).or_default().push(i);
    }
    for v in by_base.values_mut() {
        v.sort_by_key(|i| (i.pr_url.as_deref().and_then(number), i.id));
    }
    by_base
}

/// The whole queue, every PR judged (`hall queue`). Reads GitHub once per
/// PR that passes the local checks; never acts.
pub fn queue(repo: &Repo) -> Result<Vec<Entry>> {
    let items = repo.index()?.items(true)?;
    let mut state = load(repo);
    let watched = delivery::watched(&items);
    let mut look = Look {
        repo,
        open: watched
            .iter()
            .filter_map(|i| Some((number(i.pr_url.as_deref()?)?, i)))
            .collect(),
        targets: BTreeMap::new(),
    };
    let mut out = Vec::new();
    for (base, line) in lines(&items) {
        let busy = line.iter().any(|i| i.merged.is_some());
        let rebasing = state.rebasing(&base, &items);
        let mut first = true;
        for (n, item) in line.iter().enumerate() {
            let (mut status, mut why, waits, _) = look.judge(item);
            if status == Status::Behind
                && let Some(r) = rebasing
            {
                status = Status::Queued;
                why = format!("{why}, once {r} is");
            }
            // Only the first ready PR on a base is next; the rest queue.
            if status == Status::Ready {
                if busy {
                    status = Status::Queued;
                    why = "ready; merges once the PR being merged is delivered".into();
                } else if !first {
                    status = Status::Queued;
                    why = "ready; merges after the PRs ahead of it".into();
                }
            }
            if matches!(status, Status::Ready | Status::Behind) {
                first = false;
            }
            out.push(Entry {
                position: n as u32 + 1,
                item: item.id,
                title: item.title.clone(),
                pr_url: item.pr_url.clone().unwrap_or_default(),
                base: base.clone(),
                status,
                why,
                waits_for: waits,
            });
        }
    }
    Ok(out)
}

/// The `merger` worker: at most one merge or one rebase request per base.
pub fn tick(ctx: &WorkerCtx) -> Result<Progress> {
    let repo = ctx.repo;
    if repo.config.merge.mode != MergeMode::Auto {
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
        return Ok(Progress::Idle);
    }
    state.last = Some(now);
    let mut look = Look {
        repo,
        open: watched
            .iter()
            .filter_map(|i| Some((number(i.pr_url.as_deref()?)?, i)))
            .collect(),
        targets: BTreeMap::new(),
    };
    let mut done = Vec::new();
    for (base, line) in lines(&items) {
        // One at a time: a merged PR is delivered before the next one goes.
        if line.iter().any(|i| i.merged.is_some()) {
            continue;
        }
        let rebasing = state.rebasing(&base, &items);
        for item in &line {
            let (mut status, mut why, _, mut verdict) = look.judge(item);
            // One rebase at a time per base; fresh PRs still merge meanwhile.
            if let (Some(Verdict::Rebase(..)), Some(r)) = (&verdict, rebasing) {
                status = Status::Queued;
                why = format!("{why}, once {r} is");
                verdict = None;
            }
            state.items.insert(
                item.id.to_string(),
                Seen {
                    status,
                    why: why.clone(),
                    at: now,
                },
            );
            let acted = match verdict {
                None => None,
                Some(Verdict::Lit(why)) => Some(hold(repo, item.id, HoldKind::Lit, why)?),
                Some(Verdict::Rebase(files, head)) => {
                    let p = rebase(repo, item, &head, files, &why)?;
                    if matches!(p, Progress::Worked { .. }) {
                        state.rebasing.insert(base.clone(), item.id.to_string());
                    }
                    Some(p)
                }
                Some(Verdict::Merge(head)) => Some(merge(repo, item, &head, &ctx.holder)?),
            };
            match acted {
                // A lit PR waits for you; the next one may still go.
                Some(Progress::Worked { what }) if status == Status::Lit => done.push(what),
                Some(Progress::Worked { what }) => {
                    done.push(what);
                    break;
                }
                Some(p @ Progress::Pause { .. }) => {
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
        format!("is next to merge but {why}"),
    )
}

fn merge(repo: &Repo, item: &Item, head: &str, holder: &str) -> Result<Progress> {
    let id = item.id;
    let url = item.pr_url.clone().unwrap_or_default();
    let ttl = Duration::seconds(MERGE_TIMEOUT_SECS as i64 + 60);
    if lease::claim(&repo.hall, id, ACTOR, holder, ttl).is_err() {
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
