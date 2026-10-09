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
    /// assayer, fixer, shipper.
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
    hall.with_repo_lock(|| {
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
    })
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
/// - assayer: checks or review, oldest ticket first. One at a time per
///   repo: while another holder has an assayer lease, there is none.
/// - fixer: fixing. shipper: hallmarked.
///
/// Held items, child items and items leased by someone else are skipped.
pub fn candidates(repo: &Repo, role: &str, holder: &str) -> Result<Vec<Item>> {
    let items = repo.index()?.items(false)?;
    let leases = all(&repo.hall);
    if role == "assayer"
        && leases
            .iter()
            .any(|l| l.role == "assayer" && l.holder != holder)
    {
        return Ok(Vec::new());
    }
    let mut out: Vec<(chrono::DateTime<Utc>, Item)> = items
        .into_iter()
        .filter(|i| match role {
            "assayer" => matches!(i.state, State::Checks | State::Review),
            "fixer" => i.state == State::Fixing,
            "shipper" => i.state == State::Hallmarked,
            _ => false,
        })
        .filter(|i| i.hold.is_none() && i.parent.is_none())
        .filter(|i| !leases.iter().any(|l| l.item == i.id && l.holder != holder))
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
        match claim(&repo.hall, cand.id, role, holder, ttl) {
            Ok(l) => return Ok(Some((repo.hall.load_item(cand.id)?, l))),
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
