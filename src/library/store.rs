//! Where learnings are read from.
//!
//! - **The base branch** (`git ls-tree` + `git cat-file --batch`): the only
//!   source for reviews and the default for search, so a branch can never
//!   soften its own review.
//! - **Open knowledge items' worktrees**: proposals that have not reached the
//!   base yet (`hall library review`, `--proposed` search, the librarian's
//!   view of what already exists).
//!
//! Usage counts come from verdict events across every journal.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;

use super::learning::{self, Learning};
use crate::error::{ErrorCode, HallError, Result};
use crate::event::Body;
use crate::item::{Item, ItemId, State};
use crate::repo::Repo;

/// Knowledge items are ordinary items whose title starts with this.
pub const KNOWLEDGE_PREFIX: &str = "Library:";

/// A learning and where it was read.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Located {
    #[serde(flatten)]
    pub learning: Learning,
    /// Repo-relative, with forward slashes.
    pub path: String,
    /// The open knowledge item that carries this version; absent when it was
    /// read from the base branch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
}

/// The learnings directory, relative to the repo root, with forward slashes
/// and no trailing slash.
pub fn dir(repo: &Repo) -> String {
    let d = repo.config.library.dir.replace('\\', "/");
    let d = d.trim_matches('/');
    if d.is_empty() {
        ".hallmark/knowledge".into()
    } else {
        d.to_string()
    }
}

fn git(dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir);
    for v in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
        c.env_remove(v);
    }
    c
}

/// `(path, blob)` for every `.md` file under the learnings dir at `rev`.
fn ls_tree(root: &Path, rev: &str, dir: &str) -> Result<Vec<(String, String)>> {
    let out = git(root)
        .args(["ls-tree", "-r", "-z", "--full-tree", rev, "--", dir])
        .output()
        .map_err(|e| HallError::new(ErrorCode::CommandFailed, format!("could not run git: {e}")))?;
    if !out.status.success() {
        return Err(HallError::new(
            ErrorCode::CommandFailed,
            format!(
                "git ls-tree {rev} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        )
        .next(["check `base` in .hallmark.toml", "git branch --list"]));
    }
    let mut files = Vec::new();
    for entry in out.stdout.split(|b| *b == 0) {
        let entry = String::from_utf8_lossy(entry);
        let Some((meta, path)) = entry.split_once('\t') else {
            continue;
        };
        let mut parts = meta.split_whitespace();
        let (_mode, kind, sha) = (parts.next(), parts.next(), parts.next());
        if kind == Some("blob")
            && path.ends_with(".md")
            && let Some(sha) = sha
        {
            files.push((path.to_string(), sha.to_string()));
        }
    }
    Ok(files)
}

/// Reads blobs in one `git cat-file --batch`.
fn cat_blobs(root: &Path, shas: &[String]) -> Result<Vec<Vec<u8>>> {
    if shas.is_empty() {
        return Ok(Vec::new());
    }
    let mut child = git(root)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| HallError::new(ErrorCode::CommandFailed, format!("could not run git: {e}")))?;
    let input: String = shas.iter().map(|s| format!("{s}\n")).collect();
    let mut stdin = child.stdin.take().expect("piped stdin");
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let mut raw = Vec::new();
    child
        .stdout
        .take()
        .expect("piped stdout")
        .read_to_end(&mut raw)?;
    let _ = writer.join();
    let _ = child.wait();
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < raw.len() {
        let Some(nl) = raw[pos..].iter().position(|b| *b == b'\n') else {
            break;
        };
        let header = String::from_utf8_lossy(&raw[pos..pos + nl]).to_string();
        pos += nl + 1;
        let mut h = header.split_whitespace();
        let (_sha, kind, size) = (h.next(), h.next(), h.next());
        if kind == Some("missing") {
            out.push(Vec::new());
            continue;
        }
        let size: usize = size.and_then(|s| s.parse().ok()).unwrap_or(0);
        let end = (pos + size).min(raw.len());
        out.push(raw[pos..end].to_vec());
        pos = end + 1;
    }
    Ok(out)
}

fn file_name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn parse_located(path: String, text: &str, item: Option<ItemId>) -> Option<Located> {
    let fallback = learning::id_from_file_name(file_name_of(&path)).map(learning::format_id);
    let learning = Learning::parse(text, fallback.as_deref()).ok()?;
    Some(Located {
        learning,
        path,
        item,
    })
}

/// Every learning on `rev` (a branch or commit), in ID order.
pub fn read_rev(repo: &Repo, rev: &str) -> Result<Vec<Located>> {
    let files = ls_tree(&repo.main_root, rev, &dir(repo))?;
    let shas: Vec<String> = files.iter().map(|(_, s)| s.clone()).collect();
    let blobs = cat_blobs(&repo.main_root, &shas)?;
    let mut out: Vec<Located> = files
        .into_iter()
        .zip(blobs)
        .filter_map(|((path, _), blob)| parse_located(path, &String::from_utf8_lossy(&blob), None))
        .collect();
    sort(&mut out);
    Ok(out)
}

/// The learnings on the base branch. An unknown base is `COMMAND_FAILED`.
pub fn read_base(repo: &Repo, base: &str) -> Result<Vec<Located>> {
    read_rev(repo, base)
}

/// Learnings as they are on disk in a worktree (committed or not).
pub fn read_worktree(repo: &Repo, worktree: &Path, item: Option<ItemId>) -> Vec<Located> {
    let d = dir(repo);
    let Ok(rd) = std::fs::read_dir(worktree.join(&d)) else {
        return Vec::new();
    };
    let mut out: Vec<Located> = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let text = std::fs::read_to_string(e.path()).ok()?;
            parse_located(format!("{d}/{name}"), &text, item)
        })
        .collect();
    sort(&mut out);
    out
}

