//! The Dispatch Dock (workstream C): ships hallmarked items.

use crate::error::Result;
use crate::worker::{Progress, WorkerCtx};

pub fn tick(_ctx: &WorkerCtx) -> Result<Progress> {
    Ok(Progress::Idle)
}
