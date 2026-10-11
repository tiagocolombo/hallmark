//! Decisions: `ready`, `pull-back`, `drop`, `retry`, `propose-ready`.
//! Each runs inside a journal transaction, so the state check and the
//! append are atomic, and each is safe to repeat.

use std::path::Path;

use chrono::Utc;
use schemars::JsonSchema;
use serde::Serialize;

use super::{ItemView, next_for};
use crate::cli::{ContinueArgs, DropArgs, ProposeReadyArgs, PullBackArgs, ReadyArgs, RetryArgs};
use crate::error::{ErrorCode, HallError, Result};
use crate::event::{Body, short};
use crate::git;
use crate::item::{Item, State};
use crate::journal::{Ticket, ev};
use crate::output::{Render, Response};
use crate::paths;
use crate::repo::{Repo, actor};

#[derive(Debug, Serialize, JsonSchema)]
pub struct FlowOut {
    #[serde(flatten)]
    pub item: ItemView,
    /// False when the item was already where the command would put it.
    pub changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Render for FlowOut {
    fn render(&self) -> String {
        let mut s = String::new();
        if let Some(n) = &self.note {
            s.push_str(n);
            s.push_str("\n\n");
        }
        s.push_str(&self.item.render());
        s
    }
}

fn wrong_state(item: &Item, what: &str) -> HallError {
    HallError::new(
        ErrorCode::WrongState,
        format!(
            "{} is {}; {what}",
            item.id,
            item.state.label().to_lowercase()
        ),
    )
    .next(next_for(item))
}

fn reply(item: &Item, changed: bool, note: Option<String>) -> Response {
    Response::new(
        FlowOut {
            item: ItemView::from(item),
            changed,
            note,
        },
        next_for(item),
    )
}

/// A parallel part never goes to review on its own: it merges into its
/// parent, and the parent is reviewed once, as a whole (ADR 0024).
fn part_not_reviewed(item: &Item) -> Option<HallError> {
    let parent = item.parent?;
    if item.state.is_terminal() {
        return None;
    }
    Some(
        HallError::new(
            ErrorCode::WrongState,
            format!(
                "{} is a parallel part of {parent}; parts merge into their parent, \
                 which goes to review once every part is in",
                item.id
            ),
        )
        .next([
            format!("hall merge {}", item.id),
            format!("hall status {parent}"),
        ]),
    )
}

/// CHILDREN_OPEN: the parent waits for its parts.
fn children_open(item: &Item, open: &[Item], what: &str) -> HallError {
    let ids: Vec<String> = open
        .iter()
        .map(|c| format!("{} ({})", c.id, c.state))
        .collect();
    let mut next = Vec::new();
    for c in open {
        next.push(format!("hall merge {}", c.id));
        next.push(format!("hall drop {} --reason \"<why>\"", c.id));
    }
    next.push(format!("hall status {}", item.id));
    HallError::new(
        ErrorCode::ChildrenOpen,
        format!(
            "{} has parts still open: {}; {what}",
            item.id,
            ids.join(", ")
        ),
    )
    .next(next)
}

pub fn ready(cwd: &Path, a: ReadyArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    let item = repo.resolve(a.id.as_deref())?;
    if let Some(e) = part_not_reviewed(&item) {
        return Err(e);
    }
    let open = super::merge::open_children(&repo, item.id)?;
    let wt = Path::new(&item.worktree);
    if !wt.is_dir() {
        return Err(HallError::new(
            ErrorCode::WrongState,
            format!("{}'s worktree {} is missing", item.id, item.worktree),
        )
        .next([format!("hall drop {} --reason \"worktree gone\"", item.id)]));
    }
    let head = git::head(wt)?;
    let dirty = git::dirty_files(wt)?;
    let actor = actor();
    let (item, (changed, note)) = repo.hall.transact(item.id, |item, _| match item.state {
        State::Development | State::AwaitingYou => {
            if !open.is_empty() {
                return Err(children_open(
                    item,
                    &open,
                    "merge or drop each one first; the whole goes to review once",
                ));
            }
            if !dirty.is_empty() {
                return Err(HallError::new(
                    ErrorCode::DirtyWorktree,
                    format!(
                        "{} has uncommitted changes ({}); review covers commits only",
                        item.id,
                        dirty.join(", ")
                    ),
                )
                .next([
                    "git add -A && git commit -m \"<message>\"",
                    &format!("hall ready {}", item.id),
                ]));
            }
            let round = item.round + 1;
            let e = ev(
                &actor,
                Some(item.id),
                Body::Ready {
                    sha: head.clone(),
                    round,
                    note: a.note.clone(),
                },
            );
            Ok((
                vec![e],
                (
                    true,
                    Some(format!("Round {round} queued at {}.", short(&head))),
                ),
            ))
        }
        State::Checks | State::Review | State::Fixing => {
            if item.frozen_sha.as_deref() == Some(head.as_str()) {
                Ok((
                    vec![],
                    (
                        false,
                        Some(format!("Already in review at {}.", short(&head))),
                    ),
                ))
            } else {
                Err(HallError::new(
                    ErrorCode::ItemInReview,
                    format!(
                        "{} is in {}, round {}, at {}; HEAD is {}",
                        item.id,
                        item.state.label().to_lowercase(),
                        item.round,
                        short(item.frozen_sha.as_deref().unwrap_or("?")),
                        short(&head)
                    ),
                )
                .next([
                    format!("hall pull-back {}", item.id),
                    format!("hall log {} --kind verdict", item.id),
                ]))
            }
        }
        State::Hallmarked if item.hallmark_sha.as_deref() == Some(head.as_str()) => Ok((
            vec![],
            (false, Some("Already hallmarked at this commit.".into())),
        )),
        _ => Err(wrong_state(item, "it can't be sent to review")),
    })?;
    if changed {
        repo.hall.queue(&Ticket {
            id: item.id,
            sha: head,
            round: item.round,
            queued_at: Utc::now(),
        })?;
    }
    Ok(reply(&item, changed, note))
}

pub fn pull_back(cwd: &Path, a: PullBackArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    let item = repo.resolve(a.id.as_deref())?;
    let actor = actor();
    let (item, changed) = repo.hall.transact(item.id, |item, _| match item.state {
        State::Development => Ok((vec![], false)),
        s if s.is_terminal() => Err(wrong_state(item, "it can't come back")),
        _ => Ok((
            vec![ev(
                &actor,
                Some(item.id),
                Body::PulledBack {
                    reason: a.reason.clone(),
                },
            )],
            true,
        )),
    })?;
    repo.hall.unqueue(item.id)?;
    let note = if changed {
        "Back in development."
    } else {
        "Already in development."
    };
    Ok(reply(&item, changed, Some(note.into())))
}

pub fn drop(cwd: &Path, a: DropArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    let item = repo.resolve(a.id.as_deref())?;
    if a.reason.trim().is_empty() {
        return Err(HallError::usage("--reason can't be empty"));
    }
    if item.state == State::Dropped {
        return Ok(reply(&item, false, Some("Already dropped.".into())));
    }
    if item.state.is_terminal() {
        return Err(wrong_state(&item, "it can't be dropped"));
    }
    let open = super::merge::open_children(&repo, item.id)?;
    if !open.is_empty() {
        return Err(children_open(
            &item,
            &open,
            "drop (or merge) each part first",
        ));
    }
    let wt = paths::normalize(Path::new(&item.worktree));
    let mut note = None;
    let removable = !a.keep_worktree && wt.is_dir() && wt != repo.main_root;
    let removed = removable && {
        match git::run(
            &repo.main_root,
            &["worktree", "remove", &wt.to_string_lossy()],
        ) {
            Ok(_) => true,
            Err(e) => {
                note = Some(format!("Kept the worktree: {}", e.message));
                false
            }
        }
    };
    let actor = actor();
    let (item, changed) = repo.hall.transact(item.id, |item, _| match item.state {
        State::Dropped => Ok((vec![], false)),
        s if s.is_terminal() => Err(wrong_state(item, "it can't be dropped")),
        _ => Ok((
            vec![ev(
                &actor,
                Some(item.id),
                Body::Dropped {
                    reason: a.reason.clone(),
                    worktree_removed: removed,
                },
            )],
            true,
        )),
    })?;
    repo.hall.unqueue(item.id)?;
    let mut msg = format!("Dropped. Branch {} and the journal are kept.", item.branch);
    if let Some(n) = note {
        msg.push(' ');
        msg.push_str(&n);
    }
    Ok(reply(&item, changed, Some(msg)))
}

pub fn retry(cwd: &Path, a: RetryArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    let item = repo.resolve(a.id.as_deref())?;
    let actor = actor();
    let (item, (changed, note)) = repo.hall.transact(item.id, |item, _| match item.state {
        State::Escalated => Ok((
            vec![ev(
                &actor,
                Some(item.id),
                Body::Retried {
                    reason: a.reason.clone(),
                },
            )],
            (true, "One more fix round allowed."),
        )),
        // A hold (budget, run_failed, ship_failed, other) is lifted; the
        // item stays where it is and the loop picks it up again (ADR 0016).
        _ if item.hold.is_some() => Ok((
            vec![ev(
                &actor,
                Some(item.id),
                Body::Released {
                    reason: Some(a.reason.clone()),
                },
            )],
            (true, "Released: the loop picks it up again."),
        )),
        State::Fixing => Ok((vec![], (false, "One more fix round allowed."))),
        _ => Err(wrong_state(
            item,
            "only escalated or held items can be retried",
        )),
    })?;
    Ok(reply(&item, changed, Some(note.into())))
}

pub fn propose_ready(cwd: &Path, a: ProposeReadyArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    let item = repo.resolve(a.id.as_deref())?;
    let actor = actor();
    let (item, changed) = repo.hall.transact(item.id, |item, _| match item.state {
        State::Development => Ok((
            vec![ev(
                &actor,
                Some(item.id),
                Body::ProposedReady {
                    summary: a.summary.clone(),
                },
            )],
            true,
        )),
        State::AwaitingYou => Ok((vec![], false)),
        _ => Err(wrong_state(
            item,
            "only items in development can be proposed",
        )),
    })?;
    let note = format!(
        "{} is waiting for your call. Ask the human: send it to review (`hall ready`) or keep going?",
        item.id
    );
    Ok(reply(&item, changed, Some(note)))
}

/// `hall continue`: move an item on from wherever it waits.
pub fn continue_item(cwd: &Path, a: ContinueArgs, mode: crate::output::Mode) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    let item = repo.resolve(a.id.as_deref())?;
    let id = item.id.to_string();
    let reason = a
        .reason
        .clone()
        .unwrap_or_else(|| "continued with hall continue".into());
    if item.hold.is_some() || item.state == State::Escalated {
        return retry(
            cwd,
            RetryArgs {
                id: Some(id),
                reason,
            },
        );
    }
    match item.state {
        State::AwaitingYou => ready(
            cwd,
            ReadyArgs {
                id: Some(id),
                note: a.reason.clone(),
            },
        ),
        State::Development => {
            // Back to work: Claude's last conversation in the worktree.
            let open = format!(
                "cd {} && claude --continue",
                crate::commands::new::shell_quote(&item.worktree)
            );
            if mode == crate::output::Mode::Text && std::path::Path::new(&item.worktree).is_dir() {
                let status = std::process::Command::new("claude")
                    .arg("--continue")
                    .current_dir(&item.worktree)
                    .status();
                if status.is_err() {
                    eprintln!("could not start claude. Run: {open}");
                }
                return Ok(Response::raw(None));
            }
            let mut r = reply(
                &item,
                false,
                Some(format!(
                    "{} is in development. Reopen its session: {open}",
                    item.id
                )),
            );
            r.value["next"] = serde_json::json!([open]);
            Ok(r)
        }
        s if s.is_terminal() => Err(wrong_state(&item, "it is finished; nothing to continue")),
        _ => {
            // In the loop and not held: it moves on its own.
            Ok(reply(
                &item,
                false,
                Some(format!(
                    "{} is in {} (round {}) and moves on its own; `hall status {}` shows what it is doing.",
                    item.id, item.state, item.round, item.id
                )),
            ))
        }
    }
}
