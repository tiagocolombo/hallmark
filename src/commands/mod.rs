//! One module per group of commands. Each returns a [`Response`] whose data
//! type is also registered in `meta::schema`.

pub mod ask;
pub mod budget;
pub mod daemon;
pub mod doctor;
pub mod fix;
pub mod flow;
pub mod hud;
pub mod init;
pub mod library;
pub mod merge;
pub mod meta;
pub mod new;
pub mod next;
pub mod notify;
pub mod record;
pub mod review;
pub mod setup;
pub mod status;
pub mod update;
pub mod watch;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;

use crate::event::short;
use crate::item::{Issue, Item, ItemId, State};
use crate::output::Render;

/// An item as commands report it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ItemView {
    pub id: ItemId,
    pub title: String,
    pub state: State,
    /// The room this state happens in.
    pub room: String,
    pub branch: String,
    pub worktree: String,
    pub base: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<Issue>,
    pub round: u32,
    pub rejects: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hallmark: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drop_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub state_since: DateTime<Utc>,
}

impl From<&Item> for ItemView {
    fn from(i: &Item) -> Self {
        ItemView {
            id: i.id,
            title: i.title.clone(),
            state: i.state,
            room: i.state.room().into(),
            branch: i.branch.clone(),
            worktree: i.worktree.clone(),
            base: i.base.clone(),
            issue: i.issue.clone(),
            round: i.round,
            rejects: i.rejects,
            sha: i.frozen_sha.clone(),
            hallmark: i.hallmark_sha.clone(),
            pr_url: i.pr_url.clone(),
            summary: i.summary.clone(),
            drop_reason: i.drop_reason.clone(),
            created_at: i.created_at,
            state_since: i.state_since,
        }
    }
}

impl Render for ItemView {
    fn render(&self) -> String {
        let mut s = format!("{} · {}\n", self.id, self.title);
        s.push_str(&format!(
            "  state     {} ({})\n",
            self.state.label(),
            self.room
        ));
        if self.round > 0 {
            s.push_str(&format!(
                "  round     {} · rejected {}\n",
                self.round, self.rejects
            ));
        }
        if let Some(sha) = &self.sha {
            s.push_str(&format!("  commit    {}\n", short(sha)));
        }
        if let Some(h) = &self.hallmark {
            s.push_str(&format!("  hallmark  {}\n", short(h)));
        }
        s.push_str(&format!("  branch    {}\n", self.branch));
        s.push_str(&format!("  worktree  {}\n", self.worktree));
        if let Some(i) = &self.issue {
            s.push_str(&format!("  issue     #{}\n", i.number));
        }
        if let Some(p) = &self.pr_url {
            s.push_str(&format!("  PR        {p}\n"));
        }
        if let Some(r) = &self.drop_reason {
            s.push_str(&format!("  dropped   \"{r}\"\n"));
        }
        s
    }
}

/// The commands that make sense for an item in its current state.
pub fn next_for(item: &Item) -> Vec<String> {
    let id = item.id;
    // A parallel part merges into its parent instead (ADR 0024).
    let child = merge::child_next(item);
    let mut n: Vec<String> = match item.state {
        _ if child.is_some() => child.unwrap_or_default(),
        State::Development => vec![
            format!("hall propose-ready {id} --summary \"<what was done>\""),
            format!("hall ready {id}"),
            format!("hall drop {id} --reason \"<why>\""),
        ],
        State::AwaitingYou => vec![format!("hall ready {id}"), format!("hall pull-back {id}")],
        State::Checks | State::Review | State::Fixing => {
            vec![format!("hall status {id}"), format!("hall pull-back {id}")]
        }
        State::Escalated => vec![
            format!("hall log {id} --kind verdict"),
            format!("hall retry {id} --reason \"<why>\""),
            format!("hall pull-back {id}"),
            format!("hall drop {id} --reason \"<why>\""),
        ],
        State::Hallmarked => vec![format!("hall status {id}")],
        State::Closed | State::Dropped => vec![format!("hall log {id}")],
    };
    n.push(format!("hall log {id}"));
    n.dedup();
    n
}

/// "12m", "3h 05m", "2d".
pub fn age(since: DateTime<Utc>) -> String {
    age_secs((Utc::now() - since).num_seconds())
}

/// A duration in seconds, as `age` writes it.
pub fn age_secs(s: i64) -> String {
    match s.max(0) {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
        s => format!("{}d", s / 86_400),
    }
}

/// A lowercase slug for branch names: whole words, at most 40 characters.
pub fn slug(title: &str) -> String {
    let mut s = String::new();
    let words = title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase);
    for w in words {
        let extra = if s.is_empty() { w.len() } else { w.len() + 1 };
        if s.len() + extra > 40 {
            if s.is_empty() {
                s = w[..40].to_string();
            }
            break;
        }
        if !s.is_empty() {
            s.push('-');
        }
        s.push_str(&w);
    }
    if s.is_empty() { "work".into() } else { s }
}

#[cfg(test)]
mod tests {
    #[test]
    fn slugs() {
        assert_eq!(
            super::slug("Retry failed webhook deliveries!"),
            "retry-failed-webhook-deliveries"
        );
        assert_eq!(super::slug("  Fix: émoji 🎉 bug  "), "fix-moji-bug");
        assert_eq!(
            super::slug("Retry failed webhook deliveries with backoff"),
            "retry-failed-webhook-deliveries-with"
        );
        assert_eq!(super::slug("???"), "work");
        assert!(super::slug(&"long words ".repeat(20)).len() <= 40);
    }
}
