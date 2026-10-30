//! ADR 0066: with `[library] commit = true`, the librarian carries the
//! Library's accepted learnings and the Gazette pages to the base through a
//! records item, reviewed and shipped like any other work. Driven through
//! the daemon's librarian tick with the fake runner.

mod c_support;
mod common;

use std::path::PathBuf;

use c_support::*;
use common::{Env, git};
use hallmark::event::Body;
use serde_json::{Value, json};

fn tick(env: &Env, script: &std::path::Path) -> Value {
    let r = env.hall_fake(
        &env.repo,
        script,
        &["daemon", "run", "--once", "--only", "librarian"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    r.json["steps"][0].clone()
}

fn what(step: &Value) -> String {
    step["what"].as_str().unwrap_or_default().to_string()
}

fn config(env: &Env, args: &[&str]) {
    let mut a = vec!["config", "set"];
    a.extend_from_slice(args);
    let r = env.hall(&env.repo, &a);
    assert_eq!(r.code, 0, "{}", r.stdout);
}

/// A hallmarked item that shipped as PR 7: closed, ready to distil.
fn closed(env: &Env) -> String {
    let (id, _, sha) = hallmarked_item(env, "Retry webhooks");
    append(
        env,
        &id,
        "shipper",
        Body::Shipped {
            sha,
            pr_url: Some("https://github.com/acme/app/pull/7".into()),
        },
    );
    assert_eq!(state(env, &id), "closed");
    id
}

fn script(env: &Env, id: &str) -> PathBuf {
    env.script(json!([{"role": "librarian", "structured": {
        "learnings": [{"updates": "", "kind": "rule", "scope": ["src/**"], "lens": "correctness",
                       "source": [format!("{id}/F1")], "title": "Retries need a ceiling",
                       "body": "An endless retry loop hides a dead endpoint."}],
        "retire": [],
        "gazette": {"kicker": "Webhooks", "headline": "Webhooks retry, and stop",
                    "deck": "One round.", "story": ["It retries."]}
    }}]))
}

fn status(env: &Env, id: &str) -> Value {
    let r = env.hall(&env.repo, &["status", id]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    r.json
}

#[test]
fn the_librarian_carries_the_records_to_the_base_through_a_records_item() {
    let env = Env::initialized();
    config(&env, &["library.commit", "true"]);
    config(&env, &["library.auto_accept", "rule"]);
    git(&env.repo, &["commit", "-q", "-am", "Keep the records"]);
    let id = closed(&env);
    let script = script(&env, &id);

    assert!(what(&tick(&env, &script)).contains(&format!("distilled {id}")));

    // The next tick starts a records item. Until one has shipped, each waits
    // for you.
    let step = tick(&env, &script);
    let w = what(&step);
    assert!(w.contains("H-0002 carries the records to main"), "{step}");
    assert!(w.contains("waits for you"), "{step}");
    let rec = status(&env, "H-0002");
    assert_eq!(
        rec["title"], "Records: Gazette for H-0001; 1 learning",
        "{rec}"
    );
    assert_eq!(rec["state"], "awaiting_you", "{rec}");
    let wt = PathBuf::from(rec["worktree"].as_str().unwrap());

    // Its commit: the page as printed, and the learning without its usage.
    let page = env
        .repo
        .join(".git/hallmark/items")
        .join(&id)
        .join("gazette.html");
    assert_eq!(
        std::fs::read(wt.join("docs/gazette/H-0001.html")).unwrap(),
        std::fs::read(&page).unwrap()
    );
    let learning =
        std::fs::read_to_string(wt.join(".hallmark/knowledge/L-0001-retries-need-a-ceiling.md"))
            .unwrap();
    assert!(learning.contains("status: accepted"), "{learning}");
    assert!(learning.contains("used: 0"), "{learning}");
    assert!(!learning.contains("last_used"), "{learning}");
    assert_eq!(
        git(&wt, &["log", "-1", "--format=%s"]),
        "docs: Hallmark records"
    );
    assert_eq!(git(&wt, &["status", "--porcelain"]), "");
    // The reviewer is told what to look for; the librarian did it all.
    let journal = env.journal("H-0002");
    let created = &journal[0];
    assert_eq!(created["actor"], "librarian");
    assert!(
        created["request"]
            .as_str()
            .unwrap()
            .contains("must not be published"),
        "{created}"
    );
    assert!(
        journal
            .iter()
            .any(|e| e["event"] == "proposed_ready" && e["actor"] == "librarian")
    );

    // Nothing else starts while it is open; a records item is never
    // distilled.
    assert_eq!(tick(&env, &script)["progress"], "idle");
    let r = env.hall(&env.repo, &["library", "distill", "H-0002"]);
    assert_ne!(r.code, 0, "{}", r.stdout);

    // You drop it: it doesn't come straight back.
    let r = env.hall(&env.repo, &["drop", "H-0002", "--reason", "not this one"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(tick(&env, &script)["progress"], "idle");

    // The PR merges: the page is reprinted from more events, so the next
    // records item carries it. The one you dropped was no yes: it waits too.
    append(
        &env,
        &id,
        "watchman",
        Body::Delivered {
            pr_url: "https://github.com/acme/app/pull/7".into(),
            outcome: "merged".into(),
        },
    );
    assert!(what(&tick(&env, &script)).contains("reprinted the Gazette"));
    let w = what(&tick(&env, &script));
    assert!(w.contains("H-0003 carries the records"), "{w}");
    assert!(w.contains("waits for you"), "{w}");
    let rec = status(&env, "H-0003");
    assert_eq!(rec["state"], "awaiting_you", "{rec}");
    let wt = PathBuf::from(rec["worktree"].as_str().unwrap());
    assert_eq!(
        std::fs::read(wt.join("docs/gazette/H-0001.html")).unwrap(),
        std::fs::read(&page).unwrap()
    );

    // You send it; it is reviewed and ships. While its PR is open nothing
    // else starts.
    let r = env.hall(&env.repo, &["ready", "H-0003"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let sha = approve(&env, "H-0003", &wt, 1, vec![], counts(0, 0, 0, 0));
    let pr = "https://github.com/acme/app/pull/8";
    append(
        &env,
        "H-0003",
        "shipper",
        Body::Shipped {
            sha,
            pr_url: Some(pr.into()),
        },
    );
    assert_eq!(state(&env, "H-0003"), "closed");
    assert_eq!(tick(&env, &script)["progress"], "idle");

    // It merges: the base has the records, and nothing is left to do.
    let branch = rec["branch"].as_str().unwrap().to_string();
    git(&env.repo, &["merge", "-q", "--ff-only", &branch]);
    append(
        &env,
        "H-0003",
        "watchman",
        Body::Delivered {
            pr_url: pr.into(),
            outcome: "merged".into(),
        },
    );
    assert_eq!(tick(&env, &script)["progress"], "idle");

    // Another hall printed the base's copy from the same events: it stands.
    let copy = env.repo.join("docs/gazette/H-0001.html");
    let text = std::fs::read_to_string(&copy).unwrap();
    let older = text.replacen("content=\"hall ", "content=\"hall 0.0.1-", 1);
    assert_ne!(older, text);
    std::fs::write(&copy, older).unwrap();
    git(&env.repo, &["commit", "-q", "-am", "An older printing"]);
    assert_eq!(tick(&env, &script)["progress"], "idle");

    // A learning is retired: the next records item takes it out. One has
    // shipped, so it goes straight to review.
    let r = env.hall(
        &env.repo,
        &["library", "retire", "L-0001", "--reason", "no longer true"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    let w = what(&tick(&env, &script));
    assert!(w.contains("H-0004 carries the records"), "{w}");
    assert!(!w.contains("waits for you"), "{w}");
    let rec = status(&env, "H-0004");
    assert_eq!(rec["title"], "Records: 1 removed", "{rec}");
    assert_eq!(rec["state"], "checks", "{rec}");
    let wt = PathBuf::from(rec["worktree"].as_str().unwrap());
    assert!(
        !wt.join(".hallmark/knowledge/L-0001-retries-need-a-ceiling.md")
            .exists()
    );
    assert!(wt.join("docs/gazette/H-0001.html").exists());
    // Only the librarian's run cost anything.
    assert_eq!(env.fake_calls().len(), 1);
}

#[test]
fn learnings_the_store_doesnt_know_are_left_alone() {
    let env = Env::initialized();
    config(&env, &["library.commit", "true"]);
    config(&env, &["library.auto_accept", "rule"]);
    let other = env.repo.join(".hallmark/knowledge");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(
        other.join("L-0042-from-another-clone.md"),
        "---\nid: L-0042\nkind: rule\nstatus: accepted\n---\n# From another clone\n",
    )
    .unwrap();
    git(&env.repo, &["add", "-A"]);
    git(
        &env.repo,
        &["commit", "-q", "-m", "Another clone's learning"],
    );
    let id = closed(&env);
    let script = script(&env, &id);
    tick(&env, &script);
    assert!(what(&tick(&env, &script)).contains("H-0002 carries the records"));
    let wt = PathBuf::from(status(&env, "H-0002")["worktree"].as_str().unwrap());
    assert!(
        wt.join(".hallmark/knowledge/L-0042-from-another-clone.md")
            .exists()
    );
}

#[test]
fn off_by_default_nothing_is_committed() {
    let env = Env::initialized();
    config(&env, &["library.auto_accept", "rule"]);
    let id = closed(&env);
    let script = script(&env, &id);
    assert!(what(&tick(&env, &script)).contains(&format!("distilled {id}")));
    assert_eq!(tick(&env, &script)["progress"], "idle");
    let r = env.hall(&env.repo, &["status", "H-0002"]);
    assert_ne!(r.code, 0, "{}", r.stdout);
}
