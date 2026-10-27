//! The Stage 2 foundation: leases, `hall next`, `hall findings`, and a
//! daemon pass with the fake runner.

mod common;

use common::{Env, commit_file};
use serde_json::json;

#[test]
fn a_daemon_pass_with_nothing_to_do_is_idle() {
    let env = Env::initialized();
    let script = env.script(json!([]));
    let r = env.hall_fake(&env.repo, &script, &["daemon", "run", "--once"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let steps = r.json["steps"].as_array().unwrap();
    assert_eq!(steps.len(), hallmark::worker::ORDER.len());
    assert!(
        steps.iter().all(|s| s["progress"] == "idle"),
        "{}",
        r.stdout
    );
    let only = env.hall_fake(
        &env.repo,
        &script,
        &["daemon", "run", "--once", "--only", "watchman"],
    );
    assert_eq!(only.json["steps"].as_array().unwrap().len(), 1);
}

#[test]
fn next_claims_with_a_lease() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Retry webhooks");
    let r = env.hall(&env.repo, &["next", "--role", "assayer", "--holder", "a"]);
    assert_eq!(r.code, 0);
    assert!(r.json["work"].is_null(), "nothing is ready yet");

    commit_file(&wt, "retry.rs", "fn retry() {}");
    assert_eq!(env.hall(&wt, &["ready"]).code, 0);
    let r = env.hall(&env.repo, &["next", "--role", "assayer", "--holder", "a"]);
    assert_eq!(r.json["work"]["id"], id.as_str(), "{}", r.stdout);
    assert_eq!(r.json["work"]["lease"]["holder"], "a");
    // Someone else asking for that item gets CLAIMED (exit 7).
    let r = env.hall(
        &env.repo,
        &["next", "--role", "assayer", "--holder", "b", "--item", &id],
    );
    assert_eq!(r.code, 7, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "CLAIMED");
    // And the repo's assayer is busy: one ticket at a time.
    let r = env.hall(&env.repo, &["next", "--role", "assayer", "--holder", "b"]);
    assert!(r.json["work"].is_null());
    // Releasing frees it.
    let r = env.hall(
        &env.repo,
        &[
            "next",
            "--role",
            "assayer",
            "--holder",
            "a",
            "--item",
            &id,
            "--release",
        ],
    );
    assert_eq!(r.json["released"], true, "{}", r.stdout);
    let r = env.hall(&env.repo, &["next", "--role", "assayer", "--holder", "b"]);
    assert_eq!(r.json["work"]["id"], id.as_str());
}

#[test]
fn findings_of_a_fresh_item_are_empty() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Usage page");
    let r = env.hall(&env.repo, &["findings", &id]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["findings"], json!([]));
    assert_eq!(r.json["open"]["blocker"], 0);
}

#[test]
fn every_schema_is_served() {
    let env = Env::new();
    for name in [
        "daemon",
        "next",
        "findings",
        "verdict",
        "verdict-input",
        "library",
        "watch",
        "watch-line",
        "stats",
    ] {
        let r = env.hall(&env.root, &["schema", name]);
        assert_eq!(r.code, 0, "{name}");
        assert!(r.json["schema"].is_object(), "{name}");
    }
}
