//! The sweeper (ADR 0040): removes the worktree of each item that is done
//! with it, so they don't pile up.
//!
//! An item is done with its worktree once it shipped and nothing can send it
//! back to development: its PR merged or closed (`delivered`), or it closed
//! without a PR for the Delivery office to watch. While the PR is open the
//! worktree stays, because a rebase, review comments or red CI reopen the
//! item there. The branch and the journal stay; the Library and the journal
//! live in the git common dir, never in a worktree.
//!
//! It runs `git worktree remove` without `--force`, so a worktree with
//! uncommitted or untracked changes is kept and said once in the live log; it
//! is tried again after half an hour. Only checkouts Hallmark made are swept:
//! one the user made and adopted is theirs.
//! It never removes the main checkout, a worktree that has another branch,
//! or one a live item still uses (an adopted branch). Parts merged into their
//! parent and dropped items are left to `hall merge` and `hall drop`, which
//! honour `--keep-worktree`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::event::{Body, Event};
use crate::git;
use crate::item::{Item, State};
use crate::paths;
use crate::repo::Repo;
use crate::worker::{Progress, WorkerCtx};

pub const ACTOR: &str = "sweeper";

/// Worktrees the sweeper couldn't remove, with why, so each is said once.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Kept {
    #[serde(default)]
    noted: BTreeMap<String, String>,
    /// When each kept worktree was last tried (unix seconds): git scans the
    /// whole checkout before it refuses, so a kept one is retried only after
    /// [`RETRY_AFTER`].
    #[serde(default)]
    tried: BTreeMap<String, i64>,
}

/// How long a kept worktree waits before the next attempt, in seconds.
const RETRY_AFTER: i64 = 30 * 60;

impl Kept {
    fn path(repo: &Repo) -> PathBuf {
        repo.hall.root.join("sweeper.json")
    }

    fn load(repo: &Repo) -> Kept {
        std::fs::read_to_string(Self::path(repo))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, repo: &Repo) {
        if let Ok(s) = serde_json::to_string(self) {
            let _ = std::fs::write(Self::path(repo), s);
        }
    }
}

/// A shipped item that nothing can reopen: its PR merged or closed, or it
/// had no PR to watch. Parts and dropped items are not the sweeper's.
pub fn done_with_worktree(i: &Item) -> bool {
    i.state == State::Closed
        && i.parent.is_none()
        && i.own_worktree
        && !i.swept
        && !crate::delivery::is_watched(i)
}

/// The finished items whose worktree is still on disk, oldest first.
pub fn due(repo: &Repo, items: &[Item]) -> Vec<(Item, PathBuf)> {
    let live: Vec<PathBuf> = items
        .iter()
        .filter(|i| !i.state.is_terminal() || crate::delivery::is_watched(i))
        .map(|i| paths::normalize(Path::new(&i.worktree)))
        .collect();
    items
        .iter()
        .filter(|i| done_with_worktree(i))
        .filter_map(|i| {
            let wt = Path::new(&i.worktree);
            if !wt.is_dir() {
                return None;
            }
            let wt = paths::normalize(wt);
            (wt != repo.main_root && !live.contains(&wt)).then(|| (i.clone(), wt))
        })
        .collect()
}

pub fn tick(ctx: &WorkerCtx) -> Result<Progress> {
    let repo = ctx.repo;
    let items = repo.index()?.items(true)?;
    let due = due(repo, &items);
    if due.is_empty() {
        return Ok(Progress::Idle);
    }
    let worktrees = git::worktrees(&repo.main_root)?;
    let mut kept = Kept::load(repo);
    let mut changed = false;
    let mut out = Progress::Idle;
    for (item, wt) in due {
        let key = item.id.to_string();
        let now = chrono::Utc::now().timestamp();
        if kept.tried.get(&key).is_some_and(|t| now - t < RETRY_AFTER) {
            continue;
        }
        // Only the item's own checkout: a path that now has another branch,
        // or isn't a worktree at all, isn't ours to remove.
        let ours = worktrees
            .iter()
            .any(|w| w.path == wt && w.branch.as_deref() == Some(item.branch.as_str()));
        let why = if !ours {
            Some(format!(
                "{} is not a worktree of {} any more",
                paths::display(&wt),
                item.branch
            ))
        } else {
            match git::run(
                &repo.main_root,
                &["worktree", "remove", &wt.to_string_lossy()],
            ) {
                Ok(_) => None,
                Err(e) => Some(e.message),
            }
        };
        if let Some(why) = why {
            kept.tried.insert(key.clone(), now);
            changed = true;
            if kept.noted.get(&key) != Some(&why) {
                repo.hall.live(&format!(
                    "{key}: kept its worktree {}: {why}",
                    paths::display(&wt)
                ));
                kept.noted.insert(key, why);
                changed = true;
            }
            continue;
        }
        changed |= kept.noted.remove(&key).is_some() | kept.tried.remove(&key).is_some();
        repo.hall.append_item(
            item.id,
            &Event::new(
                ACTOR,
                Some(item.id),
                Body::WorktreeRemoved {
                    path: paths::display(&wt),
                },
            ),
        )?;
        let what = format!(
            "{key} removed its worktree {}; branch {} is kept",
            paths::display(&wt),
            item.branch
        );
        repo.hall.live(&what);
        // One removal per tick: removing a big worktree takes a while.
        out = Progress::worked(what);
        break;
    }
    if changed {
        kept.save(repo);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::ItemId;

    fn closed(own: bool, swept: bool) -> Item {
        let e = Event::new(
            "you",
            Some(ItemId(1)),
            Body::Created {
                title: "t".into(),
                branch: "b".into(),
                worktree: "/tmp/w".into(),
                base: "main".into(),
                issue: None,
                request: None,
                adopted: !own,
                parent: None,
                follow_up: None,
                pr_url: None,
                pr_head: None,
                own_worktree: own,
            },
        );
        let mut i = crate::event::fold(ItemId(1), &[e]).unwrap();
        i.state = State::Closed;
        i.swept = swept;
        i
    }

    #[test]
    fn only_unswept_worktrees_hallmark_made_are_due() {
        assert!(done_with_worktree(&closed(true, false)));
        assert!(!done_with_worktree(&closed(false, false)));
        assert!(!done_with_worktree(&closed(true, true)));
    }
}
