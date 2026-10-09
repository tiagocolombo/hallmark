//! `hall fixed` (workstream C, ADR 0019): the fixer records which findings
//! its commit fixes, disputes others, and resubmits the item.
//!
//! The fixer can never approve: `hall fixed` only ever records
//! `fix_submitted`, which sends the item back to checks. Only a later
//! review settles the findings it claims (ADR 0015).

use std::path::Path;

use chrono::Utc;
use schemars::JsonSchema;
use serde::Serialize;

use super::ItemView;
use crate::cli::FixedArgs;
use crate::error::{ErrorCode, HallError, Result};
use crate::event::{Body, Event, short};
use crate::findings::{Dispute, Ledger};
use crate::git;
use crate::item::{Item, ItemId, State};
use crate::journal::{Ticket, ev};
use crate::output::{Render, Response};
use crate::repo::{Repo, actor};

#[derive(Debug, Serialize, JsonSchema)]
pub struct FixedOut {
    #[serde(flatten)]
    pub item: ItemView,
    /// The resubmitted commit (the item's HEAD).
    pub sha: String,
    /// Findings this commit claims to fix.
    pub fixes: Vec<String>,
    /// Findings disputed instead of fixed, with the reason.
    pub disputes: Vec<Dispute>,
    /// False when this exact submission was already recorded.
    pub changed: bool,
}

impl Render for FixedOut {
    fn render(&self) -> String {
        let mut s = format!(
            "{} resubmitted at {} (round {})",
            self.item.id,
            short(&self.sha),
            self.item.round
        );
        if !self.changed {
            s.push_str(", already recorded");
        }
        s.push('\n');
        if !self.fixes.is_empty() {
            s.push_str(&format!("  fixes     {}\n", self.fixes.join(", ")));
        }
        for d in &self.disputes {
            s.push_str(&format!("  disputes  {}: {}\n", d.id, d.reason));
        }
        s
    }
}

pub fn fixed(cwd: &Path, a: FixedArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    let item = repo.resolve(a.item.as_deref())?;
    let reason = a.reason.as_deref().map(str::trim).unwrap_or_default();
    if !a.dispute.is_empty() && reason.is_empty() {
        return Err(HallError::usage(
            "--dispute needs a --reason: say concretely why the finding is wrong",
        ));
    }
    if a.dispute.is_empty() && a.reason.is_some() {
        return Err(HallError::usage(
            "--reason goes with --dispute <finding id>",
        ));
    }
    let disputes = split_ids(&a.dispute)
        .into_iter()
        .map(|id| Dispute {
            id,
            reason: reason.to_string(),
        })
        .collect();
    let out = submit(&repo, item.id, split_ids(&a.ids), disputes, &actor())?;
    let id = out.item.id;
    Ok(Response::new(
        out,
        vec![
            format!("hall status {id}"),
            format!("hall findings {id} --all"),
            format!("hall log {id}"),
        ],
    ))
}

