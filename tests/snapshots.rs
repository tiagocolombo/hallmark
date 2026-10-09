//! Snapshots of the JSON agents read. A change here is a change to the
//! agent contract: review it with `cargo insta review`.

mod common;

use common::{Env, commit_file, fixture, p};
use insta::assert_json_snapshot;
use serde_json::{Value, json};

/// Replaces the temp directory with `[root]` everywhere.
fn scrub(env: &Env, v: &Value) -> Value {
    let s = v.to_string().replace(&p(&env.root), "[root]");
    serde_json::from_str(&s).unwrap()
}

macro_rules! snap {
    ($name:expr, $value:expr) => {
        assert_json_snapshot!($name, $value, {
            ".created_at" => "[time]",
            ".state_since" => "[time]",
            ".sha" => "[sha]",
            ".note" => "[note]",
            ".deadline" => "[time]",
            ".entries[].since" => "[time]",
            ".items[].created_at" => "[time]",
            ".items[].state_since" => "[time]",
        });
    };
}

#[test]
fn error_envelope() {
    let env = Env::initialized();
    let r = env.hall(&env.repo, &["ready"]);
    assert_eq!(r.code, 3);
    snap!("error_no_item", scrub(&env, &r.json));
}

#[test]
fn new_and_ready_replies() {
    let env = Env::initialized();
    let r = env.hall(&env.repo, &["new", "Retry failed webhook deliveries"]);
    snap!("new", scrub(&env, &r.json));
    let wt = std::path::PathBuf::from(r.json["worktree"].as_str().unwrap());
    commit_file(&wt, "retry.rs", "fn retry() {}");
    let r = env.hall(&wt, &["ready"]);
    snap!("ready", scrub(&env, &r.json));
    commit_file(&wt, "more.rs", "x");
    let r = env.hall(&wt, &["ready"]);
    assert_eq!(r.code, 4);
    let mut v = scrub(&env, &r.json);
    // The message names two commits.
    v["error"]["message"] = json!("[message]");
    snap!("ready_in_review", v);
}

#[test]
fn status_and_inbox() {
    let env = Env::initialized();
    let (_, wt) = env.new_item("Usage page");
    env.hall(
        &wt,
        &[
            "ask",
            "billed usage or raw events?",
            "--options",
            "billed,raw",
            "--default",
            "billed",
        ],
    );
    env.new_item("CLI login flow");
    snap!(
        "status_list",
        scrub(&env, &env.hall(&env.repo, &["status"]).json)
    );
    snap!("inbox", scrub(&env, &env.hall(&env.repo, &["inbox"]).json));
}

#[test]
fn gate_denial() {
    let env = Env::initialized();
    let mut v: Value = serde_json::from_str(&fixture("hooks/pre_tool_use_write.json")).unwrap();
    v["cwd"] = json!(p(&env.repo));
    v["tool_input"]["file_path"] = json!(p(&env.repo.join("apps/web-e2e/tests/login.spec.ts")));
    let r = env.hook("pre-tool-use", &v);
    snap!("gate_deny_edit", scrub(&env, &r.json));
}

#[test]
fn help_commands() {
    let env = Env::new();
    let r = env.hall(&env.root, &["help", "--json"]);
    let mut v = r.json.clone();
    v["version"] = json!("[version]");
    snap!("help", v);
}

#[test]
fn error_schema() {
    let env = Env::new();
    snap!(
        "schema_error",
        env.hall(&env.root, &["schema", "error"]).json
    );
}
