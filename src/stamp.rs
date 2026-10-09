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
    /// Findings still open (minors and nits) when it was approved.
    pub findings: Counts,
    pub t: DateTime<Utc>,
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
    };
    let body = serde_json::to_string(&note)?;
    git::run(
        &repo.main_root,
        &["notes", "--ref", NOTES_REF, "add", "-f", "-m", &body, sha],
    )?;
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

/// The hallmark on `rev`, if it has one that parses.
pub fn read(dir: &Path, rev: &str) -> Option<Note> {
    serde_json::from_str(&git::hallmark_note(dir, rev)?).ok()
}
