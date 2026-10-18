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
/// with `--jq`), `pr merge` merges unless `merge-fails` exists, `api` (the
/// base tip's check runs) prints `tip-checks.json` or fails, and every call
/// is logged.
fn fake_gh(env: &Env) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = env.root.join("gh-fake");
    std::fs::create_dir_all(&dir).unwrap();
    let d = p(&dir);
    let gh = dir.join("gh");
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{d}/calls.log'\nn=${{3##*/}}\ncase \"$1 $2\" in\n  'pr view') case \"$*\" in *--jq*) cat \"{d}/state-$n.txt\" 2>/dev/null || echo OPEN ;; *) cat \"{d}/pr-$n.json\" ;; esac ;;\n  'pr merge') if [ -f '{d}/merge-fails' ]; then echo 'GraphQL: Base branch was modified' >&2; exit 1; fi; echo MERGED > \"{d}/state-$n.txt\"; echo \"merged $3\" ;;\n  'api '*) cat '{d}/tip-checks.json' 2>/dev/null || exit 1 ;;\n  *) exit 1 ;;\nesac\n"
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
    shipped_files(env, gh, n, &[file])
}

/// A hallmarked item that changes each of `files`, shipped as PR `n` with
/// green CI.
fn shipped_files(env: &Env, gh: &Path, n: u64, files: &[&str]) -> (String, PathBuf, String) {
    shipped_ci(env, gh, n, files, true)
}

/// Like `shipped_files`, with CI green or not yet reported.
fn shipped_ci(env: &Env, gh: &Path, n: u64, files: &[&str], ci: bool) -> (String, PathBuf, String) {
    let (id, wt) = env.new_item(&format!("Change {}", files.join(" and ")));
    for file in files {
        commit_file(&wt, file, &format!("// {file}\n"));
    }
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
    if ci {
        green(env, &id, n);
    }
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
fn an_older_pr_depending_on_a_newer_one_sharing_a_file_does_not_deadlock() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (first, _, sha) = shipped(&env, &gh, 1, "a.rs");
    pr(
        &gh,
        1,
        "MERGEABLE",
        &sha,
        "## Why\n\nDepends on #3.",
        json!([]),
    );
    let (second, _, _) = shipped(&env, &gh, 2, "a.rs");
    let (third, _, _) = shipped(&env, &gh, 3, "a.rs");

    let q = queue(&env, &gh);
    // The walk is dependency-first, then oldest: #2, #3, then #1.
    assert_eq!(status_of(&q, &second)["status"], "ready");
    assert_eq!(status_of(&q, &third)["status"], "waits_for");
    assert_eq!(status_of(&q, &first)["status"], "waits_for");

    pass(&env, &gh);
    assert_eq!(events(&env, &second, "pr_merged").len(), 1);
    assert!(events(&env, &first, "pr_merged").is_empty());
    assert!(events(&env, &third, "pr_merged").is_empty());
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
    // No GitHub call for a PR whose CI failed. (One whose CI hasn't
    // reported costs one: a conflict would keep its CI from ever running.)
    std::fs::remove_file(gh.parent().unwrap().join("calls.log")).unwrap();
    pass(&env, &gh);
    assert!(!calls(&gh).contains("pr view"), "{}", calls(&gh));

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
fn a_pr_is_brought_up_to_date_once_at_its_turn_and_lanes_move_in_parallel() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (first, _, _) = shipped(&env, &gh, 1, "a.rs");
    let (other_lane, _, _) = shipped(&env, &gh, 2, "b.rs");
    let (same_file, _, _) = shipped(&env, &gh, 3, "a.rs");
    let (fresh, _, _) = shipped(&env, &gh, 4, "c.rs");
    // Main moves on both a.rs and b.rs.
    commit_file(&env.repo, "a.rs", "// main's a.rs\n");
    commit_file(&env.repo, "b.rs", "// main's b.rs\n");
    git(&env.repo, &["push", "-q", "origin", "main"]);

    // ADR 0050: the first PR on a.rs and the one on b.rs share nothing, so
    // both go back at once; the second PR on a.rs waits for the first to
    // merge, or that merge would make its rebase stale; the fresh one merges.
    pass(&env, &gh);
    assert_eq!(state(&env, &first), "development");
    assert_eq!(state(&env, &other_lane), "development");
    assert_eq!(state(&env, &same_file), "closed");
    assert!(events(&env, &same_file, "rebase_requested").is_empty());
    assert_eq!(events(&env, &fresh, "pr_merged").len(), 1);
    let q = queue(&env, &gh);
    let e = status_of(&q, &same_file);
    assert_eq!(e["status"], "waits_for", "{q}");
    assert_eq!(e["waits_for"], json!([url(1)]), "{q}");
    assert!(e["why"].as_str().unwrap().contains("#1 (a.rs)"), "{q}");
    // The ones sent back are in the queue, on their way.
    assert_eq!(status_of(&q, &first)["status"], "updating", "{q}");

    // Still waiting while the first is on its way back.
    delivered(&env, &fresh, 4);
    pass(&env, &gh);
    assert_eq!(state(&env, &same_file), "closed");

    // Once the first is gone (merged, or here dropped), its turn comes.
    assert_eq!(
        env.hall(&env.repo, &["drop", &first, "--reason", "superseded"])
            .code,
        0
    );
    pass(&env, &gh);
    assert_eq!(state(&env, &same_file), "development");
    assert_eq!(events(&env, &same_file, "rebase_requested").len(), 1);
}

