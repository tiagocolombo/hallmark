//! Answers reach the asking session (workstream D): `claude -p --resume`,
//! and defaults after a question's wait expires.

use crate::error::Result;
use crate::worker::{Progress, WorkerCtx};

pub fn tick(_ctx: &WorkerCtx) -> Result<Progress> {
    Ok(Progress::Idle)
}
