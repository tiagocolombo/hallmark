//! The Library (ADR 0021, ADR 0027): learnings kept in the local store next
//! to the journal, search, briefings for reviews, the librarian and
//! retirement.
//!
//! - `learning`: the file format (frontmatter + title + body).
//! - `store`: the local store (`<git-common-dir>/hallmark/library/`), its
//!   history and lock; usage from verdict events.
//! - `search`: SQLite FTS5 (built in) or qmd.
//! - `briefing`: what a review is told.
//! - `librarian`: distilling items and retiring learnings.
//! - `portable`: `hall library import` and `export`.

pub mod briefing;
pub mod learning;
pub mod librarian;
pub mod portable;
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

/// The briefing for a review of `changed` paths. Learnings come from the
/// local store, which no branch can change, and only accepted ones are
/// used: first those whose scope matches a changed path, then a search with
/// terms from `diff`. The cap follows the review's `effort` (2k for `low`,
/// 4k for `medium`, `briefing_limit` otherwise).
pub fn briefing(
    repo: &Repo,
    changed: &[String],
    diff: &str,
    effort: Option<&str>,
) -> Result<Briefing> {
    briefing::briefing(repo, changed, diff, effort)
}

/// Distils closed and escalated items into proposed learnings.
pub fn librarian_tick(ctx: &WorkerCtx) -> Result<Progress> {
    librarian::tick(ctx)
}
