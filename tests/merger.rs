//! ADR 0048: the merger merges each shipped PR once it is ready, one at a
//! time per base, in dependency order. Everything goes to a local bare
//! remote and a fake `gh` that keeps one file per PR.
#![cfg(unix)]

mod c_support;
mod common;

use std::path::{Path, PathBuf};

use c_support::*;
use common::{Env, commit_file, git, p};
use hallmark::event::Body;
use serde_json::{Value, json};

fn url(n: u64) -> String {
    format!("https://github.com/acme/app/pull/{n}")
}

/// A fake `gh`: `pr view <url>` prints `pr-<n>.json` (or `state-<n>.txt`
/// with `--jq`), `pr merge` merges unless `merge-fails` exists, and every
/// call is logged.
fn fake_gh(env: &Env) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = env.root.join("gh-fake");
    std::fs::create_dir_all(&dir).unwrap();
    let d = p(&dir);
    let gh = dir.join("gh");
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{d}/calls.log'\nn=${{3##*/}}\ncase \"$1 $2\" in\n  'pr view') case \"$*\" in *--jq*) cat \"{d}/state-$n.txt\" 2>/dev/null || echo OPEN ;; *) cat \"{d}/pr-$n.json\" ;; esac ;;\n  'pr merge') if [ -f '{d}/merge-fails' ]; then echo 'GraphQL: Base branch was modified' >&2; exit 1; fi; echo MERGED > \"{d}/state-$n.txt\"; echo \"merged $3\" ;;\n  *) exit 1 ;;\nesac\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    gh
}

/// PR `n` as `gh pr view` prints it.
fn pr(gh: &Path, n: u64, mergeable: &str, oid: &str, body: &str, reviews: Value) {
    let v = json!({"state": "OPEN", "mergeable": mergeable, "headRefOid": oid,
                   "body": body, "reviews": reviews});
    std::fs::write(
        gh.parent().unwrap().join(format!("pr-{n}.json")),
        v.to_string(),
    )
    .unwrap();
}

fn calls(gh: &Path) -> String {
    std::fs::read_to_string(gh.parent().unwrap().join("calls.log")).unwrap_or_default()
}

/// One pass of the merger, as the daemon runs it. Forgets when it last
/// looked, so every pass looks; keeps the rest of its state.
fn pass(env: &Env, gh: &Path) -> Value {
    let state = env.repo.join(".git/hallmark/merger.json");
    if let Ok(text) = std::fs::read_to_string(&state) {
        let mut v: Value = serde_json::from_str(&text).unwrap();
        v.as_object_mut().unwrap().remove("last");
        std::fs::write(&state, v.to_string()).unwrap();
    }
    let out = env
        .cmd(&env.repo)
        .env("HALL_GH", gh)
        .args(["--json", "daemon", "run", "--once", "--only", "merger"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v["steps"][0].clone()
}

fn queue(env: &Env, gh: &Path) -> Value {
    let out = env
        .cmd(&env.repo)
        .env("HALL_GH", gh)
        .args(["--json", "queue"])
        .output()
        .unwrap();
    serde_json::from_slice(&out.stdout).unwrap()
}

/// A remote that has `main`, and the merger on.
fn setup(env: &Env) {
    bare_remote(env);
    git(&env.repo, &["push", "-q", "origin", "main"]);
    set_config(env, "merge.mode", "\"auto\"");
}

/// A hallmarked item that changes `file`, shipped as PR `n` with green CI.
fn shipped(env: &Env, gh: &Path, n: u64, file: &str) -> (String, PathBuf, String) {
    let (id, wt) = env.new_item(&format!("Change {file}"));
    commit_file(&wt, file, &format!("// {file}\n"));
    assert_eq!(env.hall(&wt, &["ready"]).code, 0);
    let sha = git(&wt, &["rev-parse", "HEAD"]);
    approve(env, &id, &wt, 1, vec![], counts(0, 0, 0, 0));
    let branch = git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
    git(&wt, &["push", "-q", "origin", &branch]);
    append(
        env,
        &id,
        "shipper",
        Body::Shipped {
            sha: sha.clone(),
            pr_url: Some(url(n)),
        },
    );
    green(env, &id, n);
    pr(gh, n, "MERGEABLE", &sha, "", json!([]));
    (id, wt, sha)
}

fn green(env: &Env, id: &str, n: u64) {
    append(
        env,
        id,
        "ci",
        Body::PrChecks {
            pr_url: url(n),
            passed: true,
            failing: vec![],
            ended: None,
            not_started: None,
        },
    );
}

fn delivered(env: &Env, id: &str, n: u64) {
    append(
        env,
        id,
        "delivery",
        Body::Delivered {
            pr_url: url(n),
            outcome: "merged".into(),
        },
    );
}

fn status_of<'a>(q: &'a Value, id: &str) -> &'a Value {
    q["queue"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["item"] == id)
        .unwrap_or_else(|| panic!("{id} not in {q}"))
}

