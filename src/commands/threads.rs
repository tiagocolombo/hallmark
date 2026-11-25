//! `hall threads`: the agents' conversations as Q&A threads (ADR 0070).
//! With no argument it lists every item's threads, live ones first; with an
//! item ID only that item's; with a thread ID (`H-0042/F3`) it shows that
//! thread whole, from the question to where it ended.

use std::fmt::Write as _;
use std::path::Path;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::ThreadsArgs;
use crate::error::{ErrorCode, HallError, Result};
use crate::item::{ItemId, State};
use crate::output::{Render, Response, clip};
use crate::repo::Repo;
use crate::threads::{self, Kind, PostKind, Thread, ThreadStatus};

/// At most this many threads are listed unless `--limit` says otherwise.
const LIMIT: usize = 50;

/// A thread as the list shows it: everything but its posts.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ThreadRow {
    pub id: String,
    pub item: ItemId,
    pub item_title: String,
    pub item_state: State,
    pub kind: Kind,
    pub status: ThreadStatus,
    pub title: String,
    pub asked_by: String,
    pub asked_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
    pub tags: Vec<String>,
    pub replies: usize,
}

impl From<&Thread> for ThreadRow {
    fn from(t: &Thread) -> Self {
        ThreadRow {
            id: t.id.clone(),
            item: t.item,
            item_title: t.item_title.clone(),
            item_state: t.item_state,
            kind: t.kind,
            status: t.status,
            title: t.title.clone(),
            asked_by: t.asked_by.clone(),
            asked_at: t.asked_at,
            last_at: t.last_at,
            tags: t.tags.clone(),
            replies: t.replies,
        }
    }
}

/// Threads by status, over every thread that matched (not only those shown).
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct Tally {
    pub open: usize,
    pub answered: usize,
    pub resolved: usize,
    pub withdrawn: usize,
    pub refuted: usize,
    pub closed: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ThreadsList {
    /// The item, when the list is one item's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
    /// Threads that matched, before `--limit`.
    pub total: usize,
    pub tally: Tally,
    /// Live threads (open, answered) first, then the most recently active.
    pub threads: Vec<ThreadRow>,
    /// Items whose journal could not be read: their threads are missing.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unreadable: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ThreadOut {
    #[serde(flatten)]
    pub thread: Thread,
}

#[derive(Serialize, JsonSchema)]
#[serde(untagged)]
#[allow(dead_code)]
pub enum ThreadsReply {
    List(crate::output::Out<ThreadsList>),
    Thread(crate::output::Out<ThreadOut>),
}

impl Render for ThreadsList {
    fn render(&self) -> String {
        let mut s = String::new();
        let t = &self.tally;
        let scope = self.item.map(|i| format!(" on {i}")).unwrap_or_default();
        let _ = writeln!(
            s,
            "{} thread{}{scope} · {} open · {} answered · {} resolved · {} withdrawn · {} refuted · {} closed",
            self.total,
            if self.total == 1 { "" } else { "s" },
            t.open,
            t.answered,
            t.resolved,
            t.withdrawn,
            t.refuted,
            t.closed
        );
        if self.threads.is_empty() {
            s.push_str("No threads yet: they start when an item is planned, reviewed or asks you something.\n");
        }
        for r in &self.threads {
            let tags = if r.tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", r.tags.join(" · "))
            };
            let _ = writeln!(
                s,
                "  {:<9} {:<14} {}{}",
                r.status.as_str(),
                r.id,
                clip(&r.title, 70),
                tags
            );
            let _ = writeln!(
                s,
                "  {:<9} {:<14} asked by {} · {} repl{} · last {}",
                "",
                "",
                r.asked_by,
                r.replies,
                if r.replies == 1 { "y" } else { "ies" },
                r.last_at.format("%Y-%m-%d %H:%M")
            );
        }
        if self.threads.len() < self.total {
            let _ = writeln!(
                s,
                "… {} more; --limit shows more",
                self.total - self.threads.len()
            );
        }
        for u in &self.unreadable {
            let _ = writeln!(s, "{u}: its journal could not be read");
        }
        s
    }
}

