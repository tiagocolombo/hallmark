//! `git` as a subprocess.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{ErrorCode, HallError, Result};

pub const NOTES_REF: &str = "refs/notes/hallmark";

fn command(dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir);
    // Hooks may run inside another git process; never inherit its repo.
    for v in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
        c.env_remove(v);
    }
    c
}

/// Runs git and returns trimmed stdout, or COMMAND_FAILED with stderr.
pub fn run(dir: &Path, args: &[&str]) -> Result<String> {
    let out = command(dir).args(args).output().map_err(|e| {
        HallError::new(ErrorCode::CommandFailed, format!("could not run git: {e}"))
            .next(["hall doctor"])
    })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        Err(HallError::new(
            ErrorCode::CommandFailed,
            format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ))
    }
}

pub fn ok(dir: &Path, args: &[&str]) -> bool {
    command(dir)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The worktree root containing `dir`.
pub fn toplevel(dir: &Path) -> Result<PathBuf> {
    run(dir, &["rev-parse", "--show-toplevel"]).map(|s| crate::paths::normalize(Path::new(&s)))
}

/// The repository's shared git directory, the same from every worktree.
pub fn common_dir(dir: &Path) -> Result<PathBuf> {
    let s = run(dir, &["rev-parse", "--git-common-dir"])?;
    let p = PathBuf::from(&s);
    let p = if p.is_absolute() { p } else { dir.join(p) };
    Ok(crate::paths::normalize(&p))
}

pub fn head(dir: &Path) -> Result<String> {
    run(dir, &["rev-parse", "HEAD"])
}

pub fn current_branch(dir: &Path) -> Option<String> {
    run(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .ok()
        .filter(|s| !s.is_empty())
}

/// Tracked files with uncommitted changes. Untracked files don't count:
/// they aren't part of any commit.
pub fn dirty_files(dir: &Path) -> Result<Vec<String>> {
    let s = run(dir, &["status", "--porcelain", "--untracked-files=no"])?;
    Ok(s.lines()
        .map(|l| l.get(3..).unwrap_or(l).to_string())
        .collect())
}

pub fn branch_exists(dir: &Path, branch: &str) -> bool {
    ok(
        dir,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
}

pub fn is_ignored(dir: &Path, path: &Path) -> bool {
    ok(dir, &["check-ignore", "-q", &path.to_string_lossy()])
}

pub fn subject(dir: &Path, rev: &str) -> String {
    run(dir, &["log", "-1", "--format=%s", rev]).unwrap_or_default()
}

/// The hallmark note on a commit, if it has one.
pub fn hallmark_note(dir: &Path, rev: &str) -> Option<String> {
    run(dir, &["notes", "--ref", NOTES_REF, "show", rev])
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// The commit `rev` names, if it names one.
pub fn rev(dir: &Path, rev: &str) -> Option<String> {
    run(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
    )
    .ok()
    .filter(|s| !s.is_empty())
}

/// The commit `branch` is at on `remote`, by `git ls-remote`: one small
/// request, no fetch, and never a prompt. `Some("")` when the remote has no
/// such branch; None when the remote can't be asked.
pub fn remote_head(dir: &Path, remote: &str, branch: &str) -> Option<String> {
    let out = command(dir)
        .args(["ls-remote", remote, &format!("refs/heads/{branch}")])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string()
    })
}

/// Whether `a` is an ancestor of (or equal to) `b`.
pub fn is_ancestor(dir: &Path, a: &str, b: &str) -> bool {
    ok(dir, &["merge-base", "--is-ancestor", a, b])
}

pub fn merge_base(dir: &Path, a: &str, b: &str) -> Option<String> {
    run(dir, &["merge-base", a, b])
        .ok()
        .filter(|s| !s.is_empty())
}

/// The stable patch ID of the diff `from..to`: equal for the same change
/// on a different base. None for an empty diff.
pub fn patch_id(dir: &Path, from: &str, to: &str) -> Option<String> {
    use std::io::Write;
    use std::process::Stdio;
    let diff = command(dir)
        .args(["diff", "--no-color", "--no-ext-diff", from, to])
        .output()
        .ok()
        .filter(|o| o.status.success())?
        .stdout;
    if diff.is_empty() {
        return None;
    }
    let mut child = command(dir)
        .args(["patch-id", "--stable"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(&diff).ok()?;
    let out = child.wait_with_output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string)
}

/// Fetches `origin <base>` into `refs/remotes/origin/<base>`, bounded to 20 s
/// and never prompting: a stalled network or missing credentials leave the
/// local copy as it was.
pub fn fetch_base_capped(dir: &Path, base: &str) {
    let child = command(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "fetch",
            "--quiet",
            "origin",
            &format!("+refs/heads/{base}:refs/remotes/origin/{base}"),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        let start = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if start.elapsed() < std::time::Duration::from_secs(20) => {
                    crate::daemon::nap(std::time::Duration::from_millis(50));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
    }
}

/// A rebase stopped half-way in this worktree.
pub fn rebase_in_progress(dir: &Path) -> bool {
    ["rebase-merge", "rebase-apply"].iter().any(|d| {
        run(dir, &["rev-parse", "--git-path", d])
            .map(|p| crate::paths::resolve(dir, Path::new(&p)).exists())
            .unwrap_or(false)
    })
}

/// Files with unresolved conflicts.
pub fn conflicted(dir: &Path) -> Vec<String> {
    run(dir, &["diff", "--name-only", "--diff-filter=U"])
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: Option<String>,
}

/// Every worktree of the repository; the main one comes first.
pub fn worktrees(dir: &Path) -> Result<Vec<Worktree>> {
    let s = run(dir, &["worktree", "list", "--porcelain"])?;
    let mut out = Vec::new();
    let mut cur: Option<Worktree> = None;
    for line in s.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if let Some(w) = cur.take() {
                out.push(w);
            }
            cur = Some(Worktree {
                path: crate::paths::normalize(Path::new(p)),
                branch: None,
                head: None,
            });
        } else if let Some(b) = line.strip_prefix("branch ") {
            if let Some(w) = cur.as_mut() {
                w.branch = Some(b.trim_start_matches("refs/heads/").to_string());
            }
        } else if let Some(h) = line.strip_prefix("HEAD ")
            && let Some(w) = cur.as_mut()
        {
            w.head = Some(h.to_string());
        }
    }
    if let Some(w) = cur {
        out.push(w);
    }
    Ok(out)
}

/// The repo's default branch: origin/HEAD, else main or master, else the
/// current branch.
pub fn default_branch(dir: &Path) -> String {
    if let Ok(s) = run(
        dir,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    ) && let Some(b) = s.strip_prefix("origin/")
    {
        return b.to_string();
    }
    for b in ["main", "master", "trunk"] {
        if branch_exists(dir, b) {
            return b.to_string();
        }
    }
    current_branch(dir).unwrap_or_else(|| "main".into())
}

pub fn version() -> Option<String> {
    let out = Command::new("git").arg("--version").output().ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .trim_start_matches("git version ")
            .to_string()
    })
}