#[test]
fn a_conflicting_pr_is_brought_up_to_date_at_its_turn() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (first, _, sha) = shipped(&env, &gh, 1, "a.rs");
    let (second, _, _) = shipped(&env, &gh, 2, "a.rs");
    pr(&gh, 1, "CONFLICTING", &sha, "", json!([]));

    let q = queue(&env, &gh);
    assert_eq!(status_of(&q, &first)["status"], "conflicting", "{q}");
    // The ready one on the same file goes after it, not around it.
    assert_eq!(status_of(&q, &second)["status"], "waits_for", "{q}");

    pass(&env, &gh);
    assert_eq!(state(&env, &first), "development");
    let req = events(&env, &first, "rebase_requested");
    assert_eq!(req[0]["actor"], "merger");
    assert!(events(&env, &second, "pr_merged").is_empty());
}

#[test]
fn a_pr_waiting_on_a_person_holds_up_nothing_behind_it() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (asked, _, sha) = shipped(&env, &gh, 1, "a.rs");
    let (next, _, _) = shipped(&env, &gh, 2, "a.rs");
    let review = json!([{"author": {"login": "ana"}, "state": "CHANGES_REQUESTED", "body": ""}]);
    pr(&gh, 1, "MERGEABLE", &sha, "", review);

    pass(&env, &gh);
    assert!(events(&env, &asked, "pr_merged").is_empty());
    assert_eq!(events(&env, &next, "pr_merged").len(), 1);
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
    git(
        &env.repo,
        &["commit", "--allow-empty", "-q", "-m", "Blocked by #9"],
    );
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

/// Rewrites an item's journal, `f` seeing each event.
fn edit_journal(env: &Env, id: &str, f: impl Fn(&mut Value)) {
    let p = hall(env).item_events(id.parse().unwrap());
    let text = std::fs::read_to_string(&p).unwrap();
    let lines: Vec<String> = text
        .lines()
        .map(|l| {
            let mut v: Value = serde_json::from_str(l).unwrap();
            f(&mut v);
            v.to_string()
        })
        .collect();
    std::fs::write(&p, lines.join("\n") + "\n").unwrap();
    assert_eq!(env.hall(&env.repo, &["reindex", "--force"]).code, 0);
}

/// Item `upper`, made on `lower`'s branch.
fn stack_on(env: &Env, upper: &str, branch: &str) {
    edit_journal(env, upper, |v| {
        if v["event"] == "created" {
            v["base"] = json!(branch);
        }
    });
}

fn inbox_has(env: &Env, kind: &str) -> bool {
    let inbox = env.hall(&env.repo, &["inbox"]);
    inbox.json["entries"]
        .to_string()
        .contains(&format!("\"{kind}\""))
}

#[test]
fn a_merge_that_never_lands_is_held_and_retry_merges_again() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (id, _, _) = shipped(&env, &gh, 1, "a.rs");
    pass(&env, &gh);
    assert_eq!(events(&env, &id, "pr_merged").len(), 1);

    // The command exited 0, yet the PR is still open half an hour on.
    let old = (chrono::Utc::now() - chrono::Duration::minutes(30)).to_rfc3339();
    edit_journal(&env, &id, |v| {
        if v["event"] == "pr_merged" {
            v["t"] = json!(old);
        }
    });
    std::fs::write(gh.parent().unwrap().join("state-1.txt"), "OPEN").unwrap();
    let step = pass(&env, &gh);
    assert!(
        step["what"]
            .as_str()
            .unwrap_or_default()
            .contains("merge_failed"),
        "{step}"
    );
    let held = events(&env, &id, "held");
    assert_eq!(held[0]["kind"], "merge_failed");
    assert!(inbox_has(&env, "merge_failed"));

    // Retry clears the merge, so the base is free and the PR merges again.
    assert_eq!(
        env.hall(&env.repo, &["retry", &id, "--reason", "try again"])
            .code,
        0
    );
    pass(&env, &gh);
    assert_eq!(events(&env, &id, "pr_merged").len(), 2);

    // Delivered: the inbox entry is gone.
    delivered(&env, &id, 1);
    assert!(!inbox_has(&env, "merge_failed"));
}

#[test]
fn an_older_pr_github_is_still_checking_goes_first() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (older, _, sha) = shipped(&env, &gh, 1, "a.rs");
    let (newer, _, _) = shipped(&env, &gh, 2, "b.rs");
    pr(&gh, 1, "UNKNOWN", &sha, "", json!([]));

    pass(&env, &gh);
    assert!(events(&env, &older, "pr_merged").is_empty());
    assert!(events(&env, &newer, "pr_merged").is_empty());
    let q = queue(&env, &gh);
    assert_eq!(status_of(&q, &older)["status"], "unknown", "{q}");
    assert_eq!(status_of(&q, &newer)["status"], "queued", "{q}");

    pr(&gh, 1, "MERGEABLE", &sha, "", json!([]));
    pass(&env, &gh);
    assert_eq!(events(&env, &older, "pr_merged").len(), 1);
    assert!(events(&env, &newer, "pr_merged").is_empty());
}

#[test]
fn a_dirty_worktree_is_not_reopened_for_a_rebase() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (id, wt, _) = shipped(&env, &gh, 1, "a.rs");
    commit_file(&env.repo, "a.rs", "// main's a.rs\n");
    git(&env.repo, &["push", "-q", "origin", "main"]);
    std::fs::write(wt.join("a.rs"), "// someone is editing\n").unwrap();

    pass(&env, &gh);
    assert!(events(&env, &id, "rebase_requested").is_empty());
    assert_eq!(state(&env, &id), "closed");
    let q = queue(&env, &gh);
    assert_eq!(status_of(&q, &id)["status"], "not_ours", "{q}");
}