#[test]
fn off_by_default_it_never_merges_but_the_queue_still_reads() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    bare_remote(&env);
    git(&env.repo, &["push", "-q", "origin", "main"]);
    let (id, _, _) = shipped(&env, &gh, 1, "a.rs");

    assert_eq!(pass(&env, &gh)["progress"], "idle");
    assert!(calls(&gh).is_empty(), "{}", calls(&gh));

    let q = queue(&env, &gh);
    assert_eq!(q["mode"], "off");
    assert_eq!(status_of(&q, &id)["status"], "ready");
    assert!(q["next"].to_string().contains("merge.mode auto"), "{q}");
    assert!(events(&env, &id, "pr_merged").is_empty());
}

#[test]
fn a_ready_pr_merges_with_the_configured_command_and_waits_for_delivery() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (id, _, sha) = shipped(&env, &gh, 1, "a.rs");

    let step = pass(&env, &gh);
    assert!(step["what"].as_str().unwrap().contains("merged"), "{step}");
    assert!(
        calls(&gh).contains(&format!(
            "pr merge {} --squash --match-head-commit {sha}",
            url(1)
        )),
        "{}",
        calls(&gh)
    );
    let merged = events(&env, &id, "pr_merged");
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0]["sha"], sha.as_str());
    assert_eq!(merged[0]["actor"], "merger");
    // Merging is not delivering: the watch records that.
    let q = queue(&env, &gh);
    assert_eq!(status_of(&q, &id)["status"], "merging");
    let s = env.hall(&env.repo, &["status", &id]);
    assert!(s.stdout.contains("Merging"), "{}", s.stdout);
    // Never twice.
    pass(&env, &gh);
    assert_eq!(events(&env, &id, "pr_merged").len(), 1);
}

#[test]
fn oldest_first_and_one_at_a_time_per_base() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (second, _, _) = shipped(&env, &gh, 2, "b.rs");
    let (first, _, _) = shipped(&env, &gh, 1, "a.rs");

    let q = queue(&env, &gh);
    assert_eq!(status_of(&q, &first)["position"], 1);
    assert_eq!(status_of(&q, &first)["status"], "ready");
    assert_eq!(status_of(&q, &second)["status"], "queued");

    pass(&env, &gh);
    assert_eq!(events(&env, &first, "pr_merged").len(), 1);
    assert!(events(&env, &second, "pr_merged").is_empty());
    // Until the first is delivered, nothing else on main merges.
    pass(&env, &gh);
    assert!(events(&env, &second, "pr_merged").is_empty());
    delivered(&env, &first, 1);
    pass(&env, &gh);
    assert_eq!(events(&env, &second, "pr_merged").len(), 1);
}

#[test]
fn a_pr_waits_for_the_one_it_depends_on() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (older, _, sha) = shipped(&env, &gh, 1, "a.rs");
    pr(
        &gh,
        1,
        "MERGEABLE",
        &sha,
        "## Why\n\nDepends on #2.",
        json!([]),
    );
    let (newer, _, _) = shipped(&env, &gh, 2, "b.rs");

    let q = queue(&env, &gh);
    let e = status_of(&q, &older);
    assert_eq!(e["status"], "waits_for");
    assert_eq!(e["waits_for"][0], url(2).as_str());
    assert_eq!(status_of(&q, &newer)["status"], "ready");

    pass(&env, &gh);
    assert_eq!(events(&env, &newer, "pr_merged").len(), 1);
    assert!(events(&env, &older, "pr_merged").is_empty());
    // A dependency that isn't one of ours is asked of GitHub.
    pr(&gh, 1, "MERGEABLE", &sha, "Blocked by #9", json!([]));
    std::fs::write(gh.parent().unwrap().join("state-9.txt"), "OPEN").unwrap();
    delivered(&env, &newer, 2);
    pass(&env, &gh);
    assert!(events(&env, &older, "pr_merged").is_empty());
    std::fs::write(gh.parent().unwrap().join("state-9.txt"), "MERGED").unwrap();
    pass(&env, &gh);
    assert_eq!(events(&env, &older, "pr_merged").len(), 1);
}

