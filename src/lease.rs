//! Work leases (ADR 0014). An agent or the daemon claims an item before
//! working on it, so two workers never take the same item. A lease is a file
//! under `hallmark/leases/<id>.json`; it expires on its own, so a crashed
//! worker can't hold an item forever.

use std::fs;

use chrono::{DateTime, Duration, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, HallError, Result};
use crate::item::{Item, ItemId, State};
use crate::journal::Hall;
use crate::repo::Repo;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Lease {
    pub item: ItemId,
    /// assayer, fixer, shipper, delivery.
    pub role: String,
    /// Who holds it: `daemon:<pid>`, or an agent's name.
    pub holder: String,
    pub acquired: DateTime<Utc>,
    pub expires: DateTime<Utc>,
}

impl Lease {
    pub fn live(&self, now: DateTime<Utc>) -> bool {
        self.expires > now
    }
}

fn path(hall: &Hall, id: ItemId) -> std::path::PathBuf {
    hall.leases_dir().join(format!("{id}.json"))
}

/// The item's lease, if one is live.
pub fn current(hall: &Hall, id: ItemId) -> Option<Lease> {
    let l: Lease = serde_json::from_str(&fs::read_to_string(path(hall, id)).ok()?).ok()?;
    l.live(Utc::now()).then_some(l)
}

/// Every live lease in the repo.
pub fn all(hall: &Hall) -> Vec<Lease> {
    let now = Utc::now();
    let Ok(rd) = fs::read_dir(hall.leases_dir()) else {
        return Vec::new();
    };
    rd.flatten()
        .filter_map(|e| serde_json::from_str::<Lease>(&fs::read_to_string(e.path()).ok()?).ok())
        .filter(|l| l.live(now))
        .collect()
}

/// Claims `id` for `holder`, or renews the holder's own lease. Another live
/// holder means `CLAIMED` (exit 7).
pub fn claim(hall: &Hall, id: ItemId, role: &str, holder: &str, ttl: Duration) -> Result<Lease> {
    hall.with_repo_lock(|| claim_locked(hall, id, role, holder, ttl))
}

/// `claim`, for a caller that already holds the repo lock.
fn claim_locked(hall: &Hall, id: ItemId, role: &str, holder: &str, ttl: Duration) -> Result<Lease> {
    let now = Utc::now();
    if let Some(l) = current(hall, id)
        && l.holder != holder
    {
        return Err(claimed(&l));
    }
    let lease = Lease {
        item: id,
        role: role.to_string(),
        holder: holder.to_string(),
        acquired: now,
        expires: now + ttl,
    };
    fs::create_dir_all(hall.leases_dir())?;
    let p = path(hall, id);
    let tmp = p.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(&lease)?)?;
    fs::rename(&tmp, &p)?;
    Ok(lease)
}

/// The most items in `state` that `[parallel]` lets run at once.
fn limit(repo: &Repo, state: State) -> u32 {
    let par = &repo.config.parallel;
    match state {
        State::Checks => par.checks,
        State::Review => par.review,
        State::Fixing => par.fix,
        State::Hallmarked => par.ship,
        _ => 0,
    }
    .max(1)
}

