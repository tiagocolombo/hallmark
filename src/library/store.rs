//! The Library's store (ADR 0027): learnings live next to the journal, in
//! `<git-common-dir>/hallmark/library/`, never in the repository.
//!
//! - `learnings/L-0017-<slug>.md`: the current version of each learning,
//!   whatever its status. One file per ID.
//! - `history/L-0017/<stamp>-<why>.md`: every version a change replaced
//!   (`replaced`) and every proposal you rejected (`rejected`), so nothing is
//!   lost and a rejected update can restore what it replaced.
//! - `.lock`: serializes writers (the librarian, `hall library …`), which
//!   may run at the same time from the daemon's parallel workers.
//!
//! The store is shared by every worktree and outlives them. No branch can
//! write to it, so a change under review can never soften its own review.
//!
//! Usage counts come from verdict events across every journal.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use fs4::FileExt;
use schemars::JsonSchema;
use serde::Serialize;

use super::learning::{self, Learning, Status};
use crate::error::Result;
use crate::event::Body;
use crate::repo::Repo;

/// A learning and the file it was read from.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Located {
    #[serde(flatten)]
    pub learning: Learning,
    /// The file, absolute, with forward slashes on every OS.
    pub path: String,
    /// When the file last changed.
    #[serde(skip)]
    #[schemars(skip)]
    pub modified: Option<DateTime<Utc>>,
}

/// One earlier version of a learning, from `history/`.
#[derive(Debug, Clone, PartialEq)]
pub struct Version {
    pub learning: Learning,
    pub path: PathBuf,
    /// A proposal you rejected (rather than a version a change replaced).
    pub rejected: bool,
}

/// The Library's directory under one hallmark dir.
#[derive(Debug, Clone)]
pub struct Store {
    /// `<git-common-dir>/hallmark/library`.
    pub root: PathBuf,
}

const REPLACED: &str = "replaced";
const REJECTED: &str = "rejected";

impl Store {
    /// The store under a hallmark dir (`<git-common-dir>/hallmark`).
    pub fn at(hall_root: &Path) -> Store {
        Store {
            root: hall_root.join("library"),
        }
    }

    pub fn of(repo: &Repo) -> Store {
        Store::at(&repo.hall.root)
    }

    /// Where the current versions live.
    pub fn dir(&self) -> PathBuf {
        self.root.join("learnings")
    }

    pub fn history_dir(&self) -> PathBuf {
        self.root.join("history")
    }

