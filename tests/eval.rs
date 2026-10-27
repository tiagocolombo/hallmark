//! `hall eval` (ADR 0065): today's reviewer replayed on the changes the
//! loop got wrong, scored against what the revert touched.

mod c_support;
mod common;

use c_support::*;
use common::{Env, commit_file, git};
use hallmark::event::Body;
use insta::assert_json_snapshot;
use serde_json::json;

const URL: &str = "https://github.com/acme/app/pull/7";

/// A hallmarked item whose PR merged and was then reverted on main by a
/// commit that touches `retry.rs`. Returns the item, its reviewed commit
/// and the revert commit.
fn reverted_item(env: &Env) -> (String, String, String) {
    let (id, _, sha) = hallmarked_item(env, "Retry webhooks");
    let revert = revert_it(env, &id, &sha, "retry.rs", 0);
    (id, sha, revert)
}

/// Ships `id` at `sha`, merges it and reverts it on main, `age_days` ago.
/// Touches `file` on main.
fn revert_it(env: &Env, id: &str, sha: &str, file: &str, age_days: i64) -> String {
    let (id, sha) = (id.to_string(), sha.to_string());
    append(
        env,
        &id,
        "shipper",
        Body::Shipped {
            sha: sha.clone(),
            pr_url: Some(URL.into()),
        },
    );
    append(
        env,
        &id,
        "delivery",
        Body::Delivered {
            pr_url: URL.into(),
            outcome: "merged".into(),
        },
    );
    let landed = commit_file(&env.repo, file, "fn retry() {}\n");
    let revert = commit_file(&env.repo, file, "");
    let item: hallmark::item::ItemId = id.parse().unwrap();
    let mut ev = hallmark::event::Event::new(
        "reverts",
        Some(item),
        Body::Reverted {
            pr_url: URL.into(),
            base: "main".into(),
            sha: landed,
            revert_sha: revert.clone(),
            subject: "Revert \"Retry webhooks (#7)\"".into(),
        },
    );
    ev.t -= chrono::Duration::days(age_days);
    hall(env).append_item(item, &ev).unwrap();
    assert_eq!(env.hall(&env.repo, &["reindex"]).code, 0);
    revert
}

fn eval_step(findings: serde_json::Value) -> serde_json::Value {
    json!({
        "role": "eval",
        "structured": {
            "summary": "Replayed.",
            "findings": findings,
            "prior": [],
            "learnings": []
        },
        "model": "claude-opus-5-5"
    })
}

