//! The hallmark itself (ADR 0004): a git note under `refs/notes/hallmark` on
//! the approved commit, holding JSON. Only an approved verdict strikes one,
//! and the push gate reads it.

use std::path::Path;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::event::{Body, Event};
use crate::findings::Counts;
use crate::git::{self, NOTES_REF};
use crate::item::ItemId;
use crate::repo::Repo;

/// The note's body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Note {
    pub hallmark: u32,
    pub item: ItemId,
    pub sha: String,
    pub round: u32,
    /// The reviewer's model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    /// Who recorded the approval: `assayer`, or `you` for a human override
    /// with `hall verdict`. Absent on hallmarks struck before it was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// Findings still open (minors and nits) when it was approved.
    pub findings: Counts,
    pub t: DateTime<Utc>,
    /// Set when the approval was carried across a clean rebase with an
    /// identical patch (ADR 0030): the commit the review approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebased_from: Option<String>,
}

/// Writes the note on `sha`, naming `actor` as the one who approved it, and
/// records `hallmarked`. Safe to repeat: the
/// note is overwritten with the same content, and the event is written once.
pub fn strike(
    repo: &Repo,
    item: ItemId,
    sha: &str,
    round: u32,
    reviewer: Option<String>,
    actor: &str,
    findings: Counts,
) -> Result<Note> {
    let note = Note {
        hallmark: 1,
        item,
        sha: sha.to_string(),
        round,
        reviewer,
        actor: Some(actor.to_string()),
        findings,
        t: Utc::now(),
        rebased_from: None,
    };
    let body = serde_json::to_string(&note)?;
    // Concurrent strikes would each write a notes commit on the same parent
    // and the later ref update would drop the earlier note: serialize them.
    repo.hall.with_repo_lock(|| {
        git::run(
            &repo.main_root,
            &["notes", "--ref", NOTES_REF, "add", "-f", "-m", &body, sha],
        )
    })?;
    let seen = repo
        .hall
        .read_item(item)?
        .iter()
        .any(|e| matches!(&e.body, Body::Hallmarked { sha: s, .. } if s == sha));
    if !seen {
        repo.hall.append_item(
            item,
            &Event::new(
                "assayer",
                Some(item),
                Body::Hallmarked {
                    sha: sha.to_string(),
                    round,
                },
            ),
        )?;
    }
    Ok(note)
}

/// Carries the hallmark on `from` to `to`, its clean rebase with the same
/// patch whose gates passed (ADR 0030): writes the note on `to`, naming
/// `from`, and records `hallmarked` at `to` as `actor`. Only the Dispatch
/// Dock calls it, after checking all of that.
pub fn carry(repo: &Repo, item: ItemId, from: &str, to: &str, actor: &str) -> Result<Note> {
    let old = read(&repo.main_root, from).ok_or_else(|| {
        crate::error::HallError::internal(format!("{from} has no hallmark to carry"))
    })?;
    let note = Note {
        sha: to.to_string(),
        t: Utc::now(),
        rebased_from: Some(from.to_string()),
        ..old
    };
    let body = serde_json::to_string(&note)?;
    repo.hall.with_repo_lock(|| {
        git::run(
            &repo.main_root,
            &["notes", "--ref", NOTES_REF, "add", "-f", "-m", &body, to],
        )
    })?;
    repo.hall.append_item(
        item,
        &Event::new(
            actor,
            Some(item),
            Body::Hallmarked {
                sha: to.to_string(),
                round: note.round,
            },
        ),
    )?;
    Ok(note)
}

/// The hallmark on `rev`, if it has one that parses.
pub fn read(dir: &Path, rev: &str) -> Option<Note> {
    serde_json::from_str(&git::hallmark_note(dir, rev)?).ok()
}
