//! `hall adopt` on open PRs (ADR 0029): a shipped item comes back to
//! development through its open PR, any other open PR can be taken over,
//! and the shipper updates the PR instead of opening a new one. `gh` is a
//! fake shell script, so these run on Unix only.

#![cfg(unix)]

mod c_support;
mod common;

use std::path::{Path, PathBuf};

use c_support::*;
use common::{Env, commit_file, git, p};
use serde_json::{Value, json};

const URL: &str = "https://github.com/acme/app/pull/1";

/// A fake `gh`: `pr view` answers from files the test writes; `pr edit` and
/// `pr comment` succeed; every call is logged to `calls.log`.
fn fake_gh(env: &Env) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = env.root.join("gh-fake");
    std::fs::create_dir_all(&dir).unwrap();
    let d = p(&dir);
    let gh = dir.join("gh");
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{d}/calls.log'\ncase \"$1 $2\" in\n  'pr view') case \"$*\" in *--jq*) cat '{d}/state.txt' ;; *) cat '{d}/pr.json' ;; esac ;;\n  'pr edit'|'pr comment') echo '{URL}#issuecomment-9' ;;\n  api*) echo '[]' ;;\n  *) exit 1 ;;\nesac\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    gh
}

fn pr(gh: &Path, state: &str, head: &str, oid: &str) {
    let dir = gh.parent().unwrap();
    std::fs::write(dir.join("state.txt"), state).unwrap();
    let v = json!({"headRefOid": oid, "isCrossRepository": false, "number": 1, "url": URL,
                   "title": "Retry webhooks", "body": "Retries failed deliveries.",
                   "state": state, "headRefName": head, "baseRefName": "main",
                   "reviews": [], "comments": []});
    std::fs::write(dir.join("pr.json"), v.to_string()).unwrap();
}

fn gh_calls(gh: &Path) -> String {
    std::fs::read_to_string(gh.parent().unwrap().join("calls.log")).unwrap_or_default()
}

/// `hall <args>` with the fake `gh`.
fn hall_gh(env: &Env, gh: &Path, dir: &Path, args: &[&str]) -> (i32, Value) {
    let out = env
        .cmd(dir)
        .env("HALL_GH", gh)
        .arg("--json")
        .args(args)
        .output()
        .unwrap();
    let json = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    (out.status.code().unwrap_or(-1), json)
}

/// One shipper pass with the fake `gh`.
fn ship_pass(env: &Env, gh: &Path) -> Value {
    let script = env.script(json!([]));
    let out = env
        .cmd(&env.repo)
        .env("HALL_GH", gh)
        .env("HALL_RUNNER", format!("fake:{}", script.display()))
        .args(["--json", "daemon", "run", "--once", "--only", "shipper"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v["steps"][0].clone()
}

fn branch_of(wt: &Path) -> String {
    git(wt, &["rev-parse", "--abbrev-ref", "HEAD"])
}

/// A hallmarked item shipped to a local remote as PR #1.
fn shipped(env: &Env, gh: &Path) -> (String, PathBuf, String, PathBuf) {
    let remote = bare_remote(env);
    set_config(
        env,
        "ship",
        &toml(&format!("git push -q -u origin {{branch}} && echo {URL}")),
    );
    let (id, wt, sha) = hallmarked_item(env, "Retry webhooks");
    let step = ship_pass(env, gh);
    assert!(step["what"].as_str().unwrap().contains("shipped"), "{step}");
    assert_eq!(state(env, &id), "closed");
    (id, wt, sha, remote)
}

#[test]
fn adopt_reopens_a_shipped_item_whose_pr_is_open() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    let (id, wt, sha, _) = shipped(&env, &gh);
    pr(&gh, "OPEN", &branch_of(&wt), &sha);

    let (code, r) = hall_gh(&env, &gh, &wt, &["adopt"]);
    assert_eq!(code, 0, "{r}");
    assert_eq!(r["id"], id.as_str());
    assert_eq!(r["reopened"], true);
    assert_eq!(r["created"], false);
    assert_eq!(r["state"], "development");
    assert_eq!(r["pr_url"], URL);
    let reopened = events(&env, &id, "reopened");
    assert_eq!(reopened.len(), 1);
    assert_eq!(reopened[0]["pr_head"], sha.as_str());
    assert_eq!(reopened[0]["base"], "main");

    // Asking again returns the open item; nothing is written twice.
    let (code, again) = hall_gh(&env, &gh, &wt, &["adopt"]);
    assert_eq!(code, 0, "{again}");
    assert!(again.get("reopened").is_none(), "{again}");
    assert_eq!(again["created"], false);
    assert_eq!(events(&env, &id, "reopened").len(), 1);
}

#[test]
fn a_merged_pr_is_history_and_starts_a_new_item() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    let (id, wt, sha, _) = shipped(&env, &gh);
    pr(&gh, "MERGED", &branch_of(&wt), &sha);

    let (code, r) = hall_gh(&env, &gh, &wt, &["adopt"]);
    assert_eq!(code, 0, "{r}");
    assert_eq!(r["created"], true);
    assert_ne!(r["id"], id.as_str());
    assert!(r.get("pr_url").is_none(), "{r}");
    assert_eq!(state(&env, &id), "closed");
    // `--pr` says why, and what to do instead.
    let (code, e) = hall_gh(&env, &gh, &env.repo, &["adopt", "--pr", "1"]);
    assert_eq!(code, 4, "{e}");
    assert_eq!(e["error"]["code"], "WRONG_STATE");
    assert!(
        e["error"]["next"].to_string().contains("--follow-up"),
        "{e}"
    );
}

