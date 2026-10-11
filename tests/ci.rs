//! The CI watch after shipping (ADR 0025), and follow-ups that carry the
//! failing checks and stack on an open PR. `gh` is a fake shell script, so
//! these run on Unix only; `src/ci.rs` and `src/gh.rs` unit-test the logic
//! everywhere.

#![cfg(unix)]

mod c_support;
mod common;

use std::path::{Path, PathBuf};

use c_support::*;
use common::{Env, git, p};
use hallmark::event::Body;
use serde_json::{Value, json};

const URL: &str = "https://github.com/acme/app/pull/1";

/// A fake `gh`: `pr checks`, `pr view`, `run view` and `api` answer from
/// files the test writes.
fn fake_gh(env: &Env) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = env.root.join("gh-fake");
    std::fs::create_dir_all(&dir).unwrap();
    let d = p(&dir);
    let gh = dir.join("gh");
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\ncase \"$1 $2\" in\n  'pr checks') cat '{d}/checks.json' ;;\n  'pr view') case \"$*\" in *--jq*) cat '{d}/state.txt' ;; *) cat '{d}/pr.json' ;; esac ;;\n  'run view') cat '{d}/log.txt' ;;\n  api*) case \"$2\" in *actions/jobs/*) cat '{d}/job.json' 2>/dev/null || echo '[]' ;; *annotations) cat '{d}/annotations.json' 2>/dev/null || echo '[]' ;; *) echo '[]' ;; esac ;;\n  *) exit 1 ;;\nesac\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(dir.join("log.txt"), "test (ubuntu-latest)\tRun cargo test\t2026-10-10T02:41:18.3Z thread 'x' panicked at tests/setup.rs:405:14:\ntest (ubuntu-latest)\tRun cargo test\t2026-10-10T02:41:18.4Z Text file busy\n").unwrap();
    gh
}

fn checks(gh: &Path, list: Value) {
    std::fs::write(gh.parent().unwrap().join("checks.json"), list.to_string()).unwrap();
}

fn pr(gh: &Path, state: &str, head: &str, oid: &str) {
    std::fs::write(gh.parent().unwrap().join("state.txt"), state).unwrap();
    let v = json!({"headRefOid": oid, "isCrossRepository": false,"number": 1, "url": URL, "title": "Put hall on PATH", "body": "",
                   "state": state, "headRefName": head, "baseRefName": "main",
                   "reviews": [], "comments": []});
    std::fs::write(gh.parent().unwrap().join("pr.json"), v.to_string()).unwrap();
}

fn ci_pass(env: &Env, gh: &Path) -> Value {
    let out = env
        .cmd(&env.repo)
        .env("HALL_GH", gh)
        .args(["--json", "daemon", "run", "--once", "--only", "ci"])
        .output()
        .unwrap();
    serde_json::from_slice(&out.stdout).unwrap()
}

/// A shipped (closed) item with a PR.
fn shipped(env: &Env) -> (String, String) {
    let (id, _wt, sha) = hallmarked_item(env, "Put hall on PATH");
    append(
        env,
        &id,
        "shipper",
        Body::Shipped {
            sha: sha.clone(),
            pr_url: Some(URL.into()),
        },
    );
    (id, sha)
}

