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
            "#!/bin/sh\ncase \"$1 $2\" in\n  'pr checks') cat '{d}/checks.json' ;;\n  'pr view') cat '{d}/pr.json' ;;\n  'run view') cat '{d}/log.txt' ;;\n  api*) echo '[]' ;;\n  *) exit 1 ;;\nesac\n"
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

fn pr(gh: &Path, state: &str, head: &str) {
    let v = json!({"number": 1, "url": URL, "title": "Put hall on PATH", "body": "",
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
    // Concluded: the watch is over for this item.
    std::fs::remove_file(env.repo.join(".git/hallmark/ci-watch.json")).unwrap();
    assert_eq!(ci_pass(&env, &gh)["steps"][0]["progress"], "idle");

    let inbox = env.hall(&env.repo, &["inbox"]);
    let entry = inbox.json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "ci_failed")
        .cloned()
        .expect("a ci_failed entry");
    assert_eq!(entry["item"], id.as_str());
    assert_eq!(
        entry["answer_with"][0],
        format!("hall new --follow-up {URL}")
    );

    // The follow-up carries the failing check and its log, and takes the
    // entry out of the inbox. The PR is merged, so it starts from main.
    pr(&gh, "MERGED", "hm/H-0001-put-hall-on-path");
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
    pr(&gh, "OPEN", &branch);
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
