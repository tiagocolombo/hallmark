//! ADR 0040: the sweeper removes the worktree of each item that is done with
//! it: its PR merged or closed, or it closed without a PR to watch. The
//! branch and the journal stay.
// The no-PR cases need only git and a local bare remote, so they run on every
// OS; the cases with a fake `gh` are unix-only.

mod c_support;
mod common;

use std::path::{Path, PathBuf};

use c_support::*;
use common::{Env, git, p};
use hallmark::event::Body;
use serde_json::Value;
#[cfg(unix)]
use serde_json::json;

#[cfg(unix)]
const URL: &str = "https://github.com/acme/app/pull/1";

/// A fake `gh` whose `pr view` says the PR is `state`.
#[cfg(unix)]
fn fake_gh(env: &Env, state: &str, oid: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = env.root.join("gh-fake");
    std::fs::create_dir_all(&dir).unwrap();
    let d = p(&dir);
    let gh = dir.join("gh");
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\ncase \"$1 $2\" in\n  'pr view') case \"$*\" in *--jq*) cat '{d}/state.txt' ;; *) cat '{d}/pr.json' ;; esac ;;\n  'pr edit'|'pr comment') echo '{URL}#issuecomment-9' ;;\n  *) exit 1 ;;\nesac\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    pr(&gh, state, oid);
    gh
}

#[cfg(unix)]
fn pr(gh: &Path, state: &str, oid: &str) {
    let dir = gh.parent().unwrap();
    std::fs::write(dir.join("state.txt"), state).unwrap();
    let v = json!({"state": state, "mergeable": "MERGEABLE", "headRefOid": oid, "number": 1,
                   "url": URL, "title": "Retry webhooks", "body": "", "headRefName": "x",
                   "baseRefName": "main", "isCrossRepository": false,
                   "reviews": [], "comments": []});
    std::fs::write(dir.join("pr.json"), v.to_string()).unwrap();
}

/// One pass of `worker` with the fake `gh`.
fn pass(env: &Env, gh: &Path, worker: &str) -> Value {
    let out = env
        .cmd(&env.repo)
        .env("HALL_GH", gh)
        .args(["--json", "daemon", "run", "--once", "--only", worker])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v["steps"][0].clone()
}

/// A remote with `main`, and a ship command that pushes and prints `prints`.
fn remote_shipping(env: &Env, prints: &str) {
    bare_remote(env);
    git(&env.repo, &["push", "-q", "origin", "main"]);
    set_config(
        env,
        "ship",
        &toml(&format!(
            "git push -q -f -u origin {{branch}} && echo {prints}"
        )),
    );
}

/// An item shipped with no PR to watch: closed. No `gh` is needed.
fn shipped_no_pr(env: &Env) -> (String, PathBuf, PathBuf) {
    remote_shipping(env, "shipped");
    let (id, wt, _sha) = hallmarked_item(env, "Retry webhooks");
    let gh = env.root.join("no-gh");
    pass(env, &gh, "shipper");
    assert_eq!(c_support::state(env, &id), "closed");
    (id, wt, gh)
}

/// A hallmarked item, shipped: closed, its PR `state` on GitHub.
#[cfg(unix)]
fn shipped(env: &Env, prints: &str, state: &str) -> (String, PathBuf, PathBuf) {
    remote_shipping(env, prints);
    let (id, wt, sha) = hallmarked_item(env, "Retry webhooks");
    let gh = fake_gh(env, "OPEN", &sha);
    pass(env, &gh, "shipper");
    assert_eq!(c_support::state(env, &id), "closed");
    pr(&gh, state, &sha);
    (id, wt, gh)
}

fn live_log(env: &Env) -> String {
    std::fs::read_to_string(env.repo.join(".git/hallmark/live.log")).unwrap_or_default()
}

#[cfg(unix)]
#[test]
fn a_worktree_stays_while_its_pr_is_open_and_goes_once_it_merges() {
    let env = Env::initialized();
    let (id, wt, gh) = shipped(&env, URL, "OPEN");
    let branch = git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]);

    // The Delivery office may still reopen it there.
    assert_eq!(pass(&env, &gh, "sweeper")["progress"], "idle");
    assert!(wt.is_dir());

    pr(&gh, "MERGED", "");
    let _ = std::fs::remove_file(env.repo.join(".git/hallmark/delivery-watch.json"));
    pass(&env, &gh, "delivery-watch");
    assert_eq!(events(&env, &id, "delivered").len(), 1);

    let step = pass(&env, &gh, "sweeper");
    assert!(
        step["what"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{id} removed its worktree")),
        "{step}"
    );
    assert!(!wt.exists());
    let removed = events(&env, &id, "worktree_removed");
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0]["path"], p(&wt));
    // The branch stays, and git no longer lists the worktree.
    assert_eq!(
        git(&env.repo, &["branch", "--list", &branch]).trim_start_matches(['*', ' ']),
        branch
    );
    assert!(!git(&env.repo, &["worktree", "list"]).contains(&p(&wt)));

    // Nothing left to do, and the item reads the same.
    assert_eq!(pass(&env, &gh, "sweeper")["progress"], "idle");
    let status = env.hall(&env.repo, &["status", &id]);
    assert_eq!(status.json["state"], "closed", "{}", status.stdout);
}

