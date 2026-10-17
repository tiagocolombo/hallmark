//! A rebase keeps the approval it earned (ADR 0049). When the commit under
//! review is a rewrite of the item's hallmarked commit (a rebase, with or
//! without resolved conflicts), Hallmark compares the change the reviewer
//! approved with the change now under review, file by file:
//!
//! - **the same change**: the approval is carried, with no review run; the
//!   gates already passed at the new commit;
//! - **a small difference** (up to `MEDIUM_LINES`): the reviewer reads only
//!   that difference, a rebase review;
//! - **a large one**: a full review, as for any rewritten branch.
//!
//! Each side is the diff from the commit's fork point on its base, with no
//! context lines, so a base that moved around the change doesn't make it a
//! different change; the gates and CI check that it still works there.

use std::collections::BTreeMap;

use crate::git;
use crate::item::Item;
use crate::repo::Repo;

/// How the commit under review differs from the hallmarked one it was
/// rewritten from.
#[derive(Debug, Clone)]
pub struct SinceApproval {
    /// The hallmarked commit.
    pub approved: String,
    /// The approved change against the rebased one: a diff of two diffs.
    /// Empty when they are the same change.
    pub interdiff: String,
    /// Lines added plus removed in `interdiff`.
    pub lines: u64,
    /// The files whose change differs.
    pub files: Vec<String>,
}

impl SinceApproval {
    pub fn same(&self) -> bool {
        self.files.is_empty()
    }
}

/// Compares `sha`, forked from `merge_base`, with the item's hallmarked
/// commit. None when there is nothing to compare: the item was never
/// hallmarked, its note is gone, or `sha` only adds commits on top of it
/// (the incremental review covers that).
pub fn compare(repo: &Repo, item: &Item, sha: &str, merge_base: &str) -> Option<SinceApproval> {
    let dir = &repo.main_root;
    let approved = item.hallmark_sha.clone()?;
    if approved == sha || git::is_ancestor(dir, &approved, sha) {
        return None;
    }
    if crate::stamp::read(dir, &approved).is_none_or(|n| n.item != item.id) {
        return None;
    }
    // The base only moves forward, so where the approved commit left the
    // new fork point's history is where it left its base.
    let old_fork = git::merge_base(dir, &approved, merge_base)?;
    let old = by_file(&change(dir, &old_fork, &approved)?);
    let new = by_file(&change(dir, merge_base, sha)?);
    let mut files = Vec::new();
    let (mut before, mut after) = (String::new(), String::new());
    let keys: std::collections::BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    for k in keys {
        let (a, b) = (old.get(k), new.get(k));
        if a == b {
            continue;
        }
        files.push(path_of(k));
        before.push_str(a.map(String::as_str).unwrap_or(""));
        after.push_str(b.map(String::as_str).unwrap_or(""));
    }
    let interdiff = if files.is_empty() {
        String::new()
    } else {
        diff_texts(repo, item, &before, &after)?
    };
    Some(SinceApproval {
        approved,
        lines: count(&interdiff),
        interdiff,
        files,
    })
}

/// The change `from..to` with no context lines and no renames.
fn change(dir: &std::path::Path, from: &str, to: &str) -> Option<String> {
    git::run(
        dir,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-renames",
            "-U0",
            from,
            to,
            "--",
        ],
    )
    .ok()
}

/// Splits a diff by file, keyed by its `diff --git` line, with what only
/// says where the change sits (blob ids, hunk line numbers) taken out.
fn by_file(diff: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut key: Option<String> = None;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            key = Some(line.to_string());
        }
        let Some(k) = &key else { continue };
        if line.starts_with("index ") {
            continue;
        }
        let text = out.entry(k.clone()).or_default();
        if line.starts_with("@@") {
            text.push_str("@@\n");
        } else {
            text.push_str(line);
            text.push('\n');
        }
    }
    out
}

/// `b/<path>` from a `diff --git a/<path> b/<path>` line.
fn path_of(key: &str) -> String {
    key.rsplit_once(" b/")
        .map(|(_, p)| p.to_string())
        .unwrap_or_else(|| key.to_string())
}

/// `git diff --no-index` of two texts, written as `approved.patch` and
/// `rebased.patch` in the item's runs directory, where they stay as a
/// record of what the review compared.
fn diff_texts(repo: &Repo, item: &Item, before: &str, after: &str) -> Option<String> {
    let dir = repo.hall.item_runs(item.id).join("rebase");
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::write(dir.join("approved.patch"), before).ok()?;
    std::fs::write(dir.join("rebased.patch"), after).ok()?;
    git::diff_no_index(&dir, "approved.patch", "rebased.patch")
}

/// Lines added plus removed in a diff, after its file headers.
fn count(diff: &str) -> u64 {
    let mut in_hunk = false;
    let mut n = 0;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            in_hunk = false;
        } else if line.starts_with("@@") {
            in_hunk = true;
        } else if in_hunk && (line.starts_with('+') || line.starts_with('-')) {
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn where_a_change_sits_doesnt_make_it_another_change() {
        let at = |line: u32, blob: &str| {
            format!(
                "diff --git a/src/x.rs b/src/x.rs\nindex {blob}..1111111 100644\n--- a/src/x.rs\n\
                 +++ b/src/x.rs\n@@ -{line},0 +{line},1 @@ fn main() {{\n+    retry();\n"
            )
        };
        assert_eq!(by_file(&at(10, "aaaaaaa")), by_file(&at(42, "bbbbbbb")));
        let other = at(10, "aaaaaaa").replace("retry", "retry_twice");
        assert_ne!(by_file(&at(10, "aaaaaaa")), by_file(&other));
    }

    #[test]
    fn only_changed_lines_are_counted() {
        let d = "diff --git a/approved.patch b/rebased.patch\n--- a/approved.patch\n\
                 +++ b/rebased.patch\n@@ -1,3 +1,3 @@\n diff --git a/x b/x\n--old\n+-new\n++added\n";
        assert_eq!(count(d), 3);
        assert_eq!(
            path_of("diff --git a/docs/a b.md b/docs/a b.md"),
            "docs/a b.md"
        );
    }
}
