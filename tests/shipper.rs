//! Workstream C: the shipper (the Dispatch Dock), `hall export` and
//! `hall history`. Ships go to a local bare remote, never to GitHub.

mod c_support;
mod common;

use c_support::*;
use common::{Env, commit_file, git, p};
use hallmark::findings::{Lens, PriorStatus, Severity};
use insta::assert_json_snapshot;
use serde_json::{Value, json};

fn scrub(env: &Env, v: &Value) -> Value {
    let s = v.to_string().replace(&p(&env.root), "[root]");
    serde_json::from_str(&s).unwrap()
}

fn ship_pass(env: &Env) -> Value {
    let script = env.script(json!([]));
    let r = env.hall_fake(
        &env.repo,
        &script,
        &["daemon", "run", "--once", "--only", "shipper"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    r.json["steps"][0].clone()
}

fn item_file(env: &Env, id: &str, name: &str) -> std::path::PathBuf {
    env.repo.join(".git/hallmark/items").join(id).join(name)
}

#[test]
fn ships_to_a_local_remote() {
    let env = Env::initialized();
    let remote = bare_remote(&env);
    set_config(&env, "ship", &toml("git push -q -u origin {branch}"));
    let (id, wt, sha) = hallmarked_item(&env, "Retry webhooks");
    assert_eq!(state(&env, &id), "hallmarked");

    let step = ship_pass(&env);
    assert_eq!(step["progress"], "worked", "{step}");
    assert!(step["what"].as_str().unwrap().contains("shipped"), "{step}");
    assert_eq!(state(&env, &id), "closed");
    let shipped = events(&env, &id, "shipped");
    assert_eq!(shipped.len(), 1);
    assert_eq!(shipped[0]["actor"], "shipper");
    assert_eq!(shipped[0]["sha"], sha.as_str());
    assert!(shipped[0]["pr_url"].is_null());
    // The branch reached the remote at the hallmarked commit.
    let branch = git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let remote_sha = git(&remote, &["rev-parse", &format!("refs/heads/{branch}")]);
    assert_eq!(remote_sha, sha);

    // The PR body: the review summary, the minor riding along, the record.
    let body = std::fs::read_to_string(item_file(&env, &id, "pr-body.md")).unwrap();
    for s in [
        "Round 1: retries are safe with the row lock.",
        "Hallmarked at",
        "reviewed by claude-opus-5-5",
        "### Still open (minor and nit)",
        "**F1** minor · evidence · `retry.rs:3` · No retry test",
        &format!("hall export {id}"),
    ] {
        assert!(body.contains(s), "{s} in\n{body}");
    }
    assert!(!body.contains("Closes #"));
    // And the export, written after shipping.
    let export = std::fs::read_to_string(item_file(&env, &id, "export.md")).unwrap();
    assert!(export.contains("shipped"), "{export}");
    // Nothing left to ship.
    assert_eq!(ship_pass(&env)["progress"], "idle");
}

#[test]
fn ship_reports_the_pr_url_and_closes_the_issue() {
    let env = Env::initialized();
    bare_remote(&env);
    set_config(
        &env,
        "ship",
        &toml(
            "git push -q origin {branch} && echo Opened https://github.com/acme/app/pull/7 for {issue}",
        ),
    );
    let (id, _, _) = hallmarked_item(&env, "Retry webhooks");
    // Give the item an issue, as `hall new --issue 212` would have.
    let events_path = item_file(&env, &id, "events.jsonl");
    let text = std::fs::read_to_string(&events_path).unwrap();
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let mut first: Value = serde_json::from_str(&lines[0]).unwrap();
    first["issue"] = json!({"number": 212});
    lines[0] = first.to_string();
    std::fs::write(&events_path, lines.join("\n") + "\n").unwrap();
    assert_eq!(env.hall(&env.repo, &["reindex"]).code, 0);

    let step = ship_pass(&env);
    assert_eq!(
        step["what"],
        format!("{id} shipped: https://github.com/acme/app/pull/7"),
        "{step}"
    );
    let status = env.hall(&env.repo, &["status", &id]);
    assert_eq!(status.json["pr_url"], "https://github.com/acme/app/pull/7");
    let body = std::fs::read_to_string(item_file(&env, &id, "pr-body.md")).unwrap();
    assert!(body.contains("Closes #212"), "{body}");
}

/// Ships an item whose commit says `message` and returns the PR title the
/// ship command was given.
#[cfg(unix)]
fn shipped_title(message: Option<&str>, title: &str) -> String {
    let env = Env::initialized();
    bare_remote(&env);
    let out = env.root.join("title.txt");
    set_config(
        &env,
        "ship",
        &toml(&format!("printf '%s' {{title}} > '{}'", p(&out))),
    );
    hallmarked_item_with(&env, title, message);
    assert_eq!(ship_pass(&env)["progress"], "worked");
    std::fs::read_to_string(out).unwrap()
}

#[cfg(unix)]
#[test]
fn the_pr_title_takes_the_conventional_type_of_its_commits() {
    assert_eq!(
        shipped_title(Some("feat(retry): retry webhooks"), "Retry webhooks"),
        "feat: Retry webhooks"
    );
    assert_eq!(
        shipped_title(
            Some("fix: drop the old queue\n\nBREAKING CHANGE: no more v1"),
            "Drop v1"
        ),
        "fix!: Drop v1"
    );
    // A title that is already conventional, and commits that aren't.
    assert_eq!(
        shipped_title(Some("feat: x"), "docs: say so"),
        "docs: say so"
    );
    assert_eq!(shipped_title(None, "Retry webhooks"), "Retry webhooks");
}

#[test]
fn a_branch_that_moved_past_the_hallmark_is_held() {
    let env = Env::initialized();
    bare_remote(&env);
    set_config(&env, "ship", &toml("git push -q origin {branch}"));
    let (id, wt, _) = hallmarked_item(&env, "Retry webhooks");
    commit_file(&wt, "later.rs", "unreviewed");
    let step = ship_pass(&env);
    assert!(step["what"].as_str().unwrap().contains("held"), "{step}");
    let held = events(&env, &id, "held");
    assert_eq!(held[0]["kind"], "ship_failed");
    assert!(
        held[0]["reason"]
            .as_str()
            .unwrap()
            .contains("moved past the hallmark")
    );
    assert_eq!(state(&env, &id), "hallmarked");
    assert!(events(&env, &id, "shipped").is_empty());
    // Nothing was pushed, and the held item isn't retried on its own.
    assert_eq!(ship_pass(&env)["progress"], "idle");
    let inbox = env.hall(&env.repo, &["inbox"]);
    assert!(inbox.stdout.contains(&id), "{}", inbox.stdout);
}

#[test]
fn a_failing_ship_command_holds_the_item() {
    let env = Env::initialized();
    // No remote: the push fails.
    set_config(&env, "ship", &toml("git push -q origin {branch}"));
    let (id, _, _) = hallmarked_item(&env, "Retry webhooks");
    let step = ship_pass(&env);
    assert!(step["what"].as_str().unwrap().contains("held"), "{step}");
    let held = events(&env, &id, "held");
    assert_eq!(held[0]["kind"], "ship_failed");
    assert!(
        held[0]["reason"]
            .as_str()
            .unwrap()
            .starts_with("the ship command exited")
    );
    assert_eq!(state(&env, &id), "hallmarked");
}

/// One item through the whole loop: rejected once, fixed, approved, shipped.
fn a_full_loop(env: &Env) -> (String, String) {
    bare_remote(env);
    set_config(env, "ship", &toml("git push -q origin {branch}"));
    let (id, wt, _) = fixing_item(env, "Retry webhooks");
    commit_file(
        &wt,
        "retry.rs",
        "fn retry() {\n    lock();\n    deliver();\n}\n",
    );
    let r = env.hall(
        &wt,
        &["fixed", "F1", "--dispute", "F3", "--reason", "house style"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    approve(
        env,
        &id,
        &wt,
        2,
        vec![("F1", PriorStatus::Fixed), ("F3", PriorStatus::Withdrawn)],
        counts(0, 0, 1, 0),
    );
    assert_eq!(ship_pass(env)["progress"], "worked");
    // A second item, dropped.
    let (dropped, _) = env.new_item("Usage page");
    let r = env.hall(
        &env.repo,
        &["drop", &dropped, "--reason", "superseded by H-0001"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    (id, dropped)
}

#[test]
fn export_is_the_whole_record() {
    let env = Env::initialized();
    let (id, _) = a_full_loop(&env);
    let r = env.hall(&env.repo, &["export", &id]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let md = r.json["markdown"].as_str().unwrap().to_string();
    assert!(r.json["path"].as_str().unwrap().ends_with("export.md"));
    for s in [
        &format!("# {id} · Retry webhooks"),
        "| State | closed (Dispatch Dock) |",
        "| Rounds | 2 · 1 rejected |",
        "## Timeline",
        "fix submitted",
        "### Review, round 1 · changes requested",
        "**F1** major · correctness · `retry.rs:3` · Double delivery",
        "- claims fixed: F1",
        "- disputes F3: house style",
        "### Review, round 2 · approved",
        "- F1 fixed",
        "- F3 withdrawn",
        "| F2 | 1 | minor | evidence |",
        "## Hallmark",
        "Round 2 · reviewer claude-opus-5-5",
    ] {
        assert!(md.contains(s), "{s} in\n{md}");
    }
    // The headings are the contract; times and SHAs vary.
    let headings: Vec<String> = md
        .lines()
        .filter(|l| l.starts_with('#'))
        .map(|l| {
            l.split('`')
                .enumerate()
                .map(|(i, part)| {
                    let sha = part.len() == 7 && part.chars().all(|c| c.is_ascii_hexdigit());
                    if i % 2 == 1 && sha { "[sha]" } else { part }
                })
                .collect::<Vec<_>>()
                .join("`")
        })
        .collect();
    let mut v = scrub(&env, &r.json);
    v["markdown"] = json!(headings);
    assert_json_snapshot!("c_export", v);

    // --out writes elsewhere; the item comes from the worktree too.
    let out = env.root.join("record.md");
    let r = env.hall(&env.repo, &["export", &id, "--out", &out.to_string_lossy()]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), md);
}

#[test]
fn history_says_what_was_rejected_and_why() {
    let env = Env::initialized();
    let (id, dropped) = a_full_loop(&env);
    // Still in development: only with --all.
    env.new_item("CLI login flow");

    let r = env.hall(&env.repo, &["history"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let items = r.json["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    // Newest first: the drop came after the ship.
    assert_eq!(items[0]["id"], dropped.as_str());
    assert_eq!(items[0]["outcome"], "dropped");
    assert_eq!(items[1]["id"], id.as_str());
    assert_eq!(items[1]["outcome"], "closed");
    assert_eq!(items[1]["rejected"], 1);
    assert_eq!(
        items[1]["reasons"],
        json!(["F1 major correctness: Double delivery"])
    );
    assert_eq!(items[1]["blocking"][0]["status"], "fixed");
    assert_json_snapshot!("c_history", scrub(&env, &r.json), {
        ".items[].created_at" => "[time]",
        ".items[].state_since" => "[time]",
        ".items[].ended_at" => "[time]",
        ".items[].duration_secs" => "[secs]",
        ".items[].duration" => "[duration]",
        ".items[].sha" => "[sha]",
        ".items[].hallmark" => "[sha]",
    });

    let r = env.hall(&env.repo, &["history", "--rejected", "--since", "7d"]);
    let ids: Vec<&str> = r.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [id.as_str()]);
    let r = env.hall(&env.repo, &["history", "--lens", "security"]);
    assert_eq!(r.json["items"], json!([]));
    let r = env.hall(&env.repo, &["history", "--lens", "correctness"]);
    assert_eq!(r.json["items"].as_array().unwrap().len(), 1);
    let r = env.hall(&env.repo, &["history", "--all", "--limit", "1"]);
    assert_eq!(r.json["items"].as_array().unwrap().len(), 1);
    assert_eq!(r.json["total"], 3);
    let r = env.hall(&env.repo, &["history", "--lens", "vibes"]);
    assert_eq!(r.code, 2);
    assert_eq!(r.json["error"]["code"], "USAGE");
    let r = env.hall(&env.repo, &["history", "--since", "soon"]);
    assert_eq!(r.code, 2);
}

#[test]
fn findings_from_both_rounds_are_tracked_for_history() {
    // A blocker raised in round 2 counts as a reason too.
    let env = Env::initialized();
    let (id, wt, _) = fixing_item(&env, "Retry webhooks");
    commit_file(&wt, "retry.rs", "fn retry() { lock(); }\n");
    assert_eq!(env.hall(&wt, &["fixed", "F1"]).code, 0);
    let sha = git(&wt, &["rev-parse", "HEAD"]);
    append(&env, &id, "assayer", checks(&sha, 2, true, vec![]));
    append(
        &env,
        &id,
        "assayer",
        verdict(
            &sha,
            2,
            vec![finding(
                "F4",
                2,
                Severity::Blocker,
                Lens::SecurityTenancy,
                "Reads another tenant's webhook",
            )],
            vec![("F1", PriorStatus::Fixed)],
            counts(1, 0, 1, 1),
        ),
    );
    assert_eq!(
        env.hall(&env.repo, &["drop", &id, "--reason", "rethink"])
            .code,
        0
    );
    let r = env.hall(&env.repo, &["history", "--lens", "security"]);
    let row = &r.json["items"][0];
    assert_eq!(row["rejected"], 2);
    assert_eq!(
        row["reasons"],
        json!(["F4 blocker security_tenancy: Reads another tenant's webhook"])
    );
}