#[test]
fn a_stacked_pr_whose_parent_merged_says_a_person_retargets_it() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (lower, lower_wt, _) = shipped(&env, &gh, 1, "a.rs");
    let branch = git(&lower_wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let (upper, _, _) = shipped(&env, &gh, 2, "b.rs");
    stack_on(&env, &upper, &branch);
    delivered(&env, &lower, 1);

    let q = queue(&env, &gh);
    let e = status_of(&q, &upper);
    assert_eq!(e["status"], "waits_for", "{q}");
    let why = e["why"].as_str().unwrap();
    assert!(why.contains("retarget"), "{why}");
    pass(&env, &gh);
    assert!(events(&env, &upper, "pr_merged").is_empty());
}

#[test]
fn hall_status_says_what_the_merger_is_doing() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    set_config(&env, "merge.lit", "[\"*.yml\"]");
    let (lit, _, _) = shipped(&env, &gh, 1, "ci.yml");
    let (waiter, _, sha) = shipped(&env, &gh, 2, "a.rs");
    pr(&gh, 2, "MERGEABLE", &sha, "Depends on #9", json!([]));
    let (merged, _, _) = shipped(&env, &gh, 3, "b.rs");
    pass(&env, &gh);

    let waiting = env.hall(&env.repo, &["status", &waiter]).stdout;
    assert!(waiting.contains("merger: waits_for"), "{waiting}");
    let merging = env.hall(&env.repo, &["status", &merged]).stdout;
    assert!(merging.contains("Merging: the merger merged"), "{merging}");
    let held = env.hall(&env.repo, &["status", &lit]).stdout;
    assert!(held.contains("(lit): ready to merge"), "{held}");
    assert!(inbox_has(&env, "lit"));

    // The PR merged and was delivered: nothing is left for you to do.
    delivered(&env, &lit, 1);
    assert!(!inbox_has(&env, "lit"));
}

/// Shas differ between runs.
fn scrub(v: &Value, shas: &[&str]) -> Value {
    let mut s = v.to_string();
    for sha in shas {
        s = s.replace(&sha[..7], "<sha>");
    }
    serde_json::from_str(&s).unwrap()
}

#[test]
fn hall_queue_json_is_pinned() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    let empty = env.hall(&env.repo, &["queue"]);
    insta::assert_json_snapshot!("queue_empty", empty.json);

    setup(&env);
    tip_checks(&gh, json!([{"name": "test", "conclusion": "success"}]));
    set_config(&env, "merge.lit", "[\"*.yml\"]");
    let (_, first_wt, s1) = shipped(&env, &gh, 1, "a.rs");
    let (_, _, s2) = shipped(&env, &gh, 2, "b.rs");
    let (_, _, s3) = shipped(&env, &gh, 3, "c.rs");
    pr(&gh, 3, "MERGEABLE", &s3, "Depends on #1", json!([]));
    let (_, _, s4) = shipped(&env, &gh, 4, "ci.yml");
    let (stacked, _, s5) = shipped(&env, &gh, 5, "d.rs");
    let branch = git(&first_wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
    stack_on(&env, &stacked, &branch);
    // An item of main reopened and in review, and a PR on its file.
    let (back, back_wt, s6) = shipped(&env, &gh, 6, "e.rs");
    let (_, _, s7) = shipped(&env, &gh, 7, "e.rs");
    reopen_in_review(&env, &back, &back_wt, 6, &s6);

    let q = queue(&env, &gh);
    insta::assert_json_snapshot!(
        "queue_two_bases",
        scrub(&q, &[&s1, &s2, &s3, &s4, &s5, &s6, &s7])
    );
}

#[test]
fn hall_queue_text_is_pinned() {
    use hallmark::commands::queue::QueueOut;
    use hallmark::merger::{Entry, Status};
    use hallmark::output::Render;
    let e = |position, id: &str, n, base: &str, status, why: &str| Entry {
        position,
        item: id.parse().unwrap(),
        title: format!("Change {id}"),
        pr_url: url(n),
        base: base.into(),
        status,
        why: why.into(),
        waits_for: vec![],
    };
    let out = QueueOut {
        mode: "auto".into(),
        require_ci: false,
        queue: vec![
            e(1, "H-0001", 1, "main", Status::Merging, "merged at abc1234"),
            e(2, "H-0002", 2, "main", Status::Ready, "ready: hallmarked"),
            e(
                3,
                "H-0003",
                3,
                "main",
                Status::Lit,
                "waiting for you: ci.yml",
            ),
            e(
                1,
                "H-0004",
                4,
                "hm/H-0002-x",
                Status::WaitsFor,
                "waits for #2",
            ),
            e(
                2,
                "H-0005",
                5,
                "hm/H-0002-x",
                Status::Updating,
                "being brought up to date (review); it ships again, then merges",
            ),
            Entry {
                waits_for: vec![url(5)],
                ..e(
                    3,
                    "H-0006",
                    6,
                    "hm/H-0002-x",
                    Status::WaitsFor,
                    "ready; first waits for #5 (d.rs)",
                )
            },
        ],
    };
    insta::assert_snapshot!("queue_text", out.render());
    let empty = QueueOut {
        mode: "off".into(),
        require_ci: true,
        queue: vec![],
    };
    insta::assert_snapshot!("queue_text_empty", empty.render());
}

/// Item `id` (PR `n`, shipped at `sha` from `wt`) reopened and sent to
/// review again: the loop is bringing it back.
fn reopen_in_review(env: &Env, id: &str, wt: &Path, n: u64, sha: &str) {
    append(
        env,
        id,
        "delivery",
        Body::Reopened {
            pr_url: url(n),
            pr_head: sha.into(),
            base: "main".into(),
            worktree: p(wt),
        },
    );
    commit_file(wt, "extra.rs", "// a fix\n");
    assert_eq!(env.hall(wt, &["ready"]).code, 0);
    assert_ne!(state(env, id), "development");
    assert_ne!(state(env, id), "closed");
}

