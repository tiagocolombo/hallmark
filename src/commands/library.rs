//! `hall library search|review|retire|distill|import|export` (ADR 0021,
//! ADR 0027). Every change is to the local store, under its lock, and takes
//! effect at once.

use std::path::{Path, PathBuf};

use chrono::Utc;
use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::{LibraryAction, LibraryArgs};
use crate::commands::new::shell_quote;
use crate::config::SearchBackend;
use crate::error::{ErrorCode, HallError, Result};
use crate::item::ItemId;
use crate::library::learning::{self, Kind, Status};
use crate::library::librarian::{self, Applied, Distill};
use crate::library::portable::{self, Entry, ExportFile, Imported};
use crate::library::search::{self, Hit, Options};
use crate::library::store::{self, Located, Store};
use crate::output::{Render, Response};
use crate::repo::Repo;

/// The reply of every `hall library` action, tagged by `action`.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum LibraryOut {
    Search(SearchOut),
    Review(ReviewOut),
    Retire(RetireOut),
    Distill(DistillOut),
    Import(ImportOut),
    Export(ExportOut),
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchOut {
    pub query: String,
    /// The backend that answered: builtin or qmd.
    pub backend: SearchBackend,
    /// Why the configured backend wasn't used, when it wasn't.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Best first. Accepted learnings from the local Library, plus proposed
    /// ones with `--proposed`. Retired learnings are never returned.
    pub results: Vec<Hit>,
}

