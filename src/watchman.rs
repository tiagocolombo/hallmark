//! The watchman (ADR 0014): flags items in the loop (checks, review,
//! fixing, hallmarked) whose journal has been silent for `stuck_after`
//! (30 minutes by default). It appends `stuck`, which puts the item in the
//! inbox until any later event clears the flag (`event::fold`).
//!
//! Held items are already in the inbox, and an item already flagged stays
//! flagged, so each silence is reported once. A daemon that stopped is
//! flagged by the inbox itself, from the daemon's state file.

use chrono::{DateTime, Duration, Utc};

use crate::config::parse_duration;
use crate::error::Result;
use crate::event::{Body, Event};
use crate::item::{Item, State};
use crate::worker::{Progress, WorkerCtx};

pub const ACTOR: &str = "watchman";
const DEFAULT_STUCK_AFTER_SECS: i64 = 30 * 60;

/// The states the loop works on by itself.
pub fn watched(state: State) -> bool {
    matches!(
        state,
        State::Checks | State::Review | State::Fixing | State::Hallmarked
    )
}

/// Should the watchman flag `item` at `now`?
pub fn is_stuck(item: &Item, after: Duration, now: DateTime<Utc>) -> bool {
    watched(item.state)
        && item.hold.is_none()
        && item.stuck_since.is_none()
        && now - item.updated_at >= after
}

pub fn tick(ctx: &WorkerCtx) -> Result<Progress> {
    let repo = ctx.repo;
    let after = Duration::seconds(
        parse_duration(&repo.config.stuck_after).unwrap_or(DEFAULT_STUCK_AFTER_SECS),
    );
    let now = Utc::now();
    let mut flagged = Vec::new();
    for cand in repo.index()?.items(false)? {
        if !is_stuck(&cand, after, now) {
            continue;
        }
        // Decide again under the journal's lock, from the journal itself:
        // the index may trail, and an agent may have just written.
        let (_, did) = repo.hall.transact(cand.id, |item, _| {
            if !is_stuck(item, after, Utc::now()) {
                return Ok((vec![], false));
            }
            let e = Event::new(
                ACTOR,
                Some(item.id),
                Body::Stuck {
                    state: item.state,
                    since: item.updated_at,
                },
            );
            Ok((vec![e], true))
        })?;
        if did {
            repo.hall.live(&format!(
                "{} stuck in {}: no event since {}",
                cand.id,
                cand.state,
                cand.updated_at.format("%H:%M:%S")
            ));
            flagged.push(cand.id.to_string());
        }
    }
    Ok(if flagged.is_empty() {
        Progress::Idle
    } else {
        Progress::worked(format!("flagged {} as stuck", flagged.join(", ")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::{Hold, HoldKind, ItemId};

    fn item(state: State, quiet_for: i64) -> Item {
        let t = Utc::now() - Duration::seconds(quiet_for);
        Item {
            id: ItemId(1),
            title: "t".into(),
            state,
            branch: "b".into(),
            worktree: "w".into(),
            base: "main".into(),
            issue: None,
            created_at: t,
            updated_at: t,
            state_since: t,
            round: 1,
            rejects: 0,
            frozen_sha: None,
            hallmark_sha: None,
            pr_url: None,
            pr_head: None,
            summary: None,
            drop_reason: None,
            adopted: false,
            events: 2,
            parent: None,
            follow_up: None,
            ci: None,
            rebase: None,
            delivered: None,
            hold: None,
            stuck_since: None,
            reviewed_sha: None,
            findings: Default::default(),
        }
    }

    #[test]
    fn flags_quiet_loop_items_once() {
        let after = Duration::minutes(30);
        let now = Utc::now();
        assert!(is_stuck(&item(State::Review, 3600), after, now));
        assert!(is_stuck(&item(State::Hallmarked, 3600), after, now));
        assert!(!is_stuck(&item(State::Review, 60), after, now), "recent");
        assert!(
            !is_stuck(&item(State::Development, 3600), after, now),
            "your session, not the loop"
        );
        assert!(!is_stuck(&item(State::Escalated, 3600), after, now));
        let mut flagged = item(State::Fixing, 3600);
        flagged.stuck_since = Some(now);
        assert!(!is_stuck(&flagged, after, now), "already flagged");
        let mut held = item(State::Checks, 3600);
        held.hold = Some(Hold {
            kind: HoldKind::Budget,
            reason: "over".into(),
            since: now,
        });
        assert!(!is_stuck(&held, after, now), "held items are in the inbox");
    }
}
