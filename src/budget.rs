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
//! - **Grants.** `hall retry` on a budget hold lets that one item spend more
//!   (another `per_item` by default, or `--tokens`) without touching the
//!   config: its cap is `per_item` plus every grant in its journal.

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
        "reviewer" | "review" | "security" => Some(Role::Reviewer),
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
/// delivering your answer never holds an item. The librarian answers to the
/// daily and room caps only, so distilling never holds an item. An unreadable index fails
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
        && let Some(limit) = Cap::parse(cap)
    {
        let used = room_spent(ix, r.room(), day);
        if limit.reached(&used) {
            return Err(Stop {
                level: Level::Room,
                reason: format!(
                    "the {} reached its daily cap ({} today); {} runs wait until tomorrow (UTC)",
                    room_label(r.room()),
                    limit.show(&used),
                    r.as_str()
                ),
            });
        }
    }
    // The librarian distils items that have left the loop: what they spent
    // there never holds them (the daily and room caps still apply).
    if r != Role::Librarian
        && let (Some(id), Some(cap)) = (item, b.per_item.as_deref())
        && let Some(limit) = Cap::parse(cap)
    {
        let used = item_spent(ix, id);
        // A grant is in weighted tokens: it raises a token cap only.
        let (allowed, shown, hint) = match limit {
            Cap::Tokens(base) => {
                let granted = item_granted(ix, id);
                let allowed = Cap::Tokens(base.saturating_add(granted));
                let shown = match granted {
                    0 => allowed.show(&used),
                    g => format!(
                        "{} of {} + {} granted weighted tokens",
                        fmt_tokens(used.tokens),
                        fmt_tokens(base),
                        fmt_tokens(g)
                    ),
                };
                (
                    allowed,
                    shown,
                    format!("; `hall retry` grants it {} more", fmt_tokens(base)),
                )
            }
            usd => (usd, usd.show(&used), String::new()),
        };
        if allowed.reached(&used) {
            return Err(Stop {
                level: Level::Item,
                reason: format!("{id} reached its cap ({shown} spent by the loop){hint}"),
            });
        }
    }
    Ok(())
}

/// What `item` may spend under `per_item` now: the cap plus its grants (a
/// dollar cap has none). None when `per_item` is unset or unreadable.
pub fn item_allowance(ix: &Index, b: &Budget, item: ItemId) -> Option<Cap> {
    Some(match Cap::parse(b.per_item.as_deref()?)? {
        Cap::Tokens(t) => Cap::Tokens(t.saturating_add(item_granted(ix, item))),
        usd => usd,
    })
}

/// How a daily-budget stop's reason starts; a pause with this reason is the
/// one `hall budget --grant` lifts.
pub const DAILY_SPENT: &str = "the daily budget is spent";

/// A cap: weighted tokens ("800k", "6M") or dollars at list price ("$15").
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cap {
    Tokens(u64),
    Usd(f64),
}

impl Cap {
    pub fn parse(s: &str) -> Option<Cap> {
        let t = s.trim();
        match t.strip_prefix('$') {
            Some(usd) => usd
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|v| *v >= 0.0)
                .map(Cap::Usd),
            None => parse_tokens(t).map(Cap::Tokens),
        }
    }

    pub fn reached(&self, s: &Spent) -> bool {
        match self {
            Cap::Tokens(limit) => s.tokens >= *limit,
            Cap::Usd(limit) => s.usd >= *limit,
        }
    }

    /// The fraction of the cap `s` has used (1.0 when the cap is zero).
    pub fn fraction(&self, s: &Spent) -> f64 {
        let (used, limit) = match self {
            Cap::Tokens(l) => (s.tokens as f64, *l as f64),
            Cap::Usd(l) => (s.usd, *l),
        };
        if limit <= 0.0 { 1.0 } else { used / limit }
    }

    /// The limit alone: "20M", "$15.00".
    pub fn limit_label(&self) -> String {
        match self {
            Cap::Tokens(l) => fmt_tokens(*l),
            Cap::Usd(l) => format!("${l:.2}"),
        }
    }

    /// "6.41M of 20M weighted tokens", "$4.15 of $15.00".
    pub fn show(&self, s: &Spent) -> String {
        match self {
            Cap::Tokens(limit) => format!(
                "{} of {} weighted tokens",
                fmt_tokens(s.tokens),
                fmt_tokens(*limit)
            ),
            Cap::Usd(limit) => format!("${:.2} of ${limit:.2}", s.usd),
        }
    }
}

