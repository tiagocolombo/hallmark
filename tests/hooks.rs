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
    // With a hallmark note on HEAD, the same push passes.
    git(
        &wt,
        &[
            "notes",
            "--ref",
            "refs/notes/hallmark",
            "add",
            "-m",
            "{\"item\":\"H-0001\"}",
            "HEAD",
        ],
    );
    assert_eq!(pre(&wt, "git push -u origin HEAD").stdout, "");
    // A later commit has no hallmark again.
    commit_file(&wt, "more.txt", "x");
    assert_eq!(decision(&pre(&wt, "git push").json).0, "deny");
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
