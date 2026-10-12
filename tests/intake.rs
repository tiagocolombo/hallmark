//! The Front Desk's plan, parallel parts, `hall merge` and follow-up items
//! (workstream H, ADR 0024). Every model run is the fake runner.

mod common;

use std::path::{Path, PathBuf};

use common::{Env, Reply, commit_file, fixture, git, p};
use insta::assert_json_snapshot;
use serde_json::{Value, json};

const REQUEST: &str = "Add retries to the API client and a status banner to the web app";

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
            ".children[].created_at" => "[time]",
            ".children[].state_since" => "[time]",
            ".parent.created_at" => "[time]",
            ".parent.state_since" => "[time]",
        });
    };
}

fn part(title: &str, request: &str, paths: &[&str]) -> Value {
    json!({"title": title, "request": request, "paths": paths})
}

fn split_plan() -> Value {
    json!([{"role": "intake", "structured": {
        "parallel": true,
        "reason": "The API client and the web app share no files.",
        "parts": [
            part("API client retries", "Retry failed API calls with backoff in api/.", &["api/"]),
            part("Web status banner", "Show a status banner in web/.", &["web/**"]),
        ]
    }}])
}

/// A repo with `api/` and `web/`, split into parent H-0001 and children
/// H-0002 (api) and H-0003 (web).
fn split(env: &Env) -> Reply {
    std::fs::create_dir_all(env.repo.join("api")).unwrap();
    std::fs::create_dir_all(env.repo.join("web")).unwrap();
    std::fs::write(env.repo.join("api/client.rs"), "fn call() {}\n").unwrap();
    std::fs::write(env.repo.join("web/app.ts"), "export {}\n").unwrap();
    git(&env.repo, &["add", "."]);
    git(&env.repo, &["commit", "-q", "-m", "api and web"]);
    let script = env.script(split_plan());
    let r = env.hall_fake(&env.repo, &script, &["new", REQUEST]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    r
}

fn wt(r: &Reply, i: usize) -> PathBuf {
    PathBuf::from(r.json["children"][i]["worktree"].as_str().unwrap())
}

fn events<'a>(journal: &'a [Value], kind: &str) -> Vec<&'a Value> {
    journal.iter().filter(|e| e["event"] == kind).collect()
}

#[test]
fn a_split_request_becomes_a_parent_and_children() {
    let env = Env::initialized();
    let r = split(&env);
    let j = &r.json;
    assert_eq!(j["id"], "H-0001");
    assert_eq!(j["plan"]["parallel"], true, "{}", r.stdout);
    assert_eq!(j["plan"]["ran"], true);
    let kids = j["children"].as_array().unwrap();
    assert_eq!(kids.len(), 2);
    assert_eq!(kids[0]["id"], "H-0002");
    assert_eq!(kids[1]["id"], "H-0003");
    assert_eq!(kids[0]["branch"], "hm/H-0002-api-client-retries");
    assert_eq!(kids[0]["paths"], json!(["api"]));
    assert!(
        kids[1]["request"]
            .as_str()
            .unwrap()
            .contains("status banner")
    );
    for i in 0..2 {
        let w = wt(&r, i);
        assert!(w.join("api/client.rs").is_file(), "{}", w.display());
        // Each child branch starts at its parent's tip.
        assert_eq!(
            git(&w, &["rev-parse", "HEAD"]),
            git(
                &env.repo,
                &["rev-parse", "hm/H-0001-add-retries-to-the-api-client-and-a"]
            )
        );
    }
    // The orchestrator is told exactly what to do.
    let next: Vec<&str> = j["next"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(next.iter().any(|n| n.contains("subagent")));
    assert!(next.contains(&"hall merge H-0002 (when its subagent is done)"));
    assert!(next.contains(&"hall ready H-0001"));

    let parent = env.journal("H-0001");
    let planned = events(&parent, "planned");
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0]["parallel"], true);
    assert_eq!(planned[0]["children"], json!(["H-0002", "H-0003"]));
    assert_eq!(parent[0]["request"], REQUEST);
    let child = env.journal("H-0002");
    assert_eq!(child[0]["event"], "created");
    assert_eq!(child[0]["parent"], "H-0001");
    // The raw intake result is kept with the parent.
    let runs = env.repo.join(".git/hallmark/items/H-0001/runs");
    assert_eq!(std::fs::read_dir(runs).unwrap().count(), 1);

    // One headless run: the intake agent, its schema, read-only tools, no hooks.
    let calls = env.fake_calls();
    assert_eq!(calls.len(), 1);
    let c = &calls[0];
    assert_eq!(c["role"], "intake");
    assert_eq!(c["room"], "front");
    assert_eq!(c["agent"], "hallmark:intake");
    assert_eq!(c["model"], "haiku");
    assert_eq!(c["timeout_secs"], 120);
    assert_eq!(
        c["allowed_tools"],
        json!(["Read", "Grep", "Glob", "StructuredOutput"])
    );
    assert!(c["json_schema"].as_str().unwrap().contains("parallel"));
    assert!(c["prompt"].as_str().unwrap().contains(REQUEST));
    assert!(c["prompt"].as_str().unwrap().contains("api/ (1 files)"));
    assert!(
        c["env"]
            .as_array()
            .unwrap()
            .contains(&json!(["HALL_NO_HOOKS", "1"]))
    );
    // Its usage is booked to the Front Desk, at the repo level.
    let usage = events(&env.repo_journal(), "usage")
        .into_iter()
        .filter(|e| e["room"] == "front")
        .count();
    assert_eq!(usage, 1);

    // The daemon never picks up children; status shows all three.
    let st = env.hall(&env.repo, &["status"]);
    assert_eq!(
        st.json["items"].as_array().unwrap().len(),
        3,
        "{}",
        st.stdout
    );

    snap!("intake_split_new", scrub(&env, j));
}

