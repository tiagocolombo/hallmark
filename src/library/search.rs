//! Searching the Library.
//!
//! - **Built in**: SQLite FTS5 in `<hallmark dir>/library.sqlite`, a cache
//!   rebuilt whenever the set of learnings it was built from changes (a key
//!   over their paths and contents). `hall reindex` doesn't need to know
//!   about it: deleting the file is always safe.
//! - **qmd** (`[library] search = "qmd"`): one collection per repo
//!   (`hallmark-<repo>` by default), queried one collection at a time. The
//!   learnings being searched are exported to
//!   `<hallmark dir>/library/qmd/<collection>/` and the collection is kept in
//!   sync with `qmd collection add` and `qmd update`. When qmd is missing or
//!   fails, search falls back to the built-in index with a note.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::{Connection, params};
use schemars::JsonSchema;
use serde::Serialize;

use super::learning::{Kind, Status};
use super::store::Located;
use crate::config::SearchBackend;
use crate::error::Result;
use crate::repo::Repo;

/// One search result.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Hit {
    pub id: String,
    pub kind: Kind,
    pub scope: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lens: Option<String>,
    pub status: Status,
    /// Relevance, higher is better: 0–1 for the built-in index, qmd's own
    /// score for qmd.
    pub score: f64,
    pub used: u32,
    /// The learning's file in the local store.
    pub path: String,
    pub title: String,
}

impl Hit {
    fn from(l: &Located, score: f64) -> Hit {
        Hit {
            id: l.learning.id.clone(),
            kind: l.learning.kind,
            scope: l.learning.scope.clone(),
            lens: l.learning.lens.clone(),
            status: l.learning.status,
            score,
            used: l.learning.used,
            path: l.path.clone(),
            title: l.learning.title.clone(),
        }
    }
}

/// What a search found, and how.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub hits: Vec<Hit>,
    /// builtin or qmd: the backend that answered.
    pub backend: SearchBackend,
    /// Why the configured backend was not used, when it wasn't.
    pub note: Option<String>,
}

/// Words that say nothing about a learning.
const STOP: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "into", "not", "are", "was", "but", "you",
    "its", "has", "have", "use", "let", "mut", "pub", "self", "fn", "impl", "return", "true",
    "false", "none", "some", "ok", "err", "if", "else", "match", "struct", "enum", "const",
    "static", "use", "mod", "crate", "super", "where", "type", "def", "var", "function", "import",
    "export", "class", "new", "null", "nil", "string", "str", "vec", "box", "option", "result",
    "unwrap", "clone", "into", "iter", "map", "end", "then", "when", "what", "how",
];

/// The searchable words of `text`: lowercase, deduplicated, in order.
pub fn terms(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for w in text.split(|c: char| !c.is_alphanumeric()) {
        let w = w.to_lowercase();
        if w.is_empty() || !seen.insert(w.clone()) {
            continue;
        }
        out.push(w);
    }
    out
}

/// An FTS5 MATCH expression from free text: each word quoted (so no FTS5
/// syntax gets through) as a prefix, any of them. `None` when there is no
/// word to search for.
pub fn fts_query(text: &str) -> Option<String> {
    let words: Vec<String> = terms(text)
        .into_iter()
        .take(64)
        .map(|w| format!("\"{w}\"*"))
        .collect();
    (!words.is_empty()).then(|| words.join(" OR "))
}

/// Terms worth searching for from a diff and its changed paths: identifiers
/// and words on changed lines, and path segments, most frequent first.
pub fn diff_terms(changed: &[String], diff: &str, max: usize) -> Vec<String> {
    let mut count: HashMap<String, u32> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut bump = |w: &str, n: u32| {
        let w = w.to_lowercase();
        if w.chars().count() < 3
            || w.chars().all(|c| c.is_ascii_digit())
            || STOP.contains(&w.as_str())
        {
            return;
        }
        if !count.contains_key(&w) {
            order.push(w.clone());
        }
        *count.entry(w).or_default() += n;
    };
    for p in changed {
        for seg in p.split(|c: char| !c.is_alphanumeric()) {
            // Path words weigh more than any one line.
            bump(seg, 3);
        }
    }
    for line in diff.lines() {
        let added_or_removed = (line.starts_with('+') && !line.starts_with("+++"))
            || (line.starts_with('-') && !line.starts_with("---"));
        if !added_or_removed {
            continue;
        }
        for w in line[1..].split(|c: char| !c.is_alphanumeric()) {
            bump(w, 1);
        }
    }
    let mut ranked: Vec<(usize, String)> = order.into_iter().enumerate().collect();
    ranked.sort_by(|(ia, a), (ib, b)| count[b].cmp(&count[a]).then(ia.cmp(ib)));
    ranked.into_iter().map(|(_, w)| w).take(max).collect()
}

