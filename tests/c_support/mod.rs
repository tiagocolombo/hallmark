//! Workstream C's test helpers: put items in `fixing` or `hallmarked`
//! without the assayer, by appending the events it would write.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use hallmark::event::{Body, CheckRun, Event, VerdictKind};
use hallmark::findings::{Counts, Finding, Lens, PriorRuling, PriorStatus, Severity};
use hallmark::item::ItemId;
use hallmark::journal::Hall;
use hallmark::repo::Repo;

use crate::common::{Env, commit_file, git};

pub fn hall(env: &Env) -> Hall {
    Hall::new(&env.repo.join(".git"))
}

pub fn append(env: &Env, id: &str, actor: &str, body: Body) {
    let id: ItemId = id.parse().unwrap();
    hall(env)
        .append_item(id, &Event::new(actor, Some(id), body))
        .unwrap();
}

/// Sets a top-level key in the main checkout's `.hallmark.toml`.
pub fn set_config(env: &Env, key: &str, value: &str) {
    let p = env.repo.join(".hallmark.toml");
    let text = std::fs::read_to_string(&p).unwrap();
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.starts_with(key) && t[key.len()..].trim_start().starts_with('='))
        })
        .collect();
    std::fs::write(&p, format!("{key} = {value}\n{}\n", kept.join("\n"))).unwrap();
}

pub fn toml(s: &str) -> String {
    toml_str(s)
}

fn toml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn finding(id: &str, round: u32, sev: Severity, lens: Lens, title: &str) -> Finding {
    Finding {
        id: id.into(),
        round,
        lens,
        severity: sev,
        file: "retry.rs".into(),
        line: 3,
        title: title.into(),
        scenario: format!("{title}: two workers retry at once and deliver twice"),
        fix: "take a row lock before retrying".into(),
        pre_existing: false,
    }
}

pub fn checks(sha: &str, round: u32, passed: bool, runs: Vec<CheckRun>) -> Body {
    Body::Checks {
        sha: sha.into(),
        round,
        passed,
        runs,
    }
}

pub fn verdict(
    sha: &str,
    round: u32,
    findings: Vec<Finding>,
    prior: Vec<(&str, PriorStatus)>,
    counts: Counts,
) -> Body {
    Body::Verdict {
        sha: sha.into(),
        round,
        verdict: if counts.approves() {
            VerdictKind::Approve
        } else {
            VerdictKind::Changes
        },
        escalated: false,
        findings,
        prior: prior
            .into_iter()
            .map(|(id, status)| PriorRuling {
                id: id.into(),
                status,
                note: None,
            })
            .collect(),
        counts,
        summary: Some(format!(
            "Round {round}: retries are safe with the row lock."
        )),
        report: Some(format!("reviews/r{round}.md")),
        reviewer: Some("claude-opus-5-5".into()),
        learnings: vec![],
        base: None,
        since: None,
        needs_person: None,
    }
}

pub fn counts(blocker: u32, major: u32, minor: u32, nit: u32) -> Counts {
    Counts {
        blocker,
        major,
        minor,
        nit,
    }
}

/// A new item, committed and sent to review: (id, worktree, sha).
pub fn ready_item(env: &Env, title: &str) -> (String, PathBuf, String) {
    ready_item_with(env, title, None)
}