fn hold(env: &Env, id: &str) {
    append(
        env,
        id,
        "test",
        Body::Held {
            kind: hallmark::item::HoldKind::Budget,
            reason: "capped".into(),
        },
    );
}

#[test]
fn an_older_pr_that_depends_on_a_newer_one_sharing_a_file_does_not_deadlock() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (older, _, sha) = shipped(&env, &gh, 1, "a.rs");
    pr(&gh, 1, "MERGEABLE", &sha, "Depends on #2", json!([]));
    let (newer, _, _) = shipped(&env, &gh, 2, "a.rs");

    let q = queue(&env, &gh);
    assert_eq!(status_of(&q, &older)["status"], "waits_for", "{q}");
    assert_eq!(status_of(&q, &newer)["status"], "ready", "{q}");
    pass(&env, &gh);
    assert_eq!(events(&env, &newer, "pr_merged").len(), 1);
    assert!(events(&env, &older, "pr_merged").is_empty());
    delivered(&env, &newer, 2);
    pass(&env, &gh);
    assert_eq!(events(&env, &older, "pr_merged").len(), 1);
}

#[test]
fn an_older_pr_goes_first_while_a_newer_one_is_reopened_on_its_file() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (older, _, _) = shipped(&env, &gh, 1, "a.rs");
    let (newer, wt, sha) = shipped(&env, &gh, 5, "a.rs");
    reopen_in_review(&env, &newer, &wt, 5, &sha);

    let q = queue(&env, &gh);
    assert_eq!(status_of(&q, &older)["status"], "ready", "{q}");
    assert_eq!(status_of(&q, &newer)["status"], "updating", "{q}");
    pass(&env, &gh);
    assert_eq!(events(&env, &older, "pr_merged").len(), 1);
}

#[test]
fn a_reopened_item_in_review_claims_its_files_but_a_held_one_does_not() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (back, wt, sha) = shipped(&env, &gh, 1, "a.rs");
    let (overlap, _, _) = shipped(&env, &gh, 2, "a.rs");
    let (apart, _, _) = shipped(&env, &gh, 3, "b.rs");
    reopen_in_review(&env, &back, &wt, 1, &sha);

    let q = queue(&env, &gh);
    let e = status_of(&q, &overlap);
    assert_eq!(e["status"], "waits_for", "{q}");
    assert_eq!(e["waits_for"], json!([url(1)]), "{q}");
    assert_eq!(status_of(&q, &apart)["status"], "ready", "{q}");
    pass(&env, &gh);
    assert!(events(&env, &overlap, "pr_merged").is_empty());
    assert_eq!(events(&env, &apart, "pr_merged").len(), 1);

    hold(&env, &back);
    let q = queue(&env, &gh);
    assert_eq!(status_of(&q, &back)["status"], "held", "{q}");
    assert_eq!(status_of(&q, &overlap)["status"], "queued", "{q}");
}

#[test]
fn a_behind_pr_waits_for_an_earlier_pending_one_on_its_file() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (pending, _, sha) = shipped(&env, &gh, 1, "a.rs");
    let (behind, _, _) = shipped(&env, &gh, 2, "a.rs");
    let (apart, _, _) = shipped(&env, &gh, 3, "b.rs");
    commit_file(&env.repo, "a.rs", "// main's a.rs\n");
    git(&env.repo, &["push", "-q", "origin", "main"]);
    // Shipped again after the green run: its CI is pending.
    append(
        &env,
        &pending,
        "shipper",
        Body::Shipped {
            sha,
            pr_url: Some(url(1)),
        },
    );

    let q = queue(&env, &gh);
    assert_eq!(status_of(&q, &pending)["status"], "ci_pending", "{q}");
    let e = status_of(&q, &behind);
    assert_eq!(e["status"], "waits_for", "{q}");
    assert_eq!(e["waits_for"], json!([url(1)]), "{q}");
    pass(&env, &gh);
    assert!(events(&env, &behind, "rebase_requested").is_empty());
    assert_eq!(events(&env, &apart, "pr_merged").len(), 1);
}

#[test]
fn two_behind_prs_on_different_files_are_both_sent_back_in_one_pass() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (a, _, _) = shipped(&env, &gh, 1, "a.rs");
    let (b, _, _) = shipped(&env, &gh, 2, "b.rs");
    commit_file(&env.repo, "a.rs", "// main's a.rs\n");
    commit_file(&env.repo, "b.rs", "// main's b.rs\n");
    git(&env.repo, &["push", "-q", "origin", "main"]);
    pass(&env, &gh);
    assert_eq!(state(&env, &a), "development");
    assert_eq!(state(&env, &b), "development");
}

#[test]
fn prs_that_cant_move_on_their_own_claim_nothing() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    // A held PR, a CI-red one, one that isn't ours, and one waiting for a PR
    // outside the queue: each has a later PR on the same file.
    let (held, _, _) = shipped(&env, &gh, 1, "a.rs");
    let (after_held, _, _) = shipped(&env, &gh, 2, "a.rs");
    let (red, _, _) = shipped(&env, &gh, 3, "b.rs");
    let (after_red, _, _) = shipped(&env, &gh, 4, "b.rs");
    let (foreign, _, _) = shipped(&env, &gh, 5, "c.rs");
    let (after_foreign, _, _) = shipped(&env, &gh, 6, "c.rs");
    let (outside, _, sha7) = shipped(&env, &gh, 7, "d.rs");
    let (after_outside, _, _) = shipped(&env, &gh, 8, "d.rs");
    hold(&env, &held);
    append(
        &env,
        &red,
        "ci",
        Body::PrChecks {
            pr_url: url(3),
            passed: false,
            failing: vec![],
            ended: None,
            not_started: None,
        },
    );
    pr(&gh, 5, "MERGEABLE", "deadbeef", "", json!([]));
    pr(&gh, 7, "MERGEABLE", &sha7, "Depends on #99", json!([]));
    std::fs::write(gh.parent().unwrap().join("state-99.txt"), "OPEN").unwrap();

    let q = queue(&env, &gh);
    assert_eq!(status_of(&q, &held)["status"], "held", "{q}");
    assert_eq!(status_of(&q, &red)["status"], "ci_red", "{q}");
    assert_eq!(status_of(&q, &foreign)["status"], "not_ours", "{q}");
    assert_eq!(status_of(&q, &outside)["status"], "waits_for", "{q}");
    // The first of them is ready; the rest are queued behind it, not
    // waiting on a file.
    assert_eq!(status_of(&q, &after_held)["status"], "ready", "{q}");
    assert_eq!(status_of(&q, &after_red)["status"], "queued", "{q}");
    assert_eq!(status_of(&q, &after_foreign)["status"], "queued", "{q}");
    assert_eq!(status_of(&q, &after_outside)["status"], "queued", "{q}");
}

