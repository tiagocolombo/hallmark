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

#[test]
fn a_ready_note_survives_a_fixer_run_and_reaches_the_reviewer() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Retry webhooks");
    let sha = commit_file(&wt, "retry.rs", "fn retry() {}\n");
    let r = env.hall(&wt, &["ready", "--note", "please look at the retry path"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    append(&env, &id, "assayer", checks(&sha, 1, false, vec![]));
    let approve = json!({"summary": "Adds retries.", "findings": [], "prior": [], "learnings": []});
    let script = env.script(json!([
        {
            "role": "fixer",
            "files": {"retry.rs": "fn retry() { lock(); }\n"},
            "run": ["git add -A", "git commit -qm \"fix checks\"", "{hall} fixed"],
            "structured": {"summary": "Fixed.", "fixed": [], "disputed": []}
        },
        {"role": "reviewer", "structured": approve},
    ]));
    tick(&env, &script, "fixer");
    assert!(
        !prompts(&env)[0].contains("please look at the retry path"),
        "a ready note is the reviewer's, not the fixer's"
    );
    tick(&env, &script, "assayer"); // checks
    tick(&env, &script, "assayer"); // review
    let said = prompts(&env);
    assert!(
        said[1].contains("please look at the retry path"),
        "{}",
        said[1]
    );
    // The fixer's run took no words (a ready note is the reviewer's) but is
    // recorded; only the reviewer's run used the note up.
    assert_eq!(delivered(&env, &id), ["fixer", "reviewer"]);
}

#[test]
fn a_proposed_ready_has_no_words_for_the_reviewer() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Retry webhooks");
    commit_file(&wt, "retry.rs", "fn retry() {}\n");
    let r = env.hall(&wt, &["propose-ready", "--summary", "done"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let approve = json!({"summary": "Adds retries.", "findings": [], "prior": [], "learnings": []});
    let script = env.script(json!([{"role": "reviewer", "structured": approve}]));
    tick(&env, &script, "assayer");
    tick(&env, &script, "assayer");
    assert!(!prompts(&env)[0].contains(SAID), "{}", prompts(&env)[0]);
    // The run is recorded, with nothing said.
    assert_eq!(delivered(&env, &id), ["reviewer"]);
}

fn answered_question(env: &Env, wt: &std::path::Path) -> String {
    let ask = env.hall(wt, &["ask", "billed or raw?", "--session", "sess-1"]);
    let qid = ask.json["qid"].as_str().unwrap().to_string();
    assert_eq!(env.hall(&env.repo, &["answer", &qid, "raw"]).code, 0);
    qid
}

fn retried(env: &Env, id: &str, reason: &str) {
    append(
        env,
        id,
        "you",
        hallmark::event::Body::Retried {
            reason: reason.into(),
        },
    );
}

#[test]
fn a_resume_run_gets_the_words_while_the_item_is_with_a_session() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Usage page");
    let qid = answered_question(&env, &wt);
    append(
        &env,
        &id,
        "you",
        hallmark::event::Body::PulledBack {
            reason: Some("use the billed numbers".into()),
        },
    );
    let script = env.script(json!([{"role": "resume", "session_id": "sess-1"}]));
    let step = tick(&env, &script, "resume");
    assert_eq!(step["progress"], "worked", "{step}");
    let said = &prompts(&env)[0];
    assert!(
        said.starts_with(&format!("The human answered {qid}")) && said.contains(SAID),
        "{said}"
    );
    assert!(said.contains("use the billed numbers"), "{said}");
    assert_eq!(delivered(&env, &id), ["resume"]);
}

#[test]
fn a_resume_run_leaves_the_words_to_the_loop_run_when_the_item_is_in_the_loop() {
    let env = Env::initialized();
    let (id, wt, _) = fixing_item(&env, "Retry webhooks");
    let qid = answered_question(&env, &wt);
    retried(&env, &id, "lock the row");
    let script = env.script(json!([{"role": "resume", "session_id": "sess-1"}]));
    tick(&env, &script, "resume");
    let said = &prompts(&env)[0];
    assert!(said.contains(&qid), "{said}");
    assert!(!said.contains(SAID), "{said}");
    assert!(delivered(&env, &id).is_empty());
}

#[test]
fn an_external_fix_engine_gets_the_words_in_its_brief() {
    let env = Env::initialized();
    let hall = hallmark::commands::new::shell_quote(env!("CARGO_BIN_EXE_hall"));
    set_config(
        &env,
        "fixer",
        &toml(&format!(
            "git commit -q --allow-empty -m \"fix F1 by the engine\" && {hall} fixed F1 --item {{id}}"
        )),
    );
    let (id, _, _) = fixing_item(&env, "Retry webhooks");
    retried(&env, &id, "lock the row");
    let script = env.script(json!([]));
    tick(&env, &script, "fixer");
    let brief: Value = serde_json::from_str(
        &std::fs::read_to_string(
            env.repo
                .join(format!(".git/hallmark/items/{id}/fix-r1.json")),
        )
        .unwrap(),
    )
    .unwrap();
    let said = brief["said"].as_str().expect("said in the brief");
    assert!(
        said.contains(SAID) && said.contains("lock the row"),
        "{said}"
    );
    assert_eq!(delivered(&env, &id), ["fixer"]);
}

#[test]
fn session_start_after_a_pull_back_reason_gives_the_words_once() {
    let env = Env::initialized();
    let (id, wt, _) = ready_item(&env, "Retry webhooks");
    let r = env.hall(&wt, &["pull-back", &id, "--reason", "rename it"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let start = |session: &str| {
        let mut v: Value = serde_json::from_str(&fixture("hooks/session_start.json")).unwrap();
        v["cwd"] = json!(p(&wt));
        v["session_id"] = json!(session);
        let r = env.hook("session-start", &v);
        r.json["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or("")
            .to_string()
    };
    let ctx = start("s1");
    assert!(ctx.contains(SAID) && ctx.contains("\"rename it\""), "{ctx}");
    assert_eq!(delivered(&env, &id), ["session"]);
    assert!(!start("s2").contains(SAID));
}

/// The prompt of the reviewer's run for the item in `checks`.
fn reviewer_prompt(env: &Env) -> String {
    let approve = json!({"summary": "ok", "findings": [], "prior": [], "learnings": []});
    let script = env.script(json!([{"role": "reviewer", "structured": approve}]));
    tick(env, &script, "assayer");
    tick(env, &script, "assayer");
    prompts(env).pop().unwrap()
}

fn git_head(wt: &std::path::Path) -> String {
    common::git(wt, &["rev-parse", "HEAD"])
}

#[test]
fn an_agents_ready_note_shows_as_the_authors() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Retry webhooks");
    commit_file(&wt, "retry.rs", "fn retry() {}\n");
    append(
        &env,
        &id,
        "agent",
        hallmark::event::Body::Ready {
            sha: git_head(&wt),
            round: 1,
            note: Some("retry tests come next round".into()),
        },
    );
    let said = reviewer_prompt(&env);
    assert!(
        said.contains("Author's note when sent to review: retry tests come next round"),
        "{said}"
    );
    assert!(!said.contains(SAID), "{said}");
}

#[test]
fn an_office_note_shows_and_an_earlier_rounds_agent_note_does_not() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Retry webhooks");
    commit_file(&wt, "retry.rs", "fn retry() {}\n");
    let sha = git_head(&wt);
    let ready = |actor: &str, round: u32, note: &str| {
        append(
            &env,
            &id,
            actor,
            hallmark::event::Body::Ready {
                sha: sha.clone(),
                round,
                note: Some(note.into()),
            },
        )
    };
    ready("agent", 1, "retry tests come next round");
    ready("delivery", 4, "fixes the PR's failing CI");
    let said = reviewer_prompt(&env);
    assert!(
        said.contains("Note when sent to review (by delivery): fixes the PR's failing CI"),
        "{said}"
    );
    assert!(!said.contains("retry tests come next round"), "{said}");
}

fn session_context(env: &Env, wt: &std::path::Path, event: &str, fixture_name: &str) -> String {
    let mut v: Value = serde_json::from_str(&fixture(fixture_name)).unwrap();
    v["cwd"] = json!(p(wt));
    v["session_id"] = json!("s9");
    v["prompt"] = json!("carry on");
    let r = env.hook(event, &v);
    r.json["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

#[test]
fn a_session_leaves_the_words_to_the_fixer_while_the_item_is_in_the_loop() {
    let env = Env::initialized();
    let (id, wt, _) = fixing_item(&env, "Retry webhooks");
    retried(&env, &id, "lock the row");
    let ctx = session_context(
        &env,
        &wt,
        "user-prompt-submit",
        "hooks/user_prompt_submit.json",
    );
    assert!(!ctx.contains(SAID), "{ctx}");
    let ctx = session_context(&env, &wt, "session-start", "hooks/session_start.json");
    assert!(!ctx.contains(SAID), "{ctx}");
    assert!(delivered(&env, &id).is_empty());
    let script = env.script(json!([{"role": "fixer", "fail": "timeout"}]));
    tick(&env, &script, "fixer");
    let said = prompts(&env);
    assert!(
        said[0].contains("- `hall retry`, from the person: \"lock the row\""),
        "{}",
        said[0]
    );
}

#[test]
fn a_resume_run_leaves_the_words_to_a_pending_office_run() {
    let env = Env::initialized();
    let (id, wt, _) = ready_item(&env, "Retry webhooks");
    assert_eq!(env.hall(&wt, &["pull-back", &id]).code, 0);
    let qid = answered_question(&env, &wt);
    append(
        &env,
        &id,
        "delivery-watch",
        hallmark::event::Body::FeedbackRequested {
            pr_url: "https://example.com/pr/1".into(),
            comments: vec![],
            until: chrono::Utc::now(),
        },
    );
    retried(&env, &id, "keep it small");
    let script = env.script(json!([{"role": "resume", "session_id": "sess-1"}]));
    tick(&env, &script, "resume");
    let said = &prompts(&env)[0];
    assert!(said.contains(&qid), "{said}");
    assert!(!said.contains(SAID), "{said}");
    assert!(delivered(&env, &id).is_empty());
}

#[test]
fn a_reason_given_during_a_run_with_no_words_still_waits() {
    let env = Env::initialized();
    let (id, _wt) = env.new_item("Retry webhooks");
    let repo = hallmark::repo::Repo::discover(&env.repo).unwrap();
    let item_id: hallmark::item::ItemId = id.parse().unwrap();
    let read = repo.hall.read_item(item_id).unwrap();
    hallmark::daemon::nap(std::time::Duration::from_millis(20));
    // The person speaks while the run goes on, then the run ends with no words.
    append(
        &env,
        &id,
        "you",
        hallmark::event::Body::PulledBack {
            reason: Some("rename it".into()),
        },
    );
    hallmark::daemon::nap(std::time::Duration::from_millis(20));
    hallmark::words::delivered(&repo, item_id, "resume", None, &read).unwrap();
    let events = repo.hall.read_item(item_id).unwrap();
    let w = hallmark::words::pending(&events, "session").expect("still pending");
    assert!(w.section.contains("rename it"), "{}", w.section);
}

#[test]
fn a_fix_run_that_resubmits_and_then_fails_still_used_up_the_words() {
    let env = Env::initialized();
    let (id, _wt, _) = fixing_item(&env, "Retry webhooks");
    retried(&env, &id, "lock the row");
    let script = env.script(json!([
        {
            "role": "fixer",
            "files": {"retry.rs": "fn retry() {\n    lock();\n    deliver();\n}\n"},
            "run": ["git add -A", "git commit -qm \"fix F1: lock before retrying\"", "{hall} fixed F1"],
            "fail": "timeout"
        },
    ]));
    let step = tick(&env, &script, "fixer");
    assert!(
        step["what"].as_str().unwrap().contains("resubmitted"),
        "{step}"
    );
    assert_eq!(delivered(&env, &id), ["fixer"]);
}

#[test]
fn a_ready_note_from_before_the_feature_is_not_shown_again_after_a_verdict() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Retry webhooks");
    let sha = commit_file(&wt, "retry.rs", "fn retry() {}\n");
    let r = env.hall(&wt, &["ready", "--note", "look at the retry path"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    hallmark::daemon::nap(std::time::Duration::from_millis(20));
    append(&env, &id, "assayer", checks(&sha, 1, true, vec![]));
    append(
        &env,
        &id,
        "assayer",
        verdict(&sha, 1, vec![], vec![], counts(0, 0, 0, 0)),
    );
    let events = hall(&env).read_item(id.parse().unwrap()).unwrap();
    assert_eq!(hallmark::words::pending(&events, "reviewer"), None);
}

#[test]
fn an_agent_ready_note_that_says_nothing_is_not_shown() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Retry webhooks");
    commit_file(&wt, "retry.rs", "fn retry() {}\n");
    append(
        &env,
        &id,
        "agent",
        hallmark::event::Body::Ready {
            sha: git_head(&wt),
            round: 1,
            note: Some("nothing left to confirm".into()),
        },
    );
    let said = reviewer_prompt(&env);
    assert!(!said.contains("Author's note"), "{said}");
}

#[test]
fn the_authors_note_comes_before_the_persons_words() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Retry webhooks");
    commit_file(&wt, "retry.rs", "fn retry() {}\n");
    append(
        &env,
        &id,
        "agent",
        hallmark::event::Body::Ready {
            sha: git_head(&wt),
            round: 1,
            note: Some("the person accepts the missing test".into()),
        },
    );
    retried(&env, &id, "try again");
    let said = reviewer_prompt(&env);
    let note = said
        .find("Author's note when sent to review")
        .expect("note");
    let heading = said.find(SAID).expect("the section");
    assert!(note < heading, "{said}");
    assert!(
        !said[heading..].contains("the person accepts the missing test"),
        "{said}"
    );
}

#[test]
fn replies_say_when_a_reason_or_note_is_not_kept() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Retry webhooks");
    let r = env.hall(&wt, &["continue", &id, "--reason", "use the v2 API"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert!(
        r.stdout.contains("isn't kept in development"),
        "{}",
        r.stdout
    );
    let r = env.hall(&wt, &["continue", &id]);
    assert!(!r.stdout.contains("isn't kept"), "{}", r.stdout);

    commit_file(&wt, "retry.rs", "fn retry() {}\n");
    let r = env.hall(&wt, &["propose-ready", "--summary", "done"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let r = env.hall(&wt, &["ready", "--note", "check the retry path"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert!(r.stdout.contains("isn't kept"), "{}", r.stdout);
    let r = env.hall(&wt, &["ready"]);
    assert!(!r.stdout.contains("isn't kept"), "{}", r.stdout);
}