/// Options for [`search`].
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub limit: usize,
    pub kind: Option<Kind>,
    /// Statuses to return; empty means accepted only.
    pub statuses: Vec<Status>,
}

impl Options {
    fn allows(&self, l: &Located) -> bool {
        let statuses = if self.statuses.is_empty() {
            &[Status::Accepted][..]
        } else {
            &self.statuses[..]
        };
        statuses.contains(&l.learning.status) && self.kind.is_none_or(|k| k == l.learning.kind)
    }
}

/// Searches `docs` with the repo's configured backend.
pub fn search(repo: &Repo, docs: &[Located], query: &str, opts: &Options) -> Result<Found> {
    if repo.config.library.search == SearchBackend::Qmd {
        match qmd::search(repo, docs, query, opts) {
            Ok(hits) => {
                return Ok(Found {
                    hits,
                    backend: SearchBackend::Qmd,
                    note: None,
                });
            }
            Err(why) => {
                repo.hall.live(&format!(
                    "library: qmd unavailable, built-in search used: {why}"
                ));
                let hits = builtin(repo, docs, query, opts)?;
                return Ok(Found {
                    hits,
                    backend: SearchBackend::Builtin,
                    note: Some(format!("qmd unavailable ({why}); used the built-in search")),
                });
            }
        }
    }
    Ok(Found {
        hits: builtin(repo, docs, query, opts)?,
        backend: SearchBackend::Builtin,
        note: None,
    })
}

// --- built in -------------------------------------------------------------------

pub fn db_path(repo: &Repo) -> PathBuf {
    repo.hall.root.join("library.sqlite")
}

/// A key over the documents an index was built from.
fn key(docs: &[Located]) -> String {
    let mut h = DefaultHasher::new();
    for d in docs {
        d.path.hash(&mut h);
        d.learning.render().hash(&mut h);
    }
    format!("{:016x}-{}", h.finish(), docs.len())
}

fn open(path: &Path) -> Result<Connection> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let c = Connection::open(path)?;
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
         CREATE VIRTUAL TABLE IF NOT EXISTS learnings USING fts5(
             id, title, body, kind, lens, scope,
             seq UNINDEXED,
             tokenize = 'porter unicode61'
         );",
    )?;
    Ok(c)
}

/// Rebuilds the table when `docs` differ from what it holds.
fn sync(c: &mut Connection, docs: &[Located]) -> Result<()> {
    let want = key(docs);
    let have: Option<String> = c
        .query_row("SELECT v FROM meta WHERE k = 'key'", [], |r| r.get(0))
        .ok();
    if have.as_deref() == Some(want.as_str()) {
        return Ok(());
    }
    let tx = c.transaction()?;
    tx.execute("DELETE FROM learnings", [])?;
    {
        let mut ins = tx.prepare(
            "INSERT INTO learnings (id, title, body, kind, lens, scope, seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for (i, d) in docs.iter().enumerate() {
            let l = &d.learning;
            ins.execute(params![
                l.id,
                l.title,
                l.body,
                l.kind.as_str(),
                l.lens.clone().unwrap_or_default(),
                l.scope_text(),
                i as i64
            ])?;
        }
    }
    tx.execute(
        "INSERT INTO meta (k, v) VALUES ('key', ?1)
         ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        params![want],
    )?;
    tx.commit()?;
    Ok(())
}

