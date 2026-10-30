//! Records in the repository (ADR 0066). With `[library] commit = true`, the
//! Library's accepted learnings and each finished item's Gazette page are
//! kept in the repository as well as in the local store and the journal.
//!
//! - **What is committed.** Every accepted learning, as `[library] dir`
//!   holds it (`L-0017-<slug>.md`), without the usage counts that change
//!   with every review; while an update waits for you, the accepted version
//!   stands in, as it does for briefings. A learning the store knows that is
//!   no longer accepted is removed; files the store doesn't know (another
//!   clone's) are left alone. Each closed or dropped item's Gazette page is
//!   copied to `<gazette_dir>/<id>.html`, and copied again when it was
//!   reprinted from more journal events (a merge, CI), not when only the
//!   hall that printed it changed.
//! - **How it reaches the base.** Through a records item, an ordinary item
//!   titled `Records: …`: the librarian creates it with `hall new`, commits
//!   the files in its worktree and runs `hall propose-ready`. From there it
//!   is checked, reviewed, shipped and merged like any other work; nothing
//!   is pushed straight to the base. Until a records item has shipped, each
//!   one waits for your `hall ready`, so a misconfigured repository never
//!   publishes on its own, and dropping one is never taken as a yes.
//! - **One at a time.** While a records item is open, or its PR is, nothing
//!   new is started; what changed meanwhile goes in the next one. A check
//!   that found nothing to do, or started an item, is remembered with the
//!   base tip it looked at, so it runs again only when the store, a page or
//!   the base changes. That also keeps a records item you dropped from
//!   coming straight back.
//! - The local store stays the source of truth: reviews never read
//!   learnings from a branch (ADR 0027).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::learning::{self, Status};
use super::portable;
use super::store::Store;
use crate::error::{ErrorCode, HallError, Result};
use crate::item::{Item, ItemId, State};
use crate::repo::Repo;

/// Records items are titled `Records: …`.
pub const PREFIX: &str = "Records:";

/// Whether `item` is a records item (by its title).
pub fn is_records(item: &Item) -> bool {
    item.title.starts_with(PREFIX)
}

/// What the repository is missing, compared with its base.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Plan {
    /// Learnings to write: `(repo path, text, ID)`.
    pub learnings: Vec<(String, String, String)>,
    /// Learning files to remove: `(repo path, ID)`.
    pub removed: Vec<(String, String)>,
    /// Gazette pages to copy: `(repo path, journal page, item)`.
    pub pages: Vec<(String, PathBuf, ItemId)>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.learnings.is_empty() && self.removed.is_empty() && self.pages.is_empty()
    }
}

/// What the last check saw, so an unchanged store, journal and base cost
/// nothing (`library/records.json`).
#[derive(Debug, Default, Serialize, Deserialize)]
struct Seen {
    /// The fingerprint of the inputs and the base tip of the last check
    /// that found nothing to do or started a records item.
    #[serde(default)]
    checked: Option<String>,
    /// The records item the last check started.
    #[serde(default)]
    item: Option<String>,
}

fn state_path(repo: &Repo) -> PathBuf {
    repo.hall.root.join("library").join("records.json")
}

fn load(repo: &Repo) -> Seen {
    std::fs::read_to_string(state_path(repo))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(repo: &Repo, st: &Seen) -> Result<()> {
    let path = state_path(repo);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&path, serde_json::to_string(st)?)?;
    Ok(())
}

/// A directory setting as a repo-relative path: forward slashes, no
/// leading or trailing slash.
fn rel_dir(d: &str, default: &str) -> String {
    let d = d.replace('\\', "/");
    let d = d.trim_matches('/');
    if d.is_empty() {
        default.to_string()
    } else {
        d.to_string()
    }
}

pub fn gazette_dir(repo: &Repo) -> String {
    rel_dir(&repo.config.library.gazette_dir, "docs/gazette")
}

/// A learning as the repository keeps it: accepted, with no usage counts.
pub fn committed_text(l: &learning::Learning) -> String {
    let mut l = l.clone();
    l.status = Status::Accepted;
    l.used = 0;
    l.last_used = None;
    l.render()
}

/// The journal events a page was printed from, from its head.
fn events_of(head: &str) -> Option<String> {
    let at = head.find("<meta name=\"hall-events\" content=\"")?;
    let rest = &head[at + "<meta name=\"hall-events\" content=\"".len()..];
    rest.split('"').next().map(str::to_string)
}

fn page_head(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut head = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(2048).read_to_end(&mut head))
        .ok()?;
    Some(String::from_utf8_lossy(&head).to_string())
}