#[test]
fn a_plan_that_keeps_one_item() {
    let env = Env::initialized();
    let script = env.script(json!([{"role": "intake", "structured": {
        "parallel": false, "reason": "One feature in one module.", "parts": []
    }}]));
    let r = env.hall_fake(&env.repo, &script, &["new", "Fix the login redirect"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["plan"]["parallel"], false);
    assert_eq!(r.json["plan"]["reason"], "One feature in one module.");
    assert!(r.json.get("children").is_none());
    let planned = env.journal("H-0001");
    let planned = events(&planned, "planned");
    assert_eq!(planned[0]["parallel"], false);
    assert_eq!(planned[0]["children"], json!([]));
}

#[test]
fn an_overlapping_plan_stays_one_item() {
    let env = Env::initialized();
    let script = env.script(json!([{"role": "intake", "structured": {
        "parallel": true, "reason": "two parts",
        "parts": [
            part("Server", "server work", &["src/server", "Cargo.toml"]),
            part("Client", "client work", &["src/client", "Cargo.toml"]),
        ]
    }}]));
    let r = env.hall_fake(&env.repo, &script, &["new", REQUEST]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["plan"]["parallel"], false);
    assert!(
        r.json["plan"]["reason"]
            .as_str()
            .unwrap()
            .contains("overlap"),
        "{}",
        r.stdout
    );
    assert!(r.json.get("children").is_none());
    assert!(!env.repo.join(".git/hallmark/items/H-0002").exists());
}

#[test]
fn a_single_part_or_too_many_parts_stay_one_item() {
    let env = Env::initialized();
    let parts: Vec<Value> = (0..5)
        .map(|i| part(&format!("Part {i}"), "work", &[format!("dir{i}").as_str()]))
        .collect();
    let script = env.script(json!([
        {"role": "intake", "structured": {"parallel": true, "reason": "x", "parts": [parts[0]]}},
        {"role": "intake", "structured": {"parallel": true, "reason": "x", "parts": parts}},
    ]));
    let one = env.hall_fake(&env.repo, &script, &["new", "first request"]);
    assert!(
        one.json["plan"]["reason"]
            .as_str()
            .unwrap()
            .contains("fewer than 2")
    );
    let many = env.hall_fake(&env.repo, &script, &["new", "second request"]);
    assert!(
        many.json["plan"]["reason"]
            .as_str()
            .unwrap()
            .contains("max_parts (4)"),
        "{}",
        many.stdout
    );
    assert_eq!(many.json["id"], "H-0002");
}

#[test]
fn intake_fails_open() {
    let env = Env::initialized();
    let script = env.script(json!([{"role": "intake", "fail": "error"}]));
    let r = env.hall_fake(&env.repo, &script, &["new", REQUEST]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["id"], "H-0001");
    assert_eq!(r.json["plan"]["parallel"], false);
    assert!(
        r.json["plan"]["reason"]
            .as_str()
            .unwrap()
            .starts_with("the Front Desk run failed"),
        "{}",
        r.stdout
    );
    let failed = env.repo_journal();
    let failed = events(&failed, "run_failed");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["role"], "intake");

    // A schema run without structured output fails open too.
    let script = env.script(json!([{"role": "intake", "result": "I think so"}]));
    let r = env.hall_fake(&env.repo, &script, &["new", "another request"]);
    assert_eq!(r.code, 0);
    assert_eq!(r.json["plan"]["parallel"], false);
}

