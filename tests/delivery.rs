//! ADR 0030: the Dispatch Dock rebases before it ships, and the Delivery
//! office watches each PR until it merges, rebasing it when it conflicts.
//! Everything goes to a local bare remote and a fake `gh`.
#![cfg(unix)]

mod c_support;
mod common;

use std::path::{Path, PathBuf};

use c_support::*;
use common::{Env, commit_file, git, p};
use serde_json::{Value, json};

const URL: &str = "https://github.com/acme/app/pull/1";

/// A fake `gh`: `pr view` prints `pr.json`, every call is logged.
fn fake_gh(env: &Env) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = env.root.join("gh-fake");
    std::fs::create_dir_all(&dir).unwrap();
    let d = p(&dir);
    let gh = dir.join("gh");
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{d}/calls.log'\ncase \"$1 $2\" in\n  'pr view') case \"$*\" in *--jq*) cat '{d}/state.txt' ;; *) cat '{d}/pr.json' ;; esac ;;\n  'pr edit'|'pr comment') echo '{URL}#issuecomment-9' ;;\n  *) exit 1 ;;\nesac\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    gh
}

fn pr(gh: &Path, state: &str, mergeable: &str, oid: &str) {
    let dir = gh.parent().unwrap();
    std::fs::write(dir.join("state.txt"), state).unwrap();
    let v = json!({"state": state, "mergeable": mergeable, "headRefOid": oid});
    std::fs::write(dir.join("pr.json"), v.to_string()).unwrap();
}

/// One pass of `worker`, with the fake `gh` and a fake runner script.
fn pass(env: &Env, gh: &Path, worker: &str, steps: Value) -> Value {
    let script = env.script(steps);
    let out = env
        .cmd(&env.repo)
        .env("HALL_GH", gh)
        .env("HALL_RUNNER", format!("fake:{}", script.display()))
        .args(["--json", "daemon", "run", "--once", "--only", worker])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v["steps"][0].clone()
}

/// A remote that has `main`, and a ship command that prints the PR URL.
fn remote_with_main(env: &Env) -> PathBuf {
    let remote = bare_remote(env);
    git(&env.repo, &["push", "-q", "origin", "main"]);
    set_config(
        env,
        "ship",
        &toml(&format!(
            "git push -q -f -u origin {{branch}} && echo {URL}"
        )),
    );
    remote
}

/// `main` moves on the remote with one commit.
fn main_moves(env: &Env, file: &str, body: &str) -> String {
    let sha = commit_file(&env.repo, file, body);
    git(&env.repo, &["push", "-q", "origin", "main"]);
    sha
}

fn parent(wt: &Path, rev: &str) -> String {
    git(wt, &["rev-parse", &format!("{rev}^")])
}

#[test]
fn a_clean_rebase_with_the_same_patch_carries_the_hallmark() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    let remote = remote_with_main(&env);
    let (id, wt, sha) = hallmarked_item(&env, "Retry webhooks");
    let moved = main_moves(&env, "other.rs", "fn other() {}\n");

    let step = pass(&env, &gh, "shipper", json!([]));
    assert!(step["what"].as_str().unwrap().contains("shipped"), "{step}");
    assert_eq!(state(&env, &id), "closed");

    // It shipped the rebase, on top of the moved main, never the old commit.
    let head = git(&wt, &["rev-parse", "HEAD"]);
    assert_ne!(head, sha);
    assert_eq!(parent(&wt, "HEAD"), moved);
    let branch = git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(
        git(&remote, &["rev-parse", &format!("refs/heads/{branch}")]),
        head
    );
    let rebased = events(&env, &id, "rebased");
    assert_eq!(rebased.len(), 1, "{rebased:?}");
    assert_eq!(rebased[0]["carried"], true);
    assert_eq!(rebased[0]["from"], sha.as_str());
    assert_eq!(rebased[0]["to"], head.as_str());
    assert_eq!(rebased[0]["onto"], "origin/main");
    assert_eq!(events(&env, &id, "shipped")[0]["sha"], head.as_str());
    // The hallmark moved with it, naming the approved commit.
    let note: Value = serde_json::from_str(&git(
        &wt,
        &["notes", "--ref", "refs/notes/hallmark", "show", &head],
    ))
    .unwrap();
    assert_eq!(note["rebased_from"], sha.as_str());
    let body = std::fs::read_to_string(
        env.repo
            .join(".git/hallmark/items")
            .join(&id)
            .join("pr-body.md"),
    )
    .unwrap();
    assert!(body.contains("Rebased onto `origin/main`"), "{body}");
}

#[test]
fn an_up_to_date_branch_ships_as_is() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    remote_with_main(&env);
    let (id, wt, sha) = hallmarked_item(&env, "Retry webhooks");
    pass(&env, &gh, "shipper", json!([]));
    assert_eq!(state(&env, &id), "closed");
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), sha);
    assert!(events(&env, &id, "rebased").is_empty());
}