#[test]
fn an_item_closed_without_a_pr_to_watch_loses_its_worktree_at_once() {
    let env = Env::initialized();
    let (id, wt, gh) = shipped_no_pr(&env);
    assert!(env.journal(&id).iter().all(|e| e["pr_url"].is_null()));

    let step = pass(&env, &gh, "sweeper");
    assert_eq!(step["progress"], "worked", "{step}");
    assert!(!wt.exists());
}

#[test]
fn a_dirty_worktree_is_kept_and_said_once() {
    let env = Env::initialized();
    let (id, wt, gh) = shipped_no_pr(&env);
    std::fs::write(wt.join("notes.txt"), "half a thought\n").unwrap();

    assert_eq!(pass(&env, &gh, "sweeper")["progress"], "idle");
    assert_eq!(pass(&env, &gh, "sweeper")["progress"], "idle");
    assert!(wt.join("notes.txt").is_file());
    assert!(events(&env, &id, "worktree_removed").is_empty());
    let said = format!("{id}: kept its worktree");
    assert_eq!(
        live_log(&env).matches(&said).count(),
        1,
        "{}",
        live_log(&env)
    );

    // Kept ones are retried after a back-off, not on every pass.
    assert!(env.repo.join(".git/hallmark/sweeper.json").is_file());

    // Cleaned up by hand: it goes on the next attempt (the back-off over).
    std::fs::remove_file(wt.join("notes.txt")).unwrap();
    std::fs::remove_file(env.repo.join(".git/hallmark/sweeper.json")).unwrap();
    assert_eq!(pass(&env, &gh, "sweeper")["progress"], "worked");
    assert!(!wt.exists());
}

#[test]
fn a_worktree_moved_to_another_branch_is_kept() {
    let env = Env::initialized();
    let (_id, wt, gh) = shipped_no_pr(&env);
    git(&wt, &["switch", "-q", "-c", "other-work"]);

    assert_eq!(pass(&env, &gh, "sweeper")["progress"], "idle");
    assert!(wt.is_dir());
}

#[test]
fn a_dirty_worktree_is_not_retried_on_every_pass() {
    let env = Env::initialized();
    let (_id, wt, gh) = shipped_no_pr(&env);
    std::fs::write(wt.join("notes.txt"), "half a thought\n").unwrap();
    assert_eq!(pass(&env, &gh, "sweeper")["progress"], "idle");

    // Clean now, but the back-off hasn't passed: no attempt.
    std::fs::remove_file(wt.join("notes.txt")).unwrap();
    assert_eq!(pass(&env, &gh, "sweeper")["progress"], "idle");
    assert!(wt.is_dir());
}

#[test]
fn a_worktree_a_live_item_still_uses_is_kept() {
    let env = Env::initialized();
    let (id, wt, gh) = shipped_no_pr(&env);
    let branch = git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
    // A new item on the same branch and worktree, as `hall adopt` there makes.
    let next: u32 = id[2..].parse::<u32>().unwrap() + 1;
    let live = format!("H-{next:04}");
    append(
        &env,
        &live,
        "you",
        Body::Created {
            title: "Follow on".into(),
            branch,
            worktree: p(&wt),
            base: "main".into(),
            issue: None,
            request: None,
            adopted: true,
            parent: None,
            follow_up: None,
            pr_url: None,
            pr_head: None,
            own_worktree: Some(true),
        },
    );
    env.hall(&env.repo, &["reindex"]);

    assert_eq!(pass(&env, &gh, "sweeper")["progress"], "idle");
    assert!(wt.is_dir());
}

#[test]
fn a_dropped_item_kept_with_keep_worktree_is_left_alone() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Abandoned");
    let r = env.hall(
        &env.repo,
        &["drop", &id, "--reason", "not needed", "--keep-worktree"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    let gh = env.root.join("no-gh");

    assert_eq!(pass(&env, &gh, "sweeper")["progress"], "idle");
    assert!(wt.is_dir());
}

#[test]
fn an_item_adopted_in_the_main_checkout_is_never_swept() {
    let env = Env::initialized();
    git(&env.repo, &["switch", "-q", "-c", "my-work"]);
    let r = env.hall(&env.repo, &["adopt"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let id = r.json["id"].as_str().unwrap().to_string();
    // Hallmark did not make this checkout: the journal says so.
    assert_eq!(env.journal(&id)[0]["own_worktree"], false);
    let gh = env.root.join("no-gh");
    assert_eq!(pass(&env, &gh, "sweeper")["progress"], "idle");
    assert!(env.repo.join(".git").exists());
}