#[test]
fn a_rebased_item_updates_its_pr_under_a_lease() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    let (id, wt, old, remote) = shipped(&env, &gh);
    let branch = branch_of(&wt);
    pr(&gh, "OPEN", &branch, &old);

    // main moves on; the PR is rebased onto it.
    commit_file(&env.repo, "other.rs", "fn other() {}\n");
    let (code, r) = hall_gh(&env, &gh, &wt, &["adopt"]);
    assert_eq!(code, 0, "{r}");
    git(&wt, &["rebase", "-q", "main"]);
    let new = git(&wt, &["rev-parse", "HEAD"]);
    assert_ne!(new, old);
    // The rebased branch no longer has the PR's head; adopt says so.
    let (_, again) = hall_gh(&env, &gh, &wt, &["adopt"]);
    assert!(
        again["next"][0].as_str().unwrap().contains("replaces"),
        "{again}"
    );

    // Back through review, then the shipper updates the PR.
    let (code, ready) = hall_gh(&env, &gh, &wt, &["ready"]);
    assert_eq!(code, 0, "{ready}");
    approve(&env, &id, &wt, 2, vec![], counts(0, 0, 1, 0));
    let step = ship_pass(&env, &gh);
    assert!(step["what"].as_str().unwrap().contains("shipped"), "{step}");
    assert_eq!(state(&env, &id), "closed");
    assert_eq!(
        git(&remote, &["rev-parse", &format!("refs/heads/{branch}")]),
        new
    );
    let shipped = events(&env, &id, "shipped");
    assert_eq!(shipped.len(), 2);
    assert_eq!(shipped[1]["sha"], new.as_str());
    assert_eq!(
        shipped[1]["pr_url"], URL,
        "the PR it updated, not the comment"
    );
    let calls = gh_calls(&gh);
    assert!(
        calls.contains(&format!("pr edit {URL} --base main")),
        "{calls}"
    );
    assert!(
        calls.contains(&format!("pr comment {URL} --body-file")),
        "{calls}"
    );
    assert!(!calls.contains("pr create"), "{calls}");
}

#[test]
fn adopt_accepts_a_branch_rebased_before_adopting() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    let (id, wt, old, _) = shipped(&env, &gh);
    pr(&gh, "OPEN", &branch_of(&wt), &old);

    commit_file(&env.repo, "other.rs", "fn other() {}\n");
    git(&wt, &["rebase", "-q", "main"]);
    let new = git(&wt, &["rev-parse", "HEAD"]);
    assert_ne!(new, old);

    // The PR is still at the commit the item shipped: the rebase replaces it.
    let (code, r) = hall_gh(&env, &gh, &wt, &["adopt"]);
    assert_eq!(code, 0, "{r}");
    assert_eq!(r["reopened"], true);
    assert_eq!(state(&env, &id), "development");
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), new);
}

#[test]
fn an_update_never_overwrites_a_pr_that_moved() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    let (id, wt, old, remote) = shipped(&env, &gh);
    let branch = branch_of(&wt);
    pr(&gh, "OPEN", &branch, &old);
    let (code, r) = hall_gh(&env, &gh, &wt, &["adopt"]);
    assert_eq!(code, 0, "{r}");
    commit_file(
        &wt,
        "retry.rs",
        "fn retry() {\n    lock();\n    deliver();\n}\n",
    );

    // Someone pushes to the PR after it was reopened.
    let other = env.root.join("other-clone");
    git(&env.root, &["clone", "-q", &p(&remote), &p(&other)]);
    git(&other, &["switch", "-q", &branch]);
    let theirs = commit_file(&other, "theirs.rs", "fn theirs() {}\n");
    git(&other, &["push", "-q", "origin", &branch]);

    let (code, ready) = hall_gh(&env, &gh, &wt, &["ready"]);
    assert_eq!(code, 0, "{ready}");
    approve(&env, &id, &wt, 2, vec![], counts(0, 0, 1, 0));
    let step = ship_pass(&env, &gh);
    assert!(step["what"].as_str().unwrap().contains("held"), "{step}");
    let held = events(&env, &id, "held");
    assert!(
        held[0]["reason"]
            .as_str()
            .unwrap()
            .starts_with("the update command"),
        "{held:?}"
    );
    assert_eq!(state(&env, &id), "hallmarked");
    // Their commit is still the PR's head.
    assert_eq!(
        git(&remote, &["rev-parse", &format!("refs/heads/{branch}")]),
        theirs
    );
}

