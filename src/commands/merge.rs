//! `hall merge` (workstream H, ADR 0024): a parallel part (child item)
//! merges into its parent's branch and closes. The parent is reviewed once,
//! as a whole, after every child is merged or dropped.

use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;

use super::{ItemView, next_for};
use crate::cli::MergeArgs;
use crate::error::{ErrorCode, HallError, Result};
use crate::event::{Body, short};
use crate::git;
use crate::item::{Item, ItemId, State};
use crate::journal::ev;
use crate::output::{Render, Response};
use crate::paths;
use crate::repo::{Repo, actor};

#[derive(Debug, Serialize, JsonSchema)]
pub struct MergeOut {
    /// The child, now closed.
    #[serde(flatten)]
    pub item: ItemView,
    pub parent: ItemView,
    /// The merge commit on the parent's branch.
    pub sha: String,
    /// Children of the parent still open.
    pub open_children: Vec<String>,
    /// False when the child was already merged.
    pub changed: bool,
    /// True when the child's worktree was removed (its branch is kept).
    pub worktree_removed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Render for MergeOut {
    fn render(&self) -> String {
        let mut s = if self.changed {
            format!(
                "{} merged into {} at {}.\n",
                self.item.id,
                self.parent.id,
                short(&self.sha)
            )
        } else {
            format!(
                "{} was already merged into {} at {}.\n",
                self.item.id,
                self.parent.id,
                short(&self.sha)
            )
        };
        if let Some(n) = &self.note {
            s.push_str(n);
            s.push('\n');
        }
        if self.open_children.is_empty() {
            s.push_str(&format!(
                "Every part of {} is merged; it can go to review.\n",
                self.parent.id
            ));
        } else {
            s.push_str(&format!("Still open: {}.\n", self.open_children.join(", ")));
        }
        s
    }
}

/// The `next` hints for a child item: it merges into its parent instead of
/// going to review. `None` for items that aren't open children.
pub fn child_next(item: &Item) -> Option<Vec<String>> {
    item.parent?;
    let id = item.id;
    let n = match item.state {
        State::Development => vec![
            format!("hall merge {id}"),
            format!(
                "hall propose-ready {id} --summary \"<what was done>\" --why \"<why>\" --how \"<how>\""
            ),
            format!("hall drop {id} --reason \"<why>\""),
        ],
        State::AwaitingYou => vec![format!("hall merge {id}"), format!("hall pull-back {id}")],
        _ => return None,
    };
    Some(n)
}

/// The parent's children that are still open (neither merged nor dropped).
pub fn open_children(repo: &Repo, parent: ItemId) -> Result<Vec<Item>> {
    Ok(repo
        .index()?
        .items(false)?
        .into_iter()
        .filter(|i| i.parent == Some(parent) && !i.state.is_terminal())
        .collect())
}

/// The merge commit recorded for a closed child, if it was merged.
fn merged(repo: &Repo, id: ItemId) -> Result<Option<(ItemId, String)>> {
    Ok(repo.hall.read_item(id)?.iter().find_map(|e| match &e.body {
        Body::Merged { into, sha } => Some((*into, sha.clone())),
        _ => None,
    }))
}

fn wrong(item: &Item, what: &str, next: Vec<String>) -> HallError {
    HallError::new(
        ErrorCode::WrongState,
        format!(
            "{} is {}; {what}",
            item.id,
            item.state.label().to_lowercase()
        ),
    )
    .next(next)
}

fn clean(item: &Item, role: &str) -> Result<std::path::PathBuf> {
    let wt = paths::normalize(Path::new(&item.worktree));
    if !wt.is_dir() {
        return Err(HallError::new(
            ErrorCode::WrongState,
            format!("{}'s worktree {} is missing", item.id, item.worktree),
        )
        .next([format!("hall drop {} --reason \"worktree gone\"", item.id)]));
    }
    let dirty = git::dirty_files(&wt)?;
    if !dirty.is_empty() {
        let q = crate::commands::new::shell_quote(&item.worktree);
        return Err(HallError::new(
            ErrorCode::DirtyWorktree,
            format!(
                "the {role} {} has uncommitted changes ({}); merge covers commits only",
                item.id,
                dirty.join(", ")
            ),
        )
        .next([
            format!("git -C {q} add -A && git -C {q} commit -m \"<message>\""),
            "hall merge <child>".to_string(),
        ]));
    }
    Ok(wt)
}

pub fn merge(cwd: &Path, a: MergeArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    let child = repo.resolve(a.id.as_deref())?;
    let Some(parent_id) = child.parent else {
        return Err(HallError::new(
            ErrorCode::WrongState,
            format!(
                "{} is not a parallel part of another item; `hall ready` sends it to review",
                child.id
            ),
        )
        .next(next_for(&child)));
    };
    let parent = repo.hall.load_item(parent_id)?;

    // Already merged: report it again.
    if child.state == State::Closed {
        let Some((into, sha)) = merged(&repo, child.id)? else {
            return Err(wrong(&child, "it can't be merged", next_for(&child)));
        };
        record_parent(&repo, into, child.id, &sha)?;
        let parent = repo.hall.load_item(into)?;
        return reply(&repo, &child, &parent, sha, false, false, None);
    }
    if !matches!(child.state, State::Development | State::AwaitingYou) {
        return Err(wrong(
            &child,
            "only parts in development or awaiting you can be merged",
            next_for(&child),
        ));
    }
    if parent.state != State::Development {
        return Err(HallError::new(
            ErrorCode::WrongState,
            format!(
                "{}'s parent {} is {}; parts merge only into a parent in development",
                child.id,
                parent.id,
                parent.state.label().to_lowercase()
            ),
        )
        .next([
            format!("hall pull-back {}", parent.id),
            format!("hall status {}", parent.id),
        ]));
    }
    let cwt = clean(&child, "part")?;
    let pwt = clean(&parent, "parent")?;

    let sha = repo.hall.with_repo_lock(|| {
        // Already in the parent's branch: a part with no commits, or a merge
        // that happened before a crash.
        let already = git::ok(
            &pwt,
            &["merge-base", "--is-ancestor", &child.branch, "HEAD"],
        );
        if !already {
            let msg = format!("Merge {}: {}", child.id, child.title);
            if let Err(e) = git::run(
                &pwt,
                &["merge", "--no-ff", "--no-edit", "-m", &msg, &child.branch],
            ) {
                let conflicts: Vec<String> =
                    git::run(&pwt, &["diff", "--name-only", "--diff-filter=U"])
                        .unwrap_or_default()
                        .lines()
                        .map(String::from)
                        .collect();
                let _ = git::run(&pwt, &["merge", "--abort"]);
                if conflicts.is_empty() {
                    return Err(HallError::new(
                        ErrorCode::CommandFailed,
                        format!("merging {} into {} failed: {}", child.id, parent.id, e.message),
                    )
                    .next(["hall doctor", &format!("hall merge {}", child.id)]));
                }
                let q = crate::commands::new::shell_quote(&child.worktree);
                return Err(HallError::new(
                    ErrorCode::MergeConflict,
                    format!(
                        "{} conflicts with {}'s branch in {}; nothing was merged",
                        child.id,
                        parent.id,
                        conflicts.join(", ")
                    ),
                )
                .next([
                    format!("git -C {q} merge {}", parent.branch),
                    format!(
                        "fix the conflicts in {}, then git -C {q} add -A && git -C {q} commit --no-edit",
                        child.worktree
                    ),
                    format!("hall merge {}", child.id),
                ]));
            }
        }
        let sha = git::head(&pwt)?;
        let actor = actor();
        repo.hall.transact(child.id, |item, events| {
            let done = events
                .iter()
                .any(|e| matches!(e.body, Body::Merged { .. }));
            match item.state {
                _ if done => Ok((vec![], ())),
                State::Development | State::AwaitingYou => Ok((
                    vec![ev(
                        &actor,
                        Some(item.id),
                        Body::Merged {
                            into: parent.id,
                            sha: sha.clone(),
                        },
                    )],
                    (),
                )),
                _ => Err(wrong(item, "it can't be merged", next_for(item))),
            }
        })?;
        record_parent(&repo, parent.id, child.id, &sha)?;
        Ok(sha)
    })?;

    // The part is done: its worktree goes (never the main checkout or the
    // parent's), its branch stays.
    let mut note = None;
    let removable = !a.keep_worktree && cwt != repo.main_root && cwt != pwt && cwt.is_dir();
    let removed = removable && {
        match git::run(
            &repo.main_root,
            &["worktree", "remove", &cwt.to_string_lossy()],
        ) {
            Ok(_) => true,
            Err(e) => {
                note = Some(format!("Kept the worktree: {}", e.message));
                false
            }
        }
    };
    let child = repo.hall.load_item(child.id)?;
    let parent = repo.hall.load_item(parent.id)?;
    reply(&repo, &child, &parent, sha, true, removed, note)
}

/// `child_merged` on the parent, once per child.
fn record_parent(repo: &Repo, parent: ItemId, child: ItemId, sha: &str) -> Result<()> {
    let actor = actor();
    repo.hall.transact(parent, |item, events| {
        let seen = events
            .iter()
            .any(|e| matches!(&e.body, Body::ChildMerged { child: c, .. } if *c == child));
        if seen {
            return Ok((vec![], ()));
        }
        Ok((
            vec![ev(
                &actor,
                Some(item.id),
                Body::ChildMerged {
                    child,
                    sha: sha.to_string(),
                },
            )],
            (),
        ))
    })?;
    Ok(())
}

fn reply(
    repo: &Repo,
    child: &Item,
    parent: &Item,
    sha: String,
    changed: bool,
    worktree_removed: bool,
    note: Option<String>,
) -> Result<Response> {
    let open: Vec<Item> = open_children(repo, parent.id)?
        .into_iter()
        .filter(|i| i.id != child.id)
        .collect();
    let mut next: Vec<String> = open
        .iter()
        .map(|i| format!("hall merge {}", i.id))
        .collect();
    if open.is_empty() && parent.state == State::Development {
        next.push(format!(
            "run the repo's gates in {} (the merged whole)",
            parent.worktree
        ));
        next.push(format!(
            "hall propose-ready {} --summary \"<what was done>\" --why \"<why>\" --how \"<how>\"",
            parent.id
        ));
        next.push(format!("hall ready {}", parent.id));
    }
    next.push(format!("hall status {}", parent.id));
    let out = MergeOut {
        item: ItemView::from(child),
        parent: ItemView::from(parent),
        sha,
        open_children: open.iter().map(|i| i.id.to_string()).collect(),
        changed,
        worktree_removed,
        note,
    };
    Ok(Response::new(out, next))
}