#[test]
fn behind_on_its_own_files_it_is_rebased_first_otherwise_it_merges() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (id, _, _) = shipped(&env, &gh, 1, "a.rs");
    // A release commit on main touches files the PR doesn't: still fresh.
    commit_file(&env.repo, "CHANGELOG.md", "## 0.2.0\n");
    git(&env.repo, &["push", "-q", "origin", "main"]);
    assert_eq!(status_of(&queue(&env, &gh), &id)["status"], "ready");

    // Main changes a.rs too: the PR goes back for a rebase, never merges.
    commit_file(&env.repo, "a.rs", "// main's a.rs\n");
    git(&env.repo, &["push", "-q", "origin", "main"]);
    let step = pass(&env, &gh);
    assert!(
        step["what"].as_str().unwrap().contains("reopened"),
        "{step}"
    );
    assert!(events(&env, &id, "pr_merged").is_empty());
    assert_eq!(state(&env, &id), "development");
    let req = events(&env, &id, "rebase_requested");
    assert_eq!(req[0]["actor"], "merger");
    assert_eq!(req[0]["files"], json!(["a.rs"]));
    assert_eq!(events(&env, &id, "reopened")[0]["actor"], "merger");
}

#[test]
fn ci_must_pass_at_the_shipped_head_unless_it_is_not_required() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (id, _, sha) = shipped(&env, &gh, 1, "a.rs");
    // Shipped again (an update) after the green run: that run is stale.
    append(
        &env,
        &id,
        "shipper",
        Body::Shipped {
            sha: sha.clone(),
            pr_url: Some(url(1)),
        },
    );
    assert_eq!(status_of(&queue(&env, &gh), &id)["status"], "ci_pending");
    pass(&env, &gh);
    assert!(events(&env, &id, "pr_merged").is_empty());
    // No GitHub call for a PR that can't merge yet.
    assert!(!calls(&gh).contains("pr view"), "{}", calls(&gh));

    append(
        &env,
        &id,
        "ci",
        Body::PrChecks {
            pr_url: url(1),
            passed: false,
            failing: vec![hallmark::event::FailingCheck {
                name: "test (windows-latest)".into(),
                link: None,
            }],
            ended: None,
            not_started: None,
        },
    );
    let e = queue(&env, &gh);
    assert_eq!(status_of(&e, &id)["status"], "ci_red");

    set_config(&env, "merge.require_ci", "false");
    let q = queue(&env, &gh);
    assert_eq!(q["require_ci"], false);
    assert_eq!(status_of(&q, &id)["status"], "ready");
    pass(&env, &gh);
    assert_eq!(events(&env, &id, "pr_merged").len(), 1);
}

#[test]
fn github_must_agree_and_nobody_may_have_asked_for_changes() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (id, _, sha) = shipped(&env, &gh, 1, "a.rs");

    pr(&gh, 1, "UNKNOWN", &sha, "", json!([]));
    assert_eq!(status_of(&queue(&env, &gh), &id)["status"], "unknown");
    pr(&gh, 1, "CONFLICTING", &sha, "", json!([]));
    assert_eq!(status_of(&queue(&env, &gh), &id)["status"], "conflicting");
    pr(&gh, 1, "MERGEABLE", "0000000deadbeef", "", json!([]));
    assert_eq!(status_of(&queue(&env, &gh), &id)["status"], "not_ours");
    let review =
        |who: &str, state: &str| json!({"author": {"login": who}, "state": state, "body": ""});
    pr(
        &gh,
        1,
        "MERGEABLE",
        &sha,
        "",
        json!([review("ana", "CHANGES_REQUESTED")]),
    );
    let q = queue(&env, &gh);
    assert_eq!(status_of(&q, &id)["status"], "changes_requested");
    pass(&env, &gh);
    assert!(events(&env, &id, "pr_merged").is_empty());

    pr(
        &gh,
        1,
        "MERGEABLE",
        &sha,
        "",
        json!([
            review("ana", "CHANGES_REQUESTED"),
            review("ana", "APPROVED")
        ]),
    );
    pass(&env, &gh);
    assert_eq!(events(&env, &id, "pr_merged").len(), 1);
}

