//! A person's reasons reach the agent's next prompt (ADR 0044): the reviewer
//! after `hall ready --note`, the fixer after `hall retry`, a development
//! session after `hall pull-back`. Each run that finishes is given them once;
//! a run that fails is retried with them.

mod c_support;
mod common;

use c_support::*;
use common::{Env, commit_file, fixture, p};
use serde_json::{Value, json};

const SAID: &str = "## What the person said";

fn tick(env: &Env, script: &std::path::Path, worker: &str) -> Value {
    let r = env.hall_fake(
        &env.repo,
        script,
        &["daemon", "run", "--once", "--only", worker],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    r.json["steps"][0].clone()
}

/// The prompts the fake runner was given, oldest first.
fn prompts(env: &Env) -> Vec<String> {
    env.fake_calls()
        .iter()
        .map(|c| c["prompt"].as_str().unwrap_or("").to_string())
        .collect()
}

fn delivered(env: &Env, id: &str) -> Vec<String> {
    events(env, id, "reasons_delivered")
        .iter()
        .map(|e| e["to"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn the_ready_note_reaches_the_reviewer_once_even_after_a_failed_run() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Retry webhooks");
    commit_file(&wt, "retry.rs", "fn retry() {}\n");
    let r = env.hall(&wt, &["ready", "--note", "please look at the retry path"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let approve = json!({"summary": "Adds retries.", "findings": [], "prior": [], "learnings": []});
    let script = env.script(json!([
        {"role": "reviewer", "fail": "timeout"},
        {"role": "reviewer", "structured": approve},
        {"role": "reviewer", "structured": approve},
    ]));
    tick(&env, &script, "assayer"); // checks
    tick(&env, &script, "assayer"); // the review run times out
    assert!(
        delivered(&env, &id).is_empty(),
        "a failed run wasn't given them"
    );
    tick(&env, &script, "assayer"); // retried, and approved
    assert_eq!(state(&env, &id), "hallmarked");
    let said = prompts(&env);
    assert_eq!(said.len(), 2);
    for prompt in &said {
        let at = prompt.find(SAID).expect("the section");
        assert!(
            prompt[at..].contains(
                "- `hall ready --note`, from the person: \"please look at the retry path\""
            ),
            "{prompt}"
        );
    }
    assert_eq!(delivered(&env, &id), ["reviewer"]);

    // Reviewed again later: the note isn't repeated.
    let r = env.hall(&wt, &["pull-back", &id]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    commit_file(&wt, "retry.rs", "fn retry() { deliver(); }\n");
    assert_eq!(env.hall(&wt, &["ready"]).code, 0);
    tick(&env, &script, "assayer");
    tick(&env, &script, "assayer");
    let said = prompts(&env);
    assert_eq!(said.len(), 3);
    assert!(!said[2].contains(SAID), "{}", said[2]);
}

#[test]
fn a_retry_reason_reaches_the_fixer() {
    let env = Env::initialized();
    let (id, wt, _) = fixing_item(&env, "Retry webhooks");
    let script = env.script(json!([
        {"role": "fixer", "fail": "timeout"},
        {"role": "fixer", "fail": "timeout"},
        {
            "role": "fixer",
            "files": {"retry.rs": "fn retry() {\n    lock();\n    deliver();\n}\n"},
            "run": ["git add -A", "git commit -qm \"fix F1: lock before retrying\"", "{hall} fixed F1"],
            "structured": {"summary": "Locked the row.", "fixed": ["F1"], "disputed": []}
        },
    ]));
    tick(&env, &script, "fixer");
    tick(&env, &script, "fixer");
    assert_eq!(
        events(&env, &id, "held").len(),
        1,
        "held after two failures"
    );
    let r = env.hall(
        &wt,
        &[
            "retry",
            &id,
            "--reason",
            "lock the row; never deliver twice",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    let step = tick(&env, &script, "fixer");
    assert!(
        step["what"].as_str().unwrap().contains("resubmitted"),
        "{step}"
    );
    let said = prompts(&env);
    assert_eq!(said.len(), 3);
    assert!(!said[0].contains(SAID), "nothing was said yet");
    assert!(
        said[2].contains("- `hall retry`, from the person: \"lock the row; never deliver twice\""),
        "{}",
        said[2]
    );
    assert_eq!(delivered(&env, &id), ["fixer"]);
}

#[test]
fn a_pull_back_reason_reaches_the_session_once() {
    let env = Env::initialized();
    let (id, wt, _) = ready_item(&env, "Retry webhooks");
    let prompt = |text: &str| {
        let mut v: Value = serde_json::from_str(&fixture("hooks/user_prompt_submit.json")).unwrap();
        v["cwd"] = json!(p(&wt));
        v["session_id"] = json!("s1");
        v["prompt"] = json!(text);
        let r = env.hook("user-prompt-submit", &v);
        r.json["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or("")
            .to_string()
    };
    let r = env.hall(
        &wt,
        &["pull-back", &id, "--reason", "rename it to deliver_once"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    let ctx = prompt("carry on");
    assert!(
        ctx.contains("- `hall pull-back`, from the person: \"rename it to deliver_once\""),
        "{ctx}"
    );
    assert_eq!(delivered(&env, &id), ["session"]);
    let ctx = prompt("and the tests?");
    assert!(!ctx.contains(SAID), "once: {ctx}");

    // Sent to review with a note: a session doesn't take the reviewer's words.
    commit_file(&wt, "retry.rs", "fn deliver_once() {}\n");
    let r = env.hall(&wt, &["ready", "--note", "renamed as asked"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let ctx = prompt("how is it going?");
    assert!(!ctx.contains(SAID), "{ctx}");
    assert_eq!(delivered(&env, &id), ["session"]);
}