#[test]
fn a_conflicting_pr_on_a_lit_path_is_brought_up_to_date_before_you_are_asked() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    set_config(&env, "merge.lit", "[\"*.yml\"]");
    let (id, _, sha) = shipped(&env, &gh, 1, "ci.yml");
    pr(&gh, 1, "CONFLICTING", &sha, "", json!([]));
    let q = queue(&env, &gh);
    assert_eq!(status_of(&q, &id)["status"], "conflicting", "{q}");
    pass(&env, &gh);
    assert_eq!(state(&env, &id), "development");
    assert!(events(&env, &id, "held").is_empty());
}

/// Shipped again after its green run: its CI hasn't reported at the new
/// head.
fn ci_pending(env: &Env, id: &str, n: u64, sha: &str) {
    append(
        env,
        id,
        "shipper",
        Body::Shipped {
            sha: sha.into(),
            pr_url: Some(url(n)),
        },
    );
}

#[test]
fn a_conflicting_pr_whose_ci_never_reports_is_still_brought_up_to_date() {
    // GitHub runs no pull_request workflows on a conflicting PR, so waiting
    // for its CI would wait for good, and block every PR on its files.
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (id, _, sha) = shipped(&env, &gh, 1, "a.rs");
    let (after, _, _) = shipped(&env, &gh, 2, "a.rs");
    ci_pending(&env, &id, 1, &sha);
    pr(&gh, 1, "CONFLICTING", &sha, "", json!([]));

    let q = queue(&env, &gh);
    assert_eq!(status_of(&q, &id)["status"], "conflicting", "{q}");
    assert_eq!(status_of(&q, &after)["status"], "waits_for", "{q}");
    pass(&env, &gh);
    assert_eq!(events(&env, &id, "rebase_requested").len(), 1);
    assert_eq!(state(&env, &id), "development");
    assert!(events(&env, &after, "pr_merged").is_empty());
}

#[test]
fn while_a_merge_waits_for_delivery_other_lanes_are_still_brought_up_to_date() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (merged, _, _) = shipped(&env, &gh, 1, "a.rs");
    let (other_lane, _, _) = shipped(&env, &gh, 2, "b.rs");
    let (same_lane, _, _) = shipped(&env, &gh, 3, "a.rs");
    pass(&env, &gh);
    assert_eq!(events(&env, &merged, "pr_merged").len(), 1);
    // Main moves on both files before the watch records #1 delivered.
    commit_file(&env.repo, "a.rs", "// main's a.rs\n");
    commit_file(&env.repo, "b.rs", "// main's b.rs\n");
    git(&env.repo, &["push", "-q", "origin", "main"]);

    pass(&env, &gh);
    assert_eq!(events(&env, &other_lane, "rebase_requested").len(), 1);
    assert_eq!(state(&env, &same_lane), "closed");
    assert!(events(&env, &same_lane, "rebase_requested").is_empty());
    let q = queue(&env, &gh);
    assert_eq!(status_of(&q, &merged)["status"], "merging", "{q}");
    let e = status_of(&q, &same_lane);
    assert_eq!(e["status"], "waits_for", "{q}");
    assert_eq!(e["waits_for"], json!([url(1)]), "{q}");
}

#[test]
fn a_pr_waiting_for_a_reopened_one_still_claims_its_files() {
    // #3 doesn't jump #2: whichever merged second would be rebased twice.
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (back, wt, sha) = shipped(&env, &gh, 1, "a.rs");
    let (both, _, _) = shipped_files(&env, &gh, 2, &["a.rs", "b.rs"]);
    let (later, _, _) = shipped(&env, &gh, 3, "b.rs");
    reopen_in_review(&env, &back, &wt, 1, &sha);
    commit_file(&env.repo, "b.rs", "// main's b.rs\n");
    git(&env.repo, &["push", "-q", "origin", "main"]);

    let q = queue(&env, &gh);
    let e = status_of(&q, &both);
    assert_eq!(e["status"], "waits_for", "{q}");
    assert_eq!(e["waits_for"], json!([url(1)]), "{q}");
    let e = status_of(&q, &later);
    assert_eq!(e["status"], "waits_for", "{q}");
    assert_eq!(e["waits_for"], json!([url(2)]), "{q}");
    pass(&env, &gh);
    assert!(events(&env, &both, "rebase_requested").is_empty());
    assert!(events(&env, &later, "rebase_requested").is_empty());
}

#[test]
fn a_lit_pr_on_a_claimed_file_waits_before_you_are_asked() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    set_config(&env, "merge.lit", "[\"*.yml\"]");
    let (pending, _, sha) = shipped(&env, &gh, 1, "a.rs");
    let (lit, _, _) = shipped_files(&env, &gh, 2, &["a.rs", "ci.yml"]);
    ci_pending(&env, &pending, 1, &sha);

    let q = queue(&env, &gh);
    assert_eq!(status_of(&q, &pending)["status"], "ci_pending", "{q}");
    let e = status_of(&q, &lit);
    assert_eq!(e["status"], "waits_for", "{q}");
    assert_eq!(e["waits_for"], json!([url(1)]), "{q}");
    pass(&env, &gh);
    assert!(events(&env, &lit, "held").is_empty());
}