#[test]
fn a_red_check_lands_in_the_inbox_until_a_follow_up_takes_it() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    let (id, _) = shipped(&env);
    pr(&gh, "OPEN", "x", "");

    // Still running: nothing is recorded.
    checks(
        &gh,
        json!([{"name": "fmt", "bucket": "pass"},
                       {"name": "test (ubuntu-latest)", "bucket": "pending"}]),
    );
    let r = ci_pass(&env, &gh);
    assert_eq!(r["steps"][0]["progress"], "idle", "{r}");
    assert!(events(&env, &id, "pr_checks").is_empty());

    // Red: recorded once, and in the inbox.
    checks(
        &gh,
        json!([{"name": "fmt", "bucket": "pass"},
                       {"name": "test (ubuntu-latest)", "bucket": "fail",
                        "link": "https://github.com/acme/app/actions/runs/7/job/9"}]),
    );
    std::fs::remove_file(env.repo.join(".git/hallmark/ci-watch.json")).unwrap();
    let r = ci_pass(&env, &gh);
    assert_eq!(r["steps"][0]["progress"], "worked", "{r}");
    let recorded = events(&env, &id, "pr_checks");
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["passed"], false);
    assert_eq!(recorded[0]["failing"][0]["name"], "test (ubuntu-latest)");
    // Still red on an open PR: nothing new is recorded.
    std::fs::remove_file(env.repo.join(".git/hallmark/ci-watch.json")).unwrap();
    assert_eq!(ci_pass(&env, &gh)["steps"][0]["progress"], "idle");
    assert_eq!(events(&env, &id, "pr_checks").len(), 1);

    // On an open PR it is the Delivery office's to fix: a notice, never in
    // your office.
    assert_eq!(ci_entries(&env), 0);
    let notices = env.hall(&env.repo, &["inbox", "--notices"]);
    let entry = notices.json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "ci_failed")
        .cloned()
        .unwrap_or_else(|| panic!("a ci_failed notice: {}", notices.stdout));
    assert_eq!(entry["item"], id.as_str());
    assert_eq!(
        entry["answer_with"][0],
        format!("hall new --follow-up {URL}")
    );

    // The follow-up carries the failing check and its log, and takes the
    // entry out of the inbox. The PR is merged, so it starts from main.
    pr(&gh, "MERGED", "hm/H-0001-put-hall-on-path", "");
    let out = env
        .cmd(&env.repo)
        .env("HALL_GH", &gh)
        .args(["--json", "new", "--follow-up", URL])
        .output()
        .unwrap();
    let j: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(0), "{j}");
    let follow = j["id"].as_str().unwrap().to_string();
    let created = &env.journal(&follow)[0];
    let req = created["request"].as_str().unwrap();
    assert!(
        req.contains("Failing CI checks:\n- test (ubuntu-latest)"),
        "{req}"
    );
    assert!(req.contains("Text file busy"), "{req}");
    assert_eq!(created["base"], "main");
    let inbox = env.hall(&env.repo, &["inbox"]);
    assert!(
        !inbox.json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "ci_failed"),
        "{}",
        inbox.stdout
    );
}

#[test]
fn a_follow_up_to_an_open_pr_stacks_on_its_branch() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    let (id, sha) = shipped(&env);
    let branch = env.journal(&id)[0]["branch"].as_str().unwrap().to_string();
    checks(&gh, json!([]));
    pr(&gh, "OPEN", &branch, &sha);
    let out = env
        .cmd(&env.repo)
        .env("HALL_GH", &gh)
        .args(["--json", "new", "--follow-up", URL])
        .output()
        .unwrap();
    let j: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(0), "{j}");
    // It starts from the PR's branch and is reviewed against it.
    let wt = PathBuf::from(j["worktree"].as_str().unwrap());
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), sha);
    assert_eq!(j["base"], branch.as_str());
    let created = &env.journal(j["id"].as_str().unwrap())[0];
    assert_eq!(created["base"], branch.as_str());
    assert!(created["request"].as_str().unwrap().contains(&format!(
        "starts from its branch, {branch}, and ships into it"
    )));
}

#[test]
fn a_failed_pr_that_turns_green_clears_the_inbox() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    let (id, _) = shipped(&env);
    pr(&gh, "OPEN", "x", "");
    checks(&gh, json!([{"name": "test", "bucket": "fail"}]));
    assert_eq!(ci_pass(&env, &gh)["steps"][0]["progress"], "worked");
    // Re-run, green: the watch notices, and the entry is gone.
    checks(&gh, json!([{"name": "test", "bucket": "pass"}]));
    std::fs::remove_file(env.repo.join(".git/hallmark/ci-watch.json")).unwrap();
    assert_eq!(ci_pass(&env, &gh)["steps"][0]["progress"], "worked");
    assert_eq!(events(&env, &id, "pr_checks").len(), 2);
    let inbox = env.hall(&env.repo, &["inbox"]);
    assert!(
        !inbox.json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "ci_failed")
    );
}