#[test]
fn a_rebase_that_changes_the_patch_goes_back_to_review() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    remote_with_main(&env);
    // A file both sides touch, far enough apart not to conflict but close
    // enough that the diff's context changes.
    let lines: Vec<String> = (1..=12).map(|i| format!("line {i}")).collect();
    commit_file(&env.repo, "list.txt", &(lines.join("\n") + "\n"));
    git(&env.repo, &["push", "-q", "origin", "main"]);
    let (id, wt) = env.new_item("Edit the list");
    let mut mine = lines.clone();
    mine[4] = "line 5, mine".into();
    commit_file(&wt, "list.txt", &(mine.join("\n") + "\n"));
    assert_eq!(env.hall(&wt, &["ready"]).code, 0);
    let sha = git(&wt, &["rev-parse", "HEAD"]);
    approve(&env, &id, &wt, 1, vec![], counts(0, 0, 0, 0));
    let mut theirs = lines.clone();
    theirs[7] = "line 8, theirs".into();
    main_moves(&env, "list.txt", &(theirs.join("\n") + "\n"));

    let step = pass(&env, &gh, "shipper", json!([]));
    assert!(
        step["what"].as_str().unwrap().contains("back to review"),
        "{step}"
    );
    assert_eq!(state(&env, &id), "checks");
    let rebased = events(&env, &id, "rebased");
    assert_eq!(rebased[0]["carried"], false);
    let head = git(&wt, &["rev-parse", "HEAD"]);
    assert_ne!(head, sha);
    let ready = events(&env, &id, "ready");
    assert_eq!(ready.last().unwrap()["sha"], head.as_str());
    assert_eq!(ready.last().unwrap()["round"], 2);
    assert!(events(&env, &id, "shipped").is_empty());
}

#[test]
fn gates_that_fail_after_the_rebase_send_it_back_to_review() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    remote_with_main(&env);
    set_config(&env, "gates", "[\"test ! -f boom\"]");
    let (id, _, _) = hallmarked_item(&env, "Retry webhooks");
    main_moves(&env, "boom", "x\n");

    pass(&env, &gh, "shipper", json!([]));
    assert_eq!(state(&env, &id), "checks");
    let rebased = events(&env, &id, "rebased");
    assert_eq!(rebased[0]["carried"], false);
    assert_eq!(rebased[0]["checks"][0]["exit_code"], 1);
    assert!(
        rebased[0]["reason"]
            .as_str()
            .unwrap()
            .contains("gates failed")
    );
}

/// Main changes `retry.rs`, which the item changed too.
fn conflicting_main(env: &Env) {
    main_moves(env, "retry.rs", "fn retry() {\n    log();\n}\n");
}

/// The fake dev run: resolves `retry.rs` and finishes the rebase.
fn resolving_run() -> Value {
    json!([{
        "role": "fixer",
        "files": {"retry.rs": "fn retry() {\n    log();\n    deliver();\n}\n"},
        "run": ["git add retry.rs", "GIT_EDITOR=true git rebase --continue"],
        "result": "resolved"
    }])
}

#[test]
fn a_conflict_at_the_dock_goes_to_the_delivery_office_and_back_to_review() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    remote_with_main(&env);
    let (id, wt, sha) = hallmarked_item(&env, "Retry webhooks");
    conflicting_main(&env);

    let step = pass(&env, &gh, "shipper", json!([]));
    assert!(
        step["what"].as_str().unwrap().contains("conflicts"),
        "{step}"
    );
    assert_eq!(state(&env, &id), "development");
    // The rebase was undone: the worktree is clean, at the hallmark.
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), sha);
    let req = events(&env, &id, "rebase_requested");
    assert_eq!(req[0]["onto"], "main");
    assert_eq!(req[0]["files"], json!(["retry.rs"]));
    let status = env.hall(&env.repo, &["status", &id]);
    assert_eq!(status.json["rebase"]["onto"], "main", "{}", status.stdout);

    // The Delivery office: a dev run resolves it, a script checks, and the
    // item goes to review.
    let step = pass(&env, &gh, "delivery", resolving_run());
    assert!(
        step["what"].as_str().unwrap().contains("sent to review"),
        "{step}"
    );
    assert_eq!(state(&env, &id), "checks");
    let head = git(&wt, &["rev-parse", "HEAD"]);
    assert!(git(&wt, &["merge-base", "--is-ancestor", "origin/main", &head]).is_empty());
    assert_eq!(
        std::fs::read_to_string(wt.join("retry.rs")).unwrap(),
        "fn retry() {\n    log();\n    deliver();\n}\n"
    );
    let ready = events(&env, &id, "ready");
    let last = ready.last().unwrap();
    assert_eq!(last["actor"], "delivery");
    assert_eq!(last["sha"], head.as_str());
    assert_eq!(last["note"], "rebased onto origin/main");
    // The run was told what to do, and can't push.
    let calls = env.fake_calls();
    let call = calls.last().unwrap().to_string();
    assert!(call.contains("retry.rs"), "{call}");
    assert!(!call.contains("git push"), "{call}");
}

