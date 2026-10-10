//! `gh` as a subprocess. It may not be on PATH (on Nix machines it can live
//! only in the store), so it is searched for.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::error::{ErrorCode, HallError, Result};

/// `HALL_GH`, then PATH, then the newest `/nix/store/*-gh-*/bin/gh`.
pub fn find() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("HALL_GH") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    if let Some(p) = which("gh") {
        return Some(p);
    }
    nix_store_gh()
}

/// Searches PATH (with PATHEXT on Windows).
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(|e| e.to_ascii_lowercase())
            .chain(std::iter::once(String::new()))
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let p = dir.join(format!("{name}{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// PATH with the directory of the `gh` Hallmark found in front, so
/// configured commands (the ship command) find it where Hallmark does,
/// such as in the Nix store. None when gh is missing or already first.
pub fn path_with_gh() -> Option<std::ffi::OsString> {
    let dir = find()?.parent()?.to_path_buf();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let rest: Vec<PathBuf> = std::env::split_paths(&path).collect();
    if rest.first() == Some(&dir) {
        return None;
    }
    std::env::join_paths(std::iter::once(dir).chain(rest)).ok()
}

fn nix_store_gh() -> Option<PathBuf> {
    let store = Path::new("/nix/store");
    let mut found: Vec<(Vec<u64>, PathBuf)> = std::fs::read_dir(store)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let version = name.split_once("-gh-")?.1;
            if !version.chars().next()?.is_ascii_digit() || name.ends_with(".drv") {
                return None;
            }
            let bin = e.path().join("bin/gh");
            bin.is_file().then(|| (parse_version(version), bin))
        })
        .collect();
    found.sort();
    found.pop().map(|(_, p)| p)
}

fn parse_version(v: &str) -> Vec<u64> {
    v.split(['.', '-']).map_while(|p| p.parse().ok()).collect()
}