impl Render for ThreadOut {
    fn render(&self) -> String {
        let t = &self.thread;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "{} · {} · {} on {} \"{}\"",
            t.id,
            t.status.as_str(),
            t.kind.as_str(),
            t.item,
            clip(&t.item_title, 60)
        );
        let _ = writeln!(s, "{}", t.title);
        if !t.tags.is_empty() {
            let _ = writeln!(s, "tags: {}", t.tags.join(" · "));
        }
        for p in &t.posts {
            let mark = match (p.kind, p.accepted) {
                (_, true) => "✓",
                (PostKind::Question, _) => "?",
                (PostKind::Answer, _) => "»",
                (PostKind::Comment, _) => "·",
                (PostKind::Ruling, _) => "§",
            };
            let kind = match p.kind {
                PostKind::Question => "question",
                PostKind::Answer => "answer",
                PostKind::Comment => "comment",
                PostKind::Ruling => "ruling",
            };
            let mut meta = vec![p.author.clone()];
            if let Some(r) = p.round {
                meta.push(format!("round {r}"));
            }
            if let Some(sha) = &p.sha {
                meta.push(crate::event::short(sha).to_string());
            }
            meta.push(p.t.format("%Y-%m-%d %H:%M").to_string());
            let _ = writeln!(s, "\n{mark} {kind} · {}", meta.join(" · "));
            for line in p.body.lines() {
                if line.trim().is_empty() {
                    s.push('\n');
                } else {
                    let _ = writeln!(s, "  {line}");
                }
            }
        }
        s
    }
}

pub fn threads(cwd: &Path, a: ThreadsArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    repo.require_init()?;
    let kinds = parse_all(&a.kind, "--kind", &Kind::ALL, |k| k.as_str())?;
    let statuses = parse_all(&a.status, "--status", &ThreadStatus::ALL, |s| s.as_str())?;

    // A thread ID shows that thread.
    if let Some((item, key)) = a.target.as_deref().and_then(|t| t.split_once('/')) {
        let item = repo.resolve(Some(item))?;
        let target = format!("{}/{key}", item.id);
        let events = repo.hall.read_item(item.id)?;
        let found = threads::of_item(item.id, &item.title, item.state, &events)
            .into_iter()
            .find(|t| t.id.eq_ignore_ascii_case(&target));
        let Some(thread) = found else {
            return Err(HallError::new(
                ErrorCode::ThreadNotFound,
                format!("{} has no thread {target}", item.id),
            )
            .next([format!("hall threads {}", item.id)]));
        };
        let next = thread_next(&thread);
        return Ok(Response::new(ThreadOut { thread }, next));
    }

    let only: Option<ItemId> = match a.target.as_deref() {
        Some(id) => Some(repo.resolve(Some(id))?.id),
        None => None,
    };
    let ix = repo.index()?;
    let mut all: Vec<Thread> = Vec::new();
    let mut unreadable = Vec::new();
    for item in ix.items(true)? {
        if only.is_some_and(|o| o != item.id) {
            continue;
        }
        match repo.hall.read_item(item.id) {
            Ok(events) => all.extend(threads::of_item(item.id, &item.title, item.state, &events)),
            Err(_) => unreadable.push(item.id.to_string()),
        }
    }
    all.retain(|t| {
        (kinds.is_empty() || kinds.contains(&t.kind))
            && (statuses.is_empty() || statuses.contains(&t.status))
    });
    threads::sort(&mut all);
    let mut tally = Tally::default();
    for t in &all {
        match t.status {
            ThreadStatus::Open => tally.open += 1,
            ThreadStatus::Answered => tally.answered += 1,
            ThreadStatus::Resolved => tally.resolved += 1,
            ThreadStatus::Withdrawn => tally.withdrawn += 1,
            ThreadStatus::Refuted => tally.refuted += 1,
            ThreadStatus::Closed => tally.closed += 1,
        }
    }
    let total = all.len();
    let shown: Vec<ThreadRow> = all
        .iter()
        .take(a.limit.unwrap_or(LIMIT))
        .map(ThreadRow::from)
        .collect();
    let mut next: Vec<String> = shown
        .first()
        .map(|t| vec![format!("hall threads {}", t.id)])
        .unwrap_or_default();
    if statuses.is_empty() {
        next.push("hall threads --status open,answered".into());
    }
    if only.is_none() {
        next.push("hall threads <item-id>".into());
    }
    Ok(Response::new(
        ThreadsList {
            item: only,
            total,
            tally,
            threads: shown,
            unreadable,
        },
        next,
    ))
}

fn thread_next(t: &Thread) -> Vec<String> {
    let mut next = Vec::new();
    match (t.kind, t.status) {
        (Kind::Question, ThreadStatus::Open) => next.push("hall inbox".to_string()),
        (Kind::Finding, s) if s.live() => next.push(format!("hall findings {}", t.item)),
        _ => {}
    }
    next.push(format!("hall threads {}", t.item));
    next.push(format!("hall log {}", t.item));
    next
}

/// Comma-separated values, each one of `all`.
fn parse_all<T: Copy>(
    given: &[String],
    flag: &str,
    all: &[T],
    name: impl Fn(T) -> &'static str,
) -> Result<Vec<T>> {
    given
        .iter()
        .map(|g| {
            all.iter()
                .copied()
                .find(|x| name(*x) == g.trim())
                .ok_or_else(|| {
                    let names: Vec<&str> = all.iter().map(|x| name(*x)).collect();
                    HallError::usage(format!("{flag} {g}: use one of {}", names.join(", ")))
                })
        })
        .collect()
}
