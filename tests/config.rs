//! `hall config` and `hall setup --interactive`'s terminal check
//! (docs/decisions/0033).

mod common;

use common::Env;
use insta::assert_json_snapshot;
use serde_json::Value;

fn entry(r: &common::Reply) -> &Value {
    &r.json["entries"][0]
}

#[test]
fn set_get_unset_keep_the_file_and_are_idempotent() {
    let env = Env::initialized();
    let file = env.repo.join(".hallmark.toml");

    let r = env.hall(&env.repo, &["config", "set", "budget.daily", "4M"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["changed"], true);
    assert_eq!(entry(&r)["value"], "4M");
    assert_eq!(entry(&r)["source"], "repo");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("daily = \"4M\""), "{text}");
    assert!(text.contains("# Hallmark settings for this repository."));

    let again = env.hall(&env.repo, &["config", "set", "budget.daily", "4M"]);
    assert_eq!(again.json["changed"], false);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text);

    let r = env.hall(
        &env.repo,
        &["config", "set", "gates", "make test", "make lint"],
    );
    assert_eq!(
        entry(&r)["value"],
        serde_json::json!(["make test", "make lint"])
    );
    let r = env.hall(
        &env.repo,
        &["config", "set", "claude_args", "--", "--verbose"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(entry(&r)["value"], serde_json::json!(["--verbose"]));

    let r = env.hall(&env.repo, &["config", "get", "budget.daily"]);
    assert_eq!(entry(&r)["value"], "4M");
    let r = env.hall(&env.repo, &["config", "unset", "budget.daily"]);
    assert_eq!(r.json["changed"], true);
    assert_eq!(entry(&r)["source"], "default");
    assert_eq!(entry(&r)["value"], Value::Null);
}

#[test]
fn bad_keys_and_values_are_usage_errors() {
    let env = Env::initialized();
    let before = std::fs::read_to_string(env.repo.join(".hallmark.toml")).unwrap();
    for (args, needle) in [
        (
            vec!["config", "set", "nope", "1"],
            "no Hallmark setting named nope",
        ),
        (vec!["config", "set", "budget", "1"], "budget is a table"),
        (
            vec!["config", "set", "capture.mode", "sometimes"],
            "not one of gate, ask, off",
        ),
        (
            vec!["config", "set", "max_rejects", "many"],
            "not a whole number",
        ),
        (vec!["config", "set", "stuck_after", "30"], "not a duration"),
        (vec!["config", "get", "notify"], "notify is a table"),
    ] {
        let r = env.hall(&env.repo, &args);
        assert_eq!(r.code, 2, "{args:?}: {}", r.stdout);
        assert_eq!(r.json["error"]["code"], "USAGE");
        let msg = r.json["error"]["message"].as_str().unwrap();
        assert!(msg.contains(needle), "{args:?}: {msg}");
        assert!(!r.json["error"]["next"].as_array().unwrap().is_empty());
    }
    let r = env.hall(&env.repo, &["config", "set", "budget", "1"]);
    assert_eq!(r.json["error"]["next"][0], "hall config list budget");
    let after = std::fs::read_to_string(env.repo.join(".hallmark.toml")).unwrap();
    assert_eq!(before, after);
}

#[test]
fn headless_runs_cant_change_settings() {
    let env = Env::initialized();
    let out = env
        .cmd(&env.repo)
        .env("HALL_HEADLESS", "1")
        .args(["config", "set", "capture.mode", "off"])
        .output()
        .unwrap();
    let r = common::reply(out);
    assert_eq!(r.code, 5, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "GATE_DENIED");
    // Reading is fine.
    let out = env
        .cmd(&env.repo)
        .env("HALL_HEADLESS", "1")
        .args(["config", "get", "capture.mode"])
        .output()
        .unwrap();
    assert_eq!(common::reply(out).json["entries"][0]["value"], "gate");
}

#[test]
fn a_worktree_writes_the_main_checkouts_file() {
    let env = Env::initialized();
    let (_, wt) = env.new_item("anything");
    let r = env.hall(&wt, &["config", "set", "base", "trunk"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let main = std::fs::read_to_string(env.repo.join(".hallmark.toml")).unwrap();
    let mine = std::fs::read_to_string(wt.join(".hallmark.toml")).unwrap();
    assert!(main.contains("\"trunk\""), "{main}");
    assert!(!mine.contains("\"trunk\""), "{mine}");
}

#[test]
fn user_keys_go_to_the_user_config_and_env_wins() {
    let env = Env::new();
    let qmd = env.root.join("my-qmd");
    std::fs::write(&qmd, "").unwrap();
    let qmd = qmd.to_string_lossy().to_string();
    // tools.* lives only in the user config: no --user needed, no repo either.
    let r = env.hall(&env.root, &["config", "set", "tools.qmd", &qmd]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(entry(&r)["scope"], "user");
    assert_eq!(entry(&r)["source"], "user");
    let text = std::fs::read_to_string(env.home.join("config.toml")).unwrap();
    assert!(text.contains("[tools]"), "{text}");

    // Tests point HALL_GH at a missing file; the environment wins.
    let r = env.hall(&env.root, &["config", "get", "tools.gh"]);
    assert_eq!(entry(&r)["source"], "env");

    let r = env.hall(
        &env.root,
        &[
            "config",
            "set",
            "--user",
            "notify.quiet_hours",
            "22:00-07:30",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    // Outside a repo, list shows only user keys.
    let r = env.hall(&env.root, &["config", "list"]);
    let scopes: Vec<&str> = r.json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["scope"].as_str().unwrap())
        .collect();
    assert!(scopes.iter().all(|s| *s == "user"), "{scopes:?}");
    // Repo keys need a repo.
    let r = env.hall(&env.root, &["config", "set", "base", "trunk"]);
    assert_eq!(r.json["error"]["code"], "NOT_A_REPO");
}

#[test]
fn a_repo_must_be_initialized_to_be_written() {
    let env = Env::new();
    let r = env.hall(&env.repo, &["config", "set", "base", "trunk"]);
    assert_eq!(r.json["error"]["code"], "NOT_INITIALIZED");
    assert_eq!(r.json["error"]["next"][0], "hall init");
}

/// The keys and kinds `hall config` offers: part of the contract.
#[test]
fn every_key_and_kind() {
    let env = Env::initialized();
    let r = env.hall(&env.repo, &["config", "list"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let keys: Vec<String> = r.json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            format!(
                "{} {} {}",
                e["scope"].as_str().unwrap(),
                e["key"].as_str().unwrap(),
                e["kind"].as_str().unwrap()
            )
        })
        .collect();
    assert_json_snapshot!("config_keys", keys);
}

#[test]
fn interactive_setup_needs_a_terminal() {
    let env = Env::initialized();
    let r = env.hall(&env.repo, &["setup", "--interactive"]);
    assert_eq!(r.code, 2, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "USAGE");
    assert!(
        r.json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("terminal")
    );
    assert_eq!(r.json["error"]["next"][0], "hall config list");
    let r = env.hall(&env.repo, &["setup", "claude", "--interactive"]);
    assert_eq!(r.code, 2);
}