fn sort(v: &mut [Located]) {
    v.sort_by_key(|l| {
        (
            learning::parse_id(&l.learning.id).unwrap_or(0),
            l.path.clone(),
        )
    });
}

/// Every knowledge item, open or not, in ID order.
pub fn knowledge_items(repo: &Repo) -> Result<Vec<Item>> {
    Ok(repo
        .index()?
        .items(true)?
        .into_iter()
        .filter(|i| i.title.starts_with(KNOWLEDGE_PREFIX))
        .collect())
}

/// Knowledge items whose worktree still matters: not closed or dropped.
pub fn open_knowledge(repo: &Repo) -> Result<Vec<Item>> {
    Ok(knowledge_items(repo)?
        .into_iter()
        .filter(|i| !i.state.is_terminal())
        .collect())
}

/// A knowledge item Hallmark may still commit to: in development or
/// awaiting you, so not frozen in the review loop.
pub fn editable(item: &Item) -> bool {
    matches!(item.state, State::Development | State::AwaitingYou)
}

/// The base branch plus what open knowledge items change.
#[derive(Debug, Clone, Default)]
pub struct Library {
    /// On the base branch.
    pub base: Vec<Located>,
    /// Versions in open knowledge items that differ from the base (new
    /// learnings, updates, retirements), oldest item first.
    pub pending: Vec<Located>,
}

impl Library {
    pub fn load(repo: &Repo, base: &str) -> Result<Library> {
        let base_ls = read_base(repo, base)?;
        let by_id: HashMap<&str, &Located> = base_ls
            .iter()
            .map(|l| (l.learning.id.as_str(), l))
            .collect();
        let mut pending = Vec::new();
        for item in open_knowledge(repo)? {
            for l in read_worktree(repo, Path::new(&item.worktree), Some(item.id)) {
                let same = by_id
                    .get(l.learning.id.as_str())
                    .is_some_and(|b| b.learning == l.learning);
                if !same {
                    pending.push(l);
                }
            }
        }
        Ok(Library {
            base: base_ls,
            pending,
        })
    }