/// What the loop spent: tokens weighted by price, and list-price dollars.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Spent {
    pub tokens: u64,
    pub usd: f64,
}

fn spent(ix: &Index, filter: &str, p: impl rusqlite::Params) -> Spent {
    ix.conn()
        .query_row(
            &format!(
                "SELECT {TOKENS}, SUM(cost_usd) FROM usage WHERE {filter} AND source = 'headless'"
            ),
            p,
            |r| Ok((r.get::<_, Option<f64>>(0)?, r.get::<_, Option<f64>>(1)?)),
        )
        .map(|(t, u)| Spent {
            tokens: t.unwrap_or(0.0).max(0.0).round() as u64,
            usd: u.unwrap_or(0.0),
        })
        .unwrap_or_default()
}

/// The loop's spend on `item`, ever.
pub fn item_spent(ix: &Index, item: ItemId) -> Spent {
    spent(ix, "item = ?1", params![item.to_string()])
}

/// The loop's spend in `room` on `day` (UTC).
pub fn room_spent(ix: &Index, room: &str, day: &str) -> Spent {
    spent(ix, "room = ?1 AND day = ?2", params![room, day])
}

/// The whole loop's spend on `day` (UTC).
pub fn daily_spent(ix: &Index, day: &str) -> Spent {
    spent(ix, "day = ?1", params![day])
}

