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
}

impl Render for StatusItem {
    fn render(&self) -> String {
        let mut s = self.item.render();
        s.push_str(&format!(
            "  usage     {} tokens · ${:.2}\n",
            fmt_tokens(self.usage.tokens),
            self.usage.cost_usd
        ));
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
        let out = StatusItem {
            item: ItemView::from(&item),
            usage,
            recent,
        };
        return Ok(Response::new(out, next_for(&item)));
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

pub fn inbox(cwd: &Path) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    repo.require_init()?;
    let entries = repo.index()?.inbox()?;
    let next: Vec<String> = entries
        .iter()
        .flat_map(|e| e.answer_with.first().cloned())
        .take(5)
        .collect();
    Ok(Response::new(InboxOut { entries }, next))
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
            "requests",
            "tokens",
            "≈ $"
        );
        for r in self.rows.iter().chain(std::iter::once(&self.total)) {
            s.push_str(&format!(
                "{:<22} {:>8} {:>9} {:>9.2}\n",
                clip(&r.key, 22),
                r.requests,
                fmt_tokens(r.tokens),
                r.cost_usd
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
