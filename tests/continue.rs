//! `hall continue`: one verb that moves an item on from wherever it waits.

mod common;

use common::{Env, commit_file};

#[test]
fn continue_does_what_the_item_needs() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Usage page");
    commit_file(&wt, "usage.rs", "fn usage() {}");

    // Waiting for your call: continue sends it to review.
    env.hall(&wt, &["propose-ready", "--summary", "done"]);
    let r = env.hall(&env.repo, &["continue", &id]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["state"], "checks");

    // In the loop and moving: nothing to do, and it says so.
    let r = env.hall(&env.repo, &["continue", &id]);
    assert_eq!(r.json["changed"], false);
    assert!(
        r.json["note"]
            .as_str()
            .unwrap()
            .contains("moves on its own"),
        "{}",
        r.stdout
    );

    // Held: continue releases the hold, with the reason given.
    let journal = env
        .repo
        .join(".git/hallmark/items")
        .join(&id)
        .join("events.jsonl");
    let mut text = std::fs::read_to_string(&journal).unwrap();
    text.push_str(&format!(
        "{{\"v\":1,\"t\":\"{}\",\"actor\":\"fixer\",\"item\":\"{id}\",\"event\":\"held\",\"kind\":\"budget\",\"reason\":\"cap\"}}\n",
        chrono::Utc::now().to_rfc3339()
    ));
    std::fs::write(&journal, text).unwrap();
    let r = env.hall(&env.repo, &["continue", &id, "--reason", "raised the cap"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["changed"], true);
    let last = env.journal(&id).last().cloned().unwrap();
    assert_eq!(last["event"], "released");
    assert_eq!(last["reason"], "raised the cap");

    // In development, off a terminal: how to reopen its session.
    let (dev, _) = env.new_item("Login flow");
    let r = env.hall(&env.repo, &["continue", &dev]);
    assert_eq!(r.json["changed"], false);
    assert!(
        r.json["next"][0]
            .as_str()
            .unwrap()
            .ends_with("&& claude --continue")
    );
}