    /// Runs `f` under the Library's lock, so two writers never interleave.
    /// It is not the repo lock.
    pub fn with_lock<R>(&self, f: impl FnOnce() -> Result<R>) -> Result<R> {
        std::fs::create_dir_all(&self.root)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.root.join(".lock"))?;
        FileExt::lock(&lock)?;
        let r = f();
        let _ = FileExt::unlock(&lock);
        r
    }

    /// The `.md` files in `learnings/`, by name.
    fn files(&self) -> Vec<PathBuf> {
        let Ok(rd) = std::fs::read_dir(self.dir()) else {
            return Vec::new();
        };
        let mut v: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "md"))
            .collect();
        v.sort();
        v
    }

    /// Every learning, in ID order. Files that don't parse are skipped.
    pub fn read(&self) -> Vec<Located> {
        let mut out: Vec<Located> = self.files().iter().filter_map(|p| locate(p)).collect();
        sort(&mut out);
        out
    }

    pub fn get(&self, id: &str) -> Option<Located> {
        let p = self.find(id)?;
        locate(&p)
    }

    /// The file that holds `id`: by file name, else by frontmatter.
    pub fn find(&self, id: &str) -> Option<PathBuf> {
        let n = learning::parse_id(id)?;
        let files = self.files();
        files
            .iter()
            .find(|p| name_id(p) == Some(n))
            .or_else(|| {
                files
                    .iter()
                    .find(|p| locate(p).is_some_and(|l| l.learning.id == learning::format_id(n)))
            })
            .cloned()
    }

    fn history_of(&self, id: &str) -> PathBuf {
        self.history_dir().join(id)
    }

    /// The earlier versions of `id`, oldest first.
    pub fn history(&self, id: &str) -> Vec<Version> {
        let Ok(rd) = std::fs::read_dir(self.history_of(id)) else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .collect();
        paths.sort();
        paths
            .into_iter()
            .filter_map(|p| {
                let text = std::fs::read_to_string(&p).ok()?;
                let learning = Learning::parse(&text, Some(id)).ok()?;
                let stem = p.file_stem()?.to_string_lossy().to_string();
                Some(Version {
                    learning,
                    rejected: stem.ends_with(REJECTED),
                    path: p,
                })
            })
            .collect()
    }

    /// The version a proposal replaced: the newest earlier version that was
    /// not itself a proposal. `None` for a new learning.
    pub fn previous(&self, id: &str) -> Option<Version> {
        self.history(id)
            .into_iter()
            .rev()
            .find(|v| !v.rejected && v.learning.status != Status::Proposed)
    }

    /// True when the last thing that happened to `id` is that you rejected
    /// a proposal of it (so rejecting it again changes nothing).
    pub fn was_rejected(&self, id: &str) -> bool {
        let current_proposed = self
            .get(id)
            .is_some_and(|l| l.learning.status == Status::Proposed);
        !current_proposed && self.history(id).last().is_some_and(|v| v.rejected)
    }

    /// Writes `l` as the current version: over the file that already holds
    /// its ID (which goes to `history/` first when it differs), else under
    /// its canonical name.
    pub fn write(&self, l: &Learning) -> Result<Located> {
        let dir = self.dir();
        std::fs::create_dir_all(&dir)?;
        let text = l.render();
        let path = match self.find(&l.id) {
            Some(p) => {
                let old = std::fs::read_to_string(&p).unwrap_or_default();
                if old == text {
                    return Ok(located(&p, l));
                }
                if !old.is_empty() {
                    self.archive(&l.id, &old, REPLACED)?;
                }
                p
            }
            None => dir.join(l.file_name()),
        };
        write_atomic(&path, &text)?;
        Ok(located(&path, l))
    }

    /// Adds a learning the store doesn't hold yet, as `text` under `name`
    /// when that is a learning file name for its ID, else under its
    /// canonical name. For imports, which keep files as they were.
    pub fn add(&self, l: &Learning, name: &str, text: &str) -> Result<Located> {
        let dir = self.dir();
        std::fs::create_dir_all(&dir)?;
        let plain = !name.contains(['/', '\\', ':']) && name.ends_with(".md");
        let name = if plain && learning::id_from_file_name(name) == learning::parse_id(&l.id) {
            name.to_string()
        } else {
            l.file_name()
        };
        let path = dir.join(name);
        write_atomic(&path, text)?;
        Ok(located(&path, l))
    }

    /// Rejects the current proposal of `id`: it goes to `history/` as
    /// rejected (with `reason`), and the version it replaced comes back, or
    /// the learning goes away when it was new. Returns the restored version.
    pub fn reject(
        &self,
        id: &str,
        reason: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Option<Located>> {
        let Some(path) = self.find(id) else {
            return Ok(None);
        };
        let previous = self.previous(id);
        let text = std::fs::read_to_string(&path)?;
        let record = match Learning::parse(&text, Some(id)) {
            Ok(mut l) => {
                let why = match reason.map(str::trim).filter(|r| !r.is_empty()) {
                    Some(r) => format!("{}: {r}", now.format("%Y-%m-%d")),
                    None => now.format("%Y-%m-%d").to_string(),
                };
                l.extra.retain(|(k, _)| k != "rejected");
                l.extra.push((
                    "rejected".into(),
                    serde_json::to_string(&why).unwrap_or_default(),
                ));
                l.render()
            }
            Err(_) => text,
        };
        self.archive(id, &record, REJECTED)?;
        match previous {
            Some(v) => {
                write_atomic(&path, &v.learning.render())?;
                Ok(Some(located(&path, &v.learning)))
            }
            None => {
                std::fs::remove_file(&path)?;
                Ok(None)
            }
        }
    }

    /// Copies `text` into `history/<id>/<stamp>-<why>.md`.
    fn archive(&self, id: &str, text: &str, why: &str) -> Result<PathBuf> {
        let dir = self.history_of(id);
        std::fs::create_dir_all(&dir)?;
        // No colons: the name must be valid on Windows.
        let stamp = Utc::now().format("%Y%m%dT%H%M%S%.6fZ").to_string();
        let mut p = dir.join(format!("{stamp}-{why}.md"));
        let mut n = 2;
        while p.exists() {
            p = dir.join(format!("{stamp}-{n}-{why}.md"));
            n += 1;
        }
        std::fs::write(&p, text)?;
        Ok(p)
    }

    /// The next free learning number: above every ID in `learnings/` and
    /// in `history/`, so a rejected ID is never reused.
    pub fn next_id(&self) -> u32 {
        let mut max = self
            .read()
            .iter()
            .filter_map(|l| learning::parse_id(&l.learning.id))
            .max()
            .unwrap_or(0);
        for p in self.files() {
            max = max.max(name_id(&p).unwrap_or(0));
        }
        if let Ok(rd) = std::fs::read_dir(self.history_dir()) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                max = max.max(learning::parse_id(&name).unwrap_or(0));
            }
        }
        max + 1
    }

    /// Is `id` known to the store at all (current, or in its history)?
    pub fn knows(&self, id: &str) -> bool {
        self.find(id).is_some() || self.history_of(id).is_dir()
    }

    /// What reviews are briefed on: every accepted learning. While an
    /// update of an accepted learning waits for you, the accepted version
    /// it would replace (from `history/`) stands in for it.
    pub fn accepted(&self) -> Vec<Located> {
        let mut out = Vec::new();
        for l in self.read() {
            match l.learning.status {
                Status::Accepted => out.push(l),
                Status::Proposed => {
                    if let Some(v) = self.previous(&l.learning.id)
                        && v.learning.status == Status::Accepted
                    {
                        out.push(Located {
                            modified: modified(&v.path),
                            path: crate::paths::display(&v.path),
                            learning: v.learning,
                        });
                    }
                }
                Status::Retired => {}
            }
        }
        out
    }

    /// How many learnings wait for your approval, and since when (the
    /// oldest proposal's file time). Reads only each file's frontmatter.
    pub fn proposed(&self) -> (usize, Option<DateTime<Utc>>) {
        let mut n = 0;
        let mut since: Option<DateTime<Utc>> = None;
        for p in self.files() {
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            if status_of(&text) == Some(Status::Proposed) {
                n += 1;
                if let Some(t) = modified(&p) {
                    since = Some(since.map_or(t, |s| s.min(t)));
                }
            }
        }
        (n, since)
    }
}

