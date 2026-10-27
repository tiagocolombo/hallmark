//! `hall hook <event>` with the stdin shapes captured in Step 0.

mod common;

use std::path::Path;

use common::{Env, commit_file, fixture, git, p};
use serde_json::{Value, json};

/// A captured hook input, pointed at our temp repo and session.
fn input(name: &str, cwd: &Path, session: &str) -> Value {
    let mut v: Value = serde_json::from_str(&fixture(&format!("hooks/{name}.json"))).unwrap();
    v["cwd"] = json!(p(cwd));
    v["session_id"] = json!(session);
    v
}

fn write(cwd: &Path, file: &Path) -> Value {
    let mut v = input("pre_tool_use_write", cwd, "s1");
    v["tool_input"]["file_path"] = json!(p(file));
    v
}

fn bash(event: &str, cwd: &Path, command: &str, session: &str) -> Value {
    let name = if event == "pre-tool-use" {
        "pre_tool_use_bash"
    } else {
        "post_tool_use_bash"
    };
    let mut v = input(name, cwd, session);
    v["tool_input"]["command"] = json!(command);
    v
}

fn decision(v: &Value) -> (String, Value) {
    let h = &v["hookSpecificOutput"];
    let reason: Value =
        serde_json::from_str(h["permissionDecisionReason"].as_str().unwrap_or("null")).unwrap();
    (
        h["permissionDecision"].as_str().unwrap_or("").to_string(),
        reason,
    )
}

#[test]
fn hooks_do_nothing_outside_hallmark_repos() {
    let env = Env::new(); // a git repo without `hall init`
    for (event, fixture_name) in [
        ("session-start", "session_start"),
        ("pre-tool-use", "pre_tool_use_write"),
        ("stop", "stop"),
        ("notification", "notification"),
    ] {
        let r = env.hook(event, &input(fixture_name, &env.repo, "s1"));
        assert_eq!((r.code, r.stdout.as_str()), (0, ""), "{event}");
    }
    assert!(!env.repo.join(".git/hallmark").exists());
    // Garbage on stdin never breaks a session either.
    let out = env
        .cmd(&env.repo)
        .args(["hook", "pre-tool-use"])
        .write_stdin("not json")
        .output()
        .unwrap();
    assert!(out.status.success());
}

#[test]
fn the_edit_gate() {
    let env = Env::initialized();
    let main_file = env.repo.join("src/lib.rs");

    // No item: the first edit is denied with what to run.
    let r = env.hook("pre-tool-use", &write(&env.repo, &main_file));
    assert_eq!(r.code, 0);
    let (d, reason) = decision(&r.json);
    assert_eq!(d, "deny");
    assert_eq!(reason["error"]["code"], "NO_ITEM");
    assert_eq!(
        reason["error"]["next"][0],
        "hall new --title \"<short title>\""
    );
    assert_eq!(env.repo_journal().last().unwrap()["event"], "gate_denied");

    // Files outside the repo and ignored files are not gated.
    let outside = env.root.join("notes.txt");
    assert_eq!(
        env.hook("pre-tool-use", &write(&env.repo, &outside)).stdout,
        ""
    );
    assert_eq!(
        env.hook(
            "pre-tool-use",
            &write(&env.repo, &env.repo.join("debug.log"))
        )
        .stdout,
        ""
    );
    // Neither are the repo's .git internals.
    let gitfile = env.repo.join(".git/COMMIT_EDITMSG");
    assert_eq!(
        env.hook("pre-tool-use", &write(&env.repo, &gitfile)).stdout,
        ""
    );

    // Inside an item's worktree, edits pass, even to new nested files.
    let (id, wt) = env.new_item("Fix login");
    let target = wt.join("src/new/file.rs");
    assert_eq!(env.hook("pre-tool-use", &write(&wt, &target)).stdout, "");
    // The session that edited there is now tied to the item...
    assert!(
        env.journal(&id)
            .iter()
            .any(|e| e["event"] == "session_bound" && e["session"] == "s1")
    );
    // ...so a stray edit in the main checkout points back to its worktree.
    let (d, reason) = decision(&env.hook("pre-tool-use", &write(&env.repo, &main_file)).json);
    assert_eq!(d, "deny");
    assert!(reason["error"]["message"].as_str().unwrap().contains(&id));
    assert!(
        reason["error"]["next"][0]
            .as_str()
            .unwrap()
            .starts_with("cd ")
    );
}

