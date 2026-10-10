//! The Library (ADR 0021): learnings in `.hallmark/knowledge/`, search,
//! briefings for reviews, the librarian and retirement.
//!
//! - `learning`: the file format (frontmatter + title + body).
//! - `store`: reading learnings from the base branch and from open
//!   knowledge items; usage from verdict events.
//! - `search`: SQLite FTS5 (built in) or qmd.
//! - `briefing`: what a review is told.
//! - `knowledge`: knowledge items, which carry learnings through Hallmark.
//! - `librarian`: distilling items and proposing retirements.

pub mod briefing;
pub mod knowledge;
pub mod learning;
pub mod librarian;
pub mod search;
pub mod store;

use schemars::JsonSchema;
use serde::Serialize;

use crate::error::Result;
use crate::repo::Repo;
use crate::worker::{Progress, WorkerCtx};

/// What the reviewer is told before a review, within `briefing_limit`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, JsonSchema)]
pub struct Briefing {
    /// Markdown for the review prompt; empty when there is nothing to say.
    pub text: String,
    /// The learnings included (`L-0017`, …).
    pub ids: Vec<String>,
    /// Estimated tokens.
    pub tokens: u64,
}

/// The briefing for a review of `changed` paths at `base` (learnings are
/// read from the base branch, never the branch under review). `base` is a
/// branch or a commit. Only accepted learnings are used: first those whose
/// scope matches a changed path, then a search with terms from `diff`.
pub fn briefing(repo: &Repo, base: &str, changed: &[String], diff: &str) -> Result<Briefing> {
    briefing::briefing(repo, base, changed, diff)
}

/// Distils closed and escalated items into proposed learnings.
pub fn librarian_tick(ctx: &WorkerCtx) -> Result<Progress> {
    librarian::tick(ctx)
}