#[test]
fn a_pr_that_depends_on_a_held_one_claims_nothing() {
    // A person's hold on #1 stalls #2, which depends on it, but not #3,
    // which only shares a file with #2.
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (held, _, _) = shipped(&env, &gh, 1, "a.rs");
    let (dependent, _, sha2) = shipped(&env, &gh, 2, "b.rs");
    let (after, _, _) = shipped(&env, &gh, 3, "b.rs");
    pr(&gh, 2, "MERGEABLE", &sha2, "Depends on #1", json!([]));
    hold(&env, &held);

    let q = queue(&env, &gh);
    let e = status_of(&q, &dependent);
    assert_eq!(e["status"], "waits_for", "{q}");
    assert_eq!(e["waits_for"], json!([url(1)]), "{q}");
    assert_eq!(status_of(&q, &after)["status"], "ready", "{q}");
    pass(&env, &gh);
    assert_eq!(events(&env, &after, "pr_merged").len(), 1);
    assert!(events(&env, &dependent, "pr_merged").is_empty());
}

#[test]
fn a_pr_whose_ci_never_reports_stops_claiming_its_files_when_the_watch_gives_up() {
    // CI with a `paths-ignore` that skips every file #1 changes never
    // reports: once the CI watch gives up, #1 holds up only itself.
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (silent, _, sha) = shipped(&env, &gh, 1, "docs.md");
    let (after, _, _) = shipped_files(&env, &gh, 2, &["docs.md", "b.rs"]);
    ci_pending(&env, &silent, 1, &sha);
    let q = queue(&env, &gh);
    assert_eq!(status_of(&q, &after)["status"], "waits_for", "{q}");

    // Shipped a day ago, after a green run on an earlier head.
    let ago = |h| (chrono::Utc::now() - chrono::Duration::hours(h)).to_rfc3339();
    let (shipped_at, green_at) = (ago(25), ago(26));
    edit_journal(&env, &silent, |v| {
        if v["event"] == "shipped" {
            v["t"] = json!(shipped_at);
        } else if v["event"] == "pr_checks" {
            v["t"] = json!(green_at);
        }
    });
    let q = queue(&env, &gh);
    let e = status_of(&q, &silent);
    assert_eq!(e["status"], "ci_pending", "{q}");
    assert!(e["why"].as_str().unwrap().contains("gave up"), "{q}");
    assert_eq!(status_of(&q, &after)["status"], "ready", "{q}");
    pass(&env, &gh);
    assert_eq!(events(&env, &after, "pr_merged").len(), 1);
}

#[test]
fn each_pass_keeps_its_plan_for_the_terminal_app() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (first, _, _) = shipped(&env, &gh, 1, "a.rs");
    let (second, _, _) = shipped(&env, &gh, 2, "b.rs");
    let (at, plan) = hallmark::merger::last_plan(&hall(&env));
    assert!(at.is_none() && plan.is_empty(), "no pass yet");

    pass(&env, &gh);
    let (at, plan) = hallmark::merger::last_plan(&hall(&env));
    assert!(at.is_some());
    let got: Vec<(String, &str, u32)> = plan
        .iter()
        .map(|e| (e.item.to_string(), e.status.as_str(), e.position))
        .collect();
    assert_eq!(
        got,
        vec![(first.clone(), "ready", 1), (second.clone(), "queued", 2)]
    );

    // The last PR left: the plan is empty, not the last one.
    delivered(&env, &first, 1);
    pass(&env, &gh);
    delivered(&env, &second, 2);
    pass(&env, &gh);
    let (_, plan) = hallmark::merger::last_plan(&hall(&env));
    assert!(plan.is_empty(), "{plan:?}");
}

/// What the fake GitHub says about the check runs on the base's tip.
fn tip_checks(gh: &Path, runs: Value) {
    std::fs::write(
        gh.parent().unwrap().join("tip-checks.json"),
        json!({ "check_runs": runs }).to_string(),
    )
    .unwrap();
}

#[test]
fn with_update_conflicts_a_clean_green_pr_merges_without_a_rebase() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    set_config(&env, "merge.update", "\"conflicts\"");
    let (clean, _, _) = shipped(&env, &gh, 1, "a.rs");
    let (conflicting, _, sha2) = shipped(&env, &gh, 2, "b.rs");
    pr(&gh, 2, "CONFLICTING", &sha2, "", json!([]));
    commit_file(&env.repo, "a.rs", "// main's a.rs\n");
    commit_file(&env.repo, "b.rs", "// main's b.rs\n");
    git(&env.repo, &["push", "-q", "origin", "main"]);

    let q = queue(&env, &gh);
    let e = status_of(&q, &clean);
    assert_eq!(e["status"], "ready", "{q}");
    assert!(
        e["why"]
            .as_str()
            .unwrap()
            .contains("merges cleanly as it is"),
        "{q}"
    );
    // A real conflict is still brought up to date.
    assert_eq!(status_of(&q, &conflicting)["status"], "conflicting", "{q}");
    pass(&env, &gh);
    assert_eq!(events(&env, &clean, "pr_merged").len(), 1);
    assert!(events(&env, &clean, "rebase_requested").is_empty());
    assert_eq!(events(&env, &conflicting, "rebase_requested").len(), 1);
}