#[test]
fn capture_mode_ask_and_off() {
    let env = Env::initialized();
    let toml_path = env.repo.join(".hallmark.toml");
    let toml = std::fs::read_to_string(&toml_path).unwrap();
    std::fs::write(
        &toml_path,
        toml.replace("mode = \"gate\"", "mode = \"ask\""),
    )
    .unwrap();
    let file = env.repo.join("a.txt");
    assert_eq!(
        decision(&env.hook("pre-tool-use", &write(&env.repo, &file)).json).0,
        "ask"
    );
    std::fs::write(
        &toml_path,
        toml.replace("mode = \"gate\"", "mode = \"off\""),
    )
    .unwrap();
    assert_eq!(
        env.hook("pre-tool-use", &write(&env.repo, &file)).stdout,
        ""
    );
}

#[test]
fn commits_need_an_item_and_pushes_need_a_hallmark() {
    let env = Env::initialized();
    let pre =
        |cwd: &Path, cmd: &str| env.hook("pre-tool-use", &bash("pre-tool-use", cwd, cmd, "s2"));

    // A commit in the main checkout is gated like an edit.
    let (d, reason) = decision(&pre(&env.repo, "git add -A && git commit -m 'wip'").json);
    assert_eq!(
        (d.as_str(), reason["error"]["code"].as_str()),
        ("deny", Some("NO_ITEM"))
    );
    assert_eq!(pre(&env.repo, "git status && git log -1").stdout, "");

    let (id, wt) = env.new_item("Ship it");
    assert_eq!(pre(&wt, "git commit -am 'work'").stdout, "");
    // `cd` into the worktree from elsewhere counts too.
    assert_eq!(
        pre(&env.repo, &format!("cd {} && git commit -m x", p(&wt))).stdout,
        ""
    );

    // Pushing without a hallmark is denied, whatever the capture mode.
    commit_file(&wt, "feature.txt", "done");
    for cmd in [
        "git push -u origin HEAD",
        "gh pr create --fill",
        "git -C . push",
    ] {
        let (d, reason) = decision(&pre(&wt, cmd).json);
        assert_eq!(d, "deny", "{cmd}");
        assert_eq!(reason["error"]["code"], "GATE_DENIED");
        assert_eq!(reason["error"]["next"][0], format!("hall ready {id}"));
    }
    // With a hallmark struck on HEAD, the same push passes.
    strike(&env, &id, &wt);
    assert_eq!(pre(&wt, "git push -u origin HEAD").stdout, "");
    // A later commit has no hallmark again.
    commit_file(&wt, "more.txt", "x");
    assert_eq!(decision(&pre(&wt, "git push").json).0, "deny");
}

/// Strikes a real hallmark on the worktree's HEAD, as an approved verdict
/// does: the note and the `hallmarked` event.
fn strike(env: &Env, id: &str, wt: &Path) {
    let sha = git(wt, &["rev-parse", "HEAD"]);
    let repo = hallmark::repo::Repo::discover(&env.repo).unwrap();
    hallmark::stamp::strike(
        &repo,
        id.parse().unwrap(),
        &sha,
        1,
        None,
        "assayer",
        hallmark::findings::Counts::default(),
    )
    .unwrap();
}

/// Writes `body` as the hallmark note on HEAD, as anyone with git can.
fn forge(wt: &Path, body: &str) {
    git(
        wt,
        &[
            "notes",
            "--ref",
            "refs/notes/hallmark",
            "add",
            "-f",
            "-m",
            body,
            "HEAD",
        ],
    );
}

/// The push's denial message, after checking it is a `GATE_DENIED` denial.
fn push_denied(r: &common::Reply) -> String {
    let (d, reason) = decision(&r.json);
    assert_eq!(d, "deny", "{}", r.stdout);
    assert_eq!(reason["error"]["code"], "GATE_DENIED");
    reason["error"]["message"].as_str().unwrap().to_string()
}

/// A well-formed note body, as `stamp::strike` writes it.
fn note(hallmark: u32, item: &str, sha: &str) -> String {
    json!({
        "hallmark": hallmark,
        "item": item,
        "sha": sha,
        "round": 1,
        "findings": {"blocker": 0, "major": 0, "minor": 0, "nit": 0},
        "t": "2026-01-01T00:00:00Z",
    })
    .to_string()
}

