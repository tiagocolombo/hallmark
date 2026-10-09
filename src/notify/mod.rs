//! Notifications (workstream D): desktop, ntfy, Telegram, Slack, the
//! morning digest, quiet hours and reminders.

use crate::error::Result;
use crate::worker::{Progress, WorkerCtx};

/// Sends what is new or due in this repo's inbox.
pub fn tick(_ctx: &WorkerCtx) -> Result<Progress> {
    Ok(Progress::Idle)
}