#[test]
fn adopt_pr_takes_over_a_pr_hallmark_did_not_open() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    bare_remote(&env);
    // A teammate's branch, on the remote only.
    git(&env.repo, &["switch", "-q", "-c", "fix-typo"]);
    let oid = commit_file(&env.repo, "README.md", "typo fixed\n");
    git(&env.repo, &["push", "-q", "origin", "fix-typo"]);
    git(&env.repo, &["switch", "-q", "main"]);
    git(&env.repo, &["branch", "-q", "-D", "fix-typo"]);
    pr(&gh, "OPEN", "fix-typo", &oid);

    let (code, r) = hall_gh(&env, &gh, &env.repo, &["adopt", "--pr", "1"]);
    assert_eq!(code, 0, "{r}");
    assert_eq!(r["created"], true);
    assert_eq!(r["branch"], "fix-typo");
    assert_eq!(r["base"], "main");
    assert_eq!(r["pr_url"], URL);
    let wt = PathBuf::from(r["worktree"].as_str().unwrap());
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), oid);
    let id = r["id"].as_str().unwrap().to_string();
    let created = &env.journal(&id)[0];
    assert_eq!(created["adopted"], true);
    assert_eq!(created["pr_url"], URL);
    assert_eq!(created["pr_head"], oid.as_str());
    assert!(
        created["request"]
            .as_str()
            .unwrap()
            .contains("Took over PR #1"),
        "{created}"
    );

    // Idempotent: the same PR returns the same item.
    let (code, again) = hall_gh(&env, &gh, &env.repo, &["adopt", "--pr", URL]);
    assert_eq!(code, 0, "{again}");
    assert_eq!(again["id"], id.as_str());
    assert_eq!(again["created"], false);
}

#[test]
fn adopt_on_a_branch_with_an_open_pr_takes_it_over() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    bare_remote(&env);
    let wt = env.root.join("app-feature");
    git(
        &env.repo,
        &["worktree", "add", "-q", "-b", "feature", &p(&wt)],
    );
    let oid = commit_file(&wt, "feature.rs", "fn feature() {}\n");
    pr(&gh, "OPEN", "feature", &oid);

    let (code, r) = hall_gh(&env, &gh, &wt, &["adopt"]);
    assert_eq!(code, 0, "{r}");
    assert_eq!(r["created"], true);
    assert_eq!(r["pr_url"], URL);
    let id = r["id"].as_str().unwrap();
    assert_eq!(env.journal(id)[0]["pr_head"], oid.as_str());
}

#[test]
fn reopening_can_stack_the_item_on_another_branch() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    let (id, wt, sha, _) = shipped(&env, &gh);
    pr(&gh, "OPEN", &branch_of(&wt), &sha);

    // A base that isn't a branch here is refused before anything changes.
    let (code, e) = hall_gh(&env, &gh, &wt, &["adopt", "--base", "nope"]);
    assert_eq!(code, 4, "{e}");
    assert_eq!(state(&env, &id), "closed");

    // A local branch that isn't an open PR's hallmarked tip would ship its
    // commits unreviewed: refused too.
    git(&env.repo, &["branch", "-q", "scratch"]);
    let (code, e) = hall_gh(&env, &gh, &wt, &["adopt", "--base", "scratch"]);
    assert_eq!(code, 4, "{e}");
    assert_eq!(state(&env, &id), "closed");

    let (code, r) = hall_gh(&env, &gh, &wt, &["adopt", "--base", "main"]);
    assert_eq!(code, 0, "{r}");
    assert_eq!(r["reopened"], true);
    assert_eq!(r["base"], "main");
    // An open item's base doesn't move.
    let (code, e) = hall_gh(&env, &gh, &wt, &["adopt", "--base", "scratch"]);
    assert_eq!(code, 4, "{e}");
}

#[test]
fn pull_back_on_a_shipped_item_points_to_adopt() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    let (id, _, _, _) = shipped(&env, &gh);
    let r = env.hall(&env.repo, &["pull-back", &id]);
    assert_eq!(r.code, 4, "{}", r.stdout);
    assert!(
        r.json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("hall adopt"),
        "{}",
        r.stdout
    );
    // So does the closed item's status.
    let s = env.hall(&env.repo, &["status", &id]);
    assert!(
        s.json["next"].to_string().contains("hall adopt --pr"),
        "{}",
        s.stdout
    );
}