#[test]
fn a_lit_path_waits_for_a_person_and_retry_lets_it_merge() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    set_config(&env, "merge.lit", "[\"deploy/\", \"*.yml\"]");
    let (lit, _, _) = shipped(&env, &gh, 1, "release.yml");
    let (plain, _, _) = shipped(&env, &gh, 2, "b.rs");

    let step = pass(&env, &gh);
    let what = step["what"].as_str().unwrap();
    assert!(what.contains("held (lit)"), "{step}");
    // A lit PR doesn't hold up the rest.
    assert_eq!(events(&env, &plain, "pr_merged").len(), 1);
    assert!(events(&env, &lit, "pr_merged").is_empty());
    let held = events(&env, &lit, "held");
    assert_eq!(held[0]["kind"], "lit");
    assert!(
        held[0]["reason"].as_str().unwrap().contains("release.yml"),
        "{held:?}"
    );
    let inbox = env.hall(&env.repo, &["inbox"]);
    let entries = inbox.json["entries"].to_string();
    assert!(entries.contains("\"lit\""), "{}", inbox.stdout);

    delivered(&env, &plain, 2);
    pass(&env, &gh);
    assert!(events(&env, &lit, "pr_merged").is_empty());
    let r = env.hall(&env.repo, &["retry", &lit, "--reason", "read the workflow"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    pass(&env, &gh);
    assert_eq!(events(&env, &lit, "pr_merged").len(), 1);
}

#[test]
fn a_failed_merge_is_held_for_you_and_retry_tries_again() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (id, _, _) = shipped(&env, &gh, 1, "a.rs");
    let fails = gh.parent().unwrap().join("merge-fails");
    std::fs::write(&fails, "").unwrap();

    let step = pass(&env, &gh);
    assert!(
        step["what"].as_str().unwrap().contains("merge_failed"),
        "{step}"
    );
    let held = events(&env, &id, "held");
    assert_eq!(held[0]["kind"], "merge_failed");
    assert!(
        held[0]["reason"]
            .as_str()
            .unwrap()
            .contains("Base branch was modified"),
        "{held:?}"
    );
    assert!(events(&env, &id, "pr_merged").is_empty());
    // Held: not tried again on its own.
    pass(&env, &gh);
    assert_eq!(calls(&gh).matches("pr merge").count(), 1);
    let inbox = env.hall(&env.repo, &["inbox"]);
    assert!(
        inbox.json["entries"].to_string().contains("merge_failed"),
        "{}",
        inbox.stdout
    );

    std::fs::remove_file(&fails).unwrap();
    assert_eq!(
        env.hall(&env.repo, &["retry", &id, "--reason", "base settled"])
            .code,
        0
    );
    pass(&env, &gh);
    assert_eq!(events(&env, &id, "pr_merged").len(), 1);
}

