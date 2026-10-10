//! Budgets at four levels (workstream D, ADR 0016, ADR 0020): per run, per
//! item, per room per day, and the daily total.
//!
//! - **What counts.** Only the loop's own model runs: usage recorded with
//!   source `headless` (every `claude -p` Hallmark starts: reviewer, fixer,
//!   intake, librarian, resume). Your interactive Workshop sessions
//!   (source `transcript`) are measured but never capped, so `per_item`
//!   means "what the loop spent on this item".
//! - **Today** is the UTC date, the same `day` column the index writes for
//!   every usage record; a daily cap resets at 00:00 UTC, which is also when
//!   [`Stop::progress`] lets a paused daemon go again.
//! - **Reached** means spent ≥ cap: the check runs before a run starts, so
//!   the run that crosses a cap finishes and the next one waits.
//! - **Daily grants.** `hall budget --grant` lets the loop go past `daily`
//!   for today without touching the config: today's cap is `daily` plus the
//!   latest `budget_granted` for today in the repo journal (ADR 0028).

use rusqlite::params;
use schemars::JsonSchema;
use serde::Serialize;

use crate::config::{Budget, Role, parse_tokens};
use crate::index::Index;
use crate::item::ItemId;
use crate::repo::Repo;
use crate::usage::fmt_tokens;

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

/// The UTC date budgets count against, as the index writes it (`usage.day`).
pub fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// A run role (`reviewer`, `fixer`, …, or the config's `review`, `fix`) as a
/// budgeted role. `resume` and anything else have no role caps.
pub fn role_of(role: &str) -> Option<Role> {
    match role {
        "reviewer" | "review" => Some(Role::Reviewer),
        "fixer" | "fix" => Some(Role::Fixer),
        "intake" => Some(Role::Intake),
        "librarian" => Some(Role::Librarian),
        _ => None,
    }
}

/// The `[budget.<key>]` table name for a role.
pub fn table_key(r: Role) -> &'static str {
    match r {
        Role::Reviewer => "review",
        Role::Fixer => "fix",
        Role::Intake => "intake",
        Role::Librarian => "librarian",
    }
}

/// The room's name as the building calls it.
pub fn room_label(room: &str) -> &'static str {
    match room {
        "assay" => "Assay Office",
        "repair" => "Repair Shop",
        "front" => "Front Desk",
        "library" => "Library",
        "workshop" => "Workshop",
        _ => "room",
    }
}

pub const ROLES: [Role; 4] = [Role::Reviewer, Role::Fixer, Role::Intake, Role::Librarian];

/// May a `role` run for `item` start now?
///
/// Checked in order: the daily total (the daemon pauses), the role's room
/// for today, then the item. Resume runs only answer to the daily total:
/// delivering your answer never holds an item. An unreadable index fails
/// open (logged to the Live view), like the gates.
pub fn check(repo: &Repo, item: Option<ItemId>, role: &str) -> Result<(), Stop> {
    let b = &repo.config.budget;
    let any_cap = b.daily.is_some()
        || b.per_item.is_some()
        || ROLES
            .iter()
            .any(|r| b.role(*r).and_then(|rb| rb.daily.as_ref()).is_some());
    if !any_cap {
        return Ok(());
    }
    let ix = match repo.index() {
        Ok(ix) => ix,
        Err(e) => {
            repo.hall
                .live(&format!("budget check skipped: the index failed: {e}"));
            return Ok(());
        }
    };
    check_with(&ix, b, item, role, &today())
}

/// [`check`] against an open index and a given UTC day.
pub fn check_with(
    ix: &Index,
    b: &Budget,
    item: Option<ItemId>,
    role: &str,
    day: &str,
) -> Result<(), Stop> {
    if let Some(stop) = daily_stop(ix, b, day) {
        return Err(stop);
    }
    let Some(r) = role_of(role) else {
        return Ok(());
    };
    if let Some(cap) = b.role(r).and_then(|rb| rb.daily.as_deref())
        && let Some(limit) = parse_tokens(cap)
    {
        let used = room_tokens(ix, r.room(), day);
        if used >= limit {
            return Err(Stop {
                level: Level::Room,
                reason: format!(
                    "the {} reached its daily cap ({} of {} tokens today); {} runs wait until tomorrow (UTC)",
                    room_label(r.room()),
                    fmt_tokens(used),
                    cap,
                    r.as_str()
                ),
            });
        }
    }
    if let (Some(id), Some(cap)) = (item, b.per_item.as_deref())
        && let Some(limit) = parse_tokens(cap)
    {
        let used = item_tokens(ix, id);
        if used >= limit {
            return Err(Stop {
                level: Level::Item,
                reason: format!(
                    "{id} reached its {cap} token cap ({} spent by the loop)",
                    fmt_tokens(used)
                ),
            });
        }
    }
    Ok(())
}

/// How a daily-budget stop's reason starts; a pause with this reason is the
/// one `hall budget --grant` lifts.
pub const DAILY_SPENT: &str = "the daily budget is spent";

