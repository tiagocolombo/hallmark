//! Seeing: `status`, `log`, `inbox`, `usage`, `reindex`.

use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use schemars::JsonSchema;
use serde::Serialize;

use super::{ItemView, age, next_for};
use crate::cli::{LogArgs, UsageArgs};
use crate::error::{ErrorCode, HallError, Result};
use crate::event::Event;
use crate::index::{InboxEntry, Index, UsageBy, UsageRow};
use crate::item::State;
use crate::output::{Render, Response, clip};
use crate::repo::Repo;
use crate::usage::fmt_tokens;

#[derive(Debug, Serialize, JsonSchema)]
pub struct StatusList {
    pub repo: String,
    pub items: Vec<ItemView>,
    /// Entries waiting in your inbox.
    pub inbox: usize,
}

impl Render for StatusList {
    fn render(&self) -> String {
        if self.items.is_empty() {
            return format!("{}: no open items. Inbox: {}.\n", self.repo, self.inbox);
        }
        let mut s = format!(
            "{} · {} open · inbox {}\n\n",
            self.repo,
            self.items.len(),
            self.inbox
        );
        for st in State::ALL {
            let rows: Vec<&ItemView> = self.items.iter().filter(|i| i.state == st).collect();
            if rows.is_empty() {
                continue;
            }
            s.push_str(&format!("{} ({})\n", st.label().to_uppercase(), rows.len()));
            for i in rows {
                let round = if i.round > 0 {
                    format!(" r{}", i.round)
                } else {
                    String::new()
                };
                s.push_str(&format!(
                    "  {} {}{}  {}\n",
                    i.id,
                    clip(&i.title, 50),
                    round,
                    age(i.state_since)
                ));
            }
        }
        s
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct StatusItem {
    #[serde(flatten)]
    pub item: ItemView,
    /// Total tokens and cost recorded for this item.
    pub usage: UsageRow,
    /// The last few journal events, newest last.
    pub recent: Vec<Event>,
    /// What it is doing now, and what you can do.
    pub now: crate::now::Now,
}

impl Render for StatusItem {
    fn render(&self) -> String {
        let mut s = self.item.render();
        s.push_str(&format!(
            "  usage     {} tokens · {}\n",
            fmt_tokens(self.usage.tokens),
            crate::usage::fmt_usd(self.usage.cost_usd)
        ));
        s.push_str(&self.now.render());
        if !self.recent.is_empty() {
            s.push_str("\nrecent\n");
            for e in &self.recent {
                s.push_str(&format!(
                    "  {} {:8} {}\n",
                    e.t.format("%m-%d %H:%M"),
                    e.actor,
                    e.body.summary()
                ));
            }
        }
        s
    }
}

pub fn status(cwd: &Path, id: Option<&str>, all: bool, bare: bool) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    repo.require_init()?;
    let ix = repo.index()?;
    // In a worktree, `hall status` means this item.
    let target = match id {
        Some(id) => Some(repo.resolve(Some(id))?),
        None if !all => repo.current_item(),
        None => None,
    };
    if let Some(item) = target {
        let usage = ix
            .usage(UsageBy::Item, None, Some(item.id))?
            .into_iter()
            .next()
            .unwrap_or_default();
        let events = repo.hall.read_item(item.id)?;
        let recent = events[events.len().saturating_sub(5)..].to_vec();
        let live = ix.items(false)?;
        let now = crate::now::now(&repo, &item, &live, &events);
        let next: Vec<String> = now.actions.iter().map(|a| a.command.clone()).collect();
        let out = StatusItem {
            item: ItemView::from(&item),
            usage,
            recent,
            now,
        };
        return Ok(Response::new(out, next));
    }
    let items: Vec<ItemView> = ix.items(all)?.iter().map(ItemView::from).collect();
    let inbox = ix.inbox()?.len();
    let mut next = vec!["hall new \"<request>\"".to_string(), "hall inbox".into()];
    if bare {
        next.push("hall help".into());
    }
    if let Some(i) = items.first() {
        next.push(format!("hall status {}", i.id));
    }
    Ok(Response::new(
        StatusList {
            repo: repo.name.clone(),
            items,
            inbox,
        },
        next,
    ))
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LogOut {
    pub id: String,
    pub events: Vec<Event>,
    /// Events in the journal before filtering.
    pub total: usize,
}

impl Render for LogOut {
    fn render(&self) -> String {
        let mut s = format!(
            "{} · {} of {} events\n",
            self.id,
            self.events.len(),
            self.total
        );
        for e in &self.events {
            s.push_str(&format!(
                "  {} {:8} {}\n",
                e.t.format("%m-%d %H:%M:%S"),
                e.actor,
                e.body.summary()
            ));
        }
        s
    }
}

pub fn log(cwd: &Path, a: LogArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    let item = repo.resolve(a.id.as_deref())?;
    let all = repo.hall.read_item(item.id)?;
    let kinds: Vec<String> = a
        .kind
        .iter()
        .map(|k| match k.trim() {
            "review" | "reviews" => "verdict".to_string(),
            "check" => "checks".to_string(),
            k => k.to_string(),
        })
        .collect();
    let mut events: Vec<Event> = all
        .iter()
        .filter(|e| kinds.is_empty() || kinds.iter().any(|k| k == e.body.name()))
        .cloned()
        .collect();
    if let Some(n) = a.limit {
        events = events.split_off(events.len().saturating_sub(n));
    }
    let out = LogOut {
        id: item.id.to_string(),
        total: all.len(),
        events,
    };
    Ok(Response::new(out, next_for(&item)))
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct InboxOut {
    pub entries: Vec<InboxEntry>,
}

impl Render for InboxOut {
    fn render(&self) -> String {
        if self.entries.is_empty() {
            return "Nothing needs you.\n".into();
        }
        let mut s = format!("⚑ {} waiting\n\n", self.entries.len());
        for e in &self.entries {
            let who = e.item.map(|i| i.to_string()).unwrap_or_default();
            let opts = if e.options.is_empty() {
                String::new()
            } else {
                let letters: Vec<String> = e
                    .options
                    .iter()
                    .enumerate()
                    .map(|(i, o)| format!("[{}] {o}", (b'a' + i as u8) as char))
                    .collect();
                format!("  {}", letters.join("  "))
            };
            let qid = e.qid.clone().map(|q| format!("{q} ")).unwrap_or_default();
            s.push_str(&format!(
                "  {:15} {:7} {}{}{}  {}\n",
                e.kind,
                who,
                qid,
                clip(&e.text, 60),
                opts,
                age(e.since)
            ));
        }
        s
    }
}

pub fn inbox(cwd: &Path, a: crate::cli::InboxArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    repo.require_init()?;
    let ix = repo.index()?;
    let entries = if a.all { ix.inbox_all()? } else { ix.inbox()? };
    if a.clear {
        return dismiss(&repo, entries);
    }
    if let Some(which) = &a.entry {
        if a.dismiss {
            let pos = find_entry(&entries, which)?;
            return dismiss(&repo, vec![entries[pos].clone()]);
        }
        return inbox_detail(&repo, &ix, entries, which);
    }
    let mut next: Vec<String> = Vec::new();
    if !entries.is_empty() {
        next.push("hall inbox 1".into());
    }
    next.extend(
        entries
            .iter()
            .flat_map(|e| e.answer_with.first().cloned())
            .take(5),
    );
    if !entries.is_empty() {
        next.push("hall inbox --clear".into());
    }
    Ok(Response::new(InboxOut { entries }, next))
}

/// What `hall inbox --dismiss` and `--clear` did.
#[derive(Debug, Serialize, JsonSchema)]
pub struct DismissOut {
    /// The entries hidden by this run. Each comes back when it changes.
    pub dismissed: Vec<InboxEntry>,
    /// Entries still waiting.
    pub remaining: usize,
}

impl Render for DismissOut {
    fn render(&self) -> String {
        let mut s = match self.dismissed.len() {
            0 => "Nothing to dismiss.\n".to_string(),
            1 => "Dismissed 1 entry; it comes back if it changes.\n".to_string(),
            n => format!("Dismissed {n} entries; each comes back if it changes.\n"),
        };
        for e in &self.dismissed {
            let who = e.item.map(|i| format!(" {i}")).unwrap_or_default();
            s.push_str(&format!("  {}{who}  {}\n", e.kind, clip(&e.text, 60)));
        }
        s.push_str(&match self.remaining {
            0 => "Nothing else needs you.\n".to_string(),
            n => format!("{n} still waiting.\n"),
        });
        s
    }
}

/// Hides `entries` until they change. Nothing is answered, retried or
/// dropped, and repeating it changes nothing.
pub fn dismiss(repo: &Repo, entries: Vec<InboxEntry>) -> Result<Response> {
    let done = crate::journal::dismissed(&repo.hall.root);
    let keys: Vec<String> = entries
        .iter()
        .map(|e| e.key())
        .filter(|k| !done.contains(k))
        .collect();
    if !keys.is_empty() {
        repo.hall.append(
            &repo.hall.dismissed_events(),
            &crate::event::Event::new(
                crate::repo::actor(),
                None,
                crate::event::Body::InboxDismissed { keys: keys.clone() },
            ),
        )?;
    }
    let dismissed: Vec<InboxEntry> = entries
        .into_iter()
        .filter(|e| keys.contains(&e.key()))
        .collect();
    let remaining = repo.index()?.inbox()?.len();
    let next = if remaining > 0 {
        vec!["hall inbox".to_string(), "hall inbox --all".into()]
    } else {
        vec!["hall inbox --all".to_string()]
    };
    Ok(Response::new(
        DismissOut {
            dismissed,
            remaining,
        },
        next,
    ))
}

/// The entry `which` names: its number, a question ID or an item ID.
fn find_entry(entries: &[InboxEntry], which: &str) -> Result<usize> {
    let w = which.trim();
    w.parse::<usize>()
        .ok()
        .and_then(|n| n.checked_sub(1))
        .filter(|n| *n < entries.len())
        .or_else(|| {
            entries
                .iter()
                .position(|e| e.qid.as_deref().is_some_and(|q| q.eq_ignore_ascii_case(w)))
        })
        .or_else(|| {
            let id: crate::item::ItemId = w.parse().ok()?;
            entries.iter().position(|e| e.item == Some(id))
        })
        .ok_or_else(|| {
            HallError::new(
                ErrorCode::ItemNotFound,
                format!("nothing in the inbox matches {w}"),
            )
            .next(["hall inbox"])
        })
}

/// One inbox entry, explained.
#[derive(Debug, Serialize, JsonSchema)]
pub struct InboxDetail {
    /// Its place in `hall inbox` (1 is the oldest).
    pub number: usize,
    #[serde(flatten)]
    pub entry: InboxEntry,
    /// Why it waits for you.
    pub why: String,
    /// What it rests on: the question and options, findings, failing checks.
    pub evidence: Vec<String>,
    /// What you can do, and what each does.
    pub actions: Vec<crate::now::Action>,
    /// The item's state now, when the entry is about an item.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub now: Option<crate::now::Now>,
}

impl Render for InboxDetail {
    fn render(&self) -> String {
        let who = self.entry.item.map(|i| format!(" {i}")).unwrap_or_default();
        let mut s = format!(
            "⚑ {}. {}{who} · waiting {}

{}

{}
",
            self.number,
            self.entry.kind,
            age(self.entry.since),
            self.entry.text,
            self.why
        );
        if !self.evidence.is_empty() {
            s.push('\n');
            for e in &self.evidence {
                s.push_str(&format!("  {e}\n"));
            }
        }
        s.push_str("\nWhat you can do:\n");
        for a in &self.actions {
            s.push_str(&format!("  {:44} {}\n", a.command, a.does));
        }
        s
    }
}

fn inbox_detail(
    repo: &Repo,
    ix: &Index,
    entries: Vec<InboxEntry>,
    which: &str,
) -> Result<Response> {
    let pos = find_entry(&entries, which)?;
    let out = explain(repo, ix, entries[pos].clone(), pos + 1)?;
    let actions = out.actions.clone();
    let next = actions.into_iter().map(|a| a.command).collect();
    Ok(Response::new(out, next))
}

/// One inbox entry explained: why it waits, the evidence, the actions.
/// `number` is its place in `hall inbox`.
pub fn explain(repo: &Repo, ix: &Index, entry: InboxEntry, number: usize) -> Result<InboxDetail> {
    let item = entry.item.and_then(|id| repo.hall.load_item(id).ok());
    let now = match &item {
        Some(i) => {
            let events = repo.hall.read_item(i.id)?;
            Some(crate::now::now(repo, i, &ix.items(false)?, &events))
        }
        None => None,
    };
    let act = |c: &str, d: &str| crate::now::Action {
        command: c.to_string(),
        does: d.to_string(),
    };
    let (why, mut evidence, actions): (String, Vec<String>, Vec<crate::now::Action>) = match entry
        .kind
        .as_str()
    {
        "question" => {
            let qid = entry.qid.clone().unwrap_or_default();
            let q = ix.question(&qid)?;
            let mut ev = vec![format!("question: {}", entry.text)];
            let mut acts = Vec::new();
            for (i, o) in entry.options.iter().enumerate() {
                let letter = (b'a' + i as u8) as char;
                ev.push(format!("[{letter}] {o}"));
                acts.push(act(
                    &format!("hall answer {qid} {letter}"),
                    &format!("answer \"{o}\"; the asking agent resumes with it"),
                ));
            }
            if acts.is_empty() {
                acts.push(act(
                    &format!("hall answer {qid} \"<answer>\""),
                    "answer in your words; the asking agent resumes with it",
                ));
            }
            if let Some(d) = &entry.default {
                let when = q
                    .and_then(|q| q.deadline)
                    .map(|t| format!(" at {}", t.format("%H:%M UTC")))
                    .unwrap_or_default();
                ev.push(format!("default: {d}, used if nobody answers{when}"));
            }
            (
                    "An agent asked you this and waits for the answer; its session resumes once you answer.".into(),
                    ev,
                    acts,
                )
        }
        "session_waiting" => (
            "A Claude Code session waits for you: a permission prompt, or a reply.".into(),
            vec![],
            entry
                .answer_with
                .iter()
                .map(|c| {
                    act(
                        c,
                        "you run this yourself in a terminal to open that session; Claude can't",
                    )
                })
                .collect(),
        ),
        "daemon_stopped" => (
            "The daemon should run but has no fresh heartbeat, so nothing moves through the loop."
                .into(),
            vec![],
            vec![
                act("hall daemon start", "start it again"),
                act("hall daemon status", "see what it last did"),
            ],
        ),
        kind => {
            let why = match kind {
                "ready" => {
                    "The agent says the work looks done. It goes to review only when you say so."
                }
                "escalated" => {
                    "The review rejected it too many times; the loop stopped and waits for your decision."
                }
                "budget" => "It reached its budget, so the loop stopped working on it.",
                "run_failed" => {
                    "A model run failed twice in a row, so the loop stopped working on it."
                }
                "ship_failed" => "The ship command failed, so it hasn't been pushed.",
                "feedback" => {
                    "Its PR has new comments, and the Delivery office's dev run didn't change \
                     anything for them (questions, or it disagrees). Reply on the PR, or address \
                     them in a session and `hall ready`; `hall retry` lets the office try again."
                }
                "conflict" => {
                    "Its branch conflicts with its base, and the Delivery office's headless rebase \
                     couldn't resolve it. Resolve it in a session (rebase in its worktree), then \
                     `hall ready`; or `hall retry` to let the Delivery office try again."
                }
                "stuck" => "Nothing has happened to it for longer than `stuck_after`.",
                "ci_failed" => "The shipped PR's CI failed after the review approved it.",
                _ => "It needs your decision.",
            };
            let acts = match &now {
                Some(n) => n.actions.clone(),
                None => entry
                    .answer_with
                    .iter()
                    .map(|c| act(c, "settle it"))
                    .collect(),
            };
            (why.into(), vec![], acts)
        }
    };
    if let Some(n) = &now {
        evidence.push(format!("now: {}", n.doing));
        evidence.extend(n.detail.iter().cloned());
    }
    // Always last, so the numbered actions above keep their places.
    let mut actions = actions;
    // By ID where one names it, since numbers shift as entries come and go.
    let target = entry
        .qid
        .clone()
        .or_else(|| entry.item.map(|i| i.to_string()))
        .unwrap_or_else(|| number.to_string());
    actions.push(act(
        &format!("hall inbox {target} --dismiss"),
        "dismiss it: hidden until it changes; nothing is answered or retried",
    ));
    Ok(InboxDetail {
        number,
        entry,
        why,
        evidence,
        actions,
        now,
    })
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct UsageOut {
    pub by: UsageBy,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<DateTime<Utc>>,
    pub rows: Vec<UsageRow>,
    pub total: UsageRow,
    /// Costs are API list-price equivalents, not a bill.
    pub cost_basis: String,
}

impl Render for UsageOut {
    fn render(&self) -> String {
        let mut s = format!(
            "{:<22} {:>8} {:>9} {:>9}\n",
            format!("{:?}", self.by).to_lowercase(),
            "runs",
            "tokens",
            "≈ $"
        );
        for r in self.rows.iter().chain(std::iter::once(&self.total)) {
            s.push_str(&format!(
                "{:<22} {:>8} {:>9} {:>9}\n",
                clip(&r.key, 22),
                r.records,
                fmt_tokens(r.tokens),
                crate::usage::fmt_usd(r.cost_usd).trim_start_matches('$')
            ));
        }
        s.push_str("\nCosts are API list-price equivalents, not a bill.\n");
        s
    }
}

pub fn usage(cwd: &Path, a: UsageArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    repo.require_init()?;
    let since = match &a.since {
        Some(s) => {
            let secs = crate::config::parse_duration(s).ok_or_else(|| {
                HallError::new(
                    ErrorCode::Usage,
                    format!("--since {s}: use a duration like 7d or 24h"),
                )
            })?;
            Some(Utc::now() - Duration::seconds(secs))
        }
        None => None,
    };
    let item = a.item.as_deref().map(str::parse).transpose()?;
    let ix = repo.index()?;
    let rows = ix.usage(a.by, since, item)?;
    let mut total = UsageRow {
        key: "total".into(),
        ..Default::default()
    };
    for r in &rows {
        total.records += r.records;
        total.requests += r.requests;
        total.input_tokens += r.input_tokens;
        total.output_tokens += r.output_tokens;
        total.cache_read_tokens += r.cache_read_tokens;
        total.cache_write_tokens += r.cache_write_tokens;
        total.tokens += r.tokens;
        total.cost_usd += r.cost_usd;
    }
    let out = UsageOut {
        by: a.by,
        since,
        rows,
        total,
        cost_basis: "list".into(),
    };
    Ok(Response::new(
        out,
        vec![
            "hall usage --by item".into(),
            "hall usage --by model --since 7d".into(),
        ],
    ))
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ReindexOut {
    pub items: usize,
    pub events: u64,
    pub usage_records: u64,
    pub path: String,
}

impl Render for ReindexOut {
    fn render(&self) -> String {
        format!(
            "Rebuilt the index from the journal: {} items, {} events, {} usage records.\n",
            self.items, self.events, self.usage_records
        )
    }
}

pub fn reindex(cwd: &Path) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    repo.require_init()?;
    let ix = Index::rebuild(&repo.hall)?;
    let ids = repo.hall.item_ids()?;
    for id in &ids {
        repo.hall.refresh_item_json(*id)?;
    }
    let out = ReindexOut {
        items: ids.len(),
        events: ix.event_count()?,
        usage_records: ix.usage_count()?,
        path: crate::paths::display(&repo.hall.index_path()),
    };
    Ok(Response::new(out, vec!["hall status --all".into()]))
}
