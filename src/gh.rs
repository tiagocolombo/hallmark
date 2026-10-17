//! `gh` as a subprocess. It may not be on PATH (on Nix machines it can live
//! only in the store), so it is searched for.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::error::{ErrorCode, HallError, Result};

/// `HALL_GH` or `[tools] gh`, then PATH, then the newest
/// `/nix/store/*-gh-*/bin/gh`.
pub fn find() -> Option<PathBuf> {
    if let Some(p) = crate::config::Tool::Gh.configured() {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
        // A bare name, such as `gh-enterprise`, is looked up on PATH.
        return (p.components().count() == 1)
            .then(|| which(&p.to_string_lossy()))
            .flatten();
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
            "hall config set --user tools.gh /path/to/gh",
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
    /// The author's standing on the repo: OWNER, MEMBER, COLLABORATOR,
    /// CONTRIBUTOR, NONE…
    #[serde(default, alias = "authorAssociation")]
    pub author_association: Option<String>,
    /// Reviews: APPROVED, CHANGES_REQUESTED, COMMENTED.
    #[serde(default)]
    pub state: Option<String>,
    /// Line comments: the file and line.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default, alias = "original_line")]
    pub line: Option<u64>,
    /// When it was written: `createdAt` (comments), `submittedAt`
    /// (reviews), `created_at` (line comments).
    #[serde(
        default,
        rename = "createdAt",
        alias = "submittedAt",
        alias = "created_at"
    )]
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
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
    /// The commit the PR's head is at on GitHub.
    #[serde(default, rename = "headRefOid")]
    pub head_oid: String,
    /// The PR comes from a fork.
    #[serde(default, rename = "isCrossRepository")]
    pub cross_repo: bool,
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
            "number,url,title,body,reviews,comments,state,headRefName,baseRefName,headRefOid,isCrossRepository",
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
    // with the JSON on stdout either way. A PR without checks gets an error
    // message and no JSON: that is an empty list.
    if out.stdout.iter().all(u8::is_ascii_whitespace)
        && String::from_utf8_lossy(&out.stderr).contains("no checks reported")
    {
        return Ok(Vec::new());
    }
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

/// A PR's state: OPEN, CLOSED or MERGED.
pub fn pr_state(dir: &Path, pr: &str) -> Result<String> {
    let out = Command::new(gh()?)
        .current_dir(dir)
        .args(["pr", "view", pr, "--json", "state", "--jq", ".state"])
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
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Where an open PR stands for merging.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GhMerge {
    /// OPEN, CLOSED or MERGED.
    #[serde(default)]
    pub state: String,
    /// MERGEABLE, CONFLICTING or UNKNOWN (GitHub is still computing it).
    #[serde(default)]
    pub mergeable: String,
    #[serde(default, rename = "headRefOid")]
    pub head_oid: String,
}

/// `gh pr view <pr> --json state,mergeable,headRefOid`.
pub fn pr_merge(dir: &Path, pr: &str) -> Result<GhMerge> {
    let out = Command::new(gh()?)
        .current_dir(dir)
        .args(["pr", "view", pr, "--json", "state,mergeable,headRefOid"])
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
    serde_json::from_slice(&out.stdout).map_err(|e| {
        HallError::new(
            ErrorCode::GhUnavailable,
            format!("unreadable gh output: {e}"),
        )
    })
}

/// What the merger reads of a PR in one call (ADR 0048).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GhQueue {
    /// OPEN, CLOSED or MERGED.
    #[serde(default)]
    pub state: String,
    /// MERGEABLE, CONFLICTING or UNKNOWN (GitHub is still computing it).
    #[serde(default)]
    pub mergeable: String,
    #[serde(default, rename = "headRefOid")]
    pub head_oid: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub reviews: Vec<GhComment>,
}