/// The daily total's stop, if `[budget] daily` (plus today's grant) is
/// spent for `day`.
pub fn daily_stop(ix: &Index, b: &Budget, day: &str) -> Option<Stop> {
    let cap = b.daily.as_deref()?;
    let limit = parse_tokens(cap)?;
    let granted = daily_granted(ix, day);
    let used = daily_tokens(ix, day);
    let of = match granted {
        0 => cap.to_string(),
        g => format!("{cap} + {} granted", fmt_tokens(g)),
    };
    (used >= limit.saturating_add(granted)).then(|| Stop {
        level: Level::Daily,
        reason: format!(
            "{DAILY_SPENT} ({} of {of} tokens today); the loop waits until 00:00 UTC, \
             or `hall budget --grant` lets it go on today",
            fmt_tokens(used)
        ),
    })
}

/// Tokens granted on top of `daily` for `day`: the latest grant that day.
pub fn daily_granted(ix: &Index, day: &str) -> u64 {
    ix.conn()
        .query_row(
            "SELECT json_extract(json, '$.tokens') FROM events
             WHERE event = 'budget_granted' AND json_extract(json, '$.day') = ?1
             ORDER BY id DESC LIMIT 1",
            params![day],
            |r| r.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten()
        .unwrap_or(0)
        .max(0) as u64
}

/// For hooks: a one-line note when the daily budget is spent, else None.
/// Never fails.
pub fn daily_note(repo: &Repo) -> Option<String> {
    repo.config.budget.daily.as_ref()?;
    let ix = repo.index().ok()?;
    let stop = daily_stop(&ix, &repo.config.budget, &today())?;
    Some(format!(
        "Hallmark: {}. This session isn't blocked; if the human wants the loop to go on \
         today, offer `hall budget --grant` (the cap stays as it is). `hall budget` shows the caps.",
        stop.reason
    ))
}

fn sum(ix: &Index, sql: &str, p: impl rusqlite::Params) -> u64 {
    ix.conn()
        .query_row(sql, p, |r| r.get::<_, Option<i64>>(0))
        .ok()
        .flatten()
        .unwrap_or(0)
        .max(0) as u64
}

const TOKENS: &str = "SUM(COALESCE(input,0) + COALESCE(output,0) + COALESCE(cache_read,0) + COALESCE(cache_write,0))";

/// Tokens the loop (headless runs) spent on `item`, ever.
pub fn item_tokens(ix: &Index, item: ItemId) -> u64 {
    sum(
        ix,
        &format!("SELECT {TOKENS} FROM usage WHERE item = ?1 AND source = 'headless'"),
        params![item.to_string()],
    )
}

/// Tokens the loop spent in `room` on `day` (UTC).
pub fn room_tokens(ix: &Index, room: &str, day: &str) -> u64 {
    sum(
        ix,
        &format!("SELECT {TOKENS} FROM usage WHERE room = ?1 AND day = ?2 AND source = 'headless'"),
        params![room, day],
    )
}

/// Tokens the whole loop spent on `day` (UTC).
pub fn daily_tokens(ix: &Index, day: &str) -> u64 {
    sum(
        ix,
        &format!("SELECT {TOKENS} FROM usage WHERE day = ?1 AND source = 'headless'"),
        params![day],
    )
}

/// Tokens your interactive sessions used on `day` (measured, not capped).
pub fn workshop_tokens(ix: &Index, day: &str) -> u64 {
    sum(
        ix,
        &format!("SELECT {TOKENS} FROM usage WHERE day = ?1 AND source = 'transcript'"),
        params![day],
    )
}

/// One item's loop spend.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ItemSpend {
    pub item: ItemId,
    pub tokens: u64,
    pub cost_usd: f64,
    /// Headless runs recorded.
    pub runs: u64,
}

