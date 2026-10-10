//! `hall library search|review|retire|distill` (ADR 0021).

use std::collections::BTreeSet;
use std::path::Path;

use chrono::Utc;
use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::{LibraryAction, LibraryArgs};
use crate::config::SearchBackend;
use crate::error::{ErrorCode, HallError, Result};
use crate::item::ItemId;
use crate::library::knowledge::{self, Knowledge};
use crate::library::learning::{self, Kind, Status};
use crate::library::librarian::{self, Applied, Distill};
use crate::library::search::{self, Hit, Options};
use crate::library::store::{self, Library, Located};
use crate::output::{Render, Response};
use crate::repo::{Repo, actor};

/// The reply of every `hall library` action, tagged by `action`.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum LibraryOut {
    Search(SearchOut),
    Review(ReviewOut),
    Retire(RetireOut),
    Distill(DistillOut),
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchOut {
    pub query: String,
    /// The backend that answered: builtin or qmd.
    pub backend: SearchBackend,
    /// Why the configured backend wasn't used, when it wasn't.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Best first. Accepted learnings from the base branch, plus proposed
    /// ones with `--proposed`. Retired learnings are never returned.
    pub results: Vec<Hit>,
}

/// A learning waiting for a decision.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Pending {
    pub id: String,
    pub kind: Kind,
    pub status: Status,
    /// new, update, or retire.
    pub change: String,
    pub title: String,
    pub scope: Vec<String>,
    pub path: String,
    /// The knowledge item that carries it; absent when it is proposed on
    /// the base branch itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ReviewOut {
    /// Proposed learnings still waiting for `--accept` or `--reject`.
    pub proposed: Vec<Pending>,
    /// Retirements in open knowledge items (`--reject` keeps the learning).
    pub retiring: Vec<Pending>,
    /// Accepted by this call (or already accepted).
    pub accepted: Vec<String>,
    /// Rejected by this call.
    pub rejected: Vec<String>,
    /// IDs given to `--reject` that no longer exist anywhere.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unknown: Vec<String>,
    /// Open knowledge items.
    pub items: Vec<ItemId>,
    /// Commits this call made, one per knowledge item it changed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RetireOut {
    pub id: String,
    /// The knowledge item that carries the retirement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
    /// False when it was already retired.
    pub changed: bool,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
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
                        h.item
                            .map(|i| format!("  ({} in {i})", h.status))
                            .unwrap_or_default()
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
                if r.proposed.is_empty() && r.retiring.is_empty() {
                    t.push_str("No proposed learnings.\n");
                }
                for (label, list) in [("Proposed", &r.proposed), ("Retiring", &r.retiring)] {
                    if list.is_empty() {
                        continue;
                    }
                    t.push_str(&format!("{label}:\n"));
                    for p in list {
                        t.push_str(&format!(
                            "  {}  {:7} {:14} {}{}\n",
                            p.id,
                            p.change,
                            p.kind.as_str(),
                            p.title,
                            p.item.map(|i| format!("  ({i})")).unwrap_or_default()
                        ));
                    }
                }
                t
            }
            LibraryOut::Retire(r) => match (r.changed, r.item) {
                (true, Some(i)) => format!("{} will be retired by {i}.\n", r.id),
                _ => format!("{} is already retired.\n", r.id),
            },
            LibraryOut::Distill(d) => {
                let a = &d.applied;
                let what = match a.knowledge {
                    Some(k) => format!(
                        "{} learning(s) and {} retirement(s) in {k}",
                        a.learnings.len(),
                        a.retired.len()
                    ),
                    None => "nothing worth keeping".into(),
                };
                if d.changed {
                    format!("Distilled {}: {what}.\n", d.item)
                } else {
                    format!("{} was already distilled: {what}.\n", d.item)
                }
            }
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
    let base = repo.config.base.clone();
    let (docs, statuses) = if proposed {
        let lib = Library::load(repo, &base)?;
        let docs: Vec<Located> = lib.effective().into_iter().cloned().collect();
        (docs, vec![Status::Accepted, Status::Proposed])
    } else {
        (store::read_base(repo, &base)?, vec![Status::Accepted])
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
            crate::commands::new::shell_quote(&query)
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

/// Proposed learnings and pending retirements.
fn pending(lib: &Library) -> (Vec<Pending>, Vec<Pending>) {
    let entry = |l: &Located, change: &str| Pending {
        id: l.learning.id.clone(),
        kind: l.learning.kind,
        status: l.learning.status,
        change: change.into(),
        title: l.learning.title.clone(),
        scope: l.learning.scope.clone(),
        path: l.path.clone(),
        item: l.item,
    };
    let mut proposed = Vec::new();
    let mut retiring = Vec::new();
    let mut seen = BTreeSet::new();
    // The newest version of each ID wins.
    for l in lib.pending.iter().rev() {
        if !seen.insert(l.learning.id.clone()) {
            continue;
        }
        let base = lib.base_get(&l.learning.id);
        match l.learning.status {
            Status::Proposed => {
                proposed.push(entry(l, if base.is_some() { "update" } else { "new" }))
            }
            Status::Retired if base.is_some_and(|b| b.learning.status != Status::Retired) => {
                retiring.push(entry(l, "retire"))
            }
            _ => {}
        }
    }
    for b in &lib.base {
        if b.learning.status == Status::Proposed && !seen.contains(&b.learning.id) {
            proposed.push(entry(b, "new"));
        }
    }
    let key = |p: &Pending| learning::parse_id(&p.id).unwrap_or(0);
    proposed.sort_by_key(key);
    retiring.sort_by_key(key);
    (proposed, retiring)
}

/// The raw base file, for restoring it exactly.
fn base_text(repo: &Repo, path: &str) -> Result<String> {
    let s = crate::git::run(
        &repo.main_root,
        &["show", &format!("{}:{path}", repo.config.base)],
    )?;
    Ok(format!("{s}\n"))
}

/// Knowledge items touched by one call, with what changed in each.
#[derive(Default)]
struct Touched {
    items: Vec<(Knowledge, Vec<String>)>,
}

impl Touched {
    fn note(&mut self, k: &Knowledge, line: String) {
        match self.items.iter_mut().find(|(t, _)| t.item.id == k.item.id) {
            Some((_, lines)) => lines.push(line),
            None => self.items.push((k.clone(), vec![line])),
        }
    }

    fn get(&self, id: ItemId) -> Option<Knowledge> {
        self.items
            .iter()
            .find(|(k, _)| k.item.id == id)
            .map(|(k, _)| k.clone())
    }
}

fn knowledge_for(
    repo: &Repo,
    touched: &Touched,
    item: Option<ItemId>,
    who: &str,
    what: &str,
) -> Result<Knowledge> {
    if let Some(id) = item {
        return match touched.get(id) {
            Some(k) => Ok(k),
            None => knowledge::editable(repo, id),
        };
    }
    if let Some((k, _)) = touched.items.first() {
        return Ok(k.clone());
    }
    knowledge::get_or_create(repo, who, what)
}

fn review(
    repo: &Repo,
    accept: &[String],
    reject: &[String],
    reason: Option<&str>,
) -> Result<Response> {
    let accept: Vec<String> = accept
        .iter()
        .filter(|s| !s.trim().is_empty())
        .map(|s| learning_id(s))
        .collect::<Result<_>>()?;
    let reject: Vec<String> = reject
        .iter()
        .filter(|s| !s.trim().is_empty())
        .map(|s| learning_id(s))
        .collect::<Result<_>>()?;
    if let Some(both) = accept.iter().find(|a| reject.contains(a)) {
        return Err(HallError::usage(format!(
            "{both} is in both --accept and --reject"
        )));
    }
    let who = actor();
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    let mut unknown = Vec::new();
    let mut commits = Vec::new();
    if !accept.is_empty() || !reject.is_empty() {
        knowledge::with_lock(repo, || {
            let lib = Library::load(repo, &repo.config.base)?;
            // Check every ID before writing anything.
            for id in &accept {
                if lib.get(id).is_none() {
                    return Err(not_found(id));
                }
            }
            for id in &reject {
                if lib.pending_get(id).is_none()
                    && let Some(b) = lib.base_get(id)
                    && b.learning.status != Status::Proposed
                {
                    return Err(HallError::usage(format!(
                        "{id} is {}, not proposed; retire it instead",
                        b.learning.status
                    ))
                    .next([format!("hall library retire {id} --reason \"<why>\"")]));
                }
            }
            let mut touched = Touched::default();
            for id in &accept {
                if let Some(p) = lib.pending_get(id) {
                    if p.learning.status == Status::Proposed {
                        let k = knowledge_for(repo, &touched, p.item, &who, "accept learnings")?;
                        let mut l = p.learning.clone();
                        l.status = Status::Accepted;
                        knowledge::write(repo, &k, &l, Some(&p.path))?;
                        touched.note(&k, format!("- accept {id}: {}", l.title));
                    }
                } else if let Some(b) = lib.base_get(id) {
                    if b.learning.status == Status::Proposed {
                        let k = knowledge_for(repo, &touched, None, &who, "accept learnings")?;
                        let mut l = b.learning.clone();
                        l.status = Status::Accepted;
                        knowledge::write(repo, &k, &l, Some(&b.path))?;
                        touched.note(&k, format!("- accept {id}: {}", l.title));
                    }
                } else {
                    return Err(not_found(id));
                }
                accepted.push(id.clone());
            }
            for id in &reject {
                let base = lib.base_get(id);
                if let Some(p) = lib.pending_get(id) {
                    let k = knowledge_for(repo, &touched, p.item, &who, "reject learnings")?;
                    let restore = match base {
                        Some(b) => Some((b.path.clone(), base_text(repo, &b.path)?)),
                        None => None,
                    };
                    knowledge::restore(
                        repo,
                        &k,
                        id,
                        restore.as_ref().map(|(p, t)| (p.as_str(), t.as_str())),
                    )?;
                    touched.note(&k, format!("- reject {id}: {}", p.learning.title));
                    rejected.push(id.clone());
                } else if let Some(b) = base {
                    let k = knowledge_for(repo, &touched, None, &who, "reject learnings")?;
                    knowledge::restore(repo, &k, id, None)?;
                    touched.note(&k, format!("- reject {id}: {}", b.learning.title));
                    rejected.push(id.clone());
                } else {
                    unknown.push(id.clone());
                }
            }
            for (k, lines) in &touched.items {
                let verbs: Vec<&str> = [
                    (!accepted.is_empty()).then_some("accept"),
                    (!rejected.is_empty()).then_some("reject"),
                ]
                .into_iter()
                .flatten()
                .collect();
                let mut message = format!(
                    "docs(library): {} learnings\n\n{}",
                    verbs.join(" and "),
                    lines.join("\n")
                );
                if let Some(r) = reason {
                    message.push_str(&format!("\n\nReason: {r}"));
                }
                if let Some(sha) = knowledge::commit(repo, k, &who, &message)? {
                    commits.push(sha);
                }
                if k.created {
                    knowledge::propose_ready(
                        repo,
                        k,
                        &who,
                        "Library: decisions on proposed learnings.",
                    )?;
                }
            }
            Ok(())
        })?;
    }
    let lib = Library::load(repo, &repo.config.base)?;
    let (proposed, retiring) = pending(&lib);
    let items: Vec<ItemId> = store::open_knowledge(repo)?.iter().map(|i| i.id).collect();
    let mut next = Vec::new();
    if proposed.is_empty() {
        for k in store::open_knowledge(repo)? {
            if store::editable(&k) {
                next.push(format!("hall ready {}", k.id));
            }
        }
    } else {
        let ids: Vec<&str> = proposed.iter().map(|p| p.id.as_str()).collect();
        next.push(format!("hall library review --accept {}", ids.join(",")));
        next.push(format!(
            "hall library review --reject {} --reason \"<why>\"",
            ids[0]
        ));
    }
    if !retiring.is_empty() {
        next.push(format!(
            "hall library review --reject {} --reason \"<why keep it>\"",
            retiring[0].id
        ));
    }
    if next.is_empty() {
        next.push("hall library search \"<words>\"".into());
    }
    let out = LibraryOut::Review(ReviewOut {
        proposed,
        retiring,
        accepted,
        rejected,
        unknown,
        items,
        commits,
    });
    Ok(Response::new(out, next))
}

fn retire(repo: &Repo, id: &str, reason: &str) -> Result<Response> {
    let id = learning_id(id)?;
    if reason.trim().is_empty() {
        return Err(HallError::usage("say why with --reason"));
    }
    let who = actor();
    let out = knowledge::with_lock(repo, || {
        let lib = Library::load(repo, &repo.config.base)?;
        let cur = lib.get(&id).ok_or_else(|| not_found(&id))?;
        if cur.learning.status == Status::Retired {
            return Ok(RetireOut {
                id: id.clone(),
                item: cur.item,
                changed: false,
                path: cur.path.clone(),
                commit: None,
            });
        }
        let k = match cur.item {
            Some(item) => knowledge::editable(repo, item)?,
            None => knowledge::get_or_create(repo, &who, &format!("retire {id}"))?,
        };
        let now = Utc::now();
        let mut l = cur.learning.clone();
        l.status = Status::Retired;
        l.body = format!(
            "{}\n\nRetired {}: {}",
            l.body.trim(),
            now.format("%Y-%m-%d"),
            reason.trim()
        )
        .trim()
        .to_string();
        let stats = store::usage(repo)?;
        librarian::refresh_usage(&mut l, &stats);
        let base_path = lib.base_get(&id).map(|b| b.path.clone());
        let path = knowledge::write(repo, &k, &l, base_path.as_deref())?;
        let commit = knowledge::commit(
            repo,
            &k,
            &who,
            &format!("docs(library): retire {id}\n\n{}", reason.trim()),
        )?;
        knowledge::propose_ready(
            repo,
            &k,
            &who,
            &format!("Library: retire {id} ({})", crate::output::clip(reason, 80)),
        )?;
        librarian::note_retired(repo, std::slice::from_ref(&id), k.item.id, now)?;
        Ok(RetireOut {
            id: id.clone(),
            item: Some(k.item.id),
            changed: true,
            path,
            commit,
        })
    })?;
    let mut next = vec!["hall library review".to_string()];
    if let Some(k) = out.item {
        next.insert(0, format!("hall ready {k}"));
    }
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
    if let Some(k) = applied.knowledge {
        next.push("hall library review".into());
        next.push(format!("hall ready {k}"));
    }
    next.push(format!("hall log {id} --kind distilled"));
    let out = LibraryOut::Distill(DistillOut {
        item: id,
        changed: fresh,
        applied,
    });
    Ok(Response::new(out, next))
}