fn gh() -> Result<PathBuf> {
    find().ok_or_else(|| {
        HallError::new(
            ErrorCode::GhUnavailable,
            "gh (the GitHub CLI) was not found",
        )
        .next([
            "install gh: https://cli.github.com",
            "set HALL_GH=/path/to/gh",
            "hall doctor",
        ])
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct GhIssue {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub url: Option<String>,
}

pub fn issue(dir: &Path, number: u64) -> Result<GhIssue> {
    let out = Command::new(gh()?)
        .current_dir(dir)
        .args([
            "issue",
            "view",
            &number.to_string(),
            "--json",
            "number,title,body,url",
        ])
        .output()?;
    if !out.status.success() {
        return Err(HallError::new(
            ErrorCode::GhUnavailable,
            format!(
                "gh issue view {number} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        )
        .next(["gh auth status", "hall doctor"]));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| {
        HallError::new(
            ErrorCode::GhUnavailable,
            format!("unreadable gh output: {e}"),
        )
    })
}

/// Who wrote a comment or review.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GhAuthor {
    #[serde(default)]
    pub login: String,
}

/// A PR comment, a review, or a line comment on the diff.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GhComment {
    #[serde(default, alias = "user")]
    pub author: Option<GhAuthor>,
    #[serde(default)]
    pub body: String,
    /// Reviews: APPROVED, CHANGES_REQUESTED, COMMENTED.
    #[serde(default)]
    pub state: Option<String>,
    /// Line comments: the file and line.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default, alias = "original_line")]
    pub line: Option<u64>,
}

/// A pull request and the feedback on it (for follow-up items).
#[derive(Debug, Clone, Deserialize)]
pub struct GhPr {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub url: String,
    #[serde(default)]
    pub reviews: Vec<GhComment>,
    #[serde(default)]
    pub comments: Vec<GhComment>,
    /// OPEN, CLOSED or MERGED.
    #[serde(default)]
    pub state: String,
    #[serde(default, rename = "headRefName")]
    pub head: String,
    #[serde(default, rename = "baseRefName")]
    pub base: String,
    /// Comments on lines of the diff (`gh api …/pulls/<n>/comments`).
    #[serde(default, skip_deserializing)]
    pub line_comments: Vec<GhComment>,
}

/// `gh pr view <pr>` (a URL or number) with its reviews and comments, plus
/// the line comments when `gh api` can read them.
pub fn pr(dir: &Path, pr: &str) -> Result<GhPr> {
    let out = Command::new(gh()?)
        .current_dir(dir)
        .args([
            "pr",
            "view",
            pr,
            "--json",
            "number,url,title,body,reviews,comments,state,headRefName,baseRefName",
        ])
        .output()?;
    if !out.status.success() {
        return Err(HallError::new(
            ErrorCode::GhUnavailable,
            format!(
                "gh pr view {pr} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        )
        .next(["gh auth status", "hall doctor"]));
    }
    let mut p: GhPr = serde_json::from_slice(&out.stdout).map_err(|e| {
        HallError::new(
            ErrorCode::GhUnavailable,
            format!("unreadable gh output: {e}"),
        )
    })?;
    if let Some((owner, name)) = owner_repo(&p.url) {
        let api = format!(
            "repos/{owner}/{name}/pulls/{}/comments?per_page=100",
            p.number
        );
        if let Ok(o) = Command::new(gh()?)
            .current_dir(dir)
            .args(["api", &api])
            .output()
            && o.status.success()
        {
            p.line_comments = serde_json::from_slice(&o.stdout).unwrap_or_default();
        }
    }
    Ok(p)
}

/// One CI check of a PR, as `gh pr checks --json` reports it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GhCheck {
    pub name: String,
    /// pass, fail, pending, skipping or cancel.
    #[serde(default)]
    pub bucket: String,
    #[serde(default)]
    pub link: Option<String>,
}

/// The PR's CI checks. An empty list means it has none (yet).
pub fn pr_checks(dir: &Path, pr: &str) -> Result<Vec<GhCheck>> {
    let out = Command::new(gh()?)
        .current_dir(dir)
        .args(["pr", "checks", pr, "--json", "name,bucket,link"])
        .output()?;
    // `gh pr checks` exits 8 while checks are pending and 1 when one failed,
    // with the JSON on stdout either way.
    serde_json::from_slice(&out.stdout).map_err(|e| {
        HallError::new(
            ErrorCode::GhUnavailable,
            format!(
                "gh pr checks {pr}: {e}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        )
        .next(["gh auth status", "hall doctor"])
    })
}

/// The run and job IDs in a check's link
/// (`…/actions/runs/<run>/job/<job>`).
pub fn run_and_job(link: &str) -> Option<(String, String)> {
    let rest = link.split("/actions/runs/").nth(1)?;
    let mut parts = rest.split('/');
    let run = parts.next()?.to_string();
    let job = (parts.next()? == "job")
        .then(|| parts.next())
        .flatten()?
        .to_string();
    (run.chars().all(|c| c.is_ascii_digit()) && job.chars().all(|c| c.is_ascii_digit()))
        .then_some((run, job))
}

/// The tail of a failed job's log (`gh run view --job --log-failed`),
/// focused on the lines that say what broke. None when gh can't read it.
pub fn failed_log(dir: &Path, link: &str, max_lines: usize) -> Option<String> {
    let (run, job) = run_and_job(link)?;
    let out = Command::new(gh().ok()?)
        .current_dir(dir)
        .args(["run", "view", &run, "--job", &job, "--log-failed"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let excerpt = log_excerpt(&text, max_lines);
    (!excerpt.trim().is_empty()).then_some(excerpt)
}

/// The useful part of a CI log: lines around panics, errors and failures,
/// without the runner's timestamps, at most `max` lines.
pub fn log_excerpt(log: &str, max: usize) -> String {
    let lines: Vec<String> = log
        .lines()
        .map(|l| {
            // "job\tstep\t2026-10-10T02:41:18.1Z text": keep the text.
            let l = l.rsplit('\t').next().unwrap_or(l);
            match l.split_once("Z ") {
                Some((ts, rest)) if ts.len() >= 20 && ts.as_bytes()[4] == b'-' => rest.to_string(),
                _ => l.to_string(),
            }
        })
        .collect();
    let hot: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            let l = l.to_ascii_lowercase();
            l.contains("panicked")
                || l.contains("error")
                || l.contains("failed")
                || l.contains("failures:")
                || l.contains("assert")
        })
        .map(|(i, _)| i)
        .collect();
    let mut keep = vec![false; lines.len()];
    for i in hot {
        let from = i.saturating_sub(2);
        for k in keep.iter_mut().skip(from).take(i + 6 - from) {
            *k = true;
        }
    }
    let mut out: Vec<&str> = lines
        .iter()
        .zip(&keep)
        .filter(|(_, k)| **k)
        .map(|(l, _)| l.as_str())
        .collect();
    if out.is_empty() {
        out = lines.iter().map(String::as_str).collect();
    }
    let start = out.len().saturating_sub(max);
    out[start..].join("\n")
}

/// `https://github.com/o/r/pull/12` → (o, r).
fn owner_repo(url: &str) -> Option<(String, String)> {
    let rest = url.split("://").nth(1)?;
    let mut parts = rest.split('/');
    let _host = parts.next()?;
    let owner = parts.next()?.to_string();
    let name = parts.next()?.to_string();
    (parts.next()? == "pull").then_some((owner, name))
}

/// "logged in", "not logged in", or None when gh is missing.
pub fn auth_status() -> Option<(PathBuf, bool)> {
    let gh = find()?;
    let ok = Command::new(&gh)
        .args(["auth", "status"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    Some((gh, ok))
}

#[cfg(test)]
mod ci_tests {
    use super::*;

    #[test]
    fn check_links_name_their_run_and_job() {
        assert_eq!(
            run_and_job("https://github.com/o/r/actions/runs/38017825353/job/114111945371"),
            Some(("38017825353".into(), "114111945371".into()))
        );
        assert_eq!(run_and_job("https://example.com/status/1"), None);
    }

    #[test]
    fn log_excerpts_keep_what_broke() {
        let log = "test (ubuntu-latest)\tRun cargo test\t2026-10-10T02:41:18.1234567Z running 9 tests\n\
                   test (ubuntu-latest)\tRun cargo test\t2026-10-10T02:41:18.2Z test a ... ok\n\
                   test (ubuntu-latest)\tRun cargo test\t2026-10-10T02:41:18.3Z thread 'x' panicked at tests/setup.rs:405:14:\n\
                   test (ubuntu-latest)\tRun cargo test\t2026-10-10T02:41:18.4Z Text file busy";
        let e = log_excerpt(log, 40);
        assert!(
            e.contains("thread 'x' panicked at tests/setup.rs:405:14:"),
            "{e}"
        );
        assert!(e.contains("Text file busy"), "{e}");
        assert!(!e.contains("2026-10-10T"), "timestamps go: {e}");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pr_urls() {
        assert_eq!(
            super::owner_repo("https://github.com/acme/app/pull/12"),
            Some(("acme".into(), "app".into()))
        );
        assert_eq!(
            super::owner_repo("https://github.com/acme/app/issues/12"),
            None
        );
    }

    #[test]
    fn versions_sort_numerically() {
        assert!(super::parse_version("2.101.0") > super::parse_version("2.98.0"));
    }
}
