//! The hallmark itself (ADR 0004): a git note under `refs/notes/hallmark` on
//! the approved commit, holding JSON. Only an approved verdict strikes one,
//! and the push gate verifies it (ADR 0045).

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
    /// Findings still open (minors and nits) when it was approved.
    pub findings: Counts,
    pub t: DateTime<Utc>,
    /// Set when the approval was carried across a clean rebase with an
    /// identical patch (ADR 0030): the commit the review approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebased_from: Option<String>,
}

/// Writes the note on `sha` and records `hallmarked`. Safe to repeat: the
/// note is overwritten with the same content, and the event is written once.
pub fn strike(
    repo: &Repo,
    item: ItemId,
    sha: &str,
    round: u32,
    reviewer: Option<String>,
    findings: Counts,
) -> Result<Note> {
    let note = Note {
        hallmark: 1,
        item,
        sha: sha.to_string(),
        round,
        reviewer,
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

/// Names, in the note on `sha`, the hallmarked commit it is a rebase of,
/// once the approval was carried to it in review (ADR 0049).
pub fn name_rebased_from(repo: &Repo, sha: &str, from: &str) -> Result<()> {
    let Some(mut note) = read(&repo.main_root, sha) else {
        return Ok(());
    };
    note.rebased_from = Some(from.to_string());
    let body = serde_json::to_string(&note)?;
    repo.hall.with_repo_lock(|| {
        git::run(
            &repo.main_root,
            &["notes", "--ref", NOTES_REF, "add", "-f", "-m", &body, sha],
        )
    })?;
    Ok(())
}

/// The hallmark on `rev`, if it has one that parses.
pub fn read(dir: &Path, rev: &str) -> Option<Note> {
    serde_json::from_str(&git::hallmark_note(dir, rev)?).ok()
}

/// Why the note on `head` isn't a hallmark Hallmark struck (ADR 0045).
#[derive(Debug, Clone, PartialEq)]
pub enum Unverified {
    /// No note at all.
    Missing,
    /// A note that doesn't parse as a [`Note`].
    Unparsed,
    /// A note of a version this hall doesn't know.
    Version(u32),
    /// A note for another commit, copied or written onto this one.
    OtherSha(String),
    /// A note whose item's journal records no `hallmarked` at this commit.
    NotStruck(ItemId),
}

impl Unverified {
    /// Says what is wrong, naming `head` and the mismatch.
    pub fn reason(&self, head: &str) -> String {
        let at = short(head);
        match self {
            Unverified::Missing => format!("{at} has no hallmark"),
            Unverified::Unparsed => {
                format!("{at} has no hallmark: its note doesn't parse as a hallmark")
            }
            Unverified::Version(v) => format!(
                "{at} has no hallmark: its note is hallmark version {v}, and this hall knows \
                 version 1"
            ),
            Unverified::OtherSha(sha) => format!(
                "{at} has no hallmark: its note is for {}, not {at}",
                short(sha)
            ),
            Unverified::NotStruck(item) => format!(
                "{at} has no hallmark: its note names {item}, but {item}'s journal records no \
                 hallmark on {at}"
            ),
        }
    }
}

/// The hallmark on `head`, verified: the note parses, is version 1, names
/// `head` itself, and its item's journal records `hallmarked` at `head`. A
/// note anyone can write with `git notes` passes none of that unless
/// Hallmark struck it.
pub fn verify(repo: &Repo, dir: &Path, head: &str) -> std::result::Result<Note, Unverified> {
    let text = git::hallmark_note(dir, head).ok_or(Unverified::Missing)?;
    let note: Note = serde_json::from_str(&text).map_err(|_| Unverified::Unparsed)?;
    if note.hallmark != 1 {
        return Err(Unverified::Version(note.hallmark));
    }
    if note.sha != head {
        return Err(Unverified::OtherSha(note.sha));
    }
    // An item the journal can't read has struck nothing.
    let struck = repo.hall.read_item(note.item).is_ok_and(|events| {
        events
            .iter()
            .any(|e| matches!(&e.body, Body::Hallmarked { sha, .. } if sha == head))
    });
    if !struck {
        return Err(Unverified::NotStruck(note.item));
    }
    Ok(note)
}

/// The first seven characters of a sha, safe for one that came from a note
/// anyone could write.
fn short(sha: &str) -> String {
    sha.chars().take(7).collect()
}