/// A printed Gazette page the repository should hold.
struct Printed {
    /// `<gazette_dir>/<id>.html`.
    path: String,
    /// The page in the journal.
    page: PathBuf,
    id: ItemId,
    /// How many journal events it was printed from.
    events: String,
}

/// Items whose page belongs in the repository: finished (closed or
/// dropped), printed, and neither a records item nor a knowledge item.
fn pages(repo: &Repo) -> Result<Vec<Printed>> {
    let dir = gazette_dir(repo);
    let mut out = Vec::new();
    for i in repo.index()?.items(true)? {
        if !matches!(i.state, State::Closed | State::Dropped)
            || is_records(&i)
            || super::librarian::is_knowledge(&i)
        {
            continue;
        }
        let page = crate::gazette::page_path(repo, i.id);
        let Some(events) = page_head(&page).as_deref().and_then(events_of) else {
            continue;
        };
        out.push(Printed {
            path: format!("{dir}/{}.html", i.id),
            page,
            id: i.id,
            events,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// FNV-1a, 64 bits: stable across builds, which `DefaultHasher` isn't.
fn fnv(parts: &[&str]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.bytes().chain(std::iter::once(0)) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// What the repository is missing at `rev`, given the accepted learnings
/// and the printed pages.
fn plan_at(
    repo: &Repo,
    rev: &str,
    learnings: &[(String, String, String)],
    pages: &[Printed],
) -> Result<Plan> {
    let root = &repo.main_root;
    let store = Store::of(repo);
    let mut plan = Plan::default();

    let dir = portable::committed_dir(repo);
    let base = portable::ls_tree(root, rev, &dir, ".md")?;
    let blobs = portable::cat_blobs(
        root,
        &base.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>(),
    )?;
    let on_base: BTreeMap<String, String> = base
        .iter()
        .map(|(p, _)| p.clone())
        .zip(blobs.iter().map(|b| String::from_utf8_lossy(b).to_string()))
        .collect();
    let wanted: BTreeSet<&str> = learnings.iter().map(|(p, _, _)| p.as_str()).collect();
    for (path, text, id) in learnings {
        if on_base.get(path) != Some(text) {
            plan.learnings
                .push((path.clone(), text.clone(), id.clone()));
        }
    }
    for path in on_base.keys() {
        let name = path.rsplit('/').next().unwrap_or(path);
        let Some(n) = learning::id_from_file_name(name) else {
            continue;
        };
        let id = learning::format_id(n);
        if !wanted.contains(path.as_str()) && store.knows(&id) {
            plan.removed.push((path.clone(), id));
        }
    }

    let base_pages: BTreeMap<String, String> =
        portable::ls_tree(root, rev, &gazette_dir(repo), ".html")?
            .into_iter()
            .collect();
    // Read only the pages the base has, to compare what they were printed
    // from.
    let mut read: Vec<(&Printed, String)> = Vec::new();
    for p in pages {
        match base_pages.get(&p.path) {
            None => plan.pages.push((p.path.clone(), p.page.clone(), p.id)),
            Some(sha) => read.push((p, sha.clone())),
        }
    }
    let blobs = portable::cat_blobs(
        root,
        &read.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>(),
    )?;
    for ((p, _), blob) in read.into_iter().zip(blobs) {
        let head = String::from_utf8_lossy(&blob[..blob.len().min(2048)]).to_string();
        if events_of(&head).as_deref() != Some(p.events.as_str()) {
            plan.pages.push((p.path.clone(), p.page.clone(), p.id));
        }
    }
    Ok(plan)
}

/// The accepted learnings as the repository keeps them.
fn learnings(repo: &Repo) -> Vec<(String, String, String)> {
    let dir = portable::committed_dir(repo);
    Store::of(repo)
        .accepted()
        .into_iter()
        .map(|l| {
            (
                format!("{dir}/{}", l.learning.file_name()),
                committed_text(&l.learning),
                l.learning.id.clone(),
            )
        })
        .collect()
}

/// A records item still on its way: open, or shipped with its PR open.
fn in_flight(items: &[Item]) -> Option<&Item> {
    items
        .iter()
        .filter(|i| is_records(i))
        .find(|i| match i.state {
            State::Dropped => false,
            State::Closed => i.pr_url.is_some() && i.delivered.is_none(),
            _ => true,
        })
}

/// "H-0101, H-0102 and 3 more".
fn some(ids: &[String], n: usize) -> String {
    match ids.len() {
        0 => String::new(),
        k if k <= n => ids.join(", "),
        k => format!("{} and {} more", ids[..n].join(", "), k - n),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The records item's title: what it carries, in a few words.
pub fn title(plan: &Plan) -> String {
    let mut parts = Vec::new();
    let pages: Vec<String> = plan.pages.iter().map(|p| p.2.to_string()).collect();
    if !pages.is_empty() {
        parts.push(format!("Gazette for {}", some(&pages, 2)));
    }
    if !plan.learnings.is_empty() {
        parts.push(plural(plan.learnings.len(), "learning", "learnings"));
    }
    if !plan.removed.is_empty() {
        parts.push(format!("{} removed", plan.removed.len()));
    }
    crate::output::clip(&format!("{PREFIX} {}", parts.join("; ")), 72)
}

/// What the records item is, for the reviewer: its request.
fn request(repo: &Repo) -> String {
    format!(
        "Hallmark's records (ADR 0066). `[library] commit = true` keeps the Library's accepted \
         learnings in `{dir}` and each finished item's Gazette page in `{gazette}`. Hallmark \
         wrote these files from its local store and journal; this item carries them to {base} \
         like any other change.\n\n\
         Review them for what must not be published in this repository: secrets, credentials, \
         personal data, or the details of a security problem that isn't fixed yet. Don't ask \
         for changes to their wording or layout: the next records item writes them again from \
         the store and the journal.",
        dir = portable::committed_dir(repo),
        gazette = gazette_dir(repo),
        base = repo.config.base,
    )
}

/// The What of the PR description.
fn summary(repo: &Repo, plan: &Plan) -> String {
    let mut lines = Vec::new();
    if !plan.pages.is_empty() {
        let ids: Vec<String> = plan.pages.iter().map(|p| p.2.to_string()).collect();
        lines.push(format!(
            "Gazette pages in `{}` for {}.",
            gazette_dir(repo),
            ids.join(", ")
        ));
    }
    if !plan.learnings.is_empty() {
        let ids: Vec<&str> = plan.learnings.iter().map(|l| l.2.as_str()).collect();
        lines.push(format!(
            "Accepted learnings in `{}`: {}.",
            portable::committed_dir(repo),
            ids.join(", ")
        ));
    }
    if !plan.removed.is_empty() {
        let ids: Vec<&str> = plan.removed.iter().map(|l| l.1.as_str()).collect();
        lines.push(format!(
            "Removes learnings no longer accepted: {}.",
            ids.join(", ")
        ));
    }
    lines.join("\n")
}

/// Runs this hall as the librarian, in `dir`, and returns its JSON reply.
fn hall(dir: &Path, args: &[&str]) -> Result<serde_json::Value> {
    let exe = std::env::current_exe()?;
    let out = crate::process::command(&exe)
        .current_dir(dir)
        .env("HALL_ACTOR", super::librarian::ACTOR)
        .arg("--json")
        .args(args)
        .output()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    if !out.status.success() {
        let why = v["error"]["message"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| String::from_utf8_lossy(&out.stderr).trim().to_string());
        return Err(HallError::new(
            ErrorCode::CommandFailed,
            format!("hall {} failed: {why}", args.first().unwrap_or(&"")),
        ));
    }
    Ok(v)
}

/// Writes `plan` in `wt` and commits it.
fn commit(repo: &Repo, wt: &Path, plan: &Plan) -> Result<()> {
    let write = |rel: &str, text: &[u8]| -> Result<()> {
        let p = wt.join(rel);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p, text)?;
        Ok(())
    };
    for (path, text, _) in &plan.learnings {
        write(path, text.as_bytes())?;
    }
    for (path, page, _) in &plan.pages {
        write(path, &std::fs::read(page)?)?;
    }
    for (path, _) in &plan.removed {
        let p = wt.join(path);
        if p.exists() {
            std::fs::remove_file(&p)?;
        }
    }
    let dir = portable::committed_dir(repo);
    let gazette = gazette_dir(repo);
    crate::git::run(wt, &["add", "-A", "--", &dir, &gazette])?;
    let message = format!("docs: Hallmark records\n\n{}", summary(repo, plan));
    crate::git::run(wt, &["commit", "-q", "-m", &message])?;
    Ok(())
}

/// Starts a records item carrying `plan`, and sends it on. Returns its ID.
fn start(repo: &Repo, plan: &Plan, ask: bool) -> Result<String> {
    let title = title(plan);
    let request = request(repo);
    let made = hall(
        &repo.main_root,
        &["new", "--no-split", "--title", &title, &request],
    )?;
    let id = made["id"].as_str().unwrap_or_default().to_string();
    let wt = PathBuf::from(made["worktree"].as_str().unwrap_or_default());
    if id.is_empty() || !wt.is_dir() {
        return Err(HallError::new(
            ErrorCode::CommandFailed,
            "hall new returned no item and worktree".to_string(),
        ));
    }
    if let Err(e) = commit(repo, &wt, plan) {
        let reason = format!("the records could not be committed: {}", e.message);
        let _ = hall(&repo.main_root, &["drop", &id, "--reason", &reason]);
        return Err(e);
    }
    let summary = summary(repo, plan);
    let why = "`[library] commit = true` keeps the Library and the Gazette in the repository \
               (ADR 0066)."
        .to_string();
    let how = "Written by the librarian from the local store and the journal: accepted \
               learnings without their usage counts, and each finished item's Gazette page \
               as it was last printed."
        .to_string();
    let mut args = vec![
        "propose-ready",
        id.as_str(),
        "--summary",
        summary.as_str(),
        "--why",
        why.as_str(),
        "--how",
        how.as_str(),
    ];
    // Committing the Library and the Gazette is your call, until one records
    // item has shipped.
    if ask {
        args.push("--ask");
    }
    hall(&repo.main_root, &args)?;
    Ok(id)
}

/// The librarian's records step: when the repository is missing records
/// and no records item is on its way, start one. Returns what it did.
pub fn sync(repo: &Repo) -> Result<Option<String>> {
    if !repo.config.library.commit {
        return Ok(None);
    }
    let items = repo.index()?.items(true)?;
    if in_flight(&items).is_some() {
        return Ok(None);
    }
    let Some(target) = crate::delivery::resolve_ref(&repo.main_root, &repo.config.base) else {
        return Ok(None);
    };
    let tip = crate::git::rev(&repo.main_root, &target.git_ref).unwrap_or_default();
    let learnings = learnings(repo);
    let pages = pages(repo)?;
    let mut parts: Vec<&str> = vec![
        tip.as_str(),
        repo.config.library.gazette_dir.as_str(),
        repo.config.library.dir.as_str(),
    ];
    for (p, t, _) in &learnings {
        parts.push(p);
        parts.push(t);
    }
    for p in &pages {
        parts.push(&p.path);
        parts.push(&p.events);
    }
    let fingerprint = fnv(&parts);
    let mut st = load(repo);
    if st.checked.as_deref() == Some(fingerprint.as_str()) {
        return Ok(None);
    }
    let plan = plan_at(repo, &target.git_ref, &learnings, &pages)?;
    if plan.is_empty() {
        st.checked = Some(fingerprint);
        save(repo, &st)?;
        return Ok(None);
    }
    // Until a records item has shipped, each waits for you: one you dropped
    // is not a yes.
    let ask = !items
        .iter()
        .any(|i| is_records(i) && i.state == State::Closed);
    // Remembered whatever happens: a failure is tried again only once
    // something changes, not on every tick.
    st.checked = Some(fingerprint);
    let started = start(repo, &plan, ask);
    if let Ok(id) = &started {
        st.item = Some(id.clone());
    }
    save(repo, &st)?;
    let id = started?;
    let what = format!(
        "{id} carries the records to {}{}",
        repo.config.base,
        if ask {
            "; it waits for you until a records item has shipped"
        } else {
            ""
        }
    );
    repo.hall.live(&format!("library: {what}"));
    Ok(Some(what))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_say_what_the_item_carries() {
        let page = |n| {
            (
                format!("docs/gazette/H-{n:04}.html"),
                PathBuf::new(),
                ItemId(n),
            )
        };
        let plan = Plan {
            learnings: vec![("a".into(), "t".into(), "L-0001".into())],
            removed: vec![("b".into(), "L-0002".into())],
            pages: (1..=5).map(page).collect(),
        };
        assert_eq!(
            title(&plan),
            "Records: Gazette for H-0001, H-0002 and 3 more; 1 learning; 1 removed"
        );
        let plan = Plan {
            learnings: vec![],
            removed: vec![],
            pages: vec![page(7)],
        };
        assert_eq!(title(&plan), "Records: Gazette for H-0007");
    }

    #[test]
    fn the_events_a_page_was_printed_from() {
        let head = "<meta name=\"generator\" content=\"hall 1\">\n\
                    <meta name=\"hall-events\" content=\"42\">";
        assert_eq!(events_of(head).as_deref(), Some("42"));
        assert_eq!(events_of("<html>"), None);
    }

    #[test]
    fn the_fingerprint_is_stable_and_separates_parts() {
        assert_eq!(fnv(&["a", "b"]), fnv(&["a", "b"]));
        assert_ne!(fnv(&["ab", ""]), fnv(&["a", "b"]));
    }
}