/// The built-in FTS5 search. Ranked by bm25 with the title and ID weighed
/// above the body.
pub fn builtin(repo: &Repo, docs: &[Located], query: &str, opts: &Options) -> Result<Vec<Hit>> {
    let Some(q) = fts_query(query) else {
        return Ok(Vec::new());
    };
    let mut c = open(&db_path(repo))?;
    sync(&mut c, docs)?;
    let mut stmt = c.prepare(
        "SELECT seq, bm25(learnings, 6.0, 4.0, 1.0, 2.0, 1.5, 2.0) AS rank
         FROM learnings WHERE learnings MATCH ?1 ORDER BY rank",
    )?;
    let rows = stmt.query_map(params![q], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
    })?;
    let mut hits = Vec::new();
    for r in rows {
        let (seq, rank) = r?;
        let Some(d) = docs.get(seq as usize) else {
            continue;
        };
        if !opts.allows(d) {
            continue;
        }
        // bm25 is negative; better matches are more negative.
        let x = (-rank).max(0.0);
        let score = ((x / (1.0 + x)) * 1000.0).round() / 1000.0;
        hits.push(Hit::from(d, score));
        if opts.limit > 0 && hits.len() >= opts.limit {
            break;
        }
    }
    Ok(hits)
}

// --- qmd --------------------------------------------------------------------------

pub mod qmd {
    //! tobi/qmd as a search backend. Hallmark only uses its JSON command
    //! line: `qmd collection add <dir> --name <c> --mask "**/*.md"`,
    //! `qmd update`, and `qmd search --json -n <n> -c <c> <query>`.

    use super::*;

    /// The qmd binary: `HALL_QMD`, `[tools] qmd`, else `qmd` on PATH.
    pub fn bin() -> String {
        crate::config::Tool::Qmd.command()
    }

    /// `[library] collection`, else `hallmark-<repo>`.
    pub fn collection(repo: &Repo) -> String {
        repo.config
            .library
            .collection
            .clone()
            .filter(|c| !c.trim().is_empty())
            .unwrap_or_else(|| format!("hallmark-{}", repo.name))
    }

    /// Where the searched learnings are exported for qmd to index.
    pub fn export_dir(repo: &Repo) -> PathBuf {
        repo.hall
            .root
            .join("library")
            .join("qmd")
            .join(collection(repo))
    }

    /// A qmd result, as far as Hallmark reads it.
    #[derive(Debug, Clone, PartialEq)]
    pub struct QmdHit {
        /// The file qmd matched (`qmd://<collection>/L-0017-x.md`, or a
        /// relative or absolute path).
        pub file: String,
        pub score: f64,
    }