/// A learning waiting for your decision.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Pending {
    pub id: String,
    pub kind: Kind,
    pub status: Status,
    /// new, or update (rejecting an update restores the version it
    /// replaced).
    pub change: String,
    pub title: String,
    pub scope: Vec<String>,
    /// The learning's file in the local store.
    pub path: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ReviewOut {
    /// Proposed learnings still waiting for `--accept` or `--reject`.
    pub proposed: Vec<Pending>,
    /// Accepted by this call (or already accepted). A retired learning
    /// given to `--accept` is reinstated.
    pub accepted: Vec<String>,
    /// Rejected by this call (or already rejected).
    pub rejected: Vec<String>,
    /// IDs given to `--reject` that the Library doesn't know.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unknown: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RetireOut {
    pub id: String,
    /// False when it was already retired.
    pub changed: bool,
    /// The learning's file in the local store.
    pub path: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DistillOut {
    /// The item distilled.
    pub item: ItemId,
    /// False when it had already been distilled since it last moved.
    pub changed: bool,
    #[serde(flatten)]
    pub applied: Applied,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ImportOut {
    /// Where the learnings came from: `<base>:<dir>`, a directory, or an
    /// export file.
    pub source: String,
    #[serde(flatten)]
    pub result: Imported,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ExportOut {
    /// Learnings exported, every status.
    pub count: usize,
    pub ids: Vec<String>,
    /// The file written, with `--out`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub out: Option<String>,
    /// The learning files, without `--out`. This reply is itself something
    /// `hall library import` reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub learnings: Option<Vec<Entry>>,
}

impl Render for LibraryOut {
    fn render(&self) -> String {
        match self {
            LibraryOut::Search(s) => {
                let mut t = String::new();
                if let Some(n) = &s.note {
                    t.push_str(&format!("note: {n}\n"));
                }
                if s.results.is_empty() {
                    t.push_str(&format!("No learnings match \"{}\".\n", s.query));
                }
                for h in &s.results {
                    t.push_str(&format!(
                        "{}  {:14} {:.2}  {}{}\n",
                        h.id,
                        h.kind.as_str(),
                        h.score,
                        h.title,
                        if h.status == Status::Accepted {
                            String::new()
                        } else {
                            format!("  ({})", h.status)
                        }
                    ));
                    if !h.scope.is_empty() {
                        t.push_str(&format!("        scope {}\n", h.scope.join(", ")));
                    }
                }
                t
            }
            LibraryOut::Review(r) => {
                let mut t = String::new();
                if !r.accepted.is_empty() {
                    t.push_str(&format!("Accepted {}.\n", r.accepted.join(", ")));
                }
                if !r.rejected.is_empty() {
                    t.push_str(&format!("Rejected {}.\n", r.rejected.join(", ")));
                }
                if !r.unknown.is_empty() {
                    t.push_str(&format!("Unknown: {}.\n", r.unknown.join(", ")));
                }
                if r.proposed.is_empty() {
                    t.push_str("No proposed learnings.\n");
                } else {
                    t.push_str("Proposed:\n");
                    for p in &r.proposed {
                        t.push_str(&format!(
                            "  {}  {:6} {:14} {}\n",
                            p.id,
                            p.change,
                            p.kind.as_str(),
                            p.title,
                        ));
                    }
                }
                t
            }
            LibraryOut::Retire(r) => {
                if r.changed {
                    format!("Retired {}.\n", r.id)
                } else {
                    format!("{} is already retired.\n", r.id)
                }
            }
            LibraryOut::Distill(d) => {
                let a = &d.applied;
                let what = if a.learnings.is_empty() && a.retired.is_empty() {
                    "nothing worth keeping".to_string()
                } else {
                    format!(
                        "{} learning(s), {} waiting for you, {} retired",
                        a.learnings.len(),
                        a.proposed.len(),
                        a.retired.len()
                    )
                };
                if d.changed {
                    format!("Distilled {}: {what}.\n", d.item)
                } else {
                    format!("{} was already distilled: {what}.\n", d.item)
                }
            }
            LibraryOut::Import(i) => {
                let mut t = format!(
                    "Imported {} learning(s) from {}.\n",
                    i.result.imported.len(),
                    i.source
                );
                for s in &i.result.skipped {
                    t.push_str(&format!("  skipped {}: {}\n", s.path, s.reason));
                }
                t
            }
            LibraryOut::Export(e) => match &e.out {
                Some(out) => format!("Exported {} learning(s) to {out}.\n", e.count),
                None => format!(
                    "{} learning(s): {}\n",
                    e.count,
                    if e.ids.is_empty() {
                        "none".to_string()
                    } else {
                        e.ids.join(", ")
                    }
                ),
            },
        }
    }
}

pub fn library(cwd: &Path, a: LibraryArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    repo.require_init()?;
    match a.action {
        LibraryAction::Search {
            query,
            limit,
            kind,
            proposed,
        } => search_cmd(&repo, query, limit, kind, proposed),
        LibraryAction::Review {
            accept,
            reject,
            reason,
        } => review(&repo, &accept, &reject, reason.as_deref()),
        LibraryAction::Retire { id, reason } => retire(&repo, &id, &reason),
        LibraryAction::Distill { id } => distill(&repo, &id),
        LibraryAction::Import { source } => import(&repo, source),
        LibraryAction::Export { out } => export(&repo, out),
    }
}

fn learning_id(s: &str) -> Result<String> {
    learning::normalize_id(s).ok_or_else(|| {
        HallError::usage(format!(
            "\"{s}\" is not a learning ID; IDs look like L-0017"
        ))
        .next(["hall library review", "hall library search \"<words>\""])
    })
}

fn not_found(id: &str) -> HallError {
    HallError::new(ErrorCode::ItemNotFound, format!("no learning {id}")).next([
        "hall library review",
        "hall library search \"<words>\" --proposed",
    ])
}

fn search_cmd(
    repo: &Repo,
    query: String,
    limit: usize,
    kind: Option<String>,
    proposed: bool,
) -> Result<Response> {
    let kind = match kind {
        Some(k) => Some(Kind::parse(&k).ok_or_else(|| {
            HallError::usage(format!(
                "unknown kind `{k}`; use rule, false-positive, map or decision"
            ))
        })?),
        None => None,
    };
    if search::terms(&query).is_empty() {
        return Err(HallError::usage("say what to search for")
            .next(["hall library search \"webhook retries\""]));
    }
    let store = Store::of(repo);
    let (docs, statuses) = if proposed {
        (store.read(), vec![Status::Accepted, Status::Proposed])
    } else {
        (store.accepted(), vec![Status::Accepted])
    };
    let opts = Options {
        limit,
        kind,
        statuses,
    };
    let found = search::search(repo, &docs, &query, &opts)?;
    let mut next = Vec::new();
    if let Some(h) = found.hits.first() {
        next.push(format!(
            "hall library retire {} --reason \"<why it no longer holds>\"",
            h.id
        ));
    } else if !proposed {
        next.push(format!(
            "hall library search {} --proposed",
            shell_quote(&query)
        ));
    }
    next.push("hall library review".into());
    let out = LibraryOut::Search(SearchOut {
        query,
        backend: found.backend,
        note: found.note,
        results: found.hits,
    });
    Ok(Response::new(out, next))
}

/// Proposed learnings, in ID order.
fn pending(store: &Store) -> Vec<Pending> {
    store
        .read()
        .into_iter()
        .filter(|l| l.learning.status == Status::Proposed)
        .map(|l: Located| Pending {
            change: if store.previous(&l.learning.id).is_some() {
                "update".into()
            } else {
                "new".into()
            },
            id: l.learning.id,
            kind: l.learning.kind,
            status: l.learning.status,
            title: l.learning.title,
            scope: l.learning.scope,
            path: l.path,
        })
        .collect()
}

fn ids(list: &[String]) -> Result<Vec<String>> {
    list.iter()
        .filter(|s| !s.trim().is_empty())
        .map(|s| learning_id(s))
        .collect()
}

fn review(
    repo: &Repo,
    accept: &[String],
    reject: &[String],
    reason: Option<&str>,
) -> Result<Response> {
    let accept = ids(accept)?;
    let reject = ids(reject)?;
    if let Some(both) = accept.iter().find(|a| reject.contains(a)) {
        return Err(HallError::usage(format!(
            "{both} is in both --accept and --reject"
        )));
    }
    let store = Store::of(repo);
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    let mut unknown = Vec::new();
    if !accept.is_empty() || !reject.is_empty() {
        store.with_lock(|| {
            // Check every ID before writing anything.
            for id in &accept {
                if store.get(id).is_none() {
                    return Err(not_found(id));
                }
            }
            for id in &reject {
                if let Some(l) = store.get(id)
                    && l.learning.status != Status::Proposed
                    && !store.was_rejected(id)
                {
                    return Err(HallError::usage(format!(
                        "{id} is {}, not proposed; retire it instead",
                        l.learning.status
                    ))
                    .next([format!("hall library retire {id} --reason \"<why>\"")]));
                }
            }
            for id in &accept {
                let cur = store.get(id).ok_or_else(|| not_found(id))?;
                match cur.learning.status {
                    Status::Proposed => {
                        let mut l = cur.learning;
                        l.status = Status::Accepted;
                        store.write(&l)?;
                    }
                    Status::Retired => {
                        // Reinstated: the last accepted version, else this
                        // one.
                        let mut l = store
                            .previous(id)
                            .map(|v| v.learning)
                            .filter(|l| l.status == Status::Accepted)
                            .unwrap_or(cur.learning);
                        l.status = Status::Accepted;
                        store.write(&l)?;
                    }
                    Status::Accepted => {}
                }
                accepted.push(id.clone());
            }
            let now = Utc::now();
            for id in &reject {
                if store.was_rejected(id) {
                    rejected.push(id.clone());
                } else if store.get(id).is_some() {
                    store.reject(id, reason, now)?;
                    rejected.push(id.clone());
                } else {
                    unknown.push(id.clone());
                }
            }
            Ok(())
        })?;
    }
    let proposed = pending(&store);
    let mut next = Vec::new();
    if proposed.is_empty() {
        next.push("hall library search \"<words>\"".to_string());
        next.push("hall inbox".to_string());
    } else {
        let ids: Vec<&str> = proposed.iter().map(|p| p.id.as_str()).collect();
        next.push(format!("hall library review --accept {}", ids.join(",")));
        next.push(format!(
            "hall library review --reject {} --reason \"<why>\"",
            ids[0]
        ));
    }
    let out = LibraryOut::Review(ReviewOut {
        proposed,
        accepted,
        rejected,
        unknown,
    });
    Ok(Response::new(out, next))
}

fn retire(repo: &Repo, id: &str, reason: &str) -> Result<Response> {
    let id = learning_id(id)?;
    if reason.trim().is_empty() {
        return Err(HallError::usage("say why with --reason"));
    }
    let store = Store::of(repo);
    let out = store.with_lock(|| {
        let cur = store.get(&id).ok_or_else(|| not_found(&id))?;
        if cur.learning.status == Status::Retired {
            return Ok(RetireOut {
                id: id.clone(),
                changed: false,
                path: cur.path,
            });
        }
        let mut l = cur.learning;
        librarian::retire_text(&mut l, reason, Utc::now());
        let stats = store::usage(repo)?;
        librarian::refresh_usage(&mut l, &stats);
        let written = store.write(&l)?;
        Ok(RetireOut {
            id: id.clone(),
            changed: true,
            path: written.path,
        })
    })?;
    let next = vec![
        format!("hall library review --accept {id}"),
        "hall library search \"<words>\"".to_string(),
    ];
    Ok(Response::new(LibraryOut::Retire(out), next))
}

fn distill(repo: &Repo, id: &str) -> Result<Response> {
    let id: ItemId = id.parse()?;
    let runner = crate::runner::from_env();
    let (applied, fresh) = match librarian::distill(repo, runner.as_ref(), id)? {
        Distill::Done { applied, fresh } => (applied, fresh),
        Distill::Stopped { error, .. } => return Err(error),
    };
    let mut next = Vec::new();
    if Store::of(repo).proposed().0 > 0 {
        next.push("hall library review".into());
    }
    next.push(format!("hall log {id} --kind distilled"));
    let out = LibraryOut::Distill(DistillOut {
        item: id,
        changed: fresh,
        applied,
    });
    Ok(Response::new(out, next))
}

fn import(repo: &Repo, source: Option<PathBuf>) -> Result<Response> {
    let store = Store::of(repo);
    let dir = portable::committed_dir(repo);
    let (label, entries, from_base) = match source {
        None => {
            let base = &repo.config.base;
            (
                format!("{base}:{dir}"),
                portable::read_rev(repo, base)?,
                true,
            )
        }
        Some(p) => {
            let p = if p.is_absolute() { p } else { repo.cwd.join(p) };
            let label = crate::paths::display(&p);
            let entries = if p.is_dir() {
                portable::read_dir(&p)?
            } else if p.is_file() {
                let text = std::fs::read_to_string(&p)?;
                portable::parse_export(&text).map_err(|e| {
                    HallError::usage(format!("{label} is not a Library export: {e}")).next([
                        "hall library export --out <file>",
                        "hall library import <directory of learning files>",
                    ])
                })?
            } else {
                return Err(
                    HallError::usage(format!("no file or directory {label}")).next([
                        "hall library import",
                        "hall library import <export file or directory>",
                    ]),
                );
            };
            (label, entries, false)
        }
    };
    let result = store.with_lock(|| portable::import(&store, &entries))?;
    let mut next = Vec::new();
    if from_base && !entries.is_empty() {
        next.push(format!(
            "git rm -r -q {} && git commit -m \"Move the Library out of the repo\"",
            shell_quote(&dir)
        ));
    }
    if store.proposed().0 > 0 {
        next.push("hall library review".into());
    }
    next.push("hall library search \"<words>\"".into());
    let out = LibraryOut::Import(ImportOut {
        source: label,
        result,
    });
    Ok(Response::new(out, next))
}

fn export(repo: &Repo, out: Option<PathBuf>) -> Result<Response> {
    let store = Store::of(repo);
    let entries = portable::export(&store)?;
    let ids: Vec<String> = entries
        .iter()
        .filter_map(|e| learning::id_from_file_name(&e.path).map(learning::format_id))
        .collect();
    let count = entries.len();
    let reply = match out {
        Some(p) => {
            let p = if p.is_absolute() { p } else { repo.cwd.join(p) };
            let file = ExportFile {
                format: portable::FORMAT.into(),
                version: 1,
                learnings: entries,
            };
            std::fs::write(&p, serde_json::to_string_pretty(&file)? + "\n")?;
            let shown = crate::paths::display(&p);
            let next = vec![format!("hall library import {}", shell_quote(&shown))];
            let out = ExportOut {
                count,
                ids,
                out: Some(shown),
                learnings: None,
            };
            return Ok(Response::new(LibraryOut::Export(out), next));
        }
        None => ExportOut {
            count,
            ids,
            out: None,
            learnings: Some(entries),
        },
    };
    Ok(Response::new(
        LibraryOut::Export(reply),
        vec!["hall library export --out <file>".to_string()],
    ))
}