/// The status in a file's frontmatter, without parsing the rest. A file
/// without one is proposed (as [`Learning::parse`] reads it); text that is
/// not a learning is `None`.
pub fn status_of(text: &str) -> Option<Status> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    for l in lines {
        if l.trim_end() == "---" {
            return Some(Status::Proposed);
        }
        if let Some(v) = l.strip_prefix("status:") {
            let v = v.split('#').next().unwrap_or("").trim();
            let v = v.trim_matches(|c| c == '"' || c == '\'');
            return Status::parse(v);
        }
    }
    None
}

fn name_id(p: &Path) -> Option<u32> {
    p.file_name()
        .and_then(|f| f.to_str())
        .and_then(learning::id_from_file_name)
}

fn modified(p: &Path) -> Option<DateTime<Utc>> {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .map(DateTime::<Utc>::from)
}

fn located(p: &Path, l: &Learning) -> Located {
    Located {
        learning: l.clone(),
        path: crate::paths::display(p),
        modified: modified(p),
    }
}

fn locate(p: &Path) -> Option<Located> {
    let text = std::fs::read_to_string(p).ok()?;
    let fallback = name_id(p).map(learning::format_id);
    let learning = Learning::parse(&text, fallback.as_deref()).ok()?;
    Some(located(p, &learning))
}