#[test]
fn without_claude_intake_is_skipped() {
    let env = Env::initialized();
    let r = env.hall(&env.repo, &["new", REQUEST]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["plan"]["ran"], false);
    assert_eq!(
        r.json["plan"]["reason"],
        "the Front Desk did not run: claude was not found"
    );
    // Nothing recorded: no run was made.
    assert!(events(&env.journal("H-0001"), "planned").is_empty());
    assert!(events(&env.repo_journal(), "run_failed").is_empty());
}

#[test]
fn no_split_and_titles_make_no_call() {
    let env = Env::initialized();
    let script = env.script(split_plan());
    let r = env.hall_fake(&env.repo, &script, &["new", REQUEST, "--no-split"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert!(r.json.get("plan").is_none());
    let r = env.hall_fake(&env.repo, &script, &["new", "--title", "Small fix"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert!(env.fake_calls().is_empty());
}

#[test]
fn split_off_in_config_makes_no_call() {
    let env = Env::initialized();
    let cfg = env.repo.join(".hallmark.toml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(&cfg, format!("{text}\n[intake]\nsplit = \"off\"\n")).unwrap();
    let script = env.script(split_plan());
    let r = env.hall_fake(&env.repo, &script, &["new", REQUEST]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert!(r.json.get("plan").is_none());
    assert!(env.fake_calls().is_empty());
}

#[test]
fn merging_a_child_closes_it_into_the_parent() {
    let env = Env::initialized();
    let r = split(&env);
    let parent_wt = PathBuf::from(r.json["worktree"].as_str().unwrap());
    let api = wt(&r, 0);
    commit_file(&api, "api/retry.rs", "fn retry() {}\n");

    let m = env.hall(&env.repo, &["merge", "H-0002"]);
    assert_eq!(m.code, 0, "{}", m.stdout);
    assert_eq!(m.json["id"], "H-0002");
    assert_eq!(m.json["state"], "closed");
    assert_eq!(m.json["changed"], true);
    assert_eq!(m.json["worktree_removed"], true);
    assert_eq!(m.json["open_children"], json!(["H-0003"]));
    assert_eq!(m.json["parent"]["id"], "H-0001");
    let sha = m.json["sha"].as_str().unwrap().to_string();
    assert_eq!(git(&parent_wt, &["rev-parse", "HEAD"]), sha);
    assert_eq!(
        git(&parent_wt, &["log", "-1", "--format=%s"]),
        "Merge H-0002: API client retries"
    );
    assert!(parent_wt.join("api/retry.rs").is_file());
    assert!(!api.exists(), "the child's worktree is removed");
    git(
        &env.repo,
        &["rev-parse", "--verify", "hm/H-0002-api-client-retries"],
    );

    let child = env.journal("H-0002");
    let merged = events(&child, "merged");
    assert_eq!(merged[0]["into"], "H-0001");
    assert_eq!(merged[0]["sha"], sha.as_str());
    let parent = env.journal("H-0001");
    assert_eq!(events(&parent, "child_merged")[0]["child"], "H-0002");
    snap!("intake_merge", scrub(&env, &m.json));

    // Repeating it is safe.
    let again = env.hall(&env.repo, &["merge", "H-0002"]);
    assert_eq!(again.code, 0, "{}", again.stdout);
    assert_eq!(again.json["changed"], false);
    assert_eq!(again.json["sha"], sha.as_str());
    assert_eq!(events(&env.journal("H-0001"), "child_merged").len(), 1);
    assert_eq!(events(&env.journal("H-0002"), "merged").len(), 1);
}

#[test]
fn a_conflicting_merge_records_nothing() {
    let env = Env::initialized();
    let r = split(&env);
    let parent_wt = PathBuf::from(r.json["worktree"].as_str().unwrap());
    commit_file(&wt(&r, 0), "README.md", "# api\n");
    commit_file(&wt(&r, 1), "README.md", "# web\n");
    assert_eq!(env.hall(&env.repo, &["merge", "H-0002"]).code, 0);
    let before = git(&parent_wt, &["rev-parse", "HEAD"]);

    let m = env.hall(&env.repo, &["merge", "H-0003"]);
    assert_eq!(m.code, 4, "{}", m.stdout);
    assert_eq!(m.json["error"]["code"], "MERGE_CONFLICT");
    assert!(
        m.json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("README.md")
    );
    let next = m.json["error"]["next"].to_string();
    assert!(next.contains("merge hm/H-0001-"), "{next}");
    assert!(next.contains("hall merge H-0003"), "{next}");
    // The merge was aborted: the parent is untouched and clean.
    assert_eq!(git(&parent_wt, &["rev-parse", "HEAD"]), before);
    assert_eq!(git(&parent_wt, &["status", "--porcelain"]), "");
    assert!(events(&env.journal("H-0003"), "merged").is_empty());
    assert_eq!(events(&env.journal("H-0001"), "child_merged").len(), 1);
    let st = env.hall(&env.repo, &["status", "H-0003"]);
    assert_eq!(st.json["state"], "development", "{}", st.stdout);

    // Resolving in the child as told, then merging again, works.
    let child_wt = wt(&r, 1);
    let out = std::process::Command::new("git")
        .current_dir(&child_wt)
        .env("GIT_AUTHOR_NAME", "Hallmark Test")
        .env("GIT_AUTHOR_EMAIL", "test@hallmark.invalid")
        .env("GIT_COMMITTER_NAME", "Hallmark Test")
        .env("GIT_COMMITTER_EMAIL", "test@hallmark.invalid")
        .args([
            "merge",
            "-q",
            "hm/H-0001-add-retries-to-the-api-client-and-a",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success(), "the child sees the same conflict");
    std::fs::write(child_wt.join("README.md"), "# api and web\n").unwrap();
    git(&child_wt, &["add", "-A"]);
    git(&child_wt, &["commit", "-q", "--no-edit"]);
    let m = env.hall(&env.repo, &["merge", "H-0003"]);
    assert_eq!(m.code, 0, "{}", m.stdout);
    assert_eq!(m.json["open_children"], json!([]));
    assert_eq!(
        // Windows checkouts may turn LF into CRLF.
        std::fs::read_to_string(parent_wt.join("README.md"))
            .unwrap()
            .replace("\r\n", "\n"),
        "# api and web\n"
    );
}

#[test]
fn the_parent_goes_to_review_only_when_every_child_is_in() {
    let env = Env::initialized();
    let r = split(&env);
    let r1 = env.hall(&env.repo, &["ready", "H-0001"]);
    assert_eq!(r1.code, 4, "{}", r1.stdout);
    assert_eq!(r1.json["error"]["code"], "CHILDREN_OPEN");
    let next = r1.json["error"]["next"].to_string();
    assert!(next.contains("hall merge H-0002") && next.contains("hall drop H-0003"));

    // A child never goes to review itself.
    let c = env.hall(&wt(&r, 0), &["ready"]);
    assert_eq!(c.code, 4, "{}", c.stdout);
    assert_eq!(c.json["error"]["code"], "WRONG_STATE");
    assert!(
        c.json["error"]["next"]
            .to_string()
            .contains("hall merge H-0002")
    );
    // But it can say it looks done, and its hints point at merging.
    let pr = env.hall(
        &wt(&r, 0),
        &["propose-ready", "--ask", "--summary", "retries in place"],
    );
    assert_eq!(pr.code, 0, "{}", pr.stdout);
    assert_eq!(pr.json["state"], "awaiting_you");
    assert_eq!(pr.json["next"][0], "hall merge H-0002");

    // The parent can't be dropped while its parts are open.
    let d = env.hall(&env.repo, &["drop", "H-0001", "--reason", "nah"]);
    assert_eq!(d.json["error"]["code"], "CHILDREN_OPEN", "{}", d.stdout);

    commit_file(&wt(&r, 0), "api/retry.rs", "fn retry() {}\n");
    assert_eq!(env.hall(&env.repo, &["merge", "H-0002"]).code, 0);
    let d = env.hall(&env.repo, &["drop", "H-0003", "--reason", "not needed"]);
    assert_eq!(d.code, 0, "{}", d.stdout);
    let ok = env.hall(&env.repo, &["ready", "H-0001"]);
    assert_eq!(ok.code, 0, "{}", ok.stdout);
    assert_eq!(ok.json["state"], "checks");
}

#[test]
fn merge_refuses_items_without_a_parent_and_dirty_parts() {
    let env = Env::initialized();
    let r = split(&env);
    let (id, _) = env.new_item("Standalone");
    let m = env.hall(&env.repo, &["merge", &id]);
    assert_eq!(m.code, 4, "{}", m.stdout);
    assert_eq!(m.json["error"]["code"], "WRONG_STATE");

    let api = wt(&r, 0);
    commit_file(&api, "api/retry.rs", "fn retry() {}\n");
    std::fs::write(api.join("api/retry.rs"), "fn retry() { todo!() }\n").unwrap();
    let m = env.hall(&env.repo, &["merge", "H-0003"]);
    assert_eq!(
        m.code, 0,
        "a part with no commits merges as is: {}",
        m.stdout
    );
    let m = env.hall(&api, &["merge"]);
    assert_eq!(m.code, 4, "{}", m.stdout);
    assert_eq!(m.json["error"]["code"], "DIRTY_WORKTREE");
}

#[test]
fn a_part_can_be_added_by_hand() {
    let env = Env::initialized();
    let (parent, parent_wt) = env.new_item("Big change");
    commit_file(&parent_wt, "base.txt", "from the parent\n");
    let r = env.hall(&env.repo, &["new", "--title", "Docs", "--parent", &parent]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["parent"], parent.as_str());
    assert!(r.json.get("plan").is_none(), "no intake for a part");
    assert_eq!(r.json["next"][1], "hall merge H-0002", "{}", r.stdout);
    let child_wt = PathBuf::from(r.json["worktree"].as_str().unwrap());
    assert!(
        child_wt.join("base.txt").is_file(),
        "starts at the parent's tip"
    );
    assert_eq!(env.journal("H-0002")[0]["parent"], "H-0001");

    // Parts don't nest, and only a parent in development takes new parts.
    let r = env.hall(
        &env.repo,
        &["new", "--title", "Deeper", "--parent", "H-0002"],
    );
    assert_eq!(r.json["error"]["code"], "WRONG_STATE", "{}", r.stdout);
    let (other, other_wt) = env.new_item("Other");
    commit_file(&other_wt, "x.txt", "x");
    env.hall(
        &env.repo,
        &["propose-ready", "--ask", &other, "--summary", "done"],
    );
    let r = env.hall(&env.repo, &["new", "--title", "Late", "--parent", &other]);
    assert_eq!(r.json["error"]["code"], "WRONG_STATE", "{}", r.stdout);
}

#[test]
fn editing_a_part_binds_the_session_to_the_parent() {
    let env = Env::initialized();
    let r = split(&env);
    let mut v: Value = serde_json::from_str(&fixture("hooks/pre_tool_use_write.json")).unwrap();
    v["cwd"] = json!(p(&env.repo));
    v["session_id"] = json!("orchestrator");
    for i in 0..2 {
        v["tool_input"]["file_path"] = json!(p(&wt(&r, i).join("api/new.rs")));
        let h = env.hook("pre-tool-use", &v);
        assert_eq!(h.code, 0, "{}", h.stdout);
    }
    assert_eq!(events(&env.journal("H-0001"), "session_bound").len(), 1);
    assert!(events(&env.journal("H-0002"), "session_bound").is_empty());
    assert!(events(&env.journal("H-0003"), "session_bound").is_empty());

    // The parent's session context lists its parts and the flow.
    let mut s: Value = serde_json::from_str(&fixture("hooks/session_start.json")).unwrap();
    s["cwd"] = json!(p(Path::new(r.json["worktree"].as_str().unwrap())));
    s["session_id"] = json!("orchestrator");
    let h = env.hook("session-start", &s);
    let ctx = h.json["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        ctx.contains("H-0002 API client retries (development"),
        "{ctx}"
    );
    assert!(ctx.contains("one subagent per open part"), "{ctx}");

    // A session started inside a part works on that part.
    s["cwd"] = json!(p(&wt(&r, 1)));
    s["session_id"] = json!("part");
    let h = env.hook("session-start", &s);
    let ctx = h.json["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(ctx.contains("H-0003 is a parallel part of H-0001"), "{ctx}");
}

/// A fake `gh` that answers `pr view` and `api` (Unix only: a shell script).
#[cfg(unix)]
fn fake_gh(env: &Env) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let pr = json!({
        "number": 12, "url": "https://github.com/acme/app/pull/12",
        "title": "Retry webhooks", "body": "Adds retries.",
        "reviews": [{"author": {"login": "alice"}, "state": "CHANGES_REQUESTED",
                     "body": "Backoff needs a cap."}],
        "comments": [{"author": {"login": "carol"}, "body": "Log the attempt number."}]
    });
    let lines = json!([{"user": {"login": "alice"}, "path": "src/retry.rs", "line": 14,
                        "body": "Use saturating_mul here."}]);
    let dir = env.root.join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("pr.json"), pr.to_string()).unwrap();
    std::fs::write(dir.join("lines.json"), lines.to_string()).unwrap();
    let gh = dir.join("gh");
    let d = p(&dir);
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  pr) cat '{d}/pr.json' ;;\n  api) cat '{d}/lines.json' ;;\n  *) exit 1 ;;\nesac\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    gh
}

#[cfg(unix)]
#[test]
fn a_follow_up_item_starts_from_pr_feedback() {
    let env = Env::initialized();
    let gh = fake_gh(&env);
    let script = env.script(split_plan());
    let out = env
        .cmd(&env.repo)
        .env("HALL_GH", &gh)
        .env("HALL_RUNNER", format!("fake:{}", script.display()))
        .args(["new", "--follow-up", "https://github.com/acme/app/pull/12"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    let j: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(j["title"], "Follow-up: Retry webhooks");
    assert_eq!(j["follow_up"], "https://github.com/acme/app/pull/12");
    assert!(j.get("plan").is_none(), "follow-ups are not split");
    assert!(env.fake_calls().is_empty());
    let created = &env.journal("H-0001")[0];
    assert_eq!(created["follow_up"], "https://github.com/acme/app/pull/12");
    let req = created["request"].as_str().unwrap();
    assert!(
        req.contains("- @alice (changes requested): Backoff needs a cap."),
        "{req}"
    );
    assert!(
        req.contains("- @alice on src/retry.rs:14: Use saturating_mul here."),
        "{req}"
    );
    assert!(req.contains("- @carol: Log the attempt number."), "{req}");
}

#[test]
fn retry_releases_a_held_item() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Held work");
    // Not held, not escalated: nothing to retry.
    let r = env.hall(&env.repo, &["retry", &id, "--reason", "again"]);
    assert_eq!(r.code, 4, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "WRONG_STATE");

    commit_file(&wt, "a.rs", "fn a() {}\n");
    assert_eq!(env.hall(&wt, &["ready"]).code, 0);
    let journal = env
        .repo
        .join(".git/hallmark/items")
        .join(&id)
        .join("events.jsonl");
    for kind in ["budget", "run_failed", "ship_failed", "other"] {
        // Holds come from the loop; append one as the runner would.
        let held = json!({"v": 1, "t": "2026-10-09T12:00:00Z", "actor": "assayer",
                          "item": id, "event": "held", "kind": kind, "reason": "stopped"});
        let mut text = std::fs::read_to_string(&journal).unwrap();
        text.push_str(&format!("{held}\n"));
        std::fs::write(&journal, text).unwrap();

        let r = env.hall(&env.repo, &["retry", &id, "--reason", "budget raised"]);
        assert_eq!(r.code, 0, "{kind}: {}", r.stdout);
        assert_eq!(r.json["changed"], true, "{kind}");
        assert_eq!(r.json["state"], "checks", "the item keeps its place");
        let events = env.journal(&id);
        let last = events.last().unwrap();
        assert_eq!(last["event"], "released", "{kind}");
        assert_eq!(last["reason"], "budget raised");
    }
    // Released already: nothing left to retry.
    let r = env.hall(&env.repo, &["retry", &id, "--reason", "again"]);
    assert_eq!(r.code, 4, "{}", r.stdout);
}

#[test]
fn an_extension_for_the_planner_reaches_the_front_desk() {
    let env = Env::initialized();
    let r = env.hall(
        &env.repo,
        &["extensions", "enable", "mattpocock", "--role", "planner"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    let script = env.script(json!([{"role": "intake", "structured": {
        "parallel": false, "reason": "One change.", "parts": []
    }}]));
    let r = env.hall_fake(&env.repo, &script, &["new", REQUEST]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let calls = env.fake_calls();
    let prompt = calls[0]["prompt"].as_str().unwrap();
    let at = prompt
        .find("## Extensions\n")
        .unwrap_or_else(|| panic!("{prompt}"));
    assert!(prompt.find(REQUEST).unwrap() < at, "after the request");
    assert!(prompt[at..].contains("for the Front Desk"));
    assert!(prompt[at..].contains("<skill extension=\"mattpocock\" name=\"to-tickets\">"));
    assert!(
        prompt[at..].contains("any edge"),
        "how it fits a plan of parts"
    );
}