/// [`ready_item`], its commit with `message` instead of `change retry.rs`.
pub fn ready_item_with(env: &Env, title: &str, message: Option<&str>) -> (String, PathBuf, String) {
    let (id, wt) = env.new_item(title);
    let mut sha = commit_file(&wt, "retry.rs", "fn retry() {\n    deliver();\n}\n");
    if let Some(m) = message {
        git(&wt, &["commit", "-q", "--amend", "-m", m]);
        sha = git(&wt, &["rev-parse", "HEAD"]);
    }
    let r = env.hall(&wt, &["ready"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    (id, wt, sha)
}

/// An item in fixing after a rejected round 1 with F1 (major,
/// correctness), F2 (minor, evidence) and F3 (nit, repo rules).
pub fn fixing_item(env: &Env, title: &str) -> (String, PathBuf, String) {
    let (id, wt, sha) = ready_item(env, title);
    append(env, &id, "assayer", checks(&sha, 1, true, vec![]));
    append(
        env,
        &id,
        "assayer",
        verdict(
            &sha,
            1,
            vec![
                finding(
                    "F1",
                    1,
                    Severity::Major,
                    Lens::Correctness,
                    "Double delivery",
                ),
                finding("F2", 1, Severity::Minor, Lens::Evidence, "No retry test"),
                finding("F3", 1, Severity::Nit, Lens::RepoRules, "Name the constant"),
            ],
            vec![],
            counts(0, 1, 1, 1),
        ),
    );
    (id, wt, sha)
}

/// An item in fixing because its checks failed, with a log.
pub fn failing_checks_item(env: &Env, title: &str) -> (String, PathBuf, String) {
    let (id, wt, sha) = ready_item(env, title);
    let dir = env
        .repo
        .join(".git/hallmark/items")
        .join(&id)
        .join("checks");
    std::fs::create_dir_all(&dir).unwrap();
    let log: String = (1..=300)
        .map(|i| format!("test line {i}\n"))
        .collect::<String>()
        + "test retry::twice ... FAILED\n";
    std::fs::write(dir.join("r1.log"), log).unwrap();
    append(
        env,
        &id,
        "assayer",
        checks(
            &sha,
            1,
            false,
            vec![CheckRun {
                command: "cargo test".into(),
                exit_code: 101,
                duration_ms: 4200,
                log: Some("checks/r1.log".into()),
                cached: false,
            }],
        ),
    );
    (id, wt, sha)
}

/// Approves the item's frozen commit in `round` and strikes the hallmark.
pub fn approve(
    env: &Env,
    id: &str,
    wt: &Path,
    round: u32,
    prior: Vec<(&str, PriorStatus)>,
    open: Counts,
) -> String {
    let sha = git(wt, &["rev-parse", "HEAD"]);
    append(
        env,
        id,
        "assayer",
        checks(
            &sha,
            round,
            true,
            vec![CheckRun {
                command: "cargo test".into(),
                exit_code: 0,
                duration_ms: 3100,
                log: Some(format!("checks/r{round}.log")),
                cached: false,
            }],
        ),
    );
    append(
        env,
        id,
        "assayer",
        verdict(&sha, round, vec![], prior, open),
    );
    let repo = Repo::discover(&env.repo).unwrap();
    hallmark::stamp::strike(
        &repo,
        id.parse().unwrap(),
        &sha,
        round,
        Some("claude-opus-5-5".into()),
        "assayer",
        open,
    )
    .unwrap();
    sha
}

/// A hallmarked item after one approved round, with a minor riding along.
pub fn hallmarked_item(env: &Env, title: &str) -> (String, PathBuf, String) {
    hallmarked_item_with(env, title, None)
}

/// [`hallmarked_item`], its commit with `message`.
pub fn hallmarked_item_with(
    env: &Env,
    title: &str,
    message: Option<&str>,
) -> (String, PathBuf, String) {
    hallmarked_item_asking(env, title, message, None)
}

/// [`hallmarked_item_with`], the approving review asking for a person's
/// approval (`needs_person`).
pub fn hallmarked_item_asking(
    env: &Env,
    title: &str,
    message: Option<&str>,
    needs_person: Option<&str>,
) -> (String, PathBuf, String) {
    let (id, wt, _) = ready_item_with(env, title, message);
    let sha = git(&wt, &["rev-parse", "HEAD"]);
    append(env, &id, "assayer", checks(&sha, 1, true, vec![]));
    let mut v = verdict(
        &sha,
        1,
        vec![finding(
            "F1",
            1,
            Severity::Minor,
            Lens::Evidence,
            "No retry test",
        )],
        vec![],
        counts(0, 0, 1, 0),
    );
    if let Body::Verdict { needs_person: np, .. } = &mut v {
        *np = needs_person.map(String::from);
    }
    append(env, &id, "assayer", v);
    let repo = Repo::discover(&env.repo).unwrap();
    hallmark::stamp::strike(
        &repo,
        id.parse().unwrap(),
        &sha,
        1,
        Some("claude-opus-5-5".into()),
        "assayer",
        counts(0, 0, 1, 0),
    )
    .unwrap();
    (id, wt, sha)
}

/// A local bare remote named `origin` for the main checkout.
pub fn bare_remote(env: &Env) -> PathBuf {
    let remote = env.root.join("remote.git");
    git(
        &env.root,
        &["init", "-q", "--bare", &remote.to_string_lossy()],
    );
    git(
        &env.repo,
        &["remote", "add", "origin", &remote.to_string_lossy()],
    );
    remote
}

pub fn state(env: &Env, id: &str) -> String {
    let r = env.hall(&env.repo, &["status", id]);
    r.json["state"].as_str().unwrap_or_default().to_string()
}

/// The events of one kind in an item's journal.
pub fn events(env: &Env, id: &str, kind: &str) -> Vec<serde_json::Value> {
    env.journal(id)
        .into_iter()
        .filter(|e| e["event"] == kind)
        .collect()
}