/// `gh pr view <pr> --json state,mergeable,headRefOid,body,reviews`.
pub fn pr_queue(dir: &Path, pr: &str) -> Result<GhQueue> {
    let out = Command::new(gh()?)
        .current_dir(dir)
        .args([
            "pr",
            "view",
            pr,
            "--json",
            "state,mergeable,headRefOid,body,reviews",
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
    serde_json::from_slice(&out.stdout).map_err(|e| {
        HallError::new(
            ErrorCode::GhUnavailable,
            format!("unreadable gh output: {e}"),
        )
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

/// Whether a failed job ran.
#[derive(Debug, Clone, PartialEq)]
pub enum JobStart {
    Ran,
    /// Never started; the reason GitHub gives (empty when not asked for).
    Never(String),
    /// GitHub didn't say (an API error, a rate limit, an odd shape).
    Unknown,
}

/// Reads whether a failed job ran. A job that never started has no steps and
/// no runner; GitHub gives the reason in an annotation (read only when
/// `reason` is set, to spare the API on re-checks).
pub fn job_start(dir: &Path, link: &str, reason: bool) -> JobStart {
    job_start_inner(dir, link, reason).unwrap_or(JobStart::Unknown)
}

fn job_start_inner(dir: &Path, link: &str, reason: bool) -> Option<JobStart> {
    #[derive(Deserialize)]
    struct Annotation {
        #[serde(default)]
        annotation_level: String,
        #[serde(default)]
        message: String,
    }
    let (_, job) = run_and_job(link)?;
    let api = |path: String| {
        Command::new(gh().ok()?)
            .current_dir(dir)
            .args(["api", &path])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| o.stdout)
    };
    // Only a job object with an empty `steps` list counts; anything else
    // (an error, an unexpected shape) means "can't tell".
    let j: serde_json::Value = serde_json::from_slice(&api(format!(
        "repos/{{owner}}/{{repo}}/actions/jobs/{job}"
    ))?)
    .ok()?;
    let ran = !j.get("steps")?.as_array()?.is_empty()
        || j.get("runner_name")
            .and_then(|r| r.as_str())
            .is_some_and(|r| !r.is_empty());
    if ran {
        return Some(JobStart::Ran);
    }
    if !reason {
        return Some(JobStart::Never(String::new()));
    }
    let reason = api(format!(
        "repos/{{owner}}/{{repo}}/check-runs/{job}/annotations"
    ))
    .and_then(|b| serde_json::from_slice::<Vec<Annotation>>(&b).ok())
    .and_then(|a| {
        a.into_iter()
            .find(|a| a.annotation_level == "failure" && !a.message.trim().is_empty())
            .map(|a| a.message.trim().to_string())
    });
    Some(JobStart::Never(
        reason.unwrap_or_else(|| "the job never started".into()),
    ))
}

/// A failed job's log, cut down by [`log_excerpt`]. The job's own log comes
/// from the API by its id: `gh run view --job` answers with the run's latest
/// attempt, which after a re-run isn't the one that failed. `gh run view` is
/// the fallback.
pub fn failed_log(dir: &Path, link: &str, max_lines: usize) -> Option<String> {
    let (run, job) = run_and_job(link)?;
    let gh = gh().ok()?;
    let read = |args: &[&str], must_succeed: bool| {
        Command::new(&gh)
            .current_dir(dir)
            .args(args)
            .output()
            .ok()
            .filter(|o| !must_succeed || o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .filter(|t| !t.trim().is_empty())
    };
    let api = format!("repos/{{owner}}/{{repo}}/actions/jobs/{job}/logs");
    // Build logs carry color codes, which newer `gh api` refuses to print
    // without the flag; older ones don't know it.
    let text = read(&["api", "--allow-escape-sequences", &api], true)
        .or_else(|| read(&["api", &api], true))
        .or_else(|| read(&["run", "view", &run, "--job", &job, "--log-failed"], false))?;
    let excerpt = log_excerpt(&text, max_lines);
    (!excerpt.trim().is_empty()).then_some(excerpt)
}

/// The useful part of a CI log, without the runner's timestamps and color
/// codes, at most `max` lines, in passes. First the summary (`test result:
/// FAILED`, `error: test failed`). Then the failure detail (panics,
/// `failures:`, `error:`, `error[`) from the first, with a little context.
/// Then the lines that name what failed (`... FAILED`, the runner's
/// `##[error]`, which is not detail). Last, always, the lines that only
/// mention an error or a failure, newest first. A hot line's context goes in
/// whole or not at all. Without any hot line, the end of the log.
pub fn log_excerpt(log: &str, max: usize) -> String {
    let lines: Vec<String> = log
        .lines()
        .map(|l| {
            // "job\tstep\t2026-10-10T02:41:18.1Z text": keep the text.
            let l = l.rsplit('\t').next().unwrap_or(l);
            let l = l.trim_start_matches('\u{feff}');
            let l = match l.split_once("Z ") {
                Some((ts, rest)) if ts.len() >= 20 && ts.as_bytes()[4] == b'-' => rest,
                _ => l,
            };
            strip_ansi(l)
        })
        .collect();
    // The failure detail (panics, summaries, compiler errors) first, then the
    // lines that name what failed, then lines that only mention an error. The
    // runner's own `##[error]` line says nothing about the tool, so it isn't
    // detail.
    let detail = |l: &str| {
        let t = l.trim_start();
        l.contains("panicked at")
            || l.contains("test result: FAILED")
            || t.starts_with("failures:")
            || t.starts_with("error:")
            || t.starts_with("error[")
    };
    let named =
        |l: &str| l.trim_end().ends_with("... FAILED") || l.trim_start().starts_with("##[error]");
    let mentions = |l: &str| {
        let l = l.to_ascii_lowercase();
        l.contains("panicked")
            || l.contains("error")
            || l.contains("failed")
            || l.contains("failures:")
            || l.contains("assert")
    };
    let mut keep = vec![false; lines.len()];
    let mut count = 0;
    // Adds each hot line with a little context until `max` lines are kept. A
    // hot line's context goes in whole or not at all; when it doesn't fit, the
    // hot line alone does, if there is room.
    let mut take = |hot: Vec<usize>| {
        for i in hot {
            let from = i.saturating_sub(2);
            let to = (i + 6).min(lines.len());
            let new = (from..to).filter(|&k| !keep[k]).count();
            if count + new <= max {
                (from..to).for_each(|k| keep[k] = true);
                count += new;
            } else if !keep[i] && count < max {
                keep[i] = true;
                count += 1;
            }
        }
    };
    let pick = |f: &dyn Fn(&str) -> bool| -> Vec<usize> {
        (0..lines.len()).filter(|&i| f(&lines[i])).collect()
    };
    // The summary first: with many panics, their blocks would fill the budget
    // before the end of the log.
    take(pick(&|l: &str| {
        l.contains("test result: FAILED") || l.trim_start().starts_with("error: test failed")
    }));
    take(pick(&detail));
    take(pick(&named));
    // Mentions come last, latest first: the end of a log holds the summary.
    let mut m = pick(&mentions);
    m.reverse();
    take(m);
    let out: Vec<&str> = lines
        .iter()
        .zip(&keep)
        .filter(|(_, k)| **k)
        .map(|(l, _)| l.as_str())
        .collect();
    if out.is_empty() {
        let start = lines.len().saturating_sub(max);
        return lines[start..].join("\n");
    }
    out.join("\n")
}

/// `text` without terminal color codes (`ESC [ ... m` and the like).
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
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
    fn a_tsc_error_survives_the_runner_error_line() {
        let mut log = String::new();
        for n in 0..30 {
            log.push_str(&format!("2026-10-16T03:00:00.0Z building step {n}\n"));
        }
        log.push_str("2026-10-16T03:00:01.0Z src/retry.ts(10,5): error TS2322: Type 'string' is not assignable to type 'number'.\n");
        for n in 0..20 {
            log.push_str(&format!("2026-10-16T03:00:02.0Z npm error line {n}\n"));
        }
        log.push_str("2026-10-16T03:00:03.0Z ##[error]Process completed with exit code 2.\n");
        let e = log_excerpt(&log, 60);
        assert!(e.contains("error TS2322"), "{e}");
        assert!(e.contains("##[error]Process completed"), "{e}");
    }

    #[test]
    fn a_pytest_assert_survives_the_runner_error_line() {
        let mut log = String::new();
        for n in 0..80 {
            log.push_str(&format!("2026-10-16T03:00:00.0Z tests/test_{n}.py .\n"));
        }
        log.push_str("2026-10-16T03:00:01.0Z def test_x():\n2026-10-16T03:00:01.0Z > assert 1 == 2\n2026-10-16T03:00:01.0Z E assert 1 == 2\n2026-10-16T03:00:01.0Z E AssertionError\n");
        for n in 0..70 {
            log.push_str(&format!("2026-10-16T03:00:02.0Z cleanup {n}\n"));
        }
        log.push_str("2026-10-16T03:00:03.0Z ##[error]Process completed with exit code 1.\n");
        let e = log_excerpt(&log, 60);
        assert!(e.contains("E AssertionError"), "{e}");
        assert!(e.contains("##[error]Process completed"), "{e}");
    }

    #[test]
    fn many_failed_tests_do_not_push_out_the_panics() {
        let mut log = String::new();
        for n in 0..200 {
            log.push_str(&format!("2026-10-16T03:00:00.0Z test t{n} ... ok\n"));
            if n % 20 == 0 {
                log.push_str(&format!("2026-10-16T03:00:00.0Z test bad{n} ... FAILED\n"));
            }
        }
        log.push_str("2026-10-16T03:00:01.0Z failures:\n2026-10-16T03:00:01.0Z ---- bad0 stdout ----\n2026-10-16T03:00:01.0Z thread 'bad0' panicked at src/x.rs:1:1:\n2026-10-16T03:00:01.0Z test result: FAILED. 188 passed; 12 failed\n2026-10-16T03:00:02.0Z ##[error]Process completed with exit code 101.\n");
        let e = log_excerpt(&log, 60);
        assert!(e.lines().count() <= 60, "{e}");
        assert!(e.contains("panicked at src/x.rs:1:1"), "{e}");
        assert!(e.contains("test result: FAILED"), "{e}");
    }

    #[test]
    fn many_panics_do_not_push_out_the_summary() {
        // Twelve panic blocks: without the summary pass they fill all 60
        // lines before the summary at the end of the log.
        let mut log = String::new();
        for n in 0..12 {
            log.push_str(&format!(
                "2026-10-16T03:00:00.0Z ---- bad{n} stdout ----\n\
                 2026-10-16T03:00:00.0Z thread 'bad{n}' panicked at src/x.rs:{n}:1:\n\
                 2026-10-16T03:00:00.0Z assertion `left == right` failed\n\
                 2026-10-16T03:00:00.0Z   left: 1\n\
                 2026-10-16T03:00:00.0Z  right: 2\n\
                 2026-10-16T03:00:00.0Z note: run with RUST_BACKTRACE=1\n\
                 2026-10-16T03:00:00.0Z \n"
            ));
        }
        log.push_str("2026-10-16T03:00:01.0Z test result: FAILED. 188 passed; 12 failed\n2026-10-16T03:00:01.0Z error: test failed, to rerun pass `--lib`\n");
        let e = log_excerpt(&log, 60);
        assert!(e.lines().count() <= 60, "{e}");
        assert!(e.contains("test result: FAILED"), "{e}");
        assert!(e.contains("error: test failed"), "{e}");
        assert!(e.contains("panicked at src/x.rs:0:1"), "{e}");
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

    /// H-0074's red run: one test failed early, and the passing tests after
    /// it have names with "error" and "failed" in them. The failure leads.
    #[test]
    fn a_failed_test_leads_the_excerpt_over_names_that_mention_errors() {
        let mut log = String::from(
            "\u{feff}2026-10-16T03:16:49.1Z \u{1b}[1m\u{1b}[92m   Compiling\u{1b}[0m hallmark\n\
             2026-10-16T03:16:49.7Z test runner::tests::a_timed_out_run_still_ends_its_stream ... FAILED\n\
             2026-10-16T03:16:53.6Z failures:\n\
             2026-10-16T03:16:53.6Z thread 'runner::tests::a_timed_out_run_still_ends_its_stream' panicked at src/runner.rs:1550:9:\n\
             2026-10-16T03:16:53.6Z test result: FAILED. 227 passed; 1 failed\n",
        );
        for n in 0..200 {
            log.push_str(&format!(
                "2026-10-16T03:17:00.0Z test an_error_{n}_is_reported ... ok\n"
            ));
        }
        log.push_str("2026-10-16T03:17:37.6Z ##[error]Process completed with exit code 101.\n");
        let e = log_excerpt(&log, 60);
        assert!(e.lines().count() <= 60, "{e}");
        assert!(
            e.contains("a_timed_out_run_still_ends_its_stream ... FAILED"),
            "{e}"
        );
        assert!(e.contains("panicked at src/runner.rs:1550:9:"), "{e}");
        assert!(e.contains("##[error]Process completed"), "{e}");
        assert!(!e.contains('\u{1b}'), "color codes go: {e}");
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