#[test]
fn by_default_a_pr_behind_on_its_files_is_still_rebased() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (behind, _, _) = shipped(&env, &gh, 1, "a.rs");
    commit_file(&env.repo, "a.rs", "// main's a.rs\n");
    git(&env.repo, &["push", "-q", "origin", "main"]);
    pass(&env, &gh);
    assert!(events(&env, &behind, "pr_merged").is_empty());
    assert_eq!(events(&env, &behind, "rebase_requested").len(), 1);
}

#[test]
fn a_red_base_stops_the_line_until_it_is_green_again() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (ready, _, _) = shipped(&env, &gh, 1, "a.rs");
    let (behind, _, _) = shipped(&env, &gh, 2, "b.rs");
    commit_file(&env.repo, "b.rs", "// main's b.rs\n");
    git(&env.repo, &["push", "-q", "origin", "main"]);
    tip_checks(
        &gh,
        json!([
            {"name": "test (ubuntu-latest)", "conclusion": "failure"},
            {"name": "fmt", "conclusion": "success"},
            {"name": "Scorecard", "conclusion": "cancelled"},
            {"name": "clippy", "conclusion": null}
        ]),
    );

    let q = queue(&env, &gh);
    let e = status_of(&q, &ready);
    assert_eq!(e["status"], "queued", "{q}");
    assert!(
        e["why"]
            .as_str()
            .unwrap()
            .contains("main is red (test (ubuntu-latest) failed)"),
        "{q}"
    );
    pass(&env, &gh);
    assert!(events(&env, &ready, "pr_merged").is_empty());
    assert!(events(&env, &behind, "rebase_requested").is_empty());
    assert!(inbox_has(&env, "base_red"));

    // The entry names the tip and the checks that failed.
    let inbox = env.hall(&env.repo, &["inbox"]).json["entries"].to_string();
    assert!(inbox.contains("test (ubuntu-latest)"), "{inbox}");
    assert!(
        inbox.contains(&git(&env.repo, &["rev-parse", "origin/main"])[..7]),
        "{inbox}"
    );

    // Green again, with only a cancelled run besides: it goes on.
    tip_checks(
        &gh,
        json!([
            {"name": "test (ubuntu-latest)", "conclusion": "success"},
            {"name": "Scorecard", "conclusion": "cancelled"}
        ]),
    );
    pass(&env, &gh);
    assert_eq!(events(&env, &ready, "pr_merged").len(), 1);
    assert_eq!(events(&env, &behind, "rebase_requested").len(), 1);
    assert!(!inbox_has(&env, "base_red"));
}

#[test]
fn a_merge_waits_while_the_base_tip_is_still_running_checks() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (ready, _, _) = shipped(&env, &gh, 1, "a.rs");
    let (behind, _, _) = shipped(&env, &gh, 2, "b.rs");
    commit_file(&env.repo, "b.rs", "// main's b.rs\n");
    git(&env.repo, &["push", "-q", "origin", "main"]);
    tip_checks(
        &gh,
        json!([
            {"name": "test", "conclusion": null},
            {"name": "fmt", "conclusion": "success"}
        ]),
    );
    let q = queue(&env, &gh);
    let e = status_of(&q, &ready);
    assert_eq!(e["status"], "queued", "{q}");
    assert!(
        e["why"].as_str().unwrap().contains("CI is still running"),
        "{q}"
    );
    pass(&env, &gh);
    assert!(events(&env, &ready, "pr_merged").is_empty());
    // Rebases go on; nothing red is recorded.
    assert_eq!(events(&env, &behind, "rebase_requested").len(), 1);
    assert!(!inbox_has(&env, "base_red"));

    tip_checks(&gh, json!([{"name": "test", "conclusion": "success"}]));
    pass(&env, &gh);
    assert_eq!(events(&env, &ready, "pr_merged").len(), 1);
}

#[test]
fn base_red_leaves_when_the_base_goes_green_with_every_pr_waiting_on_ci() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (pending, _, _) = shipped_ci(&env, &gh, 1, &["a.rs"], false);
    let (ready, _, _) = shipped(&env, &gh, 2, "b.rs");
    tip_checks(&gh, json!([{"name": "test", "conclusion": "failure"}]));
    pass(&env, &gh);
    assert!(inbox_has(&env, "base_red"));
    assert!(events(&env, &ready, "pr_merged").is_empty());
    // The only ready PR leaves the line (the merger reads the journal): nothing
    // is ready, and the base turns green.
    delivered(&env, &ready, 2);
    tip_checks(&gh, json!([{"name": "test", "conclusion": "success"}]));
    pass(&env, &gh);
    assert!(!inbox_has(&env, "base_red"));
    assert!(events(&env, &pending, "pr_merged").is_empty());
    assert!(events(&env, &ready, "pr_merged").is_empty());
}

#[test]
fn a_timed_out_check_on_the_tip_stops_the_line() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (ready, _, _) = shipped(&env, &gh, 1, "a.rs");
    tip_checks(&gh, json!([{"name": "audit", "conclusion": "timed_out"}]));
    pass(&env, &gh);
    assert!(events(&env, &ready, "pr_merged").is_empty());
    assert!(inbox_has(&env, "base_red"));
}

#[test]
fn a_lit_pr_on_a_red_base_is_not_put_to_a_person() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    set_config(&env, "merge.lit", "[\"*.yml\"]");
    let (lit, _, _) = shipped(&env, &gh, 1, "ci.yml");
    tip_checks(&gh, json!([{"name": "test", "conclusion": "failure"}]));
    let q = queue(&env, &gh);
    assert_eq!(status_of(&q, &lit)["status"], "queued", "{q}");
    pass(&env, &gh);
    assert!(events(&env, &lit, "held").is_empty());
    assert!(!inbox_has(&env, "lit"));
}

