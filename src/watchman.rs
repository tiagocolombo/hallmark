//! The watchman (workstream A): flags items with no event for
//! `stuck_after`, and a daemon that stopped.

use crate::error::Result;
use crate::worker::{Progress, WorkerCtx};

pub fn tick(_ctx: &WorkerCtx) -> Result<Progress> {
    Ok(Progress::Idle)
}
