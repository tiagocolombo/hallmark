//! The Library (workstream E): learnings in `.hallmark/knowledge/`, search,
//! the librarian, briefings and retirement.

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
/// read from the base branch, never the branch under review).
pub fn briefing(_repo: &Repo, _base: &str, _changed: &[String], _diff: &str) -> Result<Briefing> {
    Ok(Briefing::default())
}

/// Distils closed and escalated items into proposed learnings.
pub fn librarian_tick(_ctx: &WorkerCtx) -> Result<Progress> {
    Ok(Progress::Idle)
}