#[test]
fn a_rebase_the_run_cant_finish_is_retried_once_then_held() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    remote_with_main(&env);
    let (id, wt, sha) = hallmarked_item(&env, "Retry webhooks");
    conflicting_main(&env);
    pass(&env, &gh, "shipper", json!([]));
    assert_eq!(state(&env, &id), "development");

    let gives_up = json!([{"role": "fixer", "result": "could not"}]);
    let step = pass(&env, &gh, "delivery", gives_up.clone());
    assert!(
        step["what"].as_str().unwrap().contains("attempt 1"),
        "{step}"
    );
    // Undone, so the next try starts clean.
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), sha);
    assert!(git(&wt, &["status", "--porcelain", "--untracked-files=no"]).is_empty());
    let step = pass(&env, &gh, "delivery", gives_up);
    assert!(step["what"].as_str().unwrap().contains("held"), "{step}");
    let held = events(&env, &id, "held");
    assert_eq!(held.last().unwrap()["kind"], "conflict");
    let inbox = env.hall(&env.repo, &["inbox"]);
    assert_eq!(
        inbox.json["entries"][0]["kind"], "conflict",
        "{}",
        inbox.stdout
    );
    // Held: the office leaves it alone until you retry.
    assert_eq!(pass(&env, &gh, "delivery", json!([]))["progress"], "idle");
    assert_eq!(
        env.hall(&env.repo, &["retry", &id, "--reason", "try again"])
            .code,
        0
    );
    let step = pass(&env, &gh, "delivery", resolving_run());
    assert!(
        step["what"].as_str().unwrap().contains("sent to review"),
        "{step}"
    );
}

#[test]
fn the_delivery_office_watches_a_pr_until_it_merges() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    remote_with_main(&env);
    let (id, _, sha) = hallmarked_item(&env, "Retry webhooks");
    pass(&env, &gh, "shipper", json!([]));
    assert_eq!(state(&env, &id), "closed");

    pr(&gh, "OPEN", "MERGEABLE", &sha);
    assert_eq!(pass(&env, &gh, "delivery", json!([]))["progress"], "idle");
    assert!(events(&env, &id, "delivered").is_empty());

    // Read at most every two minutes: forget the last read.
    std::fs::remove_file(env.repo.join(".git/hallmark/delivery-watch.json")).unwrap();
    pr(&gh, "MERGED", "UNKNOWN", &sha);
    let step = pass(&env, &gh, "delivery", json!([]));
    assert_eq!(
        step["what"],
        format!("{id} delivered: {URL} merged"),
        "{step}"
    );
    assert_eq!(events(&env, &id, "delivered")[0]["outcome"], "merged");
    std::fs::remove_file(env.repo.join(".git/hallmark/delivery-watch.json")).unwrap();
    assert_eq!(pass(&env, &gh, "delivery", json!([]))["progress"], "idle");
    let status = env.hall(&env.repo, &["status", &id]);
    assert_eq!(status.json["delivered"], "merged", "{}", status.stdout);
}

#[test]
fn a_pr_that_conflicts_is_reopened_rebased_and_reviewed_again() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    remote_with_main(&env);
    let (id, wt, sha) = hallmarked_item(&env, "Retry webhooks");
    pass(&env, &gh, "shipper", json!([]));
    assert_eq!(state(&env, &id), "closed");
    conflicting_main(&env);

    pr(&gh, "OPEN", "CONFLICTING", &sha);
    let step = pass(&env, &gh, "delivery", json!([]));
    assert!(
        step["what"].as_str().unwrap().contains("reopened"),
        "{step}"
    );
    assert_eq!(state(&env, &id), "development");
    let reopened = events(&env, &id, "reopened");
    assert_eq!(reopened[0]["actor"], "delivery");
    assert_eq!(reopened[0]["pr_head"], sha.as_str());
    assert_eq!(events(&env, &id, "rebase_requested").len(), 1);

    // The next pass rebases it with a dev run and sends it to review; once
    // hallmarked again, the shipper updates the same PR.
    let step = pass(&env, &gh, "delivery", resolving_run());
    assert!(
        step["what"].as_str().unwrap().contains("sent to review"),
        "{step}"
    );
    assert_eq!(state(&env, &id), "checks");
    assert_ne!(git(&wt, &["rev-parse", "HEAD"]), sha);
}

#[test]
fn a_conflicting_pr_someone_else_pushed_to_is_left_for_you() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    remote_with_main(&env);
    let (id, _, _) = hallmarked_item(&env, "Retry webhooks");
    pass(&env, &gh, "shipper", json!([]));
    pr(
        &gh,
        "OPEN",
        "CONFLICTING",
        "0123456789abcdef0123456789abcdef01234567",
    );
    assert_eq!(pass(&env, &gh, "delivery", json!([]))["progress"], "idle");
    assert_eq!(state(&env, &id), "closed");
}