/// The daily total's stop, if `[budget] daily` (plus today's grant) is
/// spent for `day`. A grant is in weighted tokens, so it raises a token cap
/// only; a dollar cap has no grant.
pub fn daily_stop(ix: &Index, b: &Budget, day: &str) -> Option<Stop> {
    let cap = b.daily.as_deref()?;
    let used = daily_spent(ix, day);
    let (limit, shown, hint) = match Cap::parse(cap)? {
        Cap::Tokens(base) => {
            let granted = daily_granted(ix, day);
            let limit = Cap::Tokens(base.saturating_add(granted));
            let shown = match granted {
                0 => limit.show(&used),
                g => format!(
                    "{} of {} + {} granted weighted tokens",
                    fmt_tokens(used.tokens),
                    fmt_tokens(base),
                    fmt_tokens(g)
                ),
            };
            (
                limit,
                shown,
                ", or `hall budget --grant` lets it go on today",
            )
        }
        usd => (usd, usd.show(&used), ""),
    };
    limit.reached(&used).then(|| Stop {
        level: Level::Daily,
        reason: format!("{DAILY_SPENT} ({shown} today); the loop waits until 00:00 UTC{hint}"),
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
        .query_row(sql, p, |r| r.get::<_, Option<f64>>(0))
        .ok()
        .flatten()
        .unwrap_or(0.0)
        .max(0.0)
        .round() as u64
}

/// Tokens weighted by price, in input tokens: a cache read costs a tenth of
/// an input token and a cache write 1.25 of one. Without the weights, a long
/// review that re-reads its cached context spends a cap ten times too fast
/// (H-0008: 6.15M cache reads, $4.15).
const TOKENS: &str = "SUM(COALESCE(input,0) + COALESCE(output,0) + COALESCE(cache_read,0) * 0.1 + COALESCE(cache_write,0) * 1.25)";

/// Tokens the loop (headless runs) spent on `item`, ever.
pub fn item_tokens(ix: &Index, item: ItemId) -> u64 {
    sum(
        ix,
        &format!("SELECT {TOKENS} FROM usage WHERE item = ?1 AND source = 'headless'"),
        params![item.to_string()],
    )
}

/// Tokens granted to `item` on top of `per_item` by `hall retry`.
pub fn item_granted(ix: &Index, item: ItemId) -> u64 {
    sum(
        ix,
        "SELECT SUM(json_extract(json, '$.granted_tokens')) FROM events
         WHERE item = ?1 AND event = 'released'",
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
            r.get::<_, Option<f64>>(1)?.unwrap_or(0.0).max(0.0).round() as i64,
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
    fn cache_reads_weigh_a_tenth_and_caps_can_be_dollars() {
        let (_d, hall) = hall();
        let id = ItemId(1);
        // H-0008's review: 6.15M cache reads, 190k cache writes, 70k output.
        add(
            &hall,
            Some(id),
            Body::Usage(Usage {
                source: "headless".into(),
                room: "assay".into(),
                role: "reviewer".into(),
                model: "claude-opus-5-5".into(),
                input_tokens: 94,
                output_tokens: 69_956,
                cache_read_tokens: 6_145_981,
                cache_write_tokens: 190_338,
                cost_usd: 4.15,
                ..Default::default()
            }),
        );
        let ix = Index::synced(&hall).unwrap();
        let day = today();
        let spent = item_spent(&ix, id);
        // 94 + 69,956 + 614,598 + 237,923 ≈ 922.6k, not 6.41M.
        assert_eq!(spent.tokens, 922_571, "{spent:?}");
        let mut b = Budget {
            per_item: Some("6M".into()),
            ..Budget::default()
        };
        assert_eq!(check_with(&ix, &b, Some(id), "fixer", &day), Ok(()));
        b.per_item = Some("$5".into());
        assert_eq!(check_with(&ix, &b, Some(id), "fixer", &day), Ok(()));
        b.per_item = Some("$4".into());
        let s = check_with(&ix, &b, Some(id), "fixer", &day).unwrap_err();
        assert!(s.reason.contains("$4.15 of $4.00"), "{}", s.reason);
        assert_eq!(Cap::parse("$ 15"), Some(Cap::Usd(15.0)));
        assert_eq!(Cap::parse("800k"), Some(Cap::Tokens(800_000)));
        assert_eq!(Cap::parse("$x"), None);
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
        assert!(
            s.reason
                .contains("H-0001 reached its cap (500k of 500k weighted tokens"),
            "{}",
            s.reason
        );
        // Resume runs never hold an item.
        assert_eq!(check_with(&ix, &b, Some(id), "resume", &day), Ok(()));
        // Nor does the librarian: it distils items that left the loop.
        assert_eq!(check_with(&ix, &b, Some(id), "librarian", &day), Ok(()));

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
    fn grants_raise_one_items_cap() {
        let (_d, hall) = hall();
        let (a, b_) = (ItemId(1), ItemId(2));
        for id in [a, b_] {
            add(
                &hall,
                Some(id),
                usage("assay", "reviewer", "headless", 500_000),
            );
        }
        add(
            &hall,
            Some(a),
            Body::Released {
                reason: Some("worth it".into()),
                granted_tokens: Some(300_000),
            },
        );
        let ix = Index::synced(&hall).unwrap();
        let day = today();
        let b = Budget {
            per_item: Some("400k".into()),
            ..Default::default()
        };
        assert_eq!(item_granted(&ix, a), 300_000);
        assert_eq!(item_allowance(&ix, &b, a), Some(Cap::Tokens(700_000)));
        assert_eq!(check_with(&ix, &b, Some(a), "fixer", &day), Ok(()));
        // The other item still answers to the unchanged cap.
        let s = check_with(&ix, &b, Some(b_), "fixer", &day).unwrap_err();
        assert_eq!(s.level, Level::Item);
        assert!(
            s.reason.contains("`hall retry` grants it 400k more"),
            "{}",
            s.reason
        );
        // The default cap, 800k, plus the grant; none when the cap is off.
        assert_eq!(
            item_allowance(&ix, &Budget::default(), a),
            Some(Cap::Tokens(1_100_000))
        );
        let off = Budget {
            per_item: None,
            ..Default::default()
        };
        assert_eq!(item_allowance(&ix, &off, a), None);
    }

    #[test]
    fn roles_map_to_config_tables() {
        assert_eq!(role_of("reviewer"), Some(Role::Reviewer));
        assert_eq!(role_of("fix"), Some(Role::Fixer));
        assert_eq!(role_of("resume"), None);
        assert_eq!(table_key(Role::Reviewer), "review");
    }
}
