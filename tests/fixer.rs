//! Workstream C: `hall fixed` and the fixer worker (the Repair Shop), with
//! the fake runner. No test spends model usage.

mod c_support;
mod common;

use c_support::*;
use common::{Env, commit_file, git, p};
use insta::assert_json_snapshot;
use serde_json::{Value, json};

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
            ".hallmark" => "[sha]",
        });
    };
}

fn fixer_pass(env: &Env, script: &std::path::Path) -> Value {
    let r = env.hall_fake(
        &env.repo,
        script,
        &["daemon", "run", "--once", "--only", "fixer"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    r.json["steps"][0].clone()
}

#[test]
fn fixed_records_fixes_and_disputes() {
    let env = Env::initialized();
    let (id, wt, sha) = fixing_item(&env, "Retry webhooks");
    assert_eq!(state(&env, &id), "fixing");
    commit_file(
        &wt,
        "retry.rs",
        "fn retry() {\n    lock();\n    deliver();\n}\n",
    );
    let r = env.hall(
        &wt,
        &[
            "fixed",
            "f1",
            "--dispute",
            "F2",
            "--reason",
            "tests/retry.rs already covers a double retry",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["state"], "checks");
    assert_eq!(r.json["round"], 2);
    assert_eq!(r.json["changed"], true);
    assert_ne!(r.json["sha"], sha.as_str());
    snap!("c_fixed", scrub(&env, &r.json));

    // The journal has it, by `you` (HALL_ACTOR in tests), and a ticket waits.
    let fx = events(&env, &id, "fix_submitted");
    assert_eq!(fx.len(), 1);
    assert_eq!(fx[0]["actor"], "you");
    assert_eq!(fx[0]["fixes"], json!(["F1"]));
    let ticket: Value = serde_json::from_str(
        &std::fs::read_to_string(env.repo.join(format!(".git/hallmark/queue/{id}.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(ticket["round"], 2);
    assert_eq!(ticket["sha"], r.json["sha"]);

    // Repeating it is safe.
    let again = env.hall(
        &wt,
        &["fixed", "F1", "--dispute", "F2", "--reason", "same reason"],
    );
    assert_eq!(again.code, 0, "{}", again.stdout);
    assert_eq!(again.json["changed"], false);
    assert_eq!(events(&env, &id, "fix_submitted").len(), 1);

    // `hall findings` shows the claim and the dispute; the review settles them.
    let f = env.hall(&env.repo, &["findings", &id, "--all"]);
    let all = f.json["findings"].as_array().unwrap();
    assert_eq!(all[0]["claimed_by"], r.json["sha"]);
    assert_eq!(all[0]["status"], "open");
    assert_eq!(
        all[1]["dispute"],
        "tests/retry.rs already covers a double retry"
    );
}

#[test]
fn fixed_refuses_what_it_cannot_record() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Usage page");
    // Not in fixing.
    let r = env.hall(&wt, &["fixed", "F1"]);
    assert_eq!(r.code, 4, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "WRONG_STATE");
    assert!(!r.json["error"]["next"].as_array().unwrap().is_empty());

    let (id2, wt2, _) = fixing_item(&env, "Retry webhooks");
    let _ = id;
    // Nothing committed yet.
    for args in [vec!["fixed", "F1"], vec!["fixed"]] {
        let r = env.hall(&wt2, &args);
        assert_eq!(r.code, 4, "{}", r.stdout);
        assert_eq!(r.json["error"]["code"], "WRONG_STATE");
        let msg = r.json["error"]["message"].as_str().unwrap();
        assert!(msg.contains("nothing committed"), "{msg}");
    }
    let r = env.hall(&wt2, &["fixed", "F1", "--dispute", "F2", "--reason", "x"]);
    assert_eq!(r.code, 4, "{}", r.stdout);
    let msg = r.json["error"]["message"].as_str().unwrap();
    assert!(msg.contains("commit the fix first"), "{msg}");
    // Uncommitted changes.
    std::fs::write(wt2.join("retry.rs"), "fn retry() { lock(); }\n").unwrap();
    let r = env.hall(&wt2, &["fixed", "F1"]);
    assert_eq!(r.code, 4);
    assert_eq!(r.json["error"]["code"], "DIRTY_WORKTREE");
    git(&wt2, &["commit", "-qam", "fix F1"]);
    // Unknown and closed IDs are usage errors that list the open ones.
    let r = env.hall(&wt2, &["fixed", "F1", "F9"]);
    assert_eq!(r.code, 2, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "USAGE");
    let msg = r.json["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("F9") && msg.contains("open: F1, F2, F3"),
        "{msg}"
    );
    // A dispute needs a reason (clap), and a fix can't also be a dispute.
    let r = env.hall(&wt2, &["fixed", "--dispute", "F2"]);
    assert_eq!(r.code, 2);
    assert_eq!(r.json["error"]["code"], "USAGE");
    let r = env.hall(&wt2, &["fixed", "F2", "--dispute", "F2", "--reason", "x"]);
    assert_eq!(r.code, 2);
    // Nothing was recorded by any of that.
    assert!(events(&env, &id2, "fix_submitted").is_empty());
    assert_eq!(state(&env, &id2), "fixing");
}

#[test]
fn a_dispute_alone_needs_no_commit_unless_checks_failed() {
    let env = Env::initialized();
    let (id, wt, sha) = fixing_item(&env, "Retry webhooks");
    let r = env.hall(
        &wt,
        &[
            "fixed",
            "--dispute",
            "F1",
            "--reason",
            "the row is locked by the caller",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["sha"], sha.as_str());
    assert_eq!(state(&env, &id), "checks");

    // When the checks then fail at that commit, disputing isn't enough.
    append(
        &env,
        &id,
        "assayer",
        checks(
            &sha,
            2,
            false,
            vec![hallmark::event::CheckRun {
                command: "cargo test".into(),
                exit_code: 1,
                duration_ms: 10,
                log: None,
                cached: false,
            }],
        ),
    );
    assert_eq!(state(&env, &id), "fixing");
    let r = env.hall(&wt, &["fixed", "--dispute", "F2", "--reason", "x"]);
    assert_eq!(r.code, 4, "{}", r.stdout);
    let msg = r.json["error"]["message"].as_str().unwrap();
    assert!(msg.contains("checks failed"), "{msg}");
    commit_file(&wt, "retry.rs", "fn retry() { deliver(); }\n");
    let r = env.hall(&wt, &["fixed"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(state(&env, &id), "checks");
    assert_eq!(r.json["round"], 3);
}

#[test]
fn the_fixer_commits_and_records_the_fix_itself() {
    let env = Env::initialized();
    set_config(&env, "gates", "[\"cargo test\"]");
    let (id, wt, _) = fixing_item(&env, "Retry webhooks");
    let script = env.script(json!([{
        "role": "fixer",
        "files": {"retry.rs": "fn retry() {\n    lock();\n    deliver();\n}\n"},
        "run": ["git add -A", "git commit -qm \"fix F1: lock before retrying\"", "{hall} fixed F1"],
        "structured": {"summary": "Locked the row.", "fixed": ["F1"], "disputed": []}
    }]));
    let step = fixer_pass(&env, &script);
    assert_eq!(step["progress"], "worked", "{step}");
    assert!(
        step["what"].as_str().unwrap().contains("resubmitted"),
        "{step}"
    );
    assert_eq!(state(&env, &id), "checks");
    let fx = events(&env, &id, "fix_submitted");
    assert_eq!(fx.len(), 1);
    assert_eq!(fx[0]["actor"], "fixer");
    assert_eq!(fx[0]["fixes"], json!(["F1"]));

    // The run: the fixer agent, in the worktree, accepting edits, with the
    // fix schema and only the tools it needs (plus the repo's gates).
    let calls = env.fake_calls();
    assert_eq!(calls.len(), 1);
    let c = &calls[0];
    assert_eq!(c["role"], "fixer");
    assert_eq!(c["room"], "repair");
    assert_eq!(c["agent"], "hallmark:fixer");
    assert_eq!(c["model"], "sonnet");
    assert_eq!(c["permission_mode"], "acceptEdits");
    assert_eq!(p(std::path::Path::new(c["cwd"].as_str().unwrap())), p(&wt));
    let schema: Value = serde_json::from_str(c["json_schema"].as_str().unwrap()).unwrap();
    assert_eq!(schema["title"], "Hallmark fix report");
    let tools: Vec<&str> = c["allowed_tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    for t in [
        "Edit",
        "Bash(git commit:*)",
        "Bash(hall fixed:*)",
        "Bash(cargo test:*)",
    ] {
        assert!(tools.contains(&t), "{t} in {tools:?}");
    }
    assert!(!tools.iter().any(|t| t.contains("push")));
    // HALL_ACTOR marks its events; PATH starts with this `hall`'s directory.
    let env_vars = c["env"].as_array().unwrap();
    assert_eq!(env_vars[0], json!(["HALL_ACTOR", "fixer"]));
    let path = env_vars.iter().find(|v| v[0] == "PATH").unwrap()[1]
        .as_str()
        .unwrap();
    assert!(env_vars.contains(&json!(["HALL_HEADLESS", "1"])));
    let exe_dir = std::path::Path::new(env!("CARGO_BIN_EXE_hall"))
        .parent()
        .unwrap();
    assert_eq!(
        std::env::split_paths(path).next().unwrap(),
        exe_dir,
        "{path}"
    );
    let prompt = c["prompt"].as_str().unwrap();
    for s in [
        "F1 · major · correctness · retry.rs:3",
        "Double delivery",
        "`cargo test`",
        "hall fixed",
    ] {
        assert!(prompt.contains(s), "{s} in {prompt}");
    }
    // Blockers and majors come before minors and nits.
    assert!(prompt.find("### F1").unwrap() < prompt.find("### F2").unwrap());
    // Usage is booked to the Repair Shop.
    let usage = events(&env, &id, "usage");
    assert_eq!(usage[0]["room"], "repair");
    assert_eq!(usage[0]["role"], "fixer");
    // The lease is gone, so the assayer can take the item.
    assert!(
        !env.repo
            .join(format!(".git/hallmark/leases/{id}.json"))
            .exists()
    );
    // Nothing is left to fix.
    let step = fixer_pass(&env, &script);
    assert_eq!(step["progress"], "idle");
}

#[test]
fn the_script_submits_from_the_fix_report() {
    let env = Env::initialized();
    let (id, _, _) = fixing_item(&env, "Retry webhooks");
    let script = env.script(json!([{
        "role": "fixer",
        "files": {"retry.rs": "fn retry() {\n    lock();\n    deliver();\n}\n"},
        "run": ["git add -A", "git commit -qm \"fix F1\""],
        "structured": {
            "summary": "Locked the row; F2 is covered.",
            "fixed": ["F1", "F9"],
            "disputed": [{"id": "F2", "reason": "tests/retry.rs covers it"}]
        }
    }]));
    let step = fixer_pass(&env, &script);
    assert_eq!(step["progress"], "worked", "{step}");
    assert_eq!(state(&env, &id), "checks");
    let fx = events(&env, &id, "fix_submitted");
    // F9 isn't a finding: dropped, not fatal.
    assert_eq!(fx[0]["fixes"], json!(["F1"]));
    assert_eq!(fx[0]["disputes"][0]["id"], "F2");
    assert_eq!(fx[0]["actor"], "fixer");
    // The fixer can't approve: the item went back to checks, nothing else.
    assert!(events(&env, &id, "hallmarked").is_empty());
}

#[test]
fn a_fixer_that_leaves_changes_fails_then_holds() {
    let env = Env::initialized();
    let (id, wt, _) = fixing_item(&env, "Retry webhooks");
    let script = env.script(json!([
        {"role": "fixer", "files": {"retry.rs": "half a fix"},
         "structured": {"summary": "s", "fixed": ["F1"], "disputed": []}},
        {"role": "fixer", "structured": {"summary": "s", "fixed": [], "disputed": []}}
    ]));
    let step = fixer_pass(&env, &script);
    assert!(
        step["what"].as_str().unwrap().contains("uncommitted"),
        "{step}"
    );
    let failed = events(&env, &id, "run_failed");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["attempt"], 1);
    assert_eq!(failed[0]["role"], "fixer");
    // The changes are kept for a person.
    assert_eq!(
        std::fs::read_to_string(wt.join("retry.rs")).unwrap(),
        "half a fix"
    );
    // The second tick doesn't spend a run on a dirty worktree: it holds.
    let step = fixer_pass(&env, &script);
    assert!(step["what"].as_str().unwrap().contains("held"), "{step}");
    assert_eq!(env.fake_calls().len(), 1);
    let held = events(&env, &id, "held");
    assert_eq!(held[0]["kind"], "run_failed");
    assert_eq!(state(&env, &id), "fixing");
    // Held items are skipped.
    assert_eq!(fixer_pass(&env, &script)["progress"], "idle");
}

#[test]
fn a_fixer_that_does_nothing_fails() {
    let env = Env::initialized();
    let (id, _, _) = fixing_item(&env, "Retry webhooks");
    let script = env.script(json!([
        {"role": "fixer", "structured": {"summary": "nothing to do", "fixed": ["F1"], "disputed": []}},
        {"role": "fixer", "fail": "timeout"}
    ]));
    let step = fixer_pass(&env, &script);
    assert!(
        step["what"].as_str().unwrap().contains("committed nothing"),
        "{step}"
    );
    // The second failure (a timeout, through the runner) holds the item.
    let step = fixer_pass(&env, &script);
    assert!(step["what"].as_str().unwrap().contains("held"), "{step}");
    let failed = events(&env, &id, "run_failed");
    assert_eq!(failed.len(), 2);
    assert_eq!(failed[1]["attempt"], 2);
    assert_eq!(events(&env, &id, "held")[0]["kind"], "run_failed");
}

#[test]
fn a_rate_limit_pauses_without_blaming_the_item() {
    let env = Env::initialized();
    let (id, _, _) = fixing_item(&env, "Retry webhooks");
    let script = env.script(json!([{"role": "fixer", "fail": "rate_limit"}]));
    let step = fixer_pass(&env, &script);
    assert_eq!(step["progress"], "pause", "{step}");
    assert!(events(&env, &id, "run_failed").is_empty());
}

#[test]
fn failing_checks_lead_the_prompt() {
    let env = Env::initialized();
    let (id, _, _) = failing_checks_item(&env, "Retry webhooks");
    let script = env.script(json!([{
        "role": "fixer",
        "files": {"retry.rs": "fn retry() { deliver(); }\n"},
        "run": ["git add -A", "git commit -qm \"fix checks\""],
        "structured": {"summary": "Fixed the failing test.", "fixed": [], "disputed": []}
    }]));
    let step = fixer_pass(&env, &script);
    assert_eq!(step["progress"], "worked", "{step}");
    let prompt = env.fake_calls()[0]["prompt"].as_str().unwrap().to_string();
    assert!(
        prompt.contains("## Failing checks (fix these first)"),
        "{prompt}"
    );
    assert!(prompt.contains("`cargo test` exited 101"));
    assert!(prompt.contains("test retry::twice ... FAILED"));
    assert!(prompt.contains("checks/r1.log"));
    assert!(
        !prompt.contains("test line 1\n"),
        "only the tail of the log"
    );
    assert_eq!(state(&env, &id), "checks");
    assert_eq!(events(&env, &id, "fix_submitted")[0]["fixes"], json!([]));
}

#[test]
fn an_external_engine_runs_in_the_worktree() {
    let env = Env::initialized();
    // The native path: cmd /C wants backslashes on Windows.
    let hall = hallmark::commands::new::shell_quote(env!("CARGO_BIN_EXE_hall"));
    // The engine commits and records the fix itself.
    set_config(
        &env,
        "fixer",
        &toml(&format!(
            "git commit -q --allow-empty -m \"fix F1 by the engine\" && {hall} fixed F1 --item {{id}}"
        )),
    );
    let (id, _, _) = fixing_item(&env, "Retry webhooks");
    let script = env.script(json!([]));
    let step = fixer_pass(&env, &script);
    assert_eq!(step["progress"], "worked", "{step}");
    assert!(env.fake_calls().is_empty(), "no claude -p run");
    assert_eq!(state(&env, &id), "checks");
    let fx = events(&env, &id, "fix_submitted");
    assert_eq!(fx[0]["fixes"], json!(["F1"]));
    assert_eq!(fx[0]["actor"], "fixer");
    // It got the brief.
    let brief: Value = serde_json::from_str(
        &std::fs::read_to_string(
            env.repo
                .join(format!(".git/hallmark/items/{id}/fix-r1.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(brief["round"], 1);
    assert_eq!(brief["findings"][0]["id"], "F1");
    assert!(
        brief["report_file"]
            .as_str()
            .unwrap()
            .ends_with("fix-r1.json.out")
    );
}

#[cfg(unix)]
#[test]
fn an_external_engine_can_report_instead() {
    let env = Env::initialized();
    set_config(
        &env,
        "fixer",
        &toml(
            "git commit -q --allow-empty -m fix && printf '%s' '{\"summary\":\"s\",\"fixed\":[\"F1\"],\"disputed\":[{\"id\":\"F3\",\"reason\":\"house style\"}]}' > {findings_file}.out",
        ),
    );
    let (id, _, _) = fixing_item(&env, "Retry webhooks");
    let step = fixer_pass(&env, &env.script(json!([])));
    assert_eq!(step["progress"], "worked", "{step}");
    let fx = events(&env, &id, "fix_submitted");
    assert_eq!(fx[0]["fixes"], json!(["F1"]));
    assert_eq!(fx[0]["disputes"][0]["id"], "F3");
}

#[test]
fn a_failing_engine_counts_as_a_failed_run() {
    let env = Env::initialized();
    set_config(&env, "fixer", &toml("git no-such-command"));
    let (id, _, _) = fixing_item(&env, "Retry webhooks");
    let step = fixer_pass(&env, &env.script(json!([])));
    assert!(
        step["what"].as_str().unwrap().contains("fix engine"),
        "{step}"
    );
    assert_eq!(events(&env, &id, "run_failed")[0]["attempt"], 1);
    assert_eq!(state(&env, &id), "fixing");
}

#[test]
fn the_repo_fix_skill_from_the_base_reaches_the_fixer() {
    let env = Env::initialized();
    let (_, _, _) = fixing_item(&env, "Retry webhooks");
    // Committed on the base after the item started: read when the fixer runs.
    let skill = env.repo.join(".hallmark/skills/hallmark-fix/SKILL.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    std::fs::write(
        &skill,
        "---\nmode: replace\n---\n\nFIX_SKILL_MARKER: run `make db-test` before `hall fixed`.\n",
    )
    .unwrap();
    git(&env.repo, &["add", "."]);
    git(&env.repo, &["commit", "-q", "-m", "fix skill"]);

    let script = env.script(json!([
        {"role": "fixer", "structured": {"summary": "nothing", "fixed": [], "disputed": []}}
    ]));
    fixer_pass(&env, &script);
    let prompt = env.fake_calls()[0]["prompt"].as_str().unwrap().to_string();
    let at = prompt
        .find("## This repository's `hallmark-fix` skill")
        .unwrap_or_else(|| panic!("{prompt}"));
    assert!(prompt.contains("replaces Hallmark's `hallmark-fix` skill"));
    assert!(prompt[at..].contains("FIX_SKILL_MARKER"));
    assert!(
        at < prompt.find("## When you are done").unwrap(),
        "before the closing steps"
    );
}

#[test]
fn an_extension_for_the_fixer_reaches_its_prompt() {
    let env = Env::initialized();
    let (_, _, _) = fixing_item(&env, "Retry webhooks");
    let r = env.hall(
        &env.repo,
        &["extensions", "enable", "mattpocock", "--role", "fixer"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert!(
        r.json["note"].is_null(),
        "only the reviewer waits for the base"
    );

    let script = env.script(json!([
        {"role": "fixer", "structured": {"summary": "nothing", "fixed": [], "disputed": []}}
    ]));
    fixer_pass(&env, &script);
    let prompt = env.fake_calls()[0]["prompt"].as_str().unwrap().to_string();
    let at = prompt
        .find("## Extensions\n")
        .unwrap_or_else(|| panic!("{prompt}"));
    assert!(prompt[at..].contains("for the fixer"));
    // Open correctness and evidence findings, gates green: `tdd` only.
    assert!(prompt[at..].contains("name=\"tdd\""));
    assert!(
        !prompt.contains("name=\"diagnosing-bugs\""),
        "no gate failed"
    );
    assert!(prompt[at..].contains("How it fits your role: Fix test-first"));
    assert!(!prompt.contains("name=\"code-review\""));
    assert!(
        at < prompt.find("## When you are done").unwrap(),
        "before the closing steps"
    );
}

#[test]
fn a_red_gate_brings_diagnosing_bugs_to_the_fixer_and_no_finding_leaves_out_tdd() {
    let env = Env::initialized();
    let (_, _, _) = failing_checks_item(&env, "Retry webhooks");
    let r = env.hall(
        &env.repo,
        &["extensions", "enable", "mattpocock", "--role", "fixer"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);

    let script = env.script(json!([
        {"role": "fixer", "structured": {"summary": "nothing", "fixed": [], "disputed": []}}
    ]));
    fixer_pass(&env, &script);
    let prompt = env.fake_calls()[0]["prompt"].as_str().unwrap().to_string();
    let at = prompt
        .find("## Extensions\n")
        .unwrap_or_else(|| panic!("{prompt}"));
    assert!(prompt[at..].contains("name=\"diagnosing-bugs\""));
    assert!(!prompt.contains("name=\"tdd\""), "no finding calls for it");
}

/// One assayer tick: runs the gates of an item in checks.
fn assayer_pass(env: &Env, script: &std::path::Path) -> Value {
    let r = env.hall_fake(
        &env.repo,
        script,
        &["daemon", "run", "--once", "--only", "assayer"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    r.json["steps"][0].clone()
}

#[test]
fn a_repo_that_sets_no_budget_holds_an_item_at_800k() {
    let env = Env::initialized();
    // No [budget] table at all: the default caps apply.
    let path = env.repo.join(".hallmark.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    let without = text.split("[budget]").next().unwrap().to_string();
    assert!(!without.contains("per_item"), "{without}");
    std::fs::write(&path, without).unwrap();
    git(&env.repo, &["commit", "-qam", "no budget"]);

    let (id, _, _) = fixing_item(&env, "Retry webhooks");
    append(
        &env,
        &id,
        "fixer",
        hallmark::event::Body::Usage(hallmark::event::Usage {
            source: "headless".into(),
            room: "repair".into(),
            role: "fixer".into(),
            model: "claude-sonnet-5".into(),
            input_tokens: 800_000,
            ..Default::default()
        }),
    );
    let script = env.script(json!([
        {"role": "fixer", "structured": {"summary": "s", "fixed": [], "disputed": []}}
    ]));
    let step = fixer_pass(&env, &script);
    assert!(step["what"].as_str().unwrap().contains("budget"), "{step}");
    assert!(env.fake_calls().is_empty(), "no run past the cap");
    let held = events(&env, &id, "held");
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(held[0]["kind"], "budget");
    let reason = held[0]["reason"].as_str().unwrap();
    assert!(reason.contains("800k of 800k"), "{reason}");

    // `"none"` turns the cap off.
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{text}\n[budget]\nper_item = \"none\"\n")).unwrap();
    let r = env.hall(&env.repo, &["retry", &id, "--reason", "no cap"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let released = events(&env, &id, "released");
    assert!(
        released.last().unwrap()["granted_tokens"].is_null(),
        "retry saw a cap: {released:?}"
    );
    let step = fixer_pass(&env, &script);
    assert_eq!(env.fake_calls().len(), 1, "{step}");
    assert_eq!(events(&env, &id, "held").len(), 1);
}

#[test]
fn three_failed_checks_in_a_row_escalate() {
    let env = Env::initialized();
    // Fails under both `sh -c` and `cmd /C`.
    set_config(
        &env,
        "gates",
        "[\"git ls-files missing-file --error-unmatch\"]",
    );
    git(&env.repo, &["commit", "-qam", "a failing gate"]);
    let (id, _, _) = ready_item(&env, "Retry webhooks");
    // A fixer that commits a no-op every round: the gate never turns green.
    let noop = json!({
        "role": "fixer",
        "run": ["git commit -q --allow-empty -m \"no-op\""],
        "structured": {"summary": "Tried again.", "fixed": [], "disputed": []}
    });
    let script = env.script(json!([noop, noop, noop, noop]));

    for round in 1..=2 {
        let step = assayer_pass(&env, &script);
        assert!(
            step["what"].as_str().unwrap().contains("checks failed"),
            "{step}"
        );
        assert_eq!(state(&env, &id), "fixing");
        assert!(events(&env, &id, "held").is_empty(), "round {round}");
        let step = fixer_pass(&env, &script);
        assert_eq!(step["progress"], "worked", "{step}");
        assert_eq!(state(&env, &id), "checks");
    }
    // The third failure in a row escalates: held for you, kind `checks`.
    let step = assayer_pass(&env, &script);
    assert!(step["what"].as_str().unwrap().contains("held"), "{step}");
    assert_eq!(state(&env, &id), "fixing");
    let held = events(&env, &id, "held");
    assert_eq!(held.len(), 1);
    assert_eq!(held[0]["kind"], "checks");
    assert_eq!(held[0]["actor"], "assayer");
    let reason = held[0]["reason"].as_str().unwrap();
    assert!(reason.contains("3 times in a row"), "{reason}");
    // The fixer leaves it alone, and it waits in your office.
    assert_eq!(fixer_pass(&env, &script)["progress"], "idle");
    assert_eq!(env.fake_calls().len(), 2);
    let inbox = env.hall(&env.repo, &["inbox"]);
    let entries = inbox.json["entries"].as_array().unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e["kind"] == "checks" && e["item"] == id.as_str()),
        "{}",
        inbox.stdout
    );

    // `hall retry` gives it another three rounds.
    let r = env.hall(&env.repo, &["retry", &id, "--reason", "one more go"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(fixer_pass(&env, &script)["progress"], "worked");
    let step = assayer_pass(&env, &script);
    assert!(
        step["what"].as_str().unwrap().contains("checks failed"),
        "{step}"
    );
    assert_eq!(events(&env, &id, "held").len(), 1);
}

#[test]
fn checks_that_pass_after_two_failures_leave_the_item_unheld() {
    let env = Env::initialized();
    // Fails until the fixer tracks fixed.txt.
    set_config(
        &env,
        "gates",
        "[\"git ls-files fixed.txt --error-unmatch\"]",
    );
    git(
        &env.repo,
        &["commit", "-qam", "a gate that needs fixed.txt"],
    );
    let (id, _, _) = ready_item(&env, "Retry webhooks");
    let noop = json!({
        "role": "fixer",
        "run": ["git commit -q --allow-empty -m \"no-op\""],
        "structured": {"summary": "Tried again.", "fixed": [], "disputed": []}
    });
    let fix = json!({
        "role": "fixer",
        "run": [
            "echo fixed> fixed.txt",
            "git add fixed.txt",
            "git commit -q -m \"add fixed.txt\""
        ],
        "structured": {"summary": "Got it.", "fixed": [], "disputed": []}
    });
    let script = env.script(json!([noop, fix]));

    for _ in 0..2 {
        let step = assayer_pass(&env, &script);
        assert!(
            step["what"].as_str().unwrap().contains("checks failed"),
            "{step}"
        );
        assert_eq!(fixer_pass(&env, &script)["progress"], "worked");
    }
    let step = assayer_pass(&env, &script);
    assert!(
        step["what"].as_str().unwrap().contains("checks passed"),
        "{step}"
    );
    assert_eq!(state(&env, &id), "review");
    assert!(events(&env, &id, "held").is_empty());
}

#[test]
fn max_check_failures_zero_never_holds() {
    let env = Env::initialized();
    set_config(
        &env,
        "gates",
        "[\"git ls-files missing-file --error-unmatch\"]",
    );
    set_config(&env, "max_check_failures", "0");
    git(&env.repo, &["commit", "-qam", "never hold"]);
    let (id, _, _) = ready_item(&env, "Retry webhooks");
    let noop = json!({
        "role": "fixer",
        "run": ["git commit -q --allow-empty -m \"no-op\""],
        "structured": {"summary": "Tried again.", "fixed": [], "disputed": []}
    });
    let script = env.script(json!([noop, noop, noop, noop]));
    for _ in 0..3 {
        let step = assayer_pass(&env, &script);
        assert!(
            step["what"].as_str().unwrap().contains("checks failed"),
            "{step}"
        );
        assert_eq!(fixer_pass(&env, &script)["progress"], "worked");
    }
    assert!(events(&env, &id, "held").is_empty());
}