#[test]
fn the_push_gate_verifies_the_hallmark_note() {
    let env = Env::initialized();
    let pre =
        |cwd: &Path, cmd: &str| env.hook("pre-tool-use", &bash("pre-tool-use", cwd, cmd, "s3"));
    let (id, wt) = env.new_item("Ship it");
    commit_file(&wt, "feature.txt", "done");
    let head = git(&wt, &["rev-parse", "HEAD"]);
    let short = &head[..7];

    // A note that isn't a hallmark: any non-empty note used to pass.
    for body in ["{\"item\":\"H-0001\"}", "approved, trust me"] {
        forge(&wt, body);
        let msg = push_denied(&pre(&wt, "git push"));
        assert!(
            msg.starts_with(&format!(
                "{short} has no hallmark: its note doesn't parse as a hallmark"
            )),
            "{msg}"
        );
    }

    // A well-formed note on this commit that Hallmark never struck: the
    // item's journal has no `hallmarked` event for it.
    forge(&wt, &note(1, &id, &head));
    let msg = push_denied(&pre(&wt, "git push"));
    assert!(
        msg.contains(&format!(
            "its note names {id}, but {id}'s journal records no hallmark on {short}"
        )),
        "{msg}"
    );
    // The same, naming an item that doesn't exist.
    forge(&wt, &note(1, "H-9999", &head));
    let msg = push_denied(&pre(&wt, "git push"));
    assert!(
        msg.contains("H-9999's journal records no hallmark"),
        "{msg}"
    );
    // A version of the note this hall doesn't know.
    forge(&wt, &note(2, &id, &head));
    let msg = push_denied(&pre(&wt, "git push"));
    assert!(msg.contains("its note is hallmark version 2"), "{msg}");

    // The real hallmark, struck by an approved verdict, passes.
    strike(&env, &id, &wt);
    assert_eq!(pre(&wt, "git push").stdout, "");

    // A real note copied onto a later commit is for the wrong sha.
    commit_file(&wt, "more.txt", "x");
    let later = git(&wt, &["rev-parse", "HEAD"]);
    let later_short = &later[..7];
    git(
        &wt,
        &[
            "notes",
            "--ref",
            "refs/notes/hallmark",
            "copy",
            &head,
            &later,
        ],
    );
    let msg = push_denied(&pre(&wt, "git push"));
    assert!(
        msg.contains(&format!("its note is for {short}, not {later_short}")),
        "{msg}"
    );
    // A sha that is no commit at all, not even ASCII, is named safely.
    let accents = "\u{e9}".repeat(8);
    forge(&wt, &note(1, &id, &accents));
    let msg = push_denied(&pre(&wt, "git push"));
    assert!(
        msg.contains(&format!(
            "its note is for {}, not {later_short}",
            "\u{e9}".repeat(7)
        )),
        "{msg}"
    );

    // The gate holds for the daemon's actors too: a headless run with a
    // forged note is denied, and the same run passes with a real hallmark.
    let headless = |actor: &str| {
        let out = env
            .cmd(&env.repo)
            .env("HALL_HEADLESS", "1")
            .env("HALL_ACTOR", actor)
            .args(["hook", "pre-tool-use"])
            .write_stdin(bash("pre-tool-use", &wt, "git push", "s4").to_string())
            .output()
            .unwrap();
        common::reply(out)
    };
    forge(&wt, &note(1, &id, &later));
    for actor in ["shipper", "assayer", "fixer"] {
        let msg = push_denied(&headless(actor));
        assert!(
            msg.contains("journal records no hallmark"),
            "{actor}: {msg}"
        );
    }
    strike(&env, &id, &wt);
    for actor in ["shipper", "assayer", "fixer"] {
        assert_eq!(headless(actor).stdout, "", "{actor}");
    }
}