/// The items the loop spent most on, most first.
pub fn top_items(ix: &Index, limit: usize) -> Vec<ItemSpend> {
    let sql = format!(
        "SELECT item, {TOKENS}, SUM(cost_usd), COUNT(*) FROM usage
         WHERE item IS NOT NULL AND source = 'headless'
         GROUP BY item ORDER BY 2 DESC, item LIMIT ?1"
    );
    let Ok(mut stmt) = ix.conn().prepare(&sql) else {
        return Vec::new();
    };
    let rows = stmt.query_map(params![limit as i64], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<i64>>(1)?.unwrap_or(0),
            r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
            r.get::<_, i64>(3)?,
        ))
    });
    let Ok(rows) = rows else {
        return Vec::new();
    };
    rows.flatten()
        .filter_map(|(item, tokens, cost, runs)| {
            Some(ItemSpend {
                item: item.parse().ok()?,
                tokens: tokens.max(0) as u64,
                cost_usd: cost,
                runs: runs.max(0) as u64,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RoleBudget;
    use crate::event::{Body, Event, Usage};
    use crate::journal::Hall;

    fn usage(room: &str, role: &str, source: &str, tokens: u64) -> Body {
        Body::Usage(Usage {
            source: source.into(),
            room: room.into(),
            role: role.into(),
            model: "m".into(),
            input_tokens: tokens,
            ..Default::default()
        })
    }

    fn hall() -> (tempfile::TempDir, Hall) {
        let dir = tempfile::tempdir().unwrap();
        let hall = Hall::new(dir.path());
        hall.create().unwrap();
        (dir, hall)
    }

    fn add(hall: &Hall, item: Option<ItemId>, body: Body) {
        let e = Event::new("hall", item, body);
        match item {
            Some(id) => hall.append(&hall.item_events(id), &e).unwrap(),
            None => hall.append(&hall.repo_events(), &e).unwrap(),
        }
    }

    #[test]
    fn levels_in_order() {
        let (_d, hall) = hall();
        let id = ItemId(1);
        add(
            &hall,
            Some(id),
            usage("assay", "reviewer", "headless", 500_000),
        );
        add(
            &hall,
            Some(id),
            usage("workshop", "dev", "transcript", 9_000_000),
        );
        let ix = Index::synced(&hall).unwrap();
        let day = today();
        let mut b = Budget::default();
        assert_eq!(check_with(&ix, &b, Some(id), "reviewer", &day), Ok(()));

        // The Workshop's transcript usage never counts.
        b.per_item = Some("800k".into());
        assert_eq!(check_with(&ix, &b, Some(id), "reviewer", &day), Ok(()));
        b.per_item = Some("500k".into());
        let s = check_with(&ix, &b, Some(id), "fixer", &day).unwrap_err();
        assert_eq!(s.level, Level::Item);
        assert!(s.reason.contains("H-0001 reached its 500k"), "{}", s.reason);
        // Resume runs never hold an item.
        assert_eq!(check_with(&ix, &b, Some(id), "resume", &day), Ok(()));

        b.review = Some(RoleBudget {
            daily: Some("400k".into()),
            ..Default::default()
        });
        let s = check_with(&ix, &b, Some(id), "reviewer", &day).unwrap_err();
        assert_eq!(s.level, Level::Room);
        assert!(s.reason.contains("Assay Office"), "{}", s.reason);
        // Another room is still open (down to the item cap).
        assert_eq!(
            check_with(&ix, &b, Some(id), "fixer", &day)
                .unwrap_err()
                .level,
            Level::Item
        );
        // Another day: rooms reopen, the item cap stays.
        assert_eq!(
            check_with(&ix, &b, Some(id), "reviewer", "1999-01-01")
                .unwrap_err()
                .level,
            Level::Item
        );

        b.daily = Some("0.5M".into());
        let s = check_with(&ix, &b, None, "resume", &day).unwrap_err();
        assert_eq!(s.level, Level::Daily);
        assert!(matches!(
            s.progress(),
            crate::worker::Progress::Pause { until: Some(_), .. }
        ));
        assert_eq!(item_tokens(&ix, id), 500_000);
        assert_eq!(workshop_tokens(&ix, &day), 9_000_000);
        assert_eq!(top_items(&ix, 5)[0].tokens, 500_000);
    }

    #[test]
    fn a_grant_raises_todays_daily_cap_and_the_latest_counts() {
        let (_d, hall) = hall();
        add(
            &hall,
            Some(ItemId(1)),
            usage("assay", "reviewer", "headless", 500_000),
        );
        let day = today();
        let grant = |tokens: u64, day: &str| Body::BudgetGranted {
            day: day.into(),
            tokens,
            reason: None,
        };
        let b = Budget {
            daily: Some("400k".into()),
            ..Default::default()
        };
        let ix = Index::synced(&hall).unwrap();
        let s = check_with(&ix, &b, None, "reviewer", &day).unwrap_err();
        assert_eq!(s.level, Level::Daily);
        assert!(s.reason.starts_with(DAILY_SPENT), "{}", s.reason);
        assert!(s.reason.contains("hall budget --grant"), "{}", s.reason);

        // Another day's grant doesn't count today.
        add(&hall, None, grant(1_000_000, "2001-01-01"));
        add(&hall, None, grant(50_000, &day));
        let ix = Index::synced(&hall).unwrap();
        assert_eq!(daily_granted(&ix, &day), 50_000);
        let s = check_with(&ix, &b, None, "reviewer", &day).unwrap_err();
        assert!(s.reason.contains("400k + 50k granted"), "{}", s.reason);

        // The latest grant for a day replaces the earlier one.
        add(&hall, None, grant(200_000, &day));
        let ix = Index::synced(&hall).unwrap();
        assert_eq!(daily_granted(&ix, &day), 200_000);
        assert_eq!(check_with(&ix, &b, None, "reviewer", &day), Ok(()));
        // The cap itself didn't move.
        assert_eq!(b.daily.as_deref(), Some("400k"));
    }

    #[test]
    fn roles_map_to_config_tables() {
        assert_eq!(role_of("reviewer"), Some(Role::Reviewer));
        assert_eq!(role_of("fix"), Some(Role::Fixer));
        assert_eq!(role_of("resume"), None);
        assert_eq!(table_key(Role::Reviewer), "review");
    }
}
