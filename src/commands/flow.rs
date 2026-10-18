//! Decisions: `ready`, `pull-back`, `drop`, `retry`, `propose-ready`.
//! Each runs inside a journal transaction, so the state check and the
//! append are atomic, and each is safe to repeat.

use std::path::Path;

use chrono::Utc;
use schemars::JsonSchema;
use serde::Serialize;

use super::{ItemView, next_for};
use crate::cli::{ContinueArgs, DropArgs, ProposeReadyArgs, PullBackArgs, ReadyArgs, RetryArgs};
use crate::config::parse_tokens;
use crate::error::{ErrorCode, HallError, Result};
use crate::event::{Body, short};
use crate::git;
use crate::item::{HoldKind, Item, State};
use crate::journal::{Ticket, ev};
use crate::output::{Render, Response};
use crate::paths;
use crate::repo::{Repo, actor};
use crate::usage::fmt_tokens;

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

/// An item the Delivery office was asked to rebase can't go to review until
/// its branch is on the base: review would see a commit that conflicts, and
/// the move would drop the request, so nothing would rebase it again
/// (ADR 0043). None when nothing is pending or the branch is already there.
fn rebase_pending(item: &Item) -> Option<HallError> {
    let req = item.rebase.as_ref()?;
    if item.state != State::Development {
        return None;
    }
    let wt = Path::new(&item.worktree);
    let target = crate::delivery::base_ref_capped(wt, &req.onto)?;
    let id = item.id;
    if git::rebase_in_progress(wt) {
        let q = crate::commands::new::shell_quote(&item.worktree);
        return Some(
            HallError::new(
                ErrorCode::WrongState,
                format!(
                    "{id} has a rebase onto {} in progress; finish it (`git rebase --continue`) \
                     or abort it in {}, then run `hall ready {id}` again",
                    target.name, item.worktree
                ),
            )
            .next([
                format!("git -C {q} rebase --continue"),
                format!("git -C {q} rebase --abort"),
                format!("hall status {id}"),
            ]),
        );
    }
    let head = git::head(wt).ok()?;
    if git::is_ancestor(wt, &target.git_ref, &head) {
        return None;
    }
    let mut next = Vec::new();
    if item.hold.is_some() {
        next.push(format!("hall retry {id} --reason \"<why>\""));
    } else {
        next.push(format!("hall status {id}"));
    }
    next.push(format!(
        "git -C {} rebase {}",
        crate::commands::new::shell_quote(&item.worktree),
        target.name
    ));
    next.push(format!("hall ready {id}"));
    Some(
        HallError::new(
            ErrorCode::WrongState,
            format!(
                "{id} waits for a rebase onto {} ({}), and its branch isn't on it yet; {}, \
                 or rebase it yourself and run `hall ready {id}` again",
                target.name,
                req.reason.trim().trim_end_matches('.'),
                if item.hold.is_some() {
                    "release the hold and the Delivery office rebases it"
                } else {
                    "the Delivery office rebases it on its next pass"
                }
            ),
        )
        .next(next),
    )
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
    if let Some(e) = rebase_pending(&item) {
        return Err(e);
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
                let kept = if a.note.as_deref().is_some_and(|n| !n.trim().is_empty()) {
                    " The --note isn't kept: it is already in review. To give one, `hall pull-back` it, then `hall ready --note`."
                } else {
                    ""
                };
                Ok((
                    vec![],
                    (
                        false,
                        Some(format!("Already in review at {}.{kept}", short(&head))),
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
        State::Closed if item.pr_url.is_some() && item.parent.is_none() => Err(wrong_state(
            item,
            "it shipped. To rebase its open PR, resolve conflicts or change it, reopen it with \
             `hall adopt` in its worktree (or `hall adopt --pr <url>`)",
        )),
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
    let tokens = match a.tokens.as_deref() {
        Some(t) => Some(
            parse_tokens(t)
                .filter(|n| *n > 0 && *n <= i64::MAX as u64)
                .ok_or_else(|| {
                    HallError::usage(format!(
                        "--tokens {t}: use a token count, such as 2M or 800k"
                    ))
                })?,
        ),
        None => None,
    };
    // A budget hold comes back at once unless the item may spend more, so
    // lifting one grants another `per_item` (or --tokens) to this item
    // alone; the config is unchanged (ADR 0016).
    let per_item = repo
        .config
        .budget
        .per_item
        .as_deref()
        .and_then(crate::budget::Cap::parse);
    // A grant is in weighted tokens: it raises a token `per_item` only.
    let (per_item, dollar_cap) = match per_item {
        Some(crate::budget::Cap::Tokens(t)) => (Some(t), false),
        Some(crate::budget::Cap::Usd(_)) => (None, true),
        None => (None, false),
    };
    // What the item has overspent its allowance by: the grant covers that
    // first, so `n` is what it may spend from now on.
    let overshoot = repo
        .index()
        .ok()
        .and_then(
            |ix| match crate::budget::item_allowance(&ix, &repo.config.budget, item.id)? {
                crate::budget::Cap::Tokens(allowance) => Some(
                    crate::budget::item_spent(&ix, item.id)
                        .tokens
                        .saturating_sub(allowance),
                ),
                crate::budget::Cap::Usd(_) => None,
            },
        )
        .unwrap_or(0);
    let grant_for = |n: u64| n.saturating_add(overshoot).min(i64::MAX as u64 / 2);
    let (item, (changed, note)) = repo.hall.transact(item.id, |item, _| {
        let budget_hold = item
            .hold
            .as_ref()
            .is_some_and(|h| h.kind == HoldKind::Budget);
        if tokens.is_some() && !budget_hold {
            return Err(HallError::usage(format!(
                "{} isn't held for budget; --tokens only applies to a budget hold",
                item.id
            ))
            .next(next_for(item)));
        }
        // Released as is, an item over a dollar cap would be held again at
        // once, and a grant can't raise that cap.
        if budget_hold && dollar_cap {
            return Err(wrong_state(
                item,
                "its `per_item` budget is in dollars, and a grant only raises a token cap: \
                 raise `[budget] per_item` to let it go on",
            ));
        }
        match item.state {
            State::Escalated => {
                let mut events = vec![ev(
                    &actor,
                    Some(item.id),
                    Body::Retried {
                        reason: a.reason.clone(),
                    },
                )];
                let mut note = "One more fix round allowed.".to_string();
                // An escalated item held for budget would be held again at
                // once, so the grant goes with the round.
                if budget_hold && let Some(n) = tokens.or(per_item) {
                    events.push(ev(
                        &actor,
                        Some(item.id),
                        Body::Released {
                            reason: Some(a.reason.clone()),
                            granted_tokens: Some(grant_for(n)),
                        },
                    ));
                    note = format!(
                        "One more fix round allowed, with {} more tokens for {}; \
                         the budget is unchanged.",
                        fmt_tokens(n),
                        item.id
                    );
                }
                Ok((events, (true, note)))
            }
            // A hold (budget, run_failed, ship_failed, other) is lifted; the
            // item stays where it is and the loop picks it up again (ADR 0016).
            _ if item.hold.is_some() => {
                let granted = if budget_hold {
                    tokens.or(per_item)
                } else {
                    None
                };
                let note = match granted {
                    Some(n) => format!(
                        "Released with {} more tokens for {}; the budget is unchanged. \
                         The loop picks it up again.",
                        fmt_tokens(n),
                        item.id
                    ),
                    None => "Released: the loop picks it up again.".to_string(),
                };
                Ok((
                    vec![ev(
                        &actor,
                        Some(item.id),
                        Body::Released {
                            reason: Some(a.reason.clone()),
                            granted_tokens: granted.map(grant_for),
                        },
                    )],
                    (true, note),
                ))
            }
            State::Fixing => Ok((vec![], (false, "One more fix round allowed.".to_string()))),
            _ => Err(wrong_state(
                item,
                "only escalated or held items can be retried",
            )),
        }
    })?;
    Ok(reply(&item, changed, Some(note)))
}

pub fn propose_ready(cwd: &Path, a: ProposeReadyArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    let item = repo.resolve(a.id.as_deref())?;
    let actor = actor();
    // A repeat (a lost reply, a retry) after it went straight to review is a
    // no-op, as `hall ready` is.
    if matches!(item.state, State::Checks | State::Review | State::Fixing)
        && item.frozen_sha.is_some()
        && item.frozen_sha.as_deref() == git::head(Path::new(&item.worktree)).ok().as_deref()
    {
        let note = format!("{} is already in review at that commit.", item.id);
        return Ok(reply(&item, false, Some(note)));
    }
    if let Some(e) = rebase_pending(&item) {
        return Err(e);
    }
    // Nothing left to confirm: straight to review (`auto_ready`).
    let wait_for = if a.ask {
        Some("you asked for the human's call".to_string())
    } else {
        left_to_confirm(&repo, &item)?
    };
    let head = (wait_for.is_none())
        .then(|| git::head(Path::new(&item.worktree)).ok())
        .flatten();
    let (item, (changed, sent)) = repo.hall.transact(item.id, |item, _| match item.state {
        State::Development => {
            let mut events = vec![ev(
                &actor,
                Some(item.id),
                Body::ProposedReady {
                    summary: a.summary.clone(),
                    why: a.why.clone().filter(|w| !w.trim().is_empty()),
                    how: a.how.clone().filter(|h| !h.trim().is_empty()),
                },
            )];
            if let Some(sha) = &head {
                events.push(ev(
                    &actor,
                    Some(item.id),
                    Body::Ready {
                        sha: sha.clone(),
                        round: item.round + 1,
                        note: Some("nothing left to confirm".into()),
                    },
                ));
            }
            Ok((events, (true, head.is_some())))
        }
        State::AwaitingYou => Ok((vec![], (false, false))),
        _ => Err(wrong_state(
            item,
            "only items in development can be proposed",
        )),
    })?;
    if sent && let Some(sha) = &head {
        repo.hall.queue(&Ticket {
            id: item.id,
            sha: sha.clone(),
            round: item.round,
            queued_at: Utc::now(),
        })?;
        let note = format!(
            "Nothing left to confirm, so {} went straight to review: round {} queued at {}. \
             Tell the human; `hall pull-back {}` if they want changes.",
            item.id,
            item.round,
            short(sha),
            item.id
        );
        return Ok(reply(&item, true, Some(note)));
    }
    let note = match wait_for {
        Some(why) if changed => format!(
            "{} is waiting for your call ({why}). Ask the human: send it to review \
             (`hall ready`) or keep going?",
            item.id
        ),
        _ => format!(
            "{} is waiting for your call. Ask the human: send it to review (`hall ready`) or keep going?",
            item.id
        ),
    };
    Ok(reply(&item, changed, Some(note)))
}

/// Why a finished item still needs the human before review, if it does:
/// `auto_ready` is off, a question is open, the worktree has uncommitted
/// changes, or parts are still open. A part never goes to review alone.
fn left_to_confirm(repo: &Repo, item: &Item) -> Result<Option<String>> {
    if !repo.config.auto_ready {
        return Ok(Some("auto_ready is off".into()));
    }
    if item.parent.is_some() {
        return Ok(Some("a part merges into its parent".into()));
    }
    if item.state != State::Development {
        return Ok(Some(format!("it is {}", item.state)));
    }
    let open_q: Vec<String> = repo
        .index()?
        .inbox_all()?
        .into_iter()
        .filter(|e| e.kind == "question" && e.item == Some(item.id))
        .filter_map(|e| e.qid)
        .collect();
    if !open_q.is_empty() {
        return Ok(Some(format!("open question {}", open_q.join(", "))));
    }
    let wt = Path::new(&item.worktree);
    if !wt.is_dir() {
        return Ok(Some("its worktree is missing".into()));
    }
    let dirty = git::dirty_files(wt)?;
    if !dirty.is_empty() {
        return Ok(Some(format!("uncommitted changes in {}", dirty.join(", "))));
    }
    if !super::merge::open_children(repo, item.id)?.is_empty() {
        return Ok(Some("parts are still open".into()));
    }
    Ok(None)
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
                tokens: None,
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
                let status = std::process::Command::new(crate::config::Tool::Claude.command())
                    .arg("--continue")
                    .current_dir(&item.worktree)
                    .status();
                if status.is_err() {
                    eprintln!("could not start claude. Run: {open}");
                }
                return Ok(Response::raw(None));
            }
            let kept = if a.reason.is_some() {
                " (the --reason isn't kept in development: say it in the session)"
            } else {
                ""
            };
            let mut r = reply(
                &item,
                false,
                Some(format!(
                    "{} is in development. Reopen its session: {open}{kept}",
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
