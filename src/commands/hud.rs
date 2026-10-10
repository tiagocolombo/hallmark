//! `hall hud`: everything the Claude Code heads-up display shows, in one
//! cheap call. It reads the journal, the index and the daemon's state only.

use std::path::Path;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::HudArgs;
use crate::error::Result;
use crate::findings::Counts;
use crate::index::UsageBy;
use crate::item::{Item, ItemId, State};
use crate::output::{Render, Response, clip};
use crate::repo::Repo;
use crate::usage::{fmt_tokens, fmt_usd};

#[derive(Debug, Serialize, JsonSchema)]
pub struct HudOut {
    pub repo: String,
    /// The item this directory (or session) works on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<HudItem>,
    /// Items the loop is working on: checks, review, fixing, hallmarked.
    pub loop_items: Vec<HudItem>,
    /// Everything waiting for you.
    pub inbox: usize,
    /// The oldest few entries of the inbox.
    pub needs_you: Vec<HudEntry>,
    pub daemon: HudDaemon,
    /// The last lines of this repo's Live log, oldest first.
    pub live: Vec<String>,
    pub today: HudToday,
    /// One line, as the band above Claude Code's prompt shows it.
    pub line: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct HudItem {
    pub id: ItemId,
    pub title: String,
    pub state: State,
    pub round: u32,
    pub rejects: u32,
    /// Open findings by severity.
    pub open: Counts,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold: Option<String>,
    pub stuck: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<ItemId>,
    pub state_since: DateTime<Utc>,
    /// What it is doing now, and for how long ("Checks, round 1: running
    /// gate 2/3 … (1m)").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub now: Option<String>,
}

impl From<&Item> for HudItem {
    fn from(i: &Item) -> Self {
        HudItem {
            id: i.id,
            title: i.title.clone(),
            state: i.state,
            round: i.round,
            rejects: i.rejects,
            open: i.findings,
            hold: i
                .hold
                .as_ref()
                .map(|h| format!("{}: {}", h.kind.as_str(), h.reason)),
            stuck: i.stuck_since.is_some(),
            parent: i.parent,
            state_since: i.state_since,
            now: None,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct HudEntry {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
    pub text: String,
    /// The command that settles it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer_with: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct HudDaemon {
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paused: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<DateTime<Utc>>,
    /// What it is doing now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct HudToday {
    /// Review-loop tokens today (UTC), what `[budget] daily` caps.
    pub loop_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daily_cap: Option<u64>,
    /// Every token today, your sessions included.
    pub tokens: u64,
    pub cost_usd: f64,
}

impl Render for HudOut {
    fn render(&self) -> String {
        let mut s = format!("{}\n", self.line);
        for i in &self.loop_items {
            s.push_str(&format!(
                "  {} {:10} r{}  {}\n",
                i.id,
                i.state.as_str(),
                i.round,
                clip(&i.title, 50)
            ));
        }
        for e in &self.needs_you {
            s.push_str(&format!("  ⚑ {} {}\n", e.kind, clip(&e.text, 60)));
        }
        for l in &self.live {
            s.push_str(&format!("  │ {}\n", clip(l, 90)));
        }
        s
    }
}

pub fn hud(cwd: &Path, a: HudArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    repo.require_init()?;
    let ix = repo.index()?;
    let items = ix.items(false)?;
    let mine = repo.item_at(&items, &repo.cwd).or_else(|| {
        let bound = repo.hall.session(a.session.as_deref()?).item?;
        items.iter().find(|i| i.id == bound).cloned()
    });
    let loop_items: Vec<HudItem> = items
        .iter()
        .filter(|i| {
            i.parent.is_none()
                && matches!(
                    i.state,
                    State::Checks | State::Review | State::Fixing | State::Hallmarked
                )
        })
        .map(|i| with_now(&repo, i, &items))
        .collect();
    let inbox = ix.inbox()?;
    let needs_you: Vec<HudEntry> = inbox
        .iter()
        .take(3)
        .map(|e| HudEntry {
            kind: e.kind.clone(),
            item: e.item,
            text: e.text.clone(),
            answer_with: e.answer_with.first().cloned(),
        })
        .collect();

    let state = crate::daemon::state::load();
    let paused = state.as_ref().and_then(|s| s.paused.clone());
    let daemon = HudDaemon {
        running: crate::daemon::state::instance_running(),
        paused: paused.as_ref().map(|p| p.reason.clone()),
        until: paused.and_then(|p| p.until),
        current: state.and_then(|s| s.current),
    };

    let midnight = Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .map(|t| t.and_utc());
    let all_today = ix.usage(UsageBy::Day, midnight, None)?;
    let today = HudToday {
        loop_tokens: crate::budget::daily_tokens(&ix, &crate::budget::today()),
        daily_cap: repo
            .config
            .budget
            .daily
            .as_deref()
            .and_then(crate::config::parse_tokens),
        tokens: all_today.iter().map(|r| r.tokens).sum(),
        cost_usd: all_today.iter().map(|r| r.cost_usd).sum(),
    };

    let live = tail(&repo.hall.live_log(), a.lines);
    let item = mine.as_ref().map(|i| with_now(&repo, i, &items));
    let line = line(
        &repo.name,
        item.as_ref(),
        &loop_items,
        inbox.len(),
        &daemon,
        &today,
    );
    let out = HudOut {
        repo: repo.name.clone(),
        item,
        loop_items,
        inbox: inbox.len(),
        needs_you,
        daemon,
        live,
        today,
        line,
    };
    let mut next = vec!["hall inbox".to_string()];
    if let Some(i) = &mine {
        next.push(format!("hall status {}", i.id));
    }
    Ok(Response::new(out, next))
}

/// An item for the HUD, with its `now` line.
fn with_now(repo: &Repo, i: &Item, items: &[Item]) -> HudItem {
    let mut h = HudItem::from(i);
    if let Ok(events) = repo.hall.read_item(i.id) {
        let n = crate::now::now(repo, i, items, &events);
        h.now = Some(format!("{} ({})", n.doing, n.elapsed));
    }
    h
}

/// "hallmark · H-0042 review r2 · daemon: reviewing H-0042 · ⚑ 2 · 1.2M/4M today"
fn line(
    repo: &str,
    item: Option<&HudItem>,
    loop_items: &[HudItem],
    inbox: usize,
    daemon: &HudDaemon,
    today: &HudToday,
) -> String {
    let mut parts = vec![repo.to_string()];
    if let Some(i) = item {
        let mut p = format!("{} {}", i.id, i.state.as_str());
        if i.round > 0 {
            p.push_str(&format!(" r{}", i.round));
        }
        if i.open.blocker + i.open.major > 0 {
            p.push_str(&format!(" · {}B {}M open", i.open.blocker, i.open.major));
        }
        if let Some(h) = &i.hold {
            p.push_str(&format!(" · held ({})", h.split(':').next().unwrap_or("")));
        }
        parts.push(p);
    }
    parts.push(match (daemon.running, &daemon.paused, &daemon.current) {
        (false, _, _) => "daemon stopped".into(),
        (true, Some(r), _) => format!("daemon paused: {}", clip(r, 30)),
        (true, None, Some(c)) if !c.is_empty() => format!("daemon: {}", clip(c, 40)),
        (true, None, _) if !loop_items.is_empty() => format!("{} in the loop", loop_items.len()),
        (true, None, _) => "daemon idle".into(),
    });
    if inbox > 0 {
        parts.push(format!("⚑ {inbox} need you"));
    }
    let spent = match today.daily_cap {
        Some(cap) => format!(
            "{}/{} today",
            fmt_tokens(today.loop_tokens),
            fmt_tokens(cap)
        ),
        None => format!("{} today", fmt_usd(today.cost_usd)),
    };
    parts.push(spent);
    parts.join(" · ")
}

/// The last `n` lines of a file; empty when it doesn't exist.
fn tail(path: &Path, n: usize) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(16 * 1024);
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(start)).is_err() || f.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let skip = usize::from(start > 0); // the first line may be cut
    lines[skip.min(lines.len())..]
        .iter()
        .rev()
        .take(n)
        .rev()
        .map(|l| l.to_string())
        .collect()
}
