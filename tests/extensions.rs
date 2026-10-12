//! `hall extensions`: skill packs by other authors, their credits, and the
//! roles that follow them (ADR 0038).

mod common;

use common::{Env, git, p, reply};
use insta::assert_json_snapshot;
use serde_json::{Value, json};

fn scrub(env: &Env, v: &Value) -> Value {
    let s = v.to_string().replace(&p(&env.root), "[root]");
    serde_json::from_str(&s).unwrap()
}

fn on(r: &Value) -> Vec<(String, bool)> {
    r["extensions"][0]["roles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| (x["role"].as_str().unwrap().to_string(), x["on"] == true))
        .collect()
}

fn roles_on(r: &Value) -> Vec<String> {
    on(r).into_iter().filter(|(_, o)| *o).map(|(r, _)| r).collect()
}

#[test]
fn the_list_credits_every_extension() {
    let env = Env::initialized();
    let r = env.hall(&env.repo, &["extensions"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_json_snapshot!("extensions_list", scrub(&env, &r.json));

    // Outside a repository it still lists them, all off.
    let r = env.hall(&env.root, &["extensions", "list"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert!(r.json.get("repo_file").is_none());
    assert_eq!(r.json["next"][0], "hall init");
    assert!(roles_on(&r.json).is_empty());
}

#[test]
fn enable_and_disable_by_role_are_idempotent() {
    let env = Env::initialized();
    let r = env.hall(
        &env.repo,
        &["extensions", "enable", "mattpocock", "--role", "fixer", "--role", "reviewer"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["changed"], true);
    assert_eq!(roles_on(&r.json), ["reviewer", "fixer"]);
    let again = env.hall(
        &env.repo,
        &["extensions", "enable", "mattpocock", "--role", "reviewer"],
    );
    assert_eq!(again.json["changed"], false);
    assert_eq!(roles_on(&again.json), ["reviewer", "fixer"]);

    // It is a setting like any other.
    let g = env.hall(&env.repo, &["config", "get", "extensions.mattpocock"]);
    assert_eq!(g.json["entries"][0]["value"], json!(["reviewer", "fixer"]));
    assert_eq!(g.json["entries"][0]["source"], "repo");

    let all = env.hall(&env.repo, &["extensions", "enable", "mattpocock"]);
    assert_eq!(roles_on(&all.json), ["planner", "developer", "reviewer", "fixer"]);
    let some = env.hall(
        &env.repo,
        &["extensions", "disable", "mattpocock", "--role", "planner"],
    );
    assert_eq!(roles_on(&some.json), ["developer", "reviewer", "fixer"]);
    let none = env.hall(&env.repo, &["extensions", "disable", "mattpocock"]);
    assert_eq!(none.json["changed"], true);
    assert!(roles_on(&none.json).is_empty());
    let again = env.hall(&env.repo, &["extensions", "disable", "mattpocock"]);
    assert_eq!(again.json["changed"], false);
    let text = std::fs::read_to_string(env.repo.join(".hallmark.toml")).unwrap();
    assert!(!text.contains("mattpocock"), "{text}");
}

#[test]
fn the_reviewer_note_goes_once_the_base_has_it() {
    let env = Env::initialized();
    let r = env.hall(&env.repo, &["extensions", "enable", "mattpocock"]);
    assert!(r.json["note"].as_str().unwrap().contains("reads .hallmark.toml from main"));
    assert!(r.json["next"].as_array().unwrap().contains(&json!("hall doctor")));
    git(&env.repo, &["commit", "-q", "-am", "extensions"]);

    // Planner only: the reviewer's setting on main is unchanged.
    let r = env.hall(
        &env.repo,
        &["extensions", "disable", "mattpocock", "--role", "planner"],
    );
    assert!(r.json["note"].is_null(), "{}", r.stdout);
    let r = env.hall(
        &env.repo,
        &["extensions", "disable", "mattpocock", "--role", "reviewer"],
    );
    assert!(r.json["note"].as_str().unwrap().contains("stops following"));
}

#[test]
fn show_prints_the_credits_and_license_or_one_skill() {
    let env = Env::initialized();
    let r = env.hall(&env.repo, &["extensions", "show", "mattpocock"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let license = r.json["license_text"].as_str().unwrap();
    assert!(license.starts_with("MIT License"));
    assert!(license.contains("Copyright (c) 2026 Matt Pocock"));
    assert_eq!(r.json["extensions"][0]["author"], "Matt Pocock");
    assert!(
        r.json["next"]
            .as_array()
            .unwrap()
            .contains(&json!("hall extensions show mattpocock tdd"))
    );

    let r = env.hall(&env.repo, &["extensions", "show", "mattpocock", "tdd"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let s = &r.json["skill"];
    assert_eq!(s["roles"], json!(["developer", "fixer"]));
    assert!(s["description"].as_str().unwrap().starts_with("Test-driven development."));
    let text = s["text"].as_str().unwrap();
    assert!(text.starts_with("# Test-Driven Development"), "{text}");
    assert!(text.contains("`tests.md`, which the skill links to:"));
    assert!(r.json.get("license_text").is_none());
}

#[test]
fn mistakes_name_what_exists() {
    let env = Env::initialized();
    let r = env.hall(&env.repo, &["extensions", "enable", "nope"]);
    assert_eq!(r.code, 2, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "USAGE");
    assert!(r.json["error"]["message"].as_str().unwrap().contains("mattpocock"));

    let r = env.hall(&env.repo, &["extensions", "show", "mattpocock", "grill-me"]);
    assert_eq!(r.json["error"]["code"], "USAGE");
    assert!(r.json["error"]["message"].as_str().unwrap().contains("code-review"));

    let r = env.hall(&env.repo, &["extensions", "enable", "mattpocock", "--role", "librarian"]);
    assert_eq!(r.json["error"]["code"], "USAGE", "{}", r.stdout);

    // Headless runs work under the settings; they can't change them.
    let r = reply(
        env.cmd(&env.repo)
            .args(["extensions", "enable", "mattpocock"])
            .env("HALL_HEADLESS", "1")
            .output()
            .unwrap(),
    );
    assert_eq!(r.json["error"]["code"], "GATE_DENIED");
    let text = std::fs::read_to_string(env.repo.join(".hallmark.toml")).unwrap();
    assert!(!text.contains("mattpocock"));

    let bare = Env::new();
    let r = bare.hall(&bare.repo, &["extensions", "enable", "mattpocock"]);
    assert_eq!(r.json["error"]["code"], "NOT_INITIALIZED", "{}", r.stdout);
}