#[test]
fn the_configured_ship_command_is_gated() {
    let env = Env::initialized();
    let toml_path = env.repo.join(".hallmark.toml");
    let toml = std::fs::read_to_string(&toml_path).unwrap();
    let toml = toml
        .lines()
        .map(|l| {
            if l.starts_with("ship ") {
                "ship = \"dev submit --pr\""
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&toml_path, toml).unwrap();
    let (_, wt) = env.new_item("x");
    let r = env.hook(
        "pre-tool-use",
        &bash("pre-tool-use", &wt, "dev submit --pr", "s3"),
    );
    assert_eq!(decision(&r.json).0, "deny");
    let r = env.hook("pre-tool-use", &bash("pre-tool-use", &wt, "dev test", "s3"));
    assert_eq!(r.stdout, "");
}

#[test]
fn session_start_says_a_newer_hall_is_out() {
    let env = Env::initialized();
    let seen = |version: &str| {
        let v = json!({
            "checked_at": chrono::Utc::now(),
            "source": hallmark::commands::update::SOURCE,
            "latest": {"version": version, "tag": format!("v{version}"), "commit": "abc"},
        });
        std::fs::write(env.home.join("update.json"), v.to_string()).unwrap();
    };
    let ctx = |session: &str| {
        let r = env.hook("session-start", &input("session_start", &env.repo, session));
        r.json["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_string()
    };
    seen("99.0.0");
    let c = ctx("s20");
    assert!(c.contains("hall 99.0.0 is available"), "{c}");
    assert!(c.contains("hall update"), "{c}");
    seen("0.0.1");
    assert!(!ctx("s21").contains("is available"));
}

#[test]
fn session_start_tells_claude_where_it_is() {
    let env = Env::initialized();
    let r = env.hook("session-start", &input("session_start", &env.repo, "s4"));
    let ctx = r.json["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert_eq!(
        r.json["hookSpecificOutput"]["hookEventName"],
        "SessionStart"
    );
    assert!(ctx.contains("hall new --title"), "{ctx}");
    assert!(ctx.contains("Never push"), "{ctx}");

    let (id, wt) = env.new_item("Fix login");
    let r = env.hook("session-start", &input("session_start", &wt, "s5"));
    let ctx = r.json["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(ctx.contains(&format!("works on {id}")), "{ctx}");
    assert!(ctx.contains("development"), "{ctx}");
    assert!(
        env.journal(&id)
            .iter()
            .any(|e| e["event"] == "session_started")
    );
}

#[test]
fn prompts_follow_the_session_into_its_item() {
    let env = Env::initialized();
    // A prompt before any item is filed in the repo journal.
    let mut v = input("user_prompt_submit", &env.repo, "s6");
    v["prompt"] = json!("the login e2e test is flaky, can you fix it?");
    assert_eq!(env.hook("user-prompt-submit", &v).stdout, "");
    assert_eq!(env.repo_journal().last().unwrap()["event"], "prompt");

    // Claude runs `hall new`; PostToolUse sees the reply and binds the session.
    let r = env.hall(&env.repo, &["new", "--title", "Fix flaky login e2e test"]);
    let mut post = bash(
        "post-tool-use",
        &env.repo,
        "hall new --title \"Fix flaky login e2e test\"",
        "s6",
    );
    post["tool_response"]["stdout"] = json!(r.stdout);
    env.hook("post-tool-use", &post);
    let events = env.journal("H-0001");
    let names: Vec<&str> = events
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["created", "session_bound", "prompt"],
        "the first prompt is carried over"
    );
    assert_eq!(
        events[2]["text"],
        "the login e2e test is flaky, can you fix it?"
    );

    // Later prompts from that session go to the item, even from the main checkout.
    let mut v = input("user_prompt_submit", &env.repo, "s6");
    v["prompt"] = json!("use the session cookie");
    env.hook("user-prompt-submit", &v);
    assert_eq!(
        env.journal("H-0001").last().unwrap()["text"],
        "use the session cookie"
    );

    // While the item is in review, the prompt hook says so.
    let wt = std::path::PathBuf::from(r.json["worktree"].as_str().unwrap());
    commit_file(&wt, "fix.txt", "x");
    env.hall(&wt, &["ready"]);
    let r = env.hook("user-prompt-submit", &v);
    let ctx = r.json["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(ctx.contains("hall pull-back H-0001"), "{ctx}");
}

#[test]
fn an_escalated_item_tells_the_session_the_reviewers_diagnosis() {
    let env = Env::initialized();
    let cfg = env.repo.join(".hallmark.toml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(&cfg, text.replace("max_rejects = 3", "max_rejects = 1")).unwrap();
    git(&env.repo, &["commit", "-q", "-am", "one reject"]);
    let (id, wt) = env.new_item("x");
    commit_file(&wt, "a.txt", "1");
    assert_eq!(env.hall(&wt, &["ready"]).code, 0);
    let script = env.script(json!([{
        "role": "reviewer",
        "structured": {
            "summary": "DIAGNOSIS_MARKER: split this change in two.",
            "findings": [{
                "lens": "correctness", "severity": "blocker", "file": "a.txt", "line": 1,
                "title": "Broken", "scenario": "it breaks", "fix": "fix it"
            }],
            "prior": [],
            "learnings": []
        },
        "model": "claude-opus-4-1"
    }]));
    for _ in 0..2 {
        let r = env.hall_fake(
            &env.repo,
            &script,
            &["daemon", "run", "--once", "--only", "assayer"],
        );
        assert_eq!(r.code, 0, "{}", r.stdout);
    }
    let st = env.hall(&env.repo, &["findings", &id]);
    assert_eq!(st.json["item"]["state"], "escalated", "{}", st.stdout);
    let v = input("user_prompt_submit", &wt, "s-esc");
    let r = env.hook("user-prompt-submit", &v);
    let ctx = r.json["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        ctx.contains("The reviewer: DIAGNOSIS_MARKER: split this change in two."),
        "{ctx}"
    );
}

#[test]
fn an_escalated_item_leaves_its_pre_existing_blocker_out_of_why_it_waits() {
    let env = Env::initialized();
    let cfg = env.repo.join(".hallmark.toml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(&cfg, text.replace("max_rejects = 3", "max_rejects = 1")).unwrap();
    git(&env.repo, &["commit", "-q", "-am", "one reject"]);
    let (id, wt) = env.new_item("x");
    commit_file(&wt, "a.txt", "1");
    assert_eq!(env.hall(&wt, &["ready"]).code, 0);
    let script = env.script(json!([{
        "role": "reviewer",
        "structured": {
            "summary": "two blockers",
            "findings": [
                {"lens": "correctness", "severity": "blocker", "file": "a.txt", "line": 1,
                 "title": "OLDBLOCKER", "scenario": "it was always broken", "fix": "fix it",
                 "pre_existing": true},
                {"lens": "correctness", "severity": "blocker", "file": "a.txt", "line": 2,
                 "title": "NEWBLOCKER", "scenario": "it breaks", "fix": "fix it"}
            ],
            "prior": [],
            "learnings": []
        },
        "model": "claude-opus-4-1"
    }]));
    for _ in 0..2 {
        let r = env.hall_fake(
            &env.repo,
            &script,
            &["daemon", "run", "--once", "--only", "assayer"],
        );
        assert_eq!(r.code, 0, "{}", r.stdout);
    }
    let st = env.hall(&env.repo, &["status", &id]);
    assert_eq!(st.json["item"]["state"], "escalated", "{}", st.stdout);
    let detail = st.json["now"]["detail"].to_string();
    assert!(detail.contains("NEWBLOCKER"), "{detail}");
    assert!(!detail.contains("OLDBLOCKER"), "{detail}");

    // The export names the open pre-existing blocker in the ride-along line.
    let r = env.hall(&env.repo, &["export", &id]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let md = r.json["markdown"].as_str().unwrap();
    let line = md
        .lines()
        .find(|l| l.contains("ride along into the PR"))
        .unwrap_or_else(|| panic!("no ride-along line in\n{md}"));
    assert!(line.contains("(pre-existing)"), "{line}");
    assert!(line.contains("F1 (pre-existing)"), "{line}");
    assert!(!line.contains("F2"), "{line}");
}

#[test]
fn commits_and_questions_are_recorded_after_the_tool_runs() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("x");
    let sha = commit_file(&wt, "a.txt", "1");
    let post = bash(
        "post-tool-use",
        &wt,
        "git add a.txt && git commit -m 'change a.txt'",
        "s7",
    );
    env.hook("post-tool-use", &post);
    env.hook("post-tool-use", &post); // recorded once
    let commits: Vec<Value> = env
        .journal(&id)
        .into_iter()
        .filter(|e| e["event"] == "commit")
        .collect();
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0]["sha"], sha.as_str());
    assert_eq!(commits[0]["subject"], "change a.txt");

    let r = env.hall(&wt, &["ask", "which?", "--options", "a,b"]);
    let mut post = bash(
        "post-tool-use",
        &wt,
        "hall ask \"which?\" --options a,b",
        "s7",
    );
    post["tool_response"]["stdout"] = json!(r.stdout);
    env.hook("post-tool-use", &post);
    let a = env.hall(&env.repo, &["answer", "Q-1", "a"]);
    assert_eq!(a.json["resume"], "claude -p --resume s7 a");
}

#[test]
fn stop_records_workshop_usage_once_per_request() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("x");
    let transcript = env.root.join("session.jsonl");
    std::fs::write(&transcript, fixture("transcript/session.jsonl")).unwrap();
    let mut v = input("stop", &wt, "s8");
    v["transcript_path"] = json!(p(&transcript));
    v["last_assistant_message"] = json!("Added backoff.ts and the retry job.");
    env.hook("stop", &v);
    env.hook("stop", &v); // nothing new in the transcript the second time

    let usage: Vec<Value> = env
        .journal(&id)
        .into_iter()
        .filter(|e| e["event"] == "usage")
        .collect();
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0]["model"], "claude-haiku-5-5");
    assert_eq!(usage[0]["requests"], 3);
    assert_eq!(usage[0]["source"], "transcript");
    let turns = env
        .journal(&id)
        .into_iter()
        .filter(|e| e["event"] == "turn")
        .count();
    assert_eq!(turns, 2);

    let r = env.hall(&env.repo, &["usage", "--by", "model"]);
    assert_eq!(r.json["rows"][0]["key"], "claude-haiku-5-5");
    assert_eq!(r.json["rows"][0]["requests"], 3);
    assert!(r.json["total"]["cost_usd"].as_f64().unwrap() > 0.0);
    assert_eq!(
        env.hall(&env.repo, &["usage", "--by", "room"]).json["rows"][0]["key"],
        "workshop"
    );
    assert_eq!(
        env.hall(&env.repo, &["usage", "--by", "item"]).json["rows"][0]["key"],
        id
    );
    let r = env.hall(&env.repo, &["usage", "--since", "1d", "--item", &id]);
    assert_eq!(r.json["total"]["requests"], 3);
    assert_eq!(env.hall(&env.repo, &["usage", "--since", "soon"]).code, 2);

    // More transcript lines later are counted from where we stopped.
    let mut more = std::fs::read_to_string(&transcript).unwrap();
    more.push_str(
        &json!({"type":"assistant","requestId":"req_new","message":{"model":"claude-opus-5-5",
            "usage":{"input_tokens":10,"output_tokens":100,"cache_read_input_tokens":1000,
                     "cache_creation_input_tokens":0}}})
        .to_string(),
    );
    more.push('\n');
    std::fs::write(&transcript, more).unwrap();
    env.hook("stop", &v);
    let r = env.hall(&env.repo, &["usage", "--by", "model"]);
    let opus = r.json["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "claude-opus-5-5")
        .unwrap()
        .clone();
    assert_eq!(opus["requests"], 1);
    // 10 in × $4 + 100 out × $20 + 1000 read × $0.20, per million.
    assert!((opus["cost_usd"].as_f64().unwrap() - 0.00224).abs() < 1e-9);
}

#[test]
fn waiting_sessions_are_notices_until_you_reply() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("x");
    let r = env.hook("notification", &input("notification", &wt, "s9"));
    assert_eq!(r.stdout, "");
    // Your own session tells you itself: never in your office.
    let inbox = env.hall(&env.repo, &["inbox"]);
    assert_eq!(inbox.json["entries"], json!([]), "{}", inbox.stdout);
    let inbox = env.hall(&env.repo, &["inbox", "--notices"]);
    let e = &inbox.json["entries"][0];
    assert_eq!(e["kind"], "session_waiting");
    assert_eq!(e["item"], id);
    assert_eq!(e["text"], "Claude is waiting for your input");
    assert_eq!(e["answer_with"][0], "claude --resume s9");

    let mut v = input("user_prompt_submit", &wt, "s9");
    v["prompt"] = json!("ok, carry on");
    env.hook("user-prompt-submit", &v);
    assert_eq!(
        env.hall(&env.repo, &["inbox", "--notices"]).json["entries"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn hook_event_names_accept_both_spellings() {
    let env = Env::initialized();
    let r = env.hook("SessionStart", &input("session_start", &env.repo, "s10"));
    assert_eq!(r.code, 0);
    assert!(r.json["hookSpecificOutput"]["additionalContext"].is_string());
}

#[test]
fn sessions_that_accept_edits_may_edit_their_item_worktree() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("x");
    let file = wt.join("hello.txt");
    // From the main checkout, in acceptEdits mode: explicitly allowed.
    let mut v = write(&env.repo, &file);
    v["permission_mode"] = json!("acceptEdits");
    let r = env.hook("pre-tool-use", &v);
    assert_eq!(r.json["hookSpecificOutput"]["permissionDecision"], "allow");
    assert!(
        r.json["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains(&id)
    );
    // In default mode, Claude Code keeps asking: no decision from us.
    v["permission_mode"] = json!("default");
    assert_eq!(env.hook("pre-tool-use", &v).stdout, "");
    // A session already in the worktree needs nothing.
    let mut v = write(&wt, &file);
    v["permission_mode"] = json!("acceptEdits");
    assert_eq!(env.hook("pre-tool-use", &v).stdout, "");
    // Never for files outside every worktree.
    let mut v = write(&env.repo, &env.repo.join("src/main.rs"));
    v["permission_mode"] = json!("acceptEdits");
    let r = env.hook("pre-tool-use", &v);
    assert_eq!(r.json["hookSpecificOutput"]["permissionDecision"], "deny");
}

#[test]
fn headless_runs_are_never_allowed_into_another_items_worktree() {
    let env = Env::initialized();
    let (_, mine) = env.new_item("mine");
    let (_, other) = env.new_item("other");
    let mut v = write(&mine, &other.join("tests/zz.rs"));
    v["permission_mode"] = json!("acceptEdits");
    let out = env
        .cmd(&env.repo)
        .env("HALL_HEADLESS", "1")
        .env("HALL_ACTOR", "fixer")
        .args(["hook", "pre-tool-use"])
        .write_stdin(v.to_string())
        .output()
        .unwrap();
    let r = common::reply(out);
    assert_ne!(
        r.json["hookSpecificOutput"]["permissionDecision"], "allow",
        "{}",
        r.stdout
    );
}

#[test]
fn docs_and_markdown_need_no_item() {
    let env = Env::initialized();
    // Documents, designs and requirements are not development work.
    for rel in [
        "docs/architecture.md",
        "docs/diagram.svg",
        "NOTES.md",
        "app/README.md",
    ] {
        let r = env.hook("pre-tool-use", &write(&env.repo, &env.repo.join(rel)));
        assert_eq!((r.code, r.stdout.as_str()), (0, ""), "{rel}");
    }
    let r = env.hook(
        "pre-tool-use",
        &write(&env.repo, &env.repo.join("src/lib.rs")),
    );
    assert_eq!(decision(&r.json).0, "deny");

    // A commit of docs only needs no item either; one with code does.
    let commit = |env: &Env| {
        env.hook(
            "pre-tool-use",
            &bash("pre-tool-use", &env.repo, "git commit -m docs", "s1"),
        )
    };
    std::fs::create_dir_all(env.repo.join("docs")).unwrap();
    std::fs::write(env.repo.join("docs/plan.md"), "# Plan\n").unwrap();
    git(&env.repo, &["add", "docs/plan.md"]);
    assert_eq!(commit(&env).stdout, "");
    std::fs::create_dir_all(env.repo.join("src")).unwrap();
    std::fs::write(env.repo.join("src/lib.rs"), "pub fn x() {}\n").unwrap();
    git(&env.repo, &["add", "src/lib.rs"]);
    assert_eq!(decision(&commit(&env).json).0, "deny");

    // Review inputs are never free, even as Markdown or with an anchor.
    std::fs::write(
        env.repo.join(".hallmark.toml"),
        "rules = [\"docs/plan.md#leave-alone\", \"CLAUDE.md\"]\n",
    )
    .unwrap();
    for rel in [
        ".hallmark.toml",
        ".hallmark/review.md",
        "CLAUDE.md",
        "docs/plan.md",
    ] {
        let r = env.hook("pre-tool-use", &write(&env.repo, &env.repo.join(rel)));
        assert_eq!(decision(&r.json).0, "deny", "{rel}");
    }

    // `git add -A` from a subdirectory also takes untracked code elsewhere.
    git(&env.repo, &["reset", "-q"]);
    std::fs::write(env.repo.join("docs/other.md"), "# Other\n").unwrap();
    git(&env.repo, &["add", "docs/other.md"]);
    let r = env.hook(
        "pre-tool-use",
        &bash(
            "pre-tool-use",
            &env.repo.join("docs"),
            "git add -A && git commit -m docs",
            "s1",
        ),
    );
    assert_eq!(decision(&r.json).0, "deny");

    // `[capture] free = []` gates everything again.
    std::fs::write(
        env.repo.join(".hallmark.toml"),
        "[capture]\nmode = \"gate\"\nfree = []\n",
    )
    .unwrap();
    let r = env.hook(
        "pre-tool-use",
        &write(&env.repo, &env.repo.join("docs/architecture.md")),
    );
    assert_eq!(decision(&r.json).0, "deny");
}

#[test]
fn session_start_carries_the_repo_hallmark_skill_from_the_base() {
    let env = Env::initialized();
    let ctx = |session: &str| {
        let r = env.hook("session-start", &input("session_start", &env.repo, session));
        r.json["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let skill = env.repo.join(".hallmark/skills/hallmark/SKILL.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    std::fs::write(
        &skill,
        "HOOK_SKILL_MARKER: build test data with tests/factories.\n",
    )
    .unwrap();
    assert!(
        !ctx("k1").contains("HOOK_SKILL_MARKER"),
        "only once it is on the base"
    );

    git(&env.repo, &["add", "."]);
    git(&env.repo, &["commit", "-q", "-m", "skill"]);
    let c = ctx("k2");
    assert!(
        c.contains("hall new --title"),
        "the usual context stays: {c}"
    );
    let at = c
        .find("## This repository's additions to the `hallmark` skill")
        .unwrap_or_else(|| panic!("{c}"));
    assert!(c[at..].contains("HOOK_SKILL_MARKER"));

    // A long skill is cut, with a pointer to the whole file.
    std::fs::write(&skill, "LONG_SKILL_LINE\n".repeat(1000)).unwrap();
    git(&env.repo, &["commit", "-q", "-am", "long skill"]);
    let c = ctx("k3");
    let lines = c.matches("LONG_SKILL_LINE").count();
    assert!(lines > 100 && lines < 1000, "{lines}");
    assert!(
        c.contains("git show main:.hallmark/skills/hallmark/SKILL.md"),
        "{c}"
    );
}

#[test]
fn session_start_lists_the_extensions_for_developers() {
    let env = Env::initialized();
    let ctx = |session: &str| {
        let r = env.hook("session-start", &input("session_start", &env.repo, session));
        r.json["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert!(!ctx("k1").contains("## Extensions"));
    let r = env.hall(
        &env.repo,
        &["extensions", "enable", "mattpocock", "--role", "reviewer"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert!(!ctx("k2").contains("## Extensions"), "not for developers");

    // From the main checkout's config, without a commit.
    env.hall(
        &env.repo,
        &["extensions", "enable", "mattpocock", "--role", "developer"],
    );
    let c = ctx("k3");
    let at = c.find("## Extensions\n").unwrap_or_else(|| panic!("{c}"));
    assert!(c[at..].contains("`hall extensions show mattpocock <skill>`"));
    assert!(c[at..].contains("- `tdd`:") && c[at..].contains("- `codebase-design`:"));
    assert!(c[at..].contains("by Matt Pocock"));
    assert!(!c.contains("Refuse to give up"), "an index, not the text");
}

/// `hall hook pre-tool-use` for a Bash command, run as `actor`.
fn bash_as(env: &Env, actor: Option<&str>, cwd: &Path, cmd: &str) -> common::Reply {
    let mut c = env.cmd(&env.repo);
    if let Some(a) = actor {
        c.env("HALL_ACTOR", a);
    }
    let out = c
        .args(["hook", "pre-tool-use"])
        .write_stdin(bash("pre-tool-use", cwd, cmd, "s7").to_string())
        .output()
        .unwrap();
    common::reply(out)
}

#[test]
fn only_the_daemon_writes_git_notes() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Notes");
    commit_file(&wt, "a.txt", "a");
    let write = "git notes --ref refs/notes/hallmark add -f -m '{}' HEAD";
    for actor in [None, Some("agent"), Some("fixer")] {
        let (d, reason) = decision(&bash_as(&env, actor, &wt, write).json);
        assert_eq!(d, "deny", "{actor:?}");
        assert_eq!(reason["error"]["code"], "GATE_DENIED");
        assert_eq!(reason["error"]["next"][0], format!("hall ready {id}"));
    }
    // Any notes ref: `core.notesRef` can point a plain `git notes` anywhere.
    assert_eq!(
        decision(&bash_as(&env, None, &wt, "git notes append -m x").json).0,
        "deny"
    );
    for actor in ["assayer", "shipper"] {
        assert_eq!(bash_as(&env, Some(actor), &wt, write).stdout, "", "{actor}");
    }
    for read in [
        "git notes show HEAD",
        "git notes --ref hallmark list",
        "git notes",
    ] {
        assert_eq!(bash_as(&env, None, &wt, read).stdout, "", "{read}");
    }
}

#[test]
fn the_strict_push_guard_in_item_worktrees() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Guard");
    commit_file(&wt, "a.txt", "a");
    for cmd in ["make push", "npm run release", "git p", "gh pr merge 7"] {
        let (d, reason) = decision(&bash_as(&env, None, &wt, cmd).json);
        assert_eq!(d, "deny", "{cmd}");
        assert_eq!(reason["error"]["code"], "GATE_DENIED", "{cmd}");
        assert_eq!(reason["error"]["next"][0], format!("hall ready {id}"));
    }
    assert!(
        env.journal(&id)
            .iter()
            .any(|e| e["event"] == "gate_denied" && e["target"] == "run make push")
    );
    for cmd in [
        "git status",
        "git log --oneline",
        "git stash list",
        "git notes show HEAD",
        "cargo test",
        "hall status",
        "grep -rn push src",
    ] {
        assert_eq!(bash_as(&env, None, &wt, cmd).stdout, "", "{cmd}");
    }
    // Outside an item's worktree the guard stays out of the way.
    assert_eq!(bash_as(&env, None, &env.repo, "make push").stdout, "");

    // `push_allow` lists the scripts a repo needs; `parse` turns it off.
    let toml_path = env.repo.join(".hallmark.toml");
    let toml = std::fs::read_to_string(&toml_path).unwrap();
    std::fs::write(
        &toml_path,
        toml.replace("[capture]\n", "[capture]\npush_allow = [\"make push\"]\n"),
    )
    .unwrap();
    assert_eq!(bash_as(&env, None, &wt, "make push").stdout, "");
    assert_eq!(
        decision(&bash_as(&env, None, &wt, "make release").json).0,
        "deny"
    );
    std::fs::write(
        &toml_path,
        toml.replace("[capture]\n", "[capture]\npush_guard = \"parse\"\n"),
    )
    .unwrap();
    assert_eq!(bash_as(&env, None, &wt, "make release").stdout, "");
    // A push the parser reads is denied in either mode.
    assert_eq!(
        decision(&bash_as(&env, None, &wt, "nice git push").json).0,
        "deny"
    );
}