#[test]
fn turning_the_merger_off_clears_base_red() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    shipped(&env, &gh, 1, "a.rs");
    tip_checks(&gh, json!([{"name": "test", "conclusion": "failure"}]));
    pass(&env, &gh);
    assert!(inbox_has(&env, "base_red"));
    set_config(&env, "merge.mode", "\"off\"");
    pass(&env, &gh);
    assert!(!inbox_has(&env, "base_red"));
}

#[test]
fn an_unreadable_tip_keeps_a_red_base_stopped() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (ready, _, _) = shipped(&env, &gh, 1, "a.rs");
    tip_checks(&gh, json!([{"name": "test", "conclusion": "failure"}]));
    pass(&env, &gh);
    assert!(inbox_has(&env, "base_red"));

    // The check-runs call now fails: main is still the same red tip.
    std::fs::remove_file(gh.parent().unwrap().join("tip-checks.json")).unwrap();
    // `hall queue` agrees with the worker: still stopped, guard not "off".
    let q = queue(&env, &gh);
    let e = status_of(&q, &ready);
    assert_eq!(e["status"], "queued", "{q}");
    assert!(!e["why"].as_str().unwrap().contains("guard is off"), "{q}");
    pass(&env, &gh);
    assert!(events(&env, &ready, "pr_merged").is_empty());
    assert!(inbox_has(&env, "base_red"));
}

#[test]
fn an_unreadable_tip_says_the_guard_is_off() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (ready, _, _) = shipped(&env, &gh, 1, "a.rs");
    let q = queue(&env, &gh);
    assert!(
        status_of(&q, &ready)["why"]
            .as_str()
            .unwrap()
            .contains("red-base guard is off"),
        "{q}"
    );
    pass(&env, &gh);
    assert_eq!(events(&env, &ready, "pr_merged").len(), 1);
}

/// Make the merger think it first saw its tip pending `secs` ago.
fn age_pending(env: &Env, secs: i64) {
    let state = env.repo.join(".git/hallmark/merger.json");
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    let old = (chrono::Utc::now() - chrono::Duration::seconds(secs)).to_rfc3339();
    for (_, p) in v["pending"].as_object_mut().unwrap() {
        p[1] = json!(old);
    }
    std::fs::write(&state, v.to_string()).unwrap();
}

#[test]
fn a_tip_with_no_check_runs_yet_is_waited_for_then_counts_as_green() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (first, _, _) = shipped(&env, &gh, 1, "a.rs");
    tip_checks(&gh, json!([]));
    pass(&env, &gh);
    assert!(events(&env, &first, "pr_merged").is_empty());
    // Still nothing long after: the repo has no CI on the base.
    age_pending(&env, 3600);
    pass(&env, &gh);
    assert_eq!(events(&env, &first, "pr_merged").len(), 1);
}

#[test]
fn a_check_waiting_on_a_person_is_not_running_ci() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (first, _, _) = shipped(&env, &gh, 1, "a.rs");
    tip_checks(
        &gh,
        json!([
            {"name": "test", "status": "completed", "conclusion": "success"},
            {"name": "deploy", "status": "waiting", "conclusion": null}
        ]),
    );
    pass(&env, &gh);
    assert_eq!(events(&env, &first, "pr_merged").len(), 1);
}

#[test]
fn checks_running_for_too_long_stop_holding_the_line() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (first, _, _) = shipped(&env, &gh, 1, "a.rs");
    tip_checks(
        &gh,
        json!([{"name": "test", "status": "in_progress", "conclusion": null}]),
    );
    pass(&env, &gh);
    assert!(events(&env, &first, "pr_merged").is_empty());
    age_pending(&env, 7 * 3600);
    pass(&env, &gh);
    assert_eq!(events(&env, &first, "pr_merged").len(), 1);
}

#[test]
fn a_red_base_stays_in_the_inbox_while_the_fix_is_still_running() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (ready, _, _) = shipped(&env, &gh, 1, "a.rs");
    tip_checks(&gh, json!([{"name": "test", "conclusion": "failure"}]));
    pass(&env, &gh);
    assert!(inbox_has(&env, "base_red"));
    // A person merged the fix by hand: main moved, its checks are running.
    commit_file(&env.repo, "fix.rs", "// fix\n");
    git(&env.repo, &["push", "-q", "origin", "main"]);
    tip_checks(&gh, json!([{"name": "test", "conclusion": null}]));
    pass(&env, &gh);
    assert!(inbox_has(&env, "base_red"));
    assert!(events(&env, &ready, "pr_merged").is_empty());
    tip_checks(&gh, json!([{"name": "test", "conclusion": "success"}]));
    pass(&env, &gh);
    assert!(!inbox_has(&env, "base_red"));
}

#[test]
fn base_red_leaves_when_the_queue_empties() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    let (only, _, _) = shipped(&env, &gh, 1, "a.rs");
    tip_checks(&gh, json!([{"name": "test", "conclusion": "failure"}]));
    pass(&env, &gh);
    assert!(inbox_has(&env, "base_red"));
    delivered(&env, &only, 1);
    pass(&env, &gh);
    assert!(!inbox_has(&env, "base_red"));
}

#[test]
fn the_by_hand_hint_names_the_repos_own_merge_command() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    setup(&env);
    set_config(&env, "merge.command", "\"gh pr merge {pr} --merge\"");
    shipped(&env, &gh, 1, "a.rs");
    tip_checks(&gh, json!([{"name": "test", "conclusion": "failure"}]));
    let q = queue(&env, &gh);
    assert!(q.to_string().contains("gh pr merge {pr} --merge"), "{q}");
    pass(&env, &gh);
    let inbox = env.hall(&env.repo, &["inbox"]).json["entries"].to_string();
    assert!(inbox.contains("gh pr merge {pr} --merge"), "{inbox}");
    assert!(!inbox.contains("--admin"), "{inbox}");
}