/// Writes through a temporary file and a rename, so a reader never sees a
/// half-written learning.
fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn sort(v: &mut [Located]) {
    v.sort_by_key(|l| {
        (
            learning::parse_id(&l.learning.id).unwrap_or(0),
            l.path.clone(),
        )
    });
}

/// How reviews used one learning, from verdict events.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UseStats {
    /// Verdicts that reported using it.
    pub used: u32,
    /// Of those, the ones where it did not hold.
    pub contradicted: u32,
    pub last_used: Option<DateTime<Utc>>,
    pub last_contradicted: Option<DateTime<Utc>>,
    /// The notes reviewers gave when it did not hold.
    pub notes: Vec<String>,
}

/// Usage of every learning, scanned from every item's verdicts.
pub fn usage(repo: &Repo) -> Result<HashMap<String, UseStats>> {
    let mut out: HashMap<String, UseStats> = HashMap::new();
    for id in repo.hall.item_ids()? {
        for e in repo.hall.read_item(id)? {
            let Body::Verdict { learnings, .. } = &e.body else {
                continue;
            };
            for u in learnings {
                let Some(lid) = learning::normalize_id(&u.id) else {
                    continue;
                };
                let s = out.entry(lid).or_default();
                s.used += 1;
                s.last_used = s.last_used.max(Some(e.t));
                if !u.held {
                    s.contradicted += 1;
                    s.last_contradicted = s.last_contradicted.max(Some(e.t));
                    if let Some(n) = &u.note {
                        s.notes.push(format!("{id}: {n}"));
                    }
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::learning::Kind;

    fn l(id: &str, status: Status, title: &str) -> Learning {
        Learning {
            id: id.into(),
            kind: Kind::Rule,
            scope: vec![],
            lens: None,
            source: vec![],
            status,
            used: 0,
            last_used: None,
            title: title.into(),
            body: String::new(),
            extra: vec![],
        }
    }

    #[test]
    fn statuses_are_read_from_the_frontmatter_only() {
        assert_eq!(
            status_of("---\nid: L-1\nstatus: accepted # ok\n---\n# t\n"),
            Some(Status::Accepted)
        );
        assert_eq!(
            status_of("---\nid: L-1\n---\nstatus: accepted\n"),
            Some(Status::Proposed)
        );
        assert_eq!(status_of("# no frontmatter\n"), None);
        assert_eq!(
            status_of("---\nstatus: \"retired\"\n---\n"),
            Some(Status::Retired)
        );
    }

    #[test]
    fn updates_keep_history_and_rejections_restore_it() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::at(dir.path());
        assert_eq!(s.next_id(), 1);
        s.write(&l("L-0001", Status::Accepted, "First")).unwrap();
        assert_eq!(s.accepted().len(), 1);

        // An update waits for you; the accepted version still briefs.
        s.write(&l("L-0001", Status::Proposed, "First, sharper"))
            .unwrap();
        assert_eq!(s.proposed().0, 1);
        assert_eq!(s.accepted()[0].learning.title, "First");
        assert_eq!(s.previous("L-0001").unwrap().learning.title, "First");

        // Rejecting it restores the accepted version.
        let back = s.reject("L-0001", Some("too vague"), Utc::now()).unwrap();
        assert_eq!(back.unwrap().learning.title, "First");
        assert_eq!(s.get("L-0001").unwrap().learning.title, "First");
        assert!(s.was_rejected("L-0001"));
        let h = s.history("L-0001");
        assert_eq!(h.len(), 2);
        assert!(h[1].rejected);
        assert!(h[1].learning.extra.iter().any(|(k, _)| k == "rejected"));

        // A new proposal, rejected, goes away; its ID is never reused.
        s.write(&l("L-0002", Status::Proposed, "Second")).unwrap();
        assert_eq!(s.reject("L-0002", None, Utc::now()).unwrap(), None);
        assert!(s.get("L-0002").is_none());
        assert_eq!(s.next_id(), 3);
        assert!(s.knows("L-0002"));
        assert_eq!(s.proposed().0, 0);
    }
}