/// `F1`, `f2`, `F3,F4` → `F1`, `F2`, `F3`, `F4`, without repeats.
pub fn split_ids(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in raw.iter().flat_map(|r| r.split([',', ' '])) {
        let id = part.trim().to_ascii_uppercase();
        if !id.is_empty() && !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// Records a fix submission for `id` at its worktree's HEAD and queues the
/// item for checks. Shared by `hall fixed` and the fixer worker, so both
/// apply the same rules:
///
/// - the item must be in fixing, its worktree clean of tracked changes;
/// - every ID must be an open finding;
/// - HEAD must differ from the reviewed commit, unless the submission only
///   disputes (and the checks didn't fail there);
/// - repeating a recorded submission returns it with `changed: false`.
pub fn submit(
    repo: &Repo,
    id: ItemId,
    fixes: Vec<String>,
    disputes: Vec<Dispute>,
    actor: &str,
) -> Result<FixedOut> {
    let fixes = split_ids(&fixes);
    let mut ds: Vec<Dispute> = Vec::new();
    for d in disputes {
        let did = d.id.trim().to_ascii_uppercase();
        if d.reason.trim().is_empty() {
            return Err(HallError::usage(format!(
                "the dispute of {did} needs a reason"
            )));
        }
        if !ds.iter().any(|x| x.id == did) {
            ds.push(Dispute {
                id: did,
                reason: d.reason.trim().to_string(),
            });
        }
    }
    let disputes = ds;
    if let Some(both) = fixes.iter().find(|f| disputes.iter().any(|d| &d.id == *f)) {
        return Err(HallError::usage(format!(
            "{both} is both fixed and disputed; pick one"
        )));
    }
    let item = repo.hall.load_item(id)?;
    let wt = Path::new(&item.worktree);
    if !wt.is_dir() {
        return Err(HallError::new(
            ErrorCode::WrongState,
            format!("{id}'s worktree {} is missing", item.worktree),
        )
        .next([format!("hall drop {id} --reason \"worktree gone\"")]));
    }
    let head = git::head(wt)?;
    let dirty = git::dirty_files(wt)?;
    let (item, (changed, fixes, disputes)) = repo.hall.transact(id, |item, events| {
        if item.state != State::Fixing {
            if let Some((sha, f, d)) = last_submission(events)
                && sha == head
                && same(&f, &fixes)
                && same(
                    &d.iter().map(|x| x.id.clone()).collect::<Vec<_>>(),
                    &disputes.iter().map(|x| x.id.clone()).collect::<Vec<_>>(),
                )
            {
                return Ok((vec![], (false, f, d)));
            }
            return Err(wrong_state(
                item,
                "only items in fixing take a fix (`hall fixed`)",
            ));
        }
        if !dirty.is_empty() {
            return Err(HallError::new(
                ErrorCode::DirtyWorktree,
                format!(
                    "{} has uncommitted changes ({}); a fix is a commit",
                    item.id,
                    dirty.join(", ")
                ),
            )
            .next([
                "git add -A && git commit -m \"fix <finding ids>: <what>\"".to_string(),
                format!("hall fixed <finding ids> --item {}", item.id),
            ]));
        }
        let ledger = Ledger::from_events(events);
        let open: Vec<String> = ledger
            .open()
            .iter()
            .map(|t| t.finding.id.clone())
            .collect();
        let unknown: Vec<&String> = fixes
            .iter()
            .chain(disputes.iter().map(|d| &d.id))
            .filter(|i| !open.contains(i))
            .collect();
        if !unknown.is_empty() {
            let names: Vec<&str> = unknown.iter().map(|s| s.as_str()).collect();
            let open_list = if open.is_empty() {
                "none are open".to_string()
            } else {
                format!("open: {}", open.join(", "))
            };
            return Err(HallError::new(
                ErrorCode::Usage,
                format!(
                    "{} {} not an open finding of {} ({open_list})",
                    names.join(", "),
                    if names.len() == 1 { "is" } else { "are" },
                    item.id
                ),
            )
            .next([format!("hall findings {}", item.id)]));
        }
        let moved = item.frozen_sha.as_deref() != Some(head.as_str());
        if !moved {
            let reviewed = short(item.frozen_sha.as_deref().unwrap_or("?")).to_string();
            let nothing = |msg: String| {
                Err(HallError::new(ErrorCode::WrongState, msg).next([
                    "git add -A && git commit -m \"fix <finding ids>: <what>\"".to_string(),
                    format!("hall fixed <finding ids> --item {}", item.id),
                ]))
            };
            if disputes.is_empty() {
                return nothing(format!(
                    "nothing committed: HEAD is still {reviewed}, the commit under review"
                ));
            }
            if !fixes.is_empty() {
                return nothing(format!(
                    "{} claimed fixed, but HEAD is still {reviewed}; commit the fix first",
                    fixes.join(", ")
                ));
            }
            if !crate::fixer::failing_checks(events).is_empty() {
                return nothing(format!(
                    "the checks failed at {reviewed}; fix them and commit before resubmitting"
                ));
            }
        }
        let e = ev(
            actor,
            Some(item.id),
            Body::FixSubmitted {
                sha: head.clone(),
                fixes: fixes.clone(),
                disputes: disputes.clone(),
            },
        );
        Ok((vec![e], (true, fixes.clone(), disputes.clone())))
    })?;
    if changed {
        repo.hall.queue(&Ticket {
            id: item.id,
            sha: head.clone(),
            round: item.round,
            queued_at: Utc::now(),
        })?;
        // The fix is in: whoever held the fixer lease is done, and the
        // assayer must not wait for it to expire.
        if let Some(l) = crate::lease::current(&repo.hall, item.id)
            && l.role == "fixer"
        {
            let _ = crate::lease::release(&repo.hall, item.id, &l.holder);
        }
        repo.hall.live(&format!(
            "{} fix submitted at {} by {actor}",
            item.id,
            short(&head)
        ));
    }
    Ok(FixedOut {
        item: ItemView::from(&item),
        sha: head,
        fixes,
        disputes,
        changed,
    })
}

/// The last `fix_submitted`: (sha, fixes, disputes).
fn last_submission(events: &[Event]) -> Option<(String, Vec<String>, Vec<Dispute>)> {
    events.iter().rev().find_map(|e| match &e.body {
        Body::FixSubmitted {
            sha,
            fixes,
            disputes,
        } => Some((sha.clone(), fixes.clone(), disputes.clone())),
        _ => None,
    })
}

fn same(a: &[String], b: &[String]) -> bool {
    let mut a = a.to_vec();
    let mut b = b.to_vec();
    a.sort();
    b.sort();
    a == b
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
    .next(super::next_for(item))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_normalized() {
        let raw = vec!["f1".to_string(), "F2,f3".into(), "F1".into(), " ".into()];
        assert_eq!(split_ids(&raw), ["F1", "F2", "F3"]);
    }

    #[test]
    fn sets_compare_unordered() {
        assert!(same(&["F2".into(), "F1".into()], &["F1".into(), "F2".into()]));
        assert!(!same(&["F1".into()], &["F1".into(), "F2".into()]));
    }
}
