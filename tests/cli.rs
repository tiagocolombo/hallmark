//! The agent contract and the item lifecycle, through the real binary.

mod common;

use common::{Env, commit_file, git};
use serde_json::{Value, json};

fn codes(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

#[test]
fn outside_a_repo_is_not_found() {
    let env = Env::new();
    let r = env.hall(&env.root, &["status"]);
    assert_eq!(r.code, 3);
    assert_eq!(codes(&r.json), "NOT_A_REPO");
    assert!(!r.json["error"]["next"].as_array().unwrap().is_empty());
}

#[test]
fn commands_need_init() {
    let env = Env::new();
    let r = env.hall(&env.repo, &["status"]);
    assert_eq!((r.code, codes(&r.json)), (3, "NOT_INITIALIZED"));
    assert_eq!(r.json["error"]["next"][0], "hall init");
}

#[test]
fn usage_errors_are_json_with_exit_2() {
    let env = Env::initialized();
    for args in [
        vec!["bogus"],
        vec!["drop", "H-0001"],
        vec!["new"],
        vec!["status", "nope"],
    ] {
        let r = env.hall(&env.repo, &args);
        assert_eq!(r.code, 2, "{args:?}: {}", r.stdout);
        assert_eq!(codes(&r.json), "USAGE", "{args:?}");
    }
}

#[test]
fn init_is_idempotent_and_never_overwrites() {
    let env = Env::new();
    std::fs::write(env.repo.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    let first = env.hall(&env.repo, &["init"]);
    assert_eq!(first.code, 0);
    assert_eq!(first.json["gates"][0], "cargo test");
    assert_eq!(first.json["base"], "main");
    let created: Vec<&str> = first.json["created"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        created,
        [".hallmark.toml", ".hallmark/review.md", "CLAUDE.md"]
    );

    let second = env.hall(&env.repo, &["init"]);
    assert_eq!(second.json["created"].as_array().unwrap().len(), 0);
    let claude = std::fs::read_to_string(env.repo.join("CLAUDE.md")).unwrap();
    assert_eq!(claude.matches("<!-- hallmark:start -->").count(), 1);
    assert!(claude.contains("Never push"));
    assert!(env.repo.join(".git/hallmark/index.sqlite").is_file());
    let reg = std::fs::read_to_string(env.home.join("repos.json")).unwrap();
    assert!(reg.contains("\"app\""));
}

#[test]
fn init_prefers_agents_md_when_it_is_the_only_one() {
    let env = Env::new();
    std::fs::write(env.repo.join("AGENTS.md"), "# Agents\n").unwrap();
    env.hall(&env.repo, &["init", "--capture", "ask", "--base", "trunk"]);
    assert!(!env.repo.join("CLAUDE.md").exists());
    assert!(
        std::fs::read_to_string(env.repo.join("AGENTS.md"))
            .unwrap()
            .contains("## Hallmark")
    );
    let toml = std::fs::read_to_string(env.repo.join(".hallmark.toml")).unwrap();
    assert!(toml.contains("mode = \"ask\""));
    assert!(toml.contains("base     = \"trunk\""));
}

#[test]
fn new_creates_an_item_with_its_own_worktree() {
    let env = Env::initialized();
    let r = env.hall(
        &env.repo,
        &[
            "new",
            "Retry failed webhook deliveries with exponential backoff",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    let j = &r.json;
    assert_eq!(j["id"], "H-0001");
    assert_eq!(j["state"], "development");
    assert_eq!(j["room"], "Workshop");
    assert_eq!(j["created"], true);
    assert_eq!(
        j["branch"],
        "hm/H-0001-retry-failed-webhook-deliveries-with"
    );
    let wt = std::path::PathBuf::from(j["worktree"].as_str().unwrap());
    assert!(wt.is_dir());
    assert_eq!(wt.file_name().unwrap(), "app-H-0001");
    assert_eq!(
        git(&wt, &["branch", "--show-current"]),
        j["branch"].as_str().unwrap()
    );
    assert!(
        j["next"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str().unwrap().starts_with("cd "))
    );

    let events = env.journal("H-0001");
    assert_eq!(events[0]["event"], "created");
    assert_eq!(
        events[0]["request"],
        "Retry failed webhook deliveries with exponential backoff"
    );
    assert_eq!(events[0]["actor"], "you");

    let (id2, _) = env.new_item("Second");
    assert_eq!(id2, "H-0002");
}

#[test]
fn the_worktree_command_is_configurable() {
    let env = Env::initialized();
    let toml = std::fs::read_to_string(env.repo.join(".hallmark.toml"))
        .unwrap()
        .replace(
            "worktree = \"git worktree add ../{repo}-{id} -b {branch}\"",
            "worktree = \"git worktree add ../wt/{id} -b {branch}\"",
        );
    std::fs::write(env.repo.join(".hallmark.toml"), toml).unwrap();
    let (_, wt) = env.new_item("Custom place");
    assert!(common::p(&wt).ends_with("/wt/H-0001"), "{}", wt.display());
}

#[test]
fn the_item_id_is_optional_inside_its_worktree() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Fix login");
    let r = env.hall(&wt, &["status"]);
    assert_eq!(r.json["id"], id);
    let sub = wt.join("deep/er");
    std::fs::create_dir_all(&sub).unwrap();
    assert_eq!(env.hall(&sub, &["status"]).json["id"], id);
    // In the main checkout there is no current item.
    let r = env.hall(&env.repo, &["ready"]);
    assert_eq!((r.code, codes(&r.json)), (3, "NO_ITEM"));
    // IDs can be written loosely.
    assert_eq!(env.hall(&env.repo, &["status", "1"]).json["id"], id);
    let r = env.hall(&env.repo, &["status", "H-0042"]);
    assert_eq!((r.code, codes(&r.json)), (3, "ITEM_NOT_FOUND"));
}

#[test]
fn the_whole_development_lifecycle() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Webhook retries");

    // The agent proposes; the item waits for you.
    let r = env.hall(
        &wt,
        &[
            "propose-ready",
            "--ask",
            "--summary",
            "retries with jitter, 14 tests",
        ],
    );
    assert_eq!(
        (r.code, r.json["state"].as_str().unwrap()),
        (0, "awaiting_you")
    );
    assert_eq!(
        env.hall(&wt, &["propose-ready", "--ask", "--summary", "again"])
            .json["changed"],
        false
    );
    let inbox = env.hall(&env.repo, &["inbox"]);
    assert_eq!(inbox.json["entries"][0]["kind"], "ready");
    assert_eq!(inbox.json["entries"][0]["item"], id);

    // "Not yet."
    let r = env.hall(&wt, &["pull-back", "--reason", "cap at 6 attempts"]);
    assert_eq!(r.json["state"], "development");
    assert_eq!(
        env.hall(&env.repo, &["inbox"]).json["entries"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    // Ready refuses uncommitted work.
    std::fs::write(wt.join("README.md"), "changed\n").unwrap();
    let r = env.hall(&wt, &["ready"]);
    assert_eq!((r.code, codes(&r.json)), (4, "DIRTY_WORKTREE"));
    git(&wt, &["commit", "-qam", "cap attempts"]);
    let sha = git(&wt, &["rev-parse", "HEAD"]);

    let r = env.hall(&wt, &["ready", "--note", "cap at 6 attempts done"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["state"], "checks");
    assert_eq!(r.json["round"], 1);
    assert_eq!(r.json["sha"], sha.as_str());
    assert_eq!(r.json["changed"], true);
    assert!(
        env.repo
            .join(format!(".git/hallmark/queue/{id}.json"))
            .is_file()
    );

    // Idempotent at the same commit.
    let again = env.hall(&wt, &["ready"]);
    assert_eq!(
        (again.code, again.json["changed"].as_bool()),
        (0, Some(false))
    );
    assert_eq!(again.json["round"], 1);

    // A new commit while in review is refused, with the way out.
    commit_file(&wt, "later.txt", "x");
    let r = env.hall(&wt, &["ready"]);
    assert_eq!((r.code, codes(&r.json)), (4, "ITEM_IN_REVIEW"));
    assert_eq!(r.json["error"]["next"][0], format!("hall pull-back {id}"));

    // Pull back, then ready again starts round 2.
    env.hall(&wt, &["pull-back"]);
    assert!(
        !env.repo
            .join(format!(".git/hallmark/queue/{id}.json"))
            .exists()
    );
    let r = env.hall(&wt, &["ready"]);
    assert_eq!(r.json["round"], 2);

    // Drop needs a reason; it removes the worktree but keeps the branch.
    let branch = r.json["branch"].as_str().unwrap().to_string();
    let r = env.hall(&env.repo, &["drop", &id]);
    assert_eq!(r.code, 2);
    let r = env.hall(
        &env.repo,
        &["drop", &id, "--reason", "superseded by H-0044"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["state"], "dropped");
    assert_eq!(r.json["drop_reason"], "superseded by H-0044");
    assert!(!wt.exists());
    assert!(!git(&env.repo, &["branch", "--list", &branch]).is_empty());
    assert_eq!(
        env.hall(&env.repo, &["drop", &id, "--reason", "x"]).json["changed"],
        false
    );

    // Terminal states don't move.
    let r = env.hall(&env.repo, &["ready", &id]);
    assert_eq!((r.code, codes(&r.json)), (4, "WRONG_STATE"));
    let r = env.hall(&env.repo, &["pull-back", &id]);
    assert_eq!(r.code, 4);

    // The journal tells the story.
    let names: Vec<String> = env
        .journal(&id)
        .iter()
        .map(|e| e["event"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        names,
        [
            "created",
            "proposed_ready",
            "pulled_back",
            "ready",
            "pulled_back",
            "ready",
            "dropped"
        ]
    );
    let log = env.hall(&env.repo, &["log", &id, "--kind", "ready", "--limit", "1"]);
    assert_eq!(log.json["events"].as_array().unwrap().len(), 1);
    assert_eq!(log.json["events"][0]["round"], 2);
    assert_eq!(log.json["total"], 7);

    // Listing hides closed and dropped items unless asked.
    assert_eq!(
        env.hall(&env.repo, &["status"]).json["items"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        env.hall(&env.repo, &["status", "--all"]).json["items"][0]["state"],
        "dropped"
    );
}

/// Headless runs (the reviewer, the fixer) get the verdicts, fixes and
/// checks from `hall log`, not every prompt and turn summary, unless they ask
/// for `--kind all` (T11).
#[test]
fn headless_log_defaults_to_verdicts_fixes_and_checks() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Retry webhooks");
    let journal = env
        .repo
        .join(".git/hallmark/items")
        .join(&id)
        .join("events.jsonl");
    let mut text = std::fs::read_to_string(&journal).unwrap();
    let t = chrono::Utc::now().to_rfc3339();
    for body in [
        json!({"event": "prompt", "text": "build the retry"}),
        json!({"event": "turn", "summary": "done, I think"}),
        json!({"event": "checks", "sha": "abc", "round": 1, "passed": true}),
        json!({"event": "verdict", "sha": "abc", "round": 1, "verdict": "changes"}),
        json!({"event": "fix_submitted", "sha": "def", "fixes": ["F1"]}),
    ] {
        let mut e = json!({"v": 1, "t": t, "actor": "you", "item": id});
        e.as_object_mut()
            .unwrap()
            .extend(body.as_object().unwrap().clone());
        text.push_str(&format!("{e}\n"));
    }
    std::fs::write(&journal, text).unwrap();
    let names = |r: &common::Reply| -> Vec<String> {
        r.json["events"]
            .as_array()
            .unwrap_or_else(|| panic!("{}", r.stdout))
            .iter()
            .map(|e| e["event"].as_str().unwrap().to_string())
            .collect()
    };
    let headless = |args: &[&str]| {
        common::reply(
            env.cmd(&env.repo)
                .env("HALL_HEADLESS", "1")
                .args(args)
                .output()
                .unwrap(),
        )
    };

    let r = headless(&["log", &id]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(names(&r), ["checks", "verdict", "fix_submitted"]);
    assert_eq!(r.json["total"], 6);
    assert_eq!(
        r.json["kinds"],
        json!(["verdict", "fix_submitted", "checks"])
    );

    // `--kind all` is the whole journal; other kinds still filter.
    let r = headless(&["log", &id, "--kind", "all"]);
    assert_eq!(names(&r).len(), 6, "{}", r.stdout);
    assert!(r.json.get("kinds").is_none(), "{}", r.stdout);
    let r = headless(&["log", &id, "--kind", "prompt"]);
    assert_eq!(names(&r), ["prompt"]);

    // In a person's session nothing changes: the whole journal.
    let r = env.hall(&env.repo, &["log", &id]);
    assert_eq!(names(&r).len(), 6, "{}", r.stdout);

    // What the reviewer is told to run: `fix` means fix_submitted.
    let r = env.hall(&env.repo, &["log", &id, "--kind", "verdict,fix,checks"]);
    assert_eq!(names(&r), ["checks", "verdict", "fix_submitted"]);
}

#[test]
fn drop_keeps_a_dirty_worktree() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Messy");
    std::fs::write(wt.join("README.md"), "unsaved work\n").unwrap();
    let r = env.hall(&env.repo, &["drop", &id, "--reason", "not needed"]);
    assert_eq!(r.json["state"], "dropped");
    assert!(wt.is_dir(), "a dirty worktree is never forced away");
    assert!(
        r.json["note"]
            .as_str()
            .unwrap()
            .contains("Kept the worktree")
    );
    let dropped = env.journal(&id).into_iter().last().unwrap();
    assert_eq!(dropped["worktree_removed"], false);
}

#[test]
fn retry_only_applies_to_escalated_items() {
    let env = Env::initialized();
    let (id, _) = env.new_item("x");
    let r = env.hall(&env.repo, &["retry", &id, "--reason", "one more"]);
    assert_eq!((r.code, codes(&r.json)), (4, "WRONG_STATE"));
}

#[test]
fn headless_fixers_may_not_ready_retry_or_drop() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("x");
    for args in [
        vec!["ready", id.as_str()],
        vec!["retry", id.as_str(), "--reason", "one more"],
        vec!["drop", id.as_str(), "--reason", "not needed"],
    ] {
        let out = env
            .cmd(&wt)
            .env("HALL_HEADLESS", "1")
            .env("HALL_ACTOR", "fixer")
            .args(&args)
            .output()
            .unwrap();
        let r = common::reply(out);
        assert_eq!((r.code, codes(&r.json)), (5, "GATE_DENIED"), "{args:?}");
        assert_eq!(
            r.json["error"]["next"],
            json!([format!("hall status {id}")])
        );
    }
    assert_eq!(
        env.hall(&env.repo, &["status", &id]).json["state"],
        "development"
    );
    assert!(wt.is_dir());
}

#[test]
fn adopt_turns_a_branch_into_an_item() {
    let env = Env::initialized();
    let r = env.hall(&env.repo, &["adopt"]);
    assert_eq!(
        (r.code, codes(&r.json)),
        (4, "WRONG_STATE"),
        "main is the base branch"
    );

    git(&env.repo, &["switch", "-q", "-c", "feature/new_billing"]);
    let r = env.hall(&env.repo, &["adopt"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["title"], "New billing");
    assert_eq!(r.json["created"], true);
    assert_eq!(r.json["worktree"], common::p(&env.repo));
    let again = env.hall(&env.repo, &["adopt"]);
    assert_eq!(
        (again.json["created"].as_bool(), again.json["id"].as_str()),
        (Some(false), Some("H-0001"))
    );
    // The adopted checkout is now the item's worktree.
    assert_eq!(env.hall(&env.repo, &["status"]).json["id"], "H-0001");
    assert_eq!(env.journal("H-0001")[0]["adopted"], true);
}

#[test]
fn a_dropped_items_questions_leave_the_inbox() {
    let env = Env::initialized();
    let (_, wt) = env.new_item("Usage page");
    assert_eq!(env.hall(&wt, &["ask", "billed or raw?"]).code, 0);
    let r = env.hall(&env.repo, &["ask", "Freeze the repo first?"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(
        env.hall(&env.repo, &["inbox"]).json["entries"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let r = env.hall(&wt, &["drop", "--reason", "not needed"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    // The repo's own question stays.
    let inbox = env.hall(&env.repo, &["inbox"]);
    let entries = inbox.json["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0]["text"], "Freeze the repo first?");
}

#[test]
fn questions_land_in_the_inbox_and_get_answered() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Usage page");
    let r = env.hall(
        &wt,
        &[
            "ask",
            "billed usage or raw events?",
            "--options",
            "billed,raw",
            "--default",
            "billed",
            "--wait",
            "4h",
            "--session",
            "sess-1",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["qid"], "Q-1");
    assert_eq!(r.json["item"], id);
    assert_eq!(r.json["status"], "waiting");
    assert!(r.json["deadline"].is_string());
    // Asking the same open question again returns it.
    let same = env.hall(
        &wt,
        &[
            "ask",
            "billed usage or raw events?",
            "--options",
            "billed,raw",
        ],
    );
    assert_eq!(
        (same.json["qid"].as_str(), same.json["created"].as_bool()),
        (Some("Q-1"), Some(false))
    );

    let bad = env.hall(&wt, &["ask", "x?", "--options", "a,b", "--default", "c"]);
    assert_eq!(bad.code, 2);

    let inbox = env.hall(&env.repo, &["inbox"]);
    let e = &inbox.json["entries"][0];
    assert_eq!(
        (e["kind"].as_str(), e["qid"].as_str()),
        (Some("question"), Some("Q-1"))
    );
    assert_eq!(e["options"], serde_json::json!(["billed", "raw"]));

    let r = env.hall(&env.repo, &["answer", "Q-1", "maybe"]);
    assert_eq!(r.code, 2);
    let r = env.hall(&env.repo, &["answer", "q-1", "b"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["answer"], "raw");
    assert_eq!(r.json["resume"], "claude -p --resume sess-1 raw");
    assert_eq!(
        env.hall(&env.repo, &["answer", "Q-1", "raw"]).json["changed"],
        false
    );
    let r = env.hall(&env.repo, &["answer", "Q-1", "billed"]);
    assert_eq!((r.code, codes(&r.json)), (4, "ALREADY_ANSWERED"));
    let r = env.hall(&env.repo, &["answer", "Q-9", "x"]);
    assert_eq!((r.code, codes(&r.json)), (3, "QUESTION_NOT_FOUND"));
    assert_eq!(
        env.hall(&env.repo, &["inbox"]).json["entries"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    // A question with no item is filed in the repo journal.
    let r = env.hall(&env.repo, &["ask", "which region?"]);
    assert_eq!(r.json["qid"], "Q-2");
    assert!(r.json.get("item").is_none());
    assert_eq!(env.repo_journal().last().unwrap()["event"], "question");
}

#[test]
fn reindex_rebuilds_the_same_view() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("One");
    env.hall(&wt, &["propose-ready", "--ask", "--summary", "done"]);
    env.new_item("Two");
    let before = env.hall(&env.repo, &["status"]).json;
    std::fs::remove_file(env.repo.join(".git/hallmark/index.sqlite")).unwrap();
    let r = env.hall(&env.repo, &["reindex"]);
    assert_eq!(r.code, 0);
    assert_eq!(r.json["items"], 2);
    assert_eq!(r.json["events"], 3);
    assert_eq!(env.hall(&env.repo, &["status"]).json, before);
    assert_eq!(
        env.hall(&env.repo, &["inbox"]).json["entries"][0]["item"],
        id
    );
}

#[test]
fn help_and_schema_describe_the_cli() {
    let env = Env::new();
    let r = env.hall(&env.root, &["help", "--json"]);
    assert_eq!(r.code, 0);
    let names: Vec<&str> = r.json["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    for c in [
        "init",
        "new",
        "ready",
        "pull-back",
        "drop",
        "ask",
        "answer",
        "hook",
        "schema",
        "setup",
    ] {
        assert!(names.contains(&c), "{c}");
    }
    let codes: Vec<i64> = r.json["exit_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["code"].as_i64().unwrap())
        .collect();
    assert_eq!(codes, [0, 1, 2, 3, 4, 5, 6, 7]);
    let drop = r.json["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "drop")
        .unwrap();
    let reason = drop["args"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "reason")
        .unwrap();
    assert_eq!(reason["required"], true);

    let s = env.hall(&env.root, &["schema", "ready"]);
    assert_eq!(s.json["name"], "ready");
    assert!(
        s.json["schema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "next")
    );
    let s = env.hall(&env.root, &["schema", "error"]);
    let enum_codes = &s.json["schema"]["$defs"]["ErrorCode"];
    assert!(enum_codes.to_string().contains("ITEM_IN_REVIEW"));
    assert_eq!(env.hall(&env.root, &["schema", "nope"]).code, 2);
    assert!(
        env.hall(&env.root, &["schema"]).json["available"]
            .as_array()
            .unwrap()
            .len()
            > 20
    );
}

#[test]
fn json_is_forced_with_the_flag_and_errors_go_to_stdout() {
    let env = Env::new();
    let out = env
        .cmd(&env.repo)
        .args(["status", "--json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["error"]["code"], "NOT_INITIALIZED");
    assert!(out.stderr.is_empty());
}

#[test]
fn doctor_reports_without_failing() {
    let env = Env::initialized();
    let r = env.hall(&env.repo, &["doctor"]);
    assert_eq!(r.code, 0);
    assert_eq!(r.json["healthy"], false, "claude is missing in tests");
    let checks = r.json["checks"].as_array().unwrap();
    let get = |n: &str| checks.iter().find(|c| c["name"] == n).unwrap().clone();
    assert_eq!(get("git")["ok"], true);
    assert_eq!(get("claude")["ok"], false);
    assert_eq!(get("plugin")["fix"], "hall setup claude");
    assert_eq!(get("repo")["ok"], true);
    assert_eq!(get("journal")["ok"], true);
    assert_eq!(get("gates")["fix"], "add gates to .hallmark.toml");
    assert!(
        r.json["next"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n == "hall setup claude")
    );
}

#[test]
fn setup_claude_dry_run_writes_nothing() {
    let env = Env::new();
    let r = env.hall(&env.root, &["setup", "claude", "--dry-run"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["dry_run"], true);
    assert!(
        r.json["steps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["status"] == "planned")
    );
    assert!(!env.home.join("claude-plugin").exists());
}

#[cfg(unix)]
#[test]
fn setup_claude_installs_through_a_local_marketplace() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    // A fake `claude` that records its arguments.
    let log = env.root.join("claude.log");
    let fake = env.root.join("claude");
    std::fs::write(
        &fake,
        format!("#!/bin/sh\necho \"$@\" >> '{}'\n", log.display()),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = env
        .cmd(&env.root)
        .env("HALL_CLAUDE", &fake)
        .args(["setup", "claude"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let calls = std::fs::read_to_string(&log).unwrap();
    let dir = env.home.join("claude-plugin");
    assert!(
        calls.contains(&format!(
            "plugin marketplace add {} --scope user",
            dir.display()
        )),
        "{calls}"
    );
    assert!(
        calls.contains("plugin install hallmark@hallmark --scope user"),
        "{calls}"
    );
    let hooks = std::fs::read_to_string(dir.join("plugins/hallmark/hooks/hooks.json")).unwrap();
    assert!(
        hooks.contains(&format!("{} hook pre-tool-use", env!("CARGO_BIN_EXE_hall"))),
        "{hooks}"
    );
    assert!(
        dir.join("plugins/hallmark/skills/hallmark/SKILL.md")
            .is_file()
    );
    let market: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join(".claude-plugin/marketplace.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(market["plugins"][0]["source"], "./plugins/hallmark");
}

#[cfg(unix)]
#[test]
fn new_from_a_github_issue() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::initialized();
    let fake = env.root.join("gh");
    std::fs::write(
        &fake,
        "#!/bin/sh\necho '{\"number\":212,\"title\":\"Dashboard shows stale usage\",\"body\":\"After a plan change.\",\"url\":\"https://github.com/o/r/issues/212\"}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = env
        .cmd(&env.repo)
        .env("HALL_GH", &fake)
        .args(["new", "--issue", "212"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["title"], "Dashboard shows stale usage");
    assert_eq!(v["issue"]["number"], 212);
    assert_eq!(v["branch"], "hm/H-0001-dashboard-shows-stale-usage");
    let created = &env.journal("H-0001")[0];
    assert!(
        created["request"]
            .as_str()
            .unwrap()
            .contains("After a plan change.")
    );
}

#[test]
fn done_with_nothing_to_confirm_goes_straight_to_review() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Webhook retries");
    let sha = commit_file(&wt, "retry.rs", "fn retry() {}\n");
    let r = env.hall(&wt, &["propose-ready", "--summary", "retries, 14 tests"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["state"], "checks", "{}", r.stdout);
    assert!(
        r.json["note"]
            .as_str()
            .unwrap()
            .contains("straight to review")
    );
    let ready = env
        .journal(&id)
        .into_iter()
        .find(|e| e["event"] == "ready")
        .unwrap();
    assert_eq!(ready["sha"], sha.as_str());
    assert_eq!(ready["note"], "nothing left to confirm");
    // Queued for the assayer, and nothing waits in the inbox.
    assert!(
        env.repo
            .join(".git/hallmark/queue")
            .join(format!("{id}.json"))
            .is_file()
    );
    assert_eq!(env.hall(&env.repo, &["inbox"]).json["entries"], json!([]));
}

#[test]
fn something_left_to_confirm_waits_for_you() {
    let env = Env::initialized();
    // An open question.
    let (_, wt) = env.new_item("Usage page");
    commit_file(&wt, "usage.rs", "fn usage() {}\n");
    env.hall(&wt, &["ask", "billed or raw?", "--options", "billed,raw"]);
    let r = env.hall(&wt, &["propose-ready", "--summary", "done"]);
    assert_eq!(r.json["state"], "awaiting_you", "{}", r.stdout);
    assert!(
        r.json["note"]
            .as_str()
            .unwrap()
            .contains("open question Q-1")
    );
    // Uncommitted changes.
    let (_, wt) = env.new_item("Dirty");
    std::fs::write(wt.join("README.md"), "changed\n").unwrap();
    let r = env.hall(&wt, &["propose-ready", "--summary", "done"]);
    assert_eq!(r.json["state"], "awaiting_you", "{}", r.stdout);
    // The agent asks for the human's call.
    let (_, wt) = env.new_item("Asked");
    let r = env.hall(&wt, &["propose-ready", "--summary", "done", "--ask"]);
    assert_eq!(r.json["state"], "awaiting_you", "{}", r.stdout);
    // auto_ready = false: always.
    let cfg = env.repo.join(".hallmark.toml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(
        &cfg,
        text.replace("auto_ready  = true", "auto_ready  = false"),
    )
    .unwrap();
    let (_, wt) = env.new_item("Off");
    commit_file(&wt, "off.rs", "fn off() {}\n");
    let r = env.hall(&wt, &["propose-ready", "--summary", "done"]);
    assert_eq!(r.json["state"], "awaiting_you", "{}", r.stdout);
}

#[test]
fn proposing_ready_twice_is_a_no_op() {
    let env = Env::initialized();
    let (_, wt) = env.new_item("Twice");
    commit_file(&wt, "twice.rs", "fn twice() {}\n");
    let first = env.hall(&wt, &["propose-ready", "--summary", "done"]);
    assert_eq!(first.json["state"], "checks", "{}", first.stdout);
    let again = env.hall(&wt, &["propose-ready", "--summary", "done"]);
    assert_eq!(again.code, 0, "{}", again.stdout);
    assert_eq!(again.json["changed"], false, "{}", again.stdout);
}

#[test]
fn doctor_lists_repo_skills_from_the_base() {
    let env = Env::initialized();
    let skills = || {
        let r = env.hall(&env.repo, &["doctor"]);
        assert_eq!(r.code, 0);
        let check = r.json["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "skills")
            .cloned();
        (check, r.json["next"].clone())
    };
    assert_eq!(skills().0, None, "no check without repo skills");

    let write = |name: &str, text: &str| {
        let f = env.repo.join(format!(".hallmark/skills/{name}/SKILL.md"));
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, text).unwrap();
    };
    write("principal-review", "Flag N+1 queries.\n");
    let (c, _) = skills();
    let c = c.unwrap();
    assert_eq!(c["ok"], true);
    assert!(
        c["detail"].as_str().unwrap().contains("isn't on main yet"),
        "{c}"
    );

    write("hallmark-fix", "---\nmode: replace\n---\nOurs.\n");
    write("review", "Misnamed.\n");
    git(&env.repo, &["add", "."]);
    git(&env.repo, &["commit", "-q", "-m", "skills"]);
    let (c, next) = skills();
    let c = c.unwrap();
    let detail = c["detail"].as_str().unwrap();
    assert_eq!(c["ok"], false, "{c}");
    assert!(detail.contains("hallmark-fix (replaces)"), "{detail}");
    assert!(detail.contains("principal-review (extends)"), "{detail}");
    assert!(
        detail.contains("`review` isn't a skill Hallmark uses"),
        "{detail}"
    );
    let fix = c["fix"].as_str().unwrap();
    assert!(
        fix.contains("principal-review, hallmark-fix, hallmark"),
        "{fix}"
    );
    assert!(next.as_array().unwrap().iter().any(|n| n == fix));
}

#[test]
fn doctor_lists_extensions_with_their_credit() {
    let env = Env::initialized();
    let ext = || {
        let r = env.hall(&env.repo, &["doctor"]);
        r.json["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "extensions")
            .map(|c| c["detail"].as_str().unwrap().to_string())
    };
    assert_eq!(ext(), None, "no check without extensions");

    env.hall(&env.repo, &["extensions", "enable", "mattpocock"]);
    let d = ext().unwrap();
    assert!(
        d.starts_with("mattpocock (by Matt Pocock, https://github.com/mattpocock/skills (MIT) at 49dd158): planner, developer, fixer, reviewer once .hallmark.toml is on main"),
        "{d}"
    );
    git(&env.repo, &["commit", "-q", "-am", "extensions"]);
    assert!(
        ext()
            .unwrap()
            .ends_with(": planner, developer, fixer, reviewer")
    );

    env.hall(&env.repo, &["extensions", "disable", "mattpocock"]);
    assert!(
        ext()
            .unwrap()
            .ends_with(": reviewer until .hallmark.toml is on main"),
        "the reviewer still follows main's"
    );
}
