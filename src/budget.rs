//! Budgets at four levels (workstream D, ADR 0016): per run, per item, per
//! room per day, and the daily total.

use schemars::JsonSchema;
use serde::Serialize;

use crate::item::ItemId;
use crate::repo::Repo;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// `max_usd` and the timeout (enforced by the runner itself).
    Run,
    /// `per_item`: the item is held for you as "budget".
    Item,
    /// `[budget.<role>] daily`: the room closes for the day.
    Room,
    /// `daily`: the daemon pauses.
    Daily,
}

/// A cap that stops a run before it starts.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Stop {
    pub level: Level,
    pub reason: String,
}

impl Stop {
    /// What a worker reports when this stop blocks it: the daily total
    /// pauses the daemon until tomorrow (UTC); the others just mean "not
    /// now" for this item or room.
    pub fn progress(&self) -> crate::worker::Progress {
        match self.level {
            Level::Daily => {
                let tomorrow = (chrono::Utc::now() + chrono::Duration::days(1))
                    .date_naive()
                    .and_hms_opt(0, 0, 0)
                    .map(|t| t.and_utc());
                crate::worker::Progress::Pause {
                    reason: self.reason.clone(),
                    until: tomorrow,
                }
            }
            _ => crate::worker::Progress::worked(format!("budget: {}", self.reason)),
        }
    }
}

/// May a `role` run for `item` start now?
pub fn check(_repo: &Repo, _item: Option<ItemId>, _role: &str) -> Result<(), Stop> {
    Ok(())
}