#[test]
fn a_pr_whose_head_moved_is_not_stacked_on() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    let (id, _) = shipped(&env);
    let branch = env.journal(&id)[0]["branch"].as_str().unwrap().to_string();
    checks(&gh, json!([]));
    // GitHub's head is not the commit that was hallmarked.
    pr(
        &gh,
        "OPEN",
        &branch,
        "0000000000000000000000000000000000000000",
    );
    let out = env
        .cmd(&env.repo)
        .env("HALL_GH", &gh)
        .args(["--json", "new", "--follow-up", URL])
        .output()
        .unwrap();
    let j: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(0), "{j}");
    assert_eq!(j["base"], "main");
}

/// `ci_failed` entries in your office.
fn ci_entries(env: &Env) -> usize {
    ci_in(env, &["inbox"])
}

/// `ci_failed` notices: the loop is on it, or it's news.
fn ci_notices(env: &Env) -> usize {
    ci_in(env, &["inbox", "--notices"])
}

fn ci_in(env: &Env, args: &[&str]) -> usize {
    env.hall(&env.repo, args).json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "ci_failed")
        .count()
}

#[test]
fn a_pr_merged_with_red_checks_keeps_its_ci_failed_notice() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    let (id, _) = shipped(&env);
    pr(&gh, "OPEN", "x", "");
    checks(
        &gh,
        json!([{"name": "test (ubuntu-latest)", "bucket": "fail"}]),
    );
    assert_eq!(ci_pass(&env, &gh)["steps"][0]["progress"], "worked");
    pr(&gh, "MERGED", "x", "");
    std::fs::remove_file(env.repo.join(".git/hallmark/ci-watch.json")).unwrap();
    assert_eq!(ci_pass(&env, &gh)["steps"][0]["progress"], "worked");
    let recorded = events(&env, &id, "pr_checks");
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[1]["passed"], false);
    assert_eq!(recorded[1]["ended"], "merged");
    // Merged: nothing left for the Delivery office, and news for you.
    assert_eq!(ci_entries(&env), 0);
    assert_eq!(ci_notices(&env), 1);
    // The watch stops there.
    std::fs::remove_file(env.repo.join(".git/hallmark/ci-watch.json")).unwrap();
    assert_eq!(ci_pass(&env, &gh)["steps"][0]["progress"], "idle");
    assert_eq!(events(&env, &id, "pr_checks").len(), 2);
}

#[test]
fn a_pr_closed_with_red_checks_leaves_the_inbox_without_passing() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    let (id, _) = shipped(&env);
    pr(&gh, "OPEN", "x", "");
    checks(&gh, json!([{"name": "test", "bucket": "fail"}]));
    assert_eq!(ci_pass(&env, &gh)["steps"][0]["progress"], "worked");
    assert_eq!(ci_notices(&env), 1);
    pr(&gh, "CLOSED", "x", "");
    std::fs::remove_file(env.repo.join(".git/hallmark/ci-watch.json")).unwrap();
    assert_eq!(ci_pass(&env, &gh)["steps"][0]["progress"], "worked");
    let recorded = events(&env, &id, "pr_checks");
    assert_eq!(recorded[1]["passed"], false);
    assert_eq!(recorded[1]["ended"], "closed");
    assert_eq!(ci_entries(&env), 0);
    assert_eq!(ci_notices(&env), 0);
}

#[test]
fn a_check_that_failed_twice_is_named_once() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    shipped(&env);
    pr(&gh, "OPEN", "x", "");
    // The same workflow ran for the push and for the pull request.
    checks(
        &gh,
        json!([{"name": "fmt", "bucket": "fail"}, {"name": "clippy", "bucket": "fail"},
               {"name": "fmt", "bucket": "fail"}, {"name": "clippy", "bucket": "fail"}]),
    );
    assert_eq!(ci_pass(&env, &gh)["steps"][0]["progress"], "worked");
    let notices = env.hall(&env.repo, &["inbox", "--notices"]);
    let text = notices.json["entries"][0]["text"].as_str().unwrap();
    assert!(
        text.ends_with(&format!(
            "CI failed on {URL}: fmt, clippy; the Delivery office fixes it"
        )),
        "{text}"
    );
}

