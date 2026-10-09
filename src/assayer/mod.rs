//! The Assay Office (workstream B): runs the gates at the frozen commit in
//! the assayer's own worktree, then the principal engineer's review.

use crate::error::Result;
use crate::worker::{Progress, WorkerCtx};

/// Takes one ticket (one at a time per repo) and moves it one step:
/// checks, or review.
pub fn tick(_ctx: &WorkerCtx) -> Result<Progress> {
    Ok(Progress::Idle)
}