    /// Parses `qmd search --json` output: an array of results (or an object
    /// with `results`), each with `file` (or `path`/`filepath`) and `score`.
    pub fn parse(json: &str) -> std::result::Result<Vec<QmdHit>, String> {
        let v: serde_json::Value =
            serde_json::from_str(json.trim()).map_err(|e| format!("unreadable qmd output: {e}"))?;
        let arr = match &v {
            serde_json::Value::Array(a) => a.clone(),
            serde_json::Value::Object(o) => o
                .get("results")
                .and_then(|r| r.as_array())
                .cloned()
                .ok_or("qmd output has no results")?,
            _ => return Err("qmd output is not a list".into()),
        };
        Ok(arr
            .iter()
            .filter_map(|r| {
                let file = ["file", "path", "filepath", "displayPath"]
                    .iter()
                    .find_map(|k| r.get(*k).and_then(|f| f.as_str()))?
                    .to_string();
                let score = r.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0);
                Some(QmdHit { file, score })
            })
            .collect())
    }

    /// The export file name for a document: its file name in the store.
    fn export_rel(d: &Located) -> String {
        d.path.rsplit('/').next().unwrap_or(&d.path).to_string()
    }

    /// Matches a qmd file back to the exported document, by file name.
    fn resolve<'a>(file: &str, by_rel: &HashMap<String, &'a Located>) -> Option<&'a Located> {
        let f = file.replace('\\', "/");
        let name = f.rsplit('/').next()?.to_ascii_lowercase();
        by_rel
            .iter()
            .find(|(rel, _)| rel.to_ascii_lowercase() == name)
            .map(|(_, d)| *d)
    }

    fn run(args: &[&str]) -> std::result::Result<String, String> {
        let out = Command::new(bin())
            .args(args)
            .output()
            .map_err(|e| format!("could not run {}: {e}", bin()))?;
        if !out.status.success() {
            return Err(format!(
                "qmd {} failed: {}",
                args.first().copied().unwrap_or(""),
                crate::output::clip(&String::from_utf8_lossy(&out.stderr), 200)
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    /// Writes `docs` to the export directory and brings the collection up
    /// to date when they changed.
    pub fn sync(repo: &Repo, docs: &[Located]) -> std::result::Result<(), String> {
        let dir = export_dir(repo);
        let marker = dir.join(".hallmark-key");
        let want = key(docs);
        let have = std::fs::read_to_string(&marker).ok();
        if have.as_deref() == Some(want.as_str()) {
            return Ok(());
        }
        let io = |e: std::io::Error| format!("qmd export: {e}");
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(io)?;
        }
        std::fs::create_dir_all(&dir).map_err(io)?;
        for d in docs {
            let p = dir.join(export_rel(d));
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).map_err(io)?;
            }
            std::fs::write(&p, d.learning.render()).map_err(io)?;
        }
        let c = collection(repo);
        let added = dir.parent().unwrap_or(&dir).join(format!("{c}.added"));
        if !added.exists() {
            let d = crate::paths::display(&dir);
            run(&["collection", "add", &d, "--name", &c, "--mask", "**/*.md"])?;
            std::fs::write(&added, "").map_err(io)?;
        }
        run(&["update"])?;
        std::fs::write(&marker, &want).map_err(io)?;
        Ok(())
    }

    /// Searches the repo's collection (and only it).
    pub fn search(
        repo: &Repo,
        docs: &[Located],
        query: &str,
        opts: &Options,
    ) -> std::result::Result<Vec<Hit>, String> {
        sync(repo, docs)?;
        let q = terms(query).join(" ");
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let n = (opts.limit.max(10) * 3).to_string();
        let c = collection(repo);
        let out = run(&["search", "--json", "-n", &n, "-c", &c, &q])?;
        let by_rel: HashMap<String, &Located> = docs.iter().map(|d| (export_rel(d), d)).collect();
        let mut seen = HashSet::new();
        let mut hits = Vec::new();
        for h in parse(&out)? {
            let Some(d) = resolve(&h.file, &by_rel) else {
                continue;
            };
            if !opts.allows(d) || !seen.insert(d.learning.id.clone()) {
                continue;
            }
            hits.push(Hit::from(d, (h.score * 1000.0).round() / 1000.0));
            if opts.limit > 0 && hits.len() >= opts.limit {
                break;
            }
        }
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_are_sanitized() {
        assert_eq!(
            fts_query("webhook retries").as_deref(),
            Some("\"webhook\"* OR \"retries\"*")
        );
        // FTS5 operators and quotes are just words or nothing.
        assert_eq!(
            fts_query("a\" OR NEAR(b) * -c:d ^e").as_deref(),
            Some("\"a\"* OR \"or\"* OR \"near\"* OR \"b\"* OR \"c\"* OR \"d\"* OR \"e\"*")
        );
        assert_eq!(fts_query("  *:()\"  "), None);
    }

    #[test]
    fn diff_terms_prefer_paths_and_repeats() {
        let diff = "--- a/x\n+++ b/x\n+fn retry_delivery(endpoint: &Endpoint) {\n+    endpoint.exists()\n context line ignored\n-    old_backoff()\n";
        let t = diff_terms(&["apps/api/src/webhooks/retry.rs".into()], diff, 50);
        // retry: a path word (3) and a line word (1).
        assert_eq!(t[0], "retry");
        assert_eq!(&t[1..3], &["apps", "api"]);
        assert!(t.contains(&"webhooks".to_string()));
        assert!(t.contains(&"endpoint".to_string()));
        assert!(t.contains(&"backoff".to_string()));
        assert!(!t.contains(&"ignored".to_string()));
        assert!(!t.contains(&"fn".to_string()));
        assert!(diff_terms(&[], diff, 2).len() == 2);
    }

    #[test]
    fn qmd_output_parses() {
        let json = r##"[
          {"docid": "#a1b2c3", "score": 0.91, "file": "qmd://hallmark-app/base/L-0017-retry.md",
           "title": "Retry", "snippet": "…"},
          {"docid": "#d4", "score": 0.4, "path": "H-0003/L-0020-x.md"},
          {"docid": "#e5"}
        ]"##;
        let hits = qmd::parse(json).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].file, "qmd://hallmark-app/base/L-0017-retry.md");
        assert_eq!(hits[0].score, 0.91);
        assert_eq!(hits[1].file, "H-0003/L-0020-x.md");
        assert_eq!(qmd::parse(r#"{"results": []}"#).unwrap(), vec![]);
        assert!(qmd::parse("not json").is_err());
    }
}
