//! Moving learnings in and out of the local store (ADR 0027).
//!
//! - **Import** reads learnings committed under `[library] dir` on the base
//!   branch (where ADR 0021 kept them), a directory of learning files, or an
//!   export file, and adds the ones the store doesn't know yet, keeping
//!   their IDs.
//! - **Export** writes every current learning, whatever its status, to one
//!   JSON file that import reads back.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::learning::{self, Learning};
use super::store::Store;
use crate::error::{ErrorCode, HallError, Result};
use crate::repo::Repo;

/// What an export file names itself.
pub const FORMAT: &str = "hallmark-library";

/// One learning file in an export.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Entry {
    /// Its file name in the store (`L-0017-<slug>.md`).
    pub path: String,
    /// The file, as it is.
    pub content: String,
}

/// An export file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExportFile {
    /// Always `hallmark-library`.
    pub format: String,
    pub version: u32,
    pub learnings: Vec<Entry>,
}

/// Every `.md` file in the store, raw, by name.
pub fn export(store: &Store) -> Result<Vec<Entry>> {
    let Ok(rd) = std::fs::read_dir(store.dir()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_file() || p.extension().is_none_or(|x| x != "md") {
            continue;
        }
        out.push(Entry {
            path: e.file_name().to_string_lossy().to_string(),
            content: std::fs::read_to_string(&p)?,
        });
    }
    out.sort_by_key(|e| {
        (
            learning::id_from_file_name(&e.path).unwrap_or(u32::MAX),
            e.path.clone(),
        )
    });
    Ok(out)
}

/// Reads an export: an object with `learnings` (an export file, or the
/// JSON reply of `hall library export`), or a bare array of entries.
pub fn parse_export(text: &str) -> std::result::Result<Vec<Entry>, String> {
    let v: serde_json::Value =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
    let arr = match v {
        serde_json::Value::Array(_) => v,
        serde_json::Value::Object(mut o) => {
            o.remove("learnings").ok_or("it has no `learnings` list")?
        }
        _ => return Err("it is not a JSON object or list".into()),
    };
    serde_json::from_value(arr)
        .map_err(|e| format!("its `learnings` are not {{path, content}}: {e}"))
}

/// A file import left out, and why.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Skipped {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub reason: String,
}

/// A learning whose ID the store already used for another learning.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Renumbered {
    pub path: String,
    pub from: String,
    pub to: String,
}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, JsonSchema)]
pub struct Imported {
    /// Learnings that came in under a new ID, because theirs was taken.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub renumbered: Vec<Renumbered>,
    /// Learnings added to the store, by ID.
    pub imported: Vec<String>,
    /// Files left out: already in the store, or not learnings.
    pub skipped: Vec<Skipped>,
}

/// Adds `entries` the store doesn't know yet, keeping their IDs. Call under
/// [`Store::with_lock`].
pub fn import(store: &Store, entries: &[Entry]) -> Result<Imported> {
    let mut out = Imported::default();
    // New numbers start above every incoming ID, so a renumbered learning
    // never collides with one still to come.
    let incoming_max = entries
        .iter()
        .filter_map(|e| {
            let name = e.path.rsplit(['/', '\\']).next().unwrap_or(&e.path);
            let fallback = learning::id_from_file_name(name).map(learning::format_id);
            Learning::parse(&e.content, fallback.as_deref())
                .ok()
                .and_then(|l| learning::parse_id(&l.id))
        })
        .max()
        .unwrap_or(0);
    let mut fresh = store.next_id().max(incoming_max + 1);
    for e in entries {
        let name = e.path.rsplit(['/', '\\']).next().unwrap_or(&e.path);
        let fallback = learning::id_from_file_name(name).map(learning::format_id);
        let l = match Learning::parse(&e.content, fallback.as_deref()) {
            Ok(l) => l,
            Err(err) => {
                out.skipped.push(Skipped {
                    path: e.path.clone(),
                    id: fallback,
                    reason: format!("not a learning: {err}"),
                });
                continue;
            }
        };
        if store.knows(&l.id) {
            let same = store
                .get(&l.id)
                .is_some_and(|c| c.learning.title == l.title)
                || store
                    .history(&l.id)
                    .iter()
                    .any(|v| v.learning.title == l.title);
            if same {
                out.skipped.push(Skipped {
                    path: e.path.clone(),
                    id: Some(l.id.clone()),
                    reason: "already in the Library".into(),
                });
                continue;
            }
            // A different learning that reuses the ID: number it afresh.
            let mut l = l;
            let old = std::mem::replace(&mut l.id, learning::format_id(fresh));
            fresh += 1;
            store.add(&l, "", &l.render())?;
            out.renumbered.push(Renumbered {
                path: e.path.clone(),
                from: old,
                to: l.id.clone(),
            });
            out.imported.push(l.id);
            continue;
        }
        store.add(&l, name, &e.content)?;
        out.imported.push(l.id);
    }
    Ok(out)
}

/// The learnings directory as configured, relative to the repo root, with
/// forward slashes and no trailing slash.
pub fn committed_dir(repo: &Repo) -> String {
    let d = repo.config.library.dir.replace('\\', "/");
    let d = d.trim_matches('/');
    if d.is_empty() {
        ".hallmark/knowledge".into()
    } else {
        d.to_string()
    }
}

/// Every `.md` file under the committed learnings dir at `rev`. An unknown
/// `rev` is `COMMAND_FAILED`.
pub fn read_rev(repo: &Repo, rev: &str) -> Result<Vec<Entry>> {
    let files = ls_tree(&repo.main_root, rev, &committed_dir(repo))?;
    let shas: Vec<String> = files.iter().map(|(_, s)| s.clone()).collect();
    let blobs = cat_blobs(&repo.main_root, &shas)?;
    Ok(files
        .into_iter()
        .zip(blobs)
        .map(|((path, _), blob)| Entry {
            path,
            content: String::from_utf8_lossy(&blob).to_string(),
        })
        .collect())
}

/// The `.md` files directly in `dir`, by name.
pub fn read_dir(dir: &Path) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir)?.flatten() {
        let p = e.path();
        if p.is_file() && p.extension().is_some_and(|x| x == "md") {
            out.push(Entry {
                path: crate::paths::display(&p),
                content: std::fs::read_to_string(&p)?,
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn git(dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir);
    for v in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
        c.env_remove(v);
    }
    c
}

/// `(path, blob)` for every `.md` file under `dir` at `rev`.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_parse_in_every_shape() {
        let e = vec![Entry {
            path: "L-0001-x.md".into(),
            content: "---\nid: L-0001\n---\n".into(),
        }];
        let file = ExportFile {
            format: FORMAT.into(),
            version: 1,
            learnings: e.clone(),
        };
        let text = serde_json::to_string(&file).unwrap();
        assert_eq!(parse_export(&text).unwrap(), e);
        let bare = serde_json::to_string(&e).unwrap();
        assert_eq!(parse_export(&bare).unwrap(), e);
        assert!(parse_export("{}").is_err());
        assert!(parse_export("nope").is_err());
        assert!(parse_export("{\"learnings\": [1]}").is_err());
    }
}