#[test]
fn a_dry_run_lists_the_reverted_items_and_scores_the_verdict_of_the_time() {
    let env = Env::initialized();
    let (id, sha, revert) = reverted_item(&env);
    // An item that was never reverted is not a case.
    env.new_item("Other");

    let r = env.hall(&env.repo, &["eval", "--dry-run"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["dry_run"], true);
    let cases = r.json["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 1, "{}", r.stdout);
    let c = &cases[0];
    assert_eq!(c["item"], id.as_str());
    assert_eq!(c["sha"], sha.as_str());
    assert_eq!(c["reverted_by"], revert.as_str());
    assert_eq!(c["revert_files"], json!(["retry.rs"]));
    // The round-1 review approved it with a minor riding along: missed.
    assert_eq!(c["then"]["rejected"], false);
    assert_eq!(c["then"]["caught"], false);
    assert_eq!(c["then"]["findings"], 1);
    assert!(c.get("now").is_none());
    assert_eq!(r.json["totals"]["cases"], 1);
    assert_eq!(r.json["totals"]["ran"], 0);
    assert!(env.fake_calls().is_empty());
}

#[test]
fn the_replay_runs_todays_reviewer_as_a_round_one_review_and_scores_it() {
    let env = Env::initialized();
    let (id, sha, _) = reverted_item(&env);
    let script = env.script(json!([eval_step(json!([{
        "lens": "correctness", "severity": "blocker", "file": "retry.rs", "line": 1,
        "title": "Two workers can retry the same delivery",
        "scenario": "both read attempt 2 before either writes",
        "fix": "claim the delivery with a row lock first"
    }, {
        "lens": "repo_rules", "severity": "nit", "file": "README.md", "line": 1,
        "title": "Name it", "scenario": "s", "fix": "f"
    }]))]));
    let r = env.hall_fake(&env.repo, &script, &["eval"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let c = &r.json["cases"][0];
    assert_eq!(c["now"]["rejected"], true);
    assert_eq!(c["now"]["caught"], true);
    assert_eq!(c["now"]["findings"], 2);
    assert_eq!(c["now"]["blocking"], 1);
    assert_eq!(c["now"]["model"], "claude-opus-5-5");
    assert_eq!(
        c["now"]["titles"],
        json!(["blocker · retry.rs · Two workers can retry the same delivery"])
    );
    let t = &r.json["totals"];
    assert_eq!((t["cases"].as_u64(), t["ran"].as_u64()), (Some(1), Some(1)));
    assert_eq!(
        (t["then_caught"].as_u64(), t["now_caught"].as_u64()),
        (Some(0), Some(1))
    );

    // The run: the reviewer's agent and schema, at the reviewed commit, as
    // a round-1 review with nothing to rule on, under the eval role.
    let calls = env.fake_calls();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call["role"], "eval");
    assert_eq!(call["room"], "assay");
    let args: Vec<&str> = call["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    let after = |flag: &str| args.iter().position(|a| *a == flag).map(|i| args[i + 1]);
    assert_eq!(after("--agent"), Some("hallmark:principal-engineer"));
    assert!(after("--json-schema").unwrap().contains("Hallmark verdict"));
    let prompt = call["prompt"].as_str().unwrap();
    assert!(
        prompt.contains(&format!("# Review {id} round 1")),
        "{prompt}"
    );
    assert!(prompt.contains(&format!("commit {sha}")), "{prompt}");
    assert!(!prompt.contains("## Open findings"), "{prompt}");
    assert!(!prompt.contains("The diff since round"), "{prompt}");
    // The checkout is gone, and the item's journal has no new verdict.
    let cwd = std::path::PathBuf::from(call["cwd"].as_str().unwrap());
    assert!(!cwd.exists());
    assert_eq!(events(&env, &id, "verdict").len(), 1);
    assert!(events(&env, &id, "run_failed").is_empty());
}

#[test]
fn a_failed_replay_is_said_and_an_unreverted_item_is_refused() {
    let env = Env::initialized();
    let (id, _, _) = reverted_item(&env);
    // No step for the eval role: the run fails, the case says so, nothing
    // is held.
    let script = env.script(json!([]));
    let r = env.hall_fake(&env.repo, &script, &["eval", &id]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let c = &r.json["cases"][0];
    assert!(c.get("now").is_none());
    assert!(
        c["skipped"].as_str().unwrap().starts_with("the run failed"),
        "{c}"
    );
    assert_eq!(r.json["totals"]["ran"], 0);
    assert!(events(&env, &id, "held").is_empty());

    let (other, _) = env.new_item("Never reverted");
    let r = env.hall_fake(&env.repo, &script, &["eval", &other]);
    assert_eq!(r.code, 4, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "WRONG_STATE");
    let r = env.hall(&env.repo, &["eval", "--since", "soon"]);
    assert_eq!(r.code, 2);
}

#[test]
fn the_limit_the_window_and_the_order_pick_the_newest_reverts() {
    let env = Env::initialized();
    let mut ids = Vec::new();
    for (n, age) in [(1, 40), (2, 20), (3, 0)] {
        let (id, _, sha) = hallmarked_item(&env, &format!("Item {n}"));
        revert_it(&env, &id, &sha, &format!("f{n}.rs"), age);
        ids.push(id);
    }
    let items = |args: &[&str]| -> Vec<String> {
        let mut a = vec!["eval", "--dry-run"];
        a.extend_from_slice(args);
        let r = env.hall(&env.repo, &a);
        assert_eq!(r.code, 0, "{}", r.stdout);
        r.json["cases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["item"].as_str().unwrap().to_string())
            .collect()
    };
    let newest_first = vec![ids[2].clone(), ids[1].clone(), ids[0].clone()];
    assert_eq!(items(&[]), newest_first);
    assert_eq!(items(&["--limit", "2"]), newest_first[..2]);
    assert_eq!(items(&["--since", "5d"]), newest_first[..1]);
    assert_eq!(items(&["--since", "30d"]), newest_first[..2]);
}

#[test]
fn a_fix_round_does_not_turn_the_replay_into_a_rebase_review() {
    let env = Env::initialized();
    let (id, wt, a) = fixing_item(&env, "Retry webhooks");
    // The fixer's commit is the one that was approved and hallmarked.
    let b = commit_file(
        &wt,
        "retry.rs",
        "fn retry() {\n    lock();\n    deliver();\n}\n",
    );
    let repo = hallmark::repo::Repo::discover(&env.repo).unwrap();
    hallmark::stamp::strike(
        &repo,
        id.parse().unwrap(),
        &b,
        2,
        Some("claude-opus-5-5".into()),
        "assayer",
        counts(0, 0, 0, 0),
    )
    .unwrap();
    revert_it(&env, &id, &b, "retry.rs", 0);

    let script = env.script(json!([eval_step(json!([]))]));
    let r = env.hall_fake(&env.repo, &script, &["eval"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let c = &r.json["cases"][0];
    assert_eq!(c["sha"], a.as_str());
    assert_eq!(c["then"]["rejected"], true);
    let calls = env.fake_calls();
    let prompt = calls[0]["prompt"].as_str().unwrap();
    assert!(!prompt.contains("What the rebase changed"), "{prompt}");
    assert!(!prompt.contains("was approved at"), "{prompt}");
    assert!(prompt.contains(&format!("commit {a}")), "{prompt}");
    assert!(prompt.contains("+    deliver();"), "{prompt}");
}

#[test]
fn a_learning_from_the_reverted_item_does_not_brief_its_replay() {
    let env = Env::initialized();
    let (id, _, _) = reverted_item(&env);
    let dir = env.repo.join(".git/hallmark/library/learnings");
    std::fs::create_dir_all(&dir).unwrap();
    let learning = |n: u32, source: &str, title: &str| {
        let text = format!(
            "---\nid: L-000{n}\nkind: rule\nscope: [\"retry.rs\"]\nlens: correctness\nsource: [{source}]\nstatus: accepted\nused: 0\n---\n# {title}\n\nbody {n}\n"
        );
        std::fs::write(dir.join(format!("L-000{n}-x.md")), text).unwrap();
    };
    learning(1, &format!("\"{id}/F1\""), "Learned from the revert");
    learning(2, "\"H-0900/F1\"", "Learned elsewhere");
    let script = env.script(json!([eval_step(json!([]))]));
    let r = env.hall_fake(&env.repo, &script, &["eval"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let calls = env.fake_calls();
    let prompt = calls[0]["prompt"].as_str().unwrap();
    assert!(prompt.contains("Learned elsewhere"), "{prompt}");
    assert!(!prompt.contains("Learned from the revert"), "{prompt}");
}

#[test]
fn replies_keep_their_shape() {
    // The dry-run and replay replies are pinned by snapshots: a renamed
    // field (`now_caught` is what ADR 0065 tells people to compare between
    // runs) or a dropped one is a contract change, reviewed on the `.snap`.
    let env = Env::initialized();
    let (id, _, _) = reverted_item(&env);
    let dry = env.hall(&env.repo, &["eval", "--dry-run"]);
    assert_eq!(dry.code, 0, "{}", dry.stdout);
    assert_json_snapshot!("eval_dry_run", dry.json, {
        ".cases[].sha" => "[sha]",
        ".cases[].reverted_by" => "[sha]",
    });
    let script = env.script(json!([eval_step(json!([{
        "lens": "correctness", "severity": "blocker", "file": "retry.rs", "line": 1,
        "title": "Two workers", "scenario": "s", "fix": "f"
    }]))]));
    let r = env.hall_fake(&env.repo, &script, &["eval", &id]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_json_snapshot!("eval_replay", r.json, {
        ".cases[].sha" => "[sha]",
        ".cases[].reverted_by" => "[sha]",
        ".cases[].now.cost_usd" => "[usd]",
        ".totals.cost_usd" => "[usd]",
    });
}

#[test]
fn a_merge_commit_merge_leaves_no_diff_and_the_case_is_skipped() {
    let env = Env::initialized();
    let (id, _, sha) = hallmarked_item(&env, "Retry webhooks");
    git(&env.repo, &["merge", "--no-ff", "-q", "-m", "merge", &sha]);
    revert_it(&env, &id, &sha, "retry.rs", 0);
    let script = env.script(json!([eval_step(json!([]))]));
    let r = env.hall_fake(&env.repo, &script, &["eval"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let c = &r.json["cases"][0];
    assert!(c.get("now").is_none(), "{c}");
    assert!(
        c["skipped"].as_str().unwrap().contains("no diff to replay"),
        "{c}"
    );
    assert!(env.fake_calls().is_empty());
}

#[test]
fn an_item_with_no_review_is_not_a_case_and_naming_it_is_refused() {
    let env = Env::initialized();
    let (reviewed, _, _) = reverted_item(&env);
    let (bare, _) = env.new_item("Shipped without a review");
    let head = git(&env.repo, &["rev-parse", "HEAD"]);
    revert_it(&env, &bare, &head, "other.rs", 0);

    let r = env.hall(&env.repo, &["eval", "--dry-run", "--limit", "1"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let cases = r.json["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 1, "{}", r.stdout);
    assert_eq!(cases[0]["item"], reviewed.as_str());

    let r = env.hall(&env.repo, &["eval", "--dry-run", &bare]);
    assert_eq!(r.code, 4, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "WRONG_STATE");
}