#[test]
fn a_stacked_pr_waits_for_the_pr_below_it() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (lower, lower_wt, _) = shipped(&env, &gh, 1, "a.rs");
    let branch = git(&lower_wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
    // A follow-up stacked on the lower PR's branch.
    let (upper, _, _) = shipped(&env, &gh, 2, "b.rs");
    let p = hall(&env).item_events(upper.parse().unwrap());
    let text = std::fs::read_to_string(&p).unwrap();
    // Its journal says it was made on the lower branch.
    let mut lines: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    lines[0]["base"] = json!(branch);
    std::fs::write(
        &p,
        lines
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    assert_eq!(env.hall(&env.repo, &["reindex", "--force"]).code, 0);

    let q = queue(&env, &gh);
    let e = status_of(&q, &upper);
    assert_eq!(e["status"], "waits_for", "{q}");
    assert_eq!(e["waits_for"][0], url(1).as_str());
    assert_eq!(status_of(&q, &lower)["status"], "ready");
}

#[test]
fn one_pr_at_a_time_is_brought_up_to_date_while_fresh_ones_merge() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (first, _, _) = shipped(&env, &gh, 1, "a.rs");
    let (second, _, _) = shipped(&env, &gh, 2, "b.rs");
    let (fresh, _, _) = shipped(&env, &gh, 3, "c.rs");
    // Main moves on both a.rs and b.rs.
    commit_file(&env.repo, "a.rs", "// main's a.rs\n");
    commit_file(&env.repo, "b.rs", "// main's b.rs\n");
    git(&env.repo, &["push", "-q", "origin", "main"]);

    pass(&env, &gh);
    assert_eq!(state(&env, &first), "development");
    // The second waits for the first's rebase; the fresh one merges.
    pass(&env, &gh);
    assert_eq!(state(&env, &second), "closed");
    assert!(events(&env, &second, "rebase_requested").is_empty());
    assert_eq!(events(&env, &fresh, "pr_merged").len(), 1);
    let q = queue(&env, &gh);
    let e = status_of(&q, &second);
    assert_eq!(e["status"], "queued", "{q}");
    assert!(e["why"].as_str().unwrap().contains(&first), "{q}");

    // Once the first is dropped (or shipped again), the second goes back.
    assert_eq!(
        env.hall(&env.repo, &["drop", &first, "--reason", "superseded"])
            .code,
        0
    );
    delivered(&env, &fresh, 3);
    pass(&env, &gh);
    assert_eq!(state(&env, &second), "development");
}

#[test]
fn a_restored_pr_keeps_its_green_ci() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (id, _, sha) = shipped(&env, &gh, 1, "a.rs");
    // CI went red, was reopened for a fix, and the re-run came back green:
    // the item is restored, with its own checks recorded just before.
    append(
        &env,
        &id,
        "delivery",
        Body::Restored {
            pr_url: url(1),
            sha,
            reason: "the re-run passed".into(),
        },
    );
    assert_eq!(status_of(&queue(&env, &gh), &id)["status"], "ready");
    pass(&env, &gh);
    assert_eq!(events(&env, &id, "pr_merged").len(), 1);
}

#[test]
fn a_dependency_named_by_a_commit_on_the_base_is_not_the_prs() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    git(&env.repo, &["commit", "--allow-empty", "-q", "-m", "Blocked by #9"]);
    git(&env.repo, &["push", "-q", "origin", "main"]);
    let (id, _, _) = shipped(&env, &gh, 1, "a.rs");
    std::fs::write(gh.parent().unwrap().join("state-9.txt"), "OPEN").unwrap();

    assert_eq!(status_of(&queue(&env, &gh), &id)["status"], "ready");
    pass(&env, &gh);
    assert_eq!(events(&env, &id, "pr_merged").len(), 1);
}

#[test]
fn moving_a_file_out_of_a_lit_path_is_still_touching_it() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    set_config(&env, "merge.lit", "[\"deploy/\"]");
    std::fs::create_dir_all(env.repo.join("deploy")).unwrap();
    commit_file(&env.repo, "deploy/x.yml", "name: deploy\n");
    git(&env.repo, &["push", "-q", "origin", "main"]);
    let (id, wt) = env.new_item("Move the workflow");
    git(&wt, &["mv", "deploy/x.yml", "x.yml"]);
    git(&wt, &["commit", "-q", "-m", "move the workflow"]);
    assert_eq!(env.hall(&wt, &["ready"]).code, 0);
    let sha = git(&wt, &["rev-parse", "HEAD"]);
    approve(&env, &id, &wt, 1, vec![], counts(0, 0, 0, 0));
    let branch = git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
    git(&wt, &["push", "-q", "origin", &branch]);
    append(
        &env,
        &id,
        "shipper",
        Body::Shipped {
            sha: sha.clone(),
            pr_url: Some(url(1)),
        },
    );
    green(&env, &id, 1);
    pr(&gh, 1, "MERGEABLE", &sha, "", json!([]));

    let step = pass(&env, &gh);
    let what = step["what"].as_str().unwrap();
    assert!(what.contains("held (lit)"), "{step}");
    assert!(events(&env, &id, "pr_merged").is_empty());
}
