//! The checks cache: every gate result, keyed on what decides it — the
//! tree checked, the gate's command, the configured gates and the OS. A gate
//! that passed on a tree passes again on it, so the bench serves a passing
//! result instead of running the gate. A failing one is never served: a
//! fixer's retry runs the gate again.
//!
//! Each key has one append-only file, `<hall>/checks-cache/<key>.jsonl`,
//! with a line per run, red or green. One key with both is a flaky gate.
//! Like the index, the cache can be deleted at any time: the gates just run.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use fs4::FileExt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::git;
use crate::journal::{Hall, read_locked};

/// What a gate's result depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    /// `git rev-parse <sha>^{tree}`.
    pub tree: String,
    /// The gate's command, as configured.
    pub command: String,
    /// A hash of the configured gates, in order: changing them starts over.
    pub gates: String,
    /// `std::env::consts::OS`.
    pub os: String,
}

impl Key {
    pub fn new(tree: &str, command: &str, gates: &[String]) -> Key {
        Key {
            tree: tree.to_string(),
            command: command.to_string(),
            gates: gates_hash(gates),
            os: std::env::consts::OS.to_string(),
        }
    }

    /// The file name: the tree, then a hash of the rest, so every key of
    /// one tree sorts together.
    pub fn id(&self) -> String {
        let rest = fnv1a(&[&self.command, &self.gates, &self.os]);
        format!("{}-{rest:016x}", self.tree)
    }

    fn matches(&self, r: &CachedRun) -> bool {
        r.tree == self.tree && r.command == self.command && r.gates == self.gates && r.os == self.os
    }
}

/// One gate run, as the cache keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CachedRun {
    /// [`Key::id`].
    pub key: String,
    pub tree: String,
    pub command: String,
    pub gates: String,
    pub os: String,
    /// The item and commit it ran for.
    pub item: String,
    pub sha: String,
    pub at: DateTime<Utc>,
    pub exit_code: i32,
    pub duration_ms: u64,
    /// The run's log, relative to the journal root
    /// (`items/H-0003/checks/r1.log`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
}

pub fn dir(hall: &Hall) -> PathBuf {
    hall.root.join("checks-cache")
}

fn path(hall: &Hall, key: &Key) -> PathBuf {
    dir(hall).join(format!("{}.jsonl", key.id()))
}

/// The tree `sha` points at, or None when git can't say.
pub fn tree(repo_dir: &Path, sha: &str) -> Option<String> {
    git::run(repo_dir, &["rev-parse", &format!("{sha}^{{tree}}")])
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Every run recorded for `key`, oldest first. Unreadable lines are skipped.
pub fn runs(hall: &Hall, key: &Key) -> Vec<CachedRun> {
    read_locked(&path(hall, key), 0)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<CachedRun>(l).ok())
        .filter(|r| key.matches(r))
        .collect()
}

/// The latest passing run for `key`: the only kind ever served.
pub fn passed(hall: &Hall, key: &Key) -> Option<CachedRun> {
    runs(hall, key).into_iter().rev().find(|r| r.exit_code == 0)
}

/// Appends one run, red or green, under the file's lock.
pub fn record(hall: &Hall, run: &CachedRun) -> Result<()> {
    let d = dir(hall);
    fs::create_dir_all(&d)?;
    let f: File = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(d.join(format!("{}.jsonl", run.key)))?;
    FileExt::lock(&f)?;
    let mut line = serde_json::to_string(run)?;
    line.push('\n');
    let r = (&f).write_all(line.as_bytes());
    let _ = FileExt::unlock(&f);
    Ok(r?)
}

/// A hash of the configured gates, in order.
pub fn gates_hash(gates: &[String]) -> String {
    let parts: Vec<&str> = gates.iter().map(String::as_str).collect();
    format!("{:016x}", fnv1a(&parts))
}

/// 64-bit FNV-1a over `parts`, each ended by a NUL. Stable across Rust
/// versions and platforms, unlike `DefaultHasher`.
fn fnv1a(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.bytes().chain(std::iter::once(0)) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_is_stable() {
        // FNV-1a of "a\0": pinned, so a key never changes between builds.
        assert_eq!(fnv1a(&["a"]), 0x089b_e207_b544_f1e4);
    }

    #[test]
    fn keys_differ_by_command_gates_and_tree() {
        let gates = vec!["make test".to_string(), "make lint".to_string()];
        let a = Key::new("t1", "make test", &gates);
        assert_eq!(a.id(), Key::new("t1", "make test", &gates).id());
        assert!(a.id().starts_with("t1-"));
        assert_ne!(a.id(), Key::new("t1", "make lint", &gates).id());
        assert_ne!(a.id(), Key::new("t2", "make test", &gates).id());
        let fewer = vec!["make test".to_string()];
        assert_ne!(a.id(), Key::new("t1", "make test", &fewer).id());
        // Order matters, and a gate can't spill into the next.
        assert_ne!(
            gates_hash(&["ab".into(), "c".into()]),
            gates_hash(&["a".into(), "bc".into()])
        );
    }

    #[test]
    fn only_a_pass_is_served_and_every_run_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let hall = Hall::new(tmp.path());
        let key = Key::new("t1", "make test", &["make test".into()]);
        assert!(passed(&hall, &key).is_none());
        let run = |exit_code, sha: &str| CachedRun {
            key: key.id(),
            tree: key.tree.clone(),
            command: key.command.clone(),
            gates: key.gates.clone(),
            os: key.os.clone(),
            item: "H-0001".into(),
            sha: sha.into(),
            at: Utc::now(),
            exit_code,
            duration_ms: 10,
            log: Some("items/H-0001/checks/r1.log".into()),
        };
        record(&hall, &run(1, "a")).unwrap();
        assert!(passed(&hall, &key).is_none(), "a red run is never served");
        record(&hall, &run(0, "b")).unwrap();
        record(&hall, &run(2, "c")).unwrap();
        assert_eq!(passed(&hall, &key).unwrap().sha, "b");
        let all = runs(&hall, &key);
        assert_eq!(
            all.iter().map(|r| r.exit_code).collect::<Vec<_>>(),
            [1, 0, 2]
        );
        let other = Key::new("t1", "make lint", &["make test".into()]);
        assert!(runs(&hall, &other).is_empty());
    }
}
