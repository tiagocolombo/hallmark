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

/// A per-run cap that isn't set shows its role's default; "none" lifts it.
#[test]
fn per_run_caps_show_their_defaults_and_take_none() {
    let env = Env::initialized();
    let r = env.hall(&env.repo, &["config", "get", "budget.review.max_usd"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(entry(&r)["value"], 6.0);
    assert_eq!(entry(&r)["source"], "default");
    let r = env.hall(&env.repo, &["config", "get", "budget.intake.max_usd"]);
    assert_eq!(entry(&r)["value"], 0.1);

    let r = env.hall(
        &env.repo,
        &["config", "set", "budget.review.max_usd", "none"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(entry(&r)["value"], "none");
    assert_eq!(entry(&r)["source"], "repo");
    let text = std::fs::read_to_string(env.repo.join(".hallmark.toml")).unwrap();
    assert!(text.contains(r#"max_usd = "none""#), "{text}");

    let r = env.hall(&env.repo, &["config", "unset", "budget.review.max_usd"]);
    assert_eq!(entry(&r)["value"], 6.0);
    assert_eq!(entry(&r)["source"], "default");
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
fn notify_keys_stay_in_the_user_config_and_a_lone_lens_is_a_list() {
    let env = Env::initialized();
    let r = env.hall(&env.repo, &["config", "set", "notify.desktop", "false"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(entry(&r)["scope"], "user");
    let repo = std::fs::read_to_string(env.repo.join(".hallmark.toml")).unwrap();
    assert!(!repo.contains("[notify]"), "{repo}");
    assert!(
        std::fs::read_to_string(env.home.join("config.toml"))
            .unwrap()
            .contains("desktop = false")
    );

    // Once the repo has its own [notify], keys go there, and unset removes
    // the table when it is left empty.
    std::fs::write(
        env.repo.join(".hallmark.toml"),
        format!("{repo}\n[notify]\ndesktop = true\n"),
    )
    .unwrap();
    let r = env.hall(&env.repo, &["config", "get", "notify.desktop"]);
    assert_eq!(entry(&r)["scope"], "repo");
    env.hall(&env.repo, &["config", "unset", "notify.desktop"]);
    let after = std::fs::read_to_string(env.repo.join(".hallmark.toml")).unwrap();
    assert!(!after.contains("[notify]"), "{after}");

    let r = env.hall(&env.repo, &["config", "set", "lenses", "correctness"]);
    assert_eq!(entry(&r)["value"], serde_json::json!(["correctness"]));
    let r = env.hall(&env.repo, &["config", "set", "lenses", "all"]);
    assert_eq!(entry(&r)["value"], "all");
}

/// `[tools]` reaches the programs Hallmark runs, and `HALL_*` still wins.
#[cfg(unix)]
#[test]
fn tools_in_the_user_config_reach_doctor_and_env_overrides() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    let script = |name: &str| {
        let p = env.root.join(name);
        std::fs::write(&p, "#!/bin/sh\necho fake 1.0\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.to_string_lossy().to_string()
    };
    let (gh, claude, other) = (script("my-gh"), script("my-claude"), script("other-gh"));
    env.hall(&env.root, &["config", "set", "tools.gh", &gh]);
    env.hall(&env.root, &["config", "set", "tools.claude", &claude]);

    let doctor = |gh_env: Option<&str>| {
        let mut c = env.cmd(&env.root);
        c.env_remove("HALL_GH").env_remove("HALL_CLAUDE");
        if let Some(v) = gh_env {
            c.env("HALL_GH", v);
        }
        common::reply(c.arg("doctor").output().unwrap()).stdout
    };
    // No variable: the file's path is used.
    let out = doctor(None);
    assert!(out.contains(&gh), "{out}");
    assert!(out.contains("fake 1.0"), "{out}");
    // An empty variable falls through to the file.
    assert!(doctor(Some("")).contains(&gh));
    // A set variable wins.
    let out = doctor(Some(&other));
    assert!(out.contains(&other) && !out.contains(&gh), "{out}");
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

/// `reviewer` was never read (L2): it is gone from the keys, and a file
/// that still sets it loads, with the key ignored.
#[test]
fn the_old_reviewer_key_is_ignored() {
    let env = Env::initialized();
    let file = env.repo.join(".hallmark.toml");
    let mut text = std::fs::read_to_string(&file).unwrap();
    text.insert_str(0, "reviewer = \"my-reviewer\"\n");
    std::fs::write(&file, text).unwrap();

    let r = env.hall(&env.repo, &["config", "list"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let keys: Vec<&str> = r.json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["key"].as_str().unwrap())
        .collect();
    assert!(keys.contains(&"fixer"), "{keys:?}");
    assert!(!keys.contains(&"reviewer"), "{keys:?}");
    assert!(!r.stdout.contains("my-reviewer"), "{}", r.stdout);

    let s = env.hall(&env.repo, &["schema", "config"]);
    assert_eq!(s.code, 0, "{}", s.stdout);
    let props = &s.json["schema"]["properties"];
    assert!(props.get("fixer").is_some(), "{}", s.stdout);
    assert!(props.get("reviewer").is_none(), "{}", s.stdout);
}