#[test]
fn a_knowledge_items_red_checks_never_wait_for_you() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    let (id, _wt, sha) = hallmarked_item(&env, "Library: learnings from H-0009");
    append(
        &env,
        &id,
        "shipper",
        Body::Shipped {
            sha,
            pr_url: Some(URL.into()),
        },
    );
    pr(&gh, "OPEN", "x", "");
    checks(&gh, json!([{"name": "test", "bucket": "fail"}]));
    assert_eq!(ci_pass(&env, &gh)["steps"][0]["progress"], "worked");
    assert_eq!(events(&env, &id, "pr_checks").len(), 1, "still recorded");
    assert_eq!(ci_entries(&env), 0);
}

#[test]
fn ci_that_never_started_is_one_entry_and_nothing_to_fix() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    let (id, _) = shipped(&env);
    pr(&gh, "OPEN", "x", "");
    // GitHub never started the jobs: no steps, no runner, and a reason.
    let dir = gh.parent().unwrap();
    std::fs::write(
        dir.join("job.json"),
        json!({"steps": [], "runner_name": "", "conclusion": "failure"}).to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("annotations.json"),
        json!([{"annotation_level": "failure",
                "message": "The job was not started because your spending limit needs to be increased."}])
        .to_string(),
    )
    .unwrap();
    checks(
        &gh,
        json!([{"name": "fmt", "bucket": "fail", "link": "https://github.com/acme/app/actions/runs/7/job/9"},
               {"name": "test", "bucket": "fail", "link": "https://github.com/acme/app/actions/runs/7/job/10"}]),
    );
    let r = ci_pass(&env, &gh);
    assert!(
        r["steps"][0]["what"]
            .as_str()
            .unwrap()
            .contains("CI never ran"),
        "{r}"
    );
    let recorded = events(&env, &id, "pr_checks");
    assert!(
        recorded[0]["not_started"]
            .as_str()
            .unwrap()
            .contains("spending limit")
    );
    // Not a code failure: no ci_failed anywhere, and one ci_down for you.
    assert_eq!(ci_entries(&env), 0);
    assert_eq!(ci_notices(&env), 0);
    let inbox = env.hall(&env.repo, &["inbox"]);
    let entries = inbox.json["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{}", inbox.stdout);
    assert_eq!(entries[0]["kind"], "ci_down");
    let text = entries[0]["text"].as_str().unwrap();
    assert!(
        text.contains("#1") && text.contains("spending limit"),
        "{text}"
    );
}

#[test]
fn an_old_red_record_of_an_outage_joins_ci_down() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    let (id, _) = shipped(&env);
    let job = "https://github.com/acme/app/actions/runs/7/job/9";
    // Recorded before `not_started` existed.
    append(
        &env,
        &id,
        "ci",
        Body::PrChecks {
            pr_url: URL.into(),
            passed: false,
            failing: vec![hallmark::event::FailingCheck {
                name: "fmt".into(),
                link: Some(job.into()),
            }],
            ended: None,
            not_started: None,
        },
    );
    pr(&gh, "OPEN", "x", "");
    let dir = gh.parent().unwrap();
    std::fs::write(
        dir.join("job.json"),
        json!({"steps": [], "runner_name": "", "conclusion": "failure"}).to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("annotations.json"),
        json!([{"annotation_level": "failure",
                "message": "The job was not started because your spending limit needs to be increased."}])
        .to_string(),
    )
    .unwrap();
    checks(&gh, json!([{"name": "fmt", "bucket": "fail", "link": job}]));
    ci_pass(&env, &gh);
    let recorded = events(&env, &id, "pr_checks");
    assert_eq!(recorded.len(), 2, "recorded again");
    assert!(recorded[1]["not_started"].is_string(), "{recorded:?}");
    let inbox = env.hall(&env.repo, &["inbox"]);
    assert_eq!(
        inbox.json["entries"][0]["kind"], "ci_down",
        "{}",
        inbox.stdout
    );
}
