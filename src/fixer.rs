//! The Repair Shop (workstream C): runs the fixer in an item's worktree.

use crate::error::Result;
use crate::worker::{Progress, WorkerCtx};

pub fn tick(_ctx: &WorkerCtx) -> Result<Progress> {
    Ok(Progress::Idle)
}