    pub fn base_get(&self, id: &str) -> Option<&Located> {
        self.base.iter().find(|l| l.learning.id == id)
    }

    /// The newest pending version of `id`.
    pub fn pending_get(&self, id: &str) -> Option<&Located> {
        self.pending.iter().rev().find(|l| l.learning.id == id)
    }

    /// One version per ID: pending versions win over the base.
    pub fn effective(&self) -> Vec<&Located> {
        let mut m: BTreeMap<u32, &Located> = BTreeMap::new();
        for l in self.base.iter().chain(self.pending.iter()) {
            m.insert(learning::parse_id(&l.learning.id).unwrap_or(0), l);
        }
        m.into_values().collect()
    }

    pub fn get(&self, id: &str) -> Option<&Located> {
        self.pending_get(id).or_else(|| self.base_get(id))
    }
}

/// The next free learning number: above every ID on the base, in open
/// knowledge items' worktrees, and on every knowledge branch that was not
/// dropped (a shipped one may not be merged yet).
pub fn next_id(repo: &Repo, lib: &Library) -> Result<u32> {
    let mut max = lib
        .base
        .iter()
        .chain(lib.pending.iter())
        .filter_map(|l| learning::parse_id(&l.learning.id))
        .max()
        .unwrap_or(0);
    let d = dir(repo);
    for item in knowledge_items(repo)? {
        if item.state == State::Dropped {
            continue;
        }
        if !item.state.is_terminal()
            && let Ok(rd) = std::fs::read_dir(Path::new(&item.worktree).join(&d))
        {
            for e in rd.flatten() {
                if let Some(n) = learning::id_from_file_name(&e.file_name().to_string_lossy()) {
                    max = max.max(n);
                }
            }
        }
        if crate::git::branch_exists(&repo.main_root, &item.branch)
            && let Ok(files) = ls_tree(&repo.main_root, &item.branch, &d)
        {
            for (p, _) in files {
                if let Some(n) = learning::id_from_file_name(file_name_of(&p)) {
                    max = max.max(n);
                }
            }
        }
    }
    Ok(max + 1)
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

/// When each learning file was created and last changed on `rev`, from one
/// `git log`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FileDates {
    pub created: DateTime<Utc>,
    pub modified: DateTime<Utc>,
}

pub fn file_dates(repo: &Repo, rev: &str) -> HashMap<String, FileDates> {
    let mut out: HashMap<String, FileDates> = HashMap::new();
    let Ok(text) = crate::git::run(
        &repo.main_root,
        &[
            "log",
            "--format=%x01%cI",
            "--name-only",
            rev,
            "--",
            &dir(repo),
        ],
    ) else {
        return out;
    };
    // Newest commit first: the first sighting is the last change, the last
    // sighting the creation.
    for block in text.split('\u{1}').filter(|b| !b.trim().is_empty()) {
        let mut lines = block.lines();
        let Some(t) = lines
            .next()
            .and_then(|d| DateTime::parse_from_rfc3339(d.trim()).ok())
            .map(|d| d.with_timezone(&Utc))
        else {
            continue;
        };
        for f in lines.map(str::trim).filter(|l| !l.is_empty()) {
            out.entry(f.to_string())
                .and_modify(|d| d.created = t)
                .or_insert(FileDates {
                    created: t,
                    modified: t,
                });
        }
    }
    out
}

/// The worktree file that holds `id`, if any.
pub fn find_in_worktree(repo: &Repo, worktree: &Path, id: &str) -> Option<PathBuf> {
    let n = learning::parse_id(id)?;
    let rd = std::fs::read_dir(worktree.join(dir(repo))).ok()?;
    rd.flatten().map(|e| e.path()).find(|p| {
        p.file_name()
            .and_then(|f| f.to_str())
            .and_then(learning::id_from_file_name)
            == Some(n)
    })
}