/// Releases the holder's lease. Releasing someone else's live lease is
/// `CLAIMED`; releasing nothing is fine.
pub fn release(hall: &Hall, id: ItemId, holder: &str) -> Result<bool> {
    hall.with_repo_lock(|| {
        match current(hall, id) {
            Some(l) if l.holder != holder => return Err(claimed(&l)),
            None if !path(hall, id).exists() => return Ok(false),
            _ => {}
        }
        match fs::remove_file(path(hall, id)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    })
}

/// The items a role may work on now, best first.
///
/// - assayer: checks or review, oldest ticket first.
/// - fixer: fixing. shipper: hallmarked.
/// - delivery: development with a rebase request (ADR 0030).
///
/// Each phase takes at most its `[parallel]` count at once: while that many
/// items in the phase are leased by others, it offers none. Held items,
/// child items and items leased by someone else are skipped.
pub fn candidates(repo: &Repo, role: &str, holder: &str) -> Result<Vec<Item>> {
    let items = repo.index()?.items(false)?;
    let leases = all(&repo.hall);
    // Each phase runs up to `[parallel]` items at once (ADR 0026).
    let par = &repo.config.parallel;
    let busy = |state: State| {
        leases
            .iter()
            .filter(|l| l.role == role && l.holder != holder)
            .filter(|l| items.iter().any(|i| i.id == l.item && i.state == state))
            .count() as u32
    };
    let room = |state: State| match state {
        // The Delivery office rebases one item at a time.
        State::Development => role == "delivery" && busy(State::Development) < 1,
        State::Checks => busy(State::Checks) < par.checks.max(1),
        State::Review => busy(State::Review) < par.review.max(1),
        State::Fixing => busy(State::Fixing) < par.fix.max(1),
        State::Hallmarked => busy(State::Hallmarked) < par.ship.max(1),
        _ => false,
    };
    let mut out: Vec<(chrono::DateTime<Utc>, Item)> = items
        .iter()
        .filter(|i| match role {
            "assayer" => matches!(i.state, State::Checks | State::Review),
            "fixer" => i.state == State::Fixing,
            "shipper" => i.state == State::Hallmarked,
            "delivery" => i.state == State::Development && i.rebase.is_some(),
            _ => false,
        })
        .filter(|i| room(i.state))
        .filter(|i| i.hold.is_none() && i.parent.is_none())
        .filter(|i| !leases.iter().any(|l| l.item == i.id && l.holder != holder))
        .cloned()
        .map(|i| {
            let when = repo
                .hall
                .ticket(i.id)
                .map(|t| t.queued_at)
                .unwrap_or(i.state_since);
            (when, i)
        })
        .collect();
    out.sort_by_key(|(t, i)| (*t, i.id));
    Ok(out.into_iter().map(|(_, i)| i).collect())
}

/// Claims the next item for `role` (or `only` that item), folded fresh
/// from its journal. `None` when there is no work.
pub fn claim_next(
    repo: &Repo,
    role: &str,
    holder: &str,
    ttl: Duration,
    only: Option<ItemId>,
) -> Result<Option<(Item, Lease)>> {
    for cand in candidates(repo, role, holder)? {
        if only.is_some_and(|o| o != cand.id) {
            continue;
        }
        // Re-check the phase limit under the lock: other slots may have
        // claimed since `candidates` took its snapshot.
        let res = repo.hall.with_repo_lock(|| {
            let busy = all(&repo.hall)
                .iter()
                .filter(|l| l.role == role && l.holder != holder && l.item != cand.id)
                .filter(|l| {
                    repo.hall
                        .load_item(l.item)
                        .is_ok_and(|i| i.state == cand.state)
                })
                .count() as u32;
            if busy >= limit(repo, cand.state) {
                return Ok(None);
            }
            claim_locked(&repo.hall, cand.id, role, holder, ttl).map(Some)
        });
        match res {
            Ok(None) => continue,
            Ok(Some(l)) => return Ok(Some((repo.hall.load_item(cand.id)?, l))),
            Err(e) if e.code == ErrorCode::Claimed => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

fn claimed(l: &Lease) -> HallError {
    HallError::new(
        ErrorCode::Claimed,
        format!(
            "{} is claimed by {} ({}) until {}",
            l.item,
            l.holder,
            l.role,
            l.expires.format("%H:%M:%S")
        ),
    )
    .next([
        format!("hall status {}", l.item),
        "hall next --role <role>".into(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_holder_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let hall = Hall::new(dir.path());
        hall.create().unwrap();
        let id = ItemId(1);
        let a = claim(&hall, id, "fixer", "a", Duration::minutes(5)).unwrap();
        assert_eq!(a.holder, "a");
        // Renewing your own lease is fine; someone else gets CLAIMED.
        claim(&hall, id, "fixer", "a", Duration::minutes(5)).unwrap();
        let err = claim(&hall, id, "fixer", "b", Duration::minutes(5)).unwrap_err();
        assert_eq!(err.code, ErrorCode::Claimed);
        assert_eq!(err.code.exit_code(), 7);
        assert_eq!(
            release(&hall, id, "b").unwrap_err().code,
            ErrorCode::Claimed
        );
        assert!(release(&hall, id, "a").unwrap());
        assert!(!release(&hall, id, "a").unwrap());
        claim(&hall, id, "fixer", "b", Duration::minutes(5)).unwrap();
    }

    #[test]
    fn expired_leases_are_free() {
        let dir = tempfile::tempdir().unwrap();
        let hall = Hall::new(dir.path());
        hall.create().unwrap();
        claim(&hall, ItemId(2), "assayer", "a", Duration::seconds(-1)).unwrap();
        assert!(current(&hall, ItemId(2)).is_none());
        assert!(all(&hall).is_empty());
        claim(&hall, ItemId(2), "assayer", "b", Duration::minutes(1)).unwrap();
        assert_eq!(all(&hall).len(), 1);
    }
}
