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

    // The description: What, Why and How, and nothing of the review.
    let body = std::fs::read_to_string(item_file(&env, &id, "pr-body.md")).unwrap();
    assert_eq!(
        body,
        format!(
            "## What\n\nRetry webhooks\n\n## Why\n\nRetry webhooks\n\n## How\n\n\
             - change retry.rs\n\n---\n\nReviewed and hallmarked as {id}: the review is in \
             the comments.\n"
        )
    );
    // The review, for the PR comment: the summary, each finding and how it
    // stands, the checks, the record.
    let review = std::fs::read_to_string(item_file(&env, &id, "review.md")).unwrap();
    for s in [
        "<!-- hallmark:review -->\n## Hallmark review: approved",
        "Round 1: retries are safe with the row lock.",
        "Hallmarked at",
        "reviewed by claude-opus-5-5",
        "### Findings",
        "- **F1** minor · evidence · `retry.rs:3` · No retry test\n  - Still open (minor rides along).",
        &format!("The full record stays local: `hall export {id}`."),
    ] {
        assert!(review.contains(s), "{s} in\n{review}");
    }
    assert!(!review.contains("Why it was reviewed again"));
    // No PR URL came back, so there was nothing to comment on.
    assert!(!step["what"].as_str().unwrap().contains("review"), "{step}");
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
    set_config(&env, "review_comment", &toml(""));
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
    // `review_comment = ""` posts nothing, so the description doesn't claim it.
    assert!(!body.contains("in the comments"), "{body}");
    assert!(body.contains("the review stays local"), "{body}");
}

/// A hallmarked item whose author said what, why and how with
/// `propose-ready`, on two commits.
fn proposed_item(env: &Env) -> (String, std::path::PathBuf) {
    let (id, wt) = env.new_item("Retry webhooks");
    commit_file(&wt, "retry.rs", "fn retry() {}\n");
    commit_file(&wt, "backoff.rs", "fn backoff() {}\n");
    let r = env.hall(
        &wt,
        &[
            "propose-ready",
            "--summary",
            "Failed webhook deliveries are retried with backoff.",
            "--why",
            "Receivers that blip lose events today; #88 lost a day of orders.",
            "--how",
            "A retry loop around `deliver`, with jittered backoff capped at 5 tries.",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    let sha = git(&wt, &["rev-parse", "HEAD"]);
    append(env, &id, "assayer", checks(&sha, 1, true, vec![]));
    append(
        env,
        &id,
        "assayer",
        verdict(&sha, 1, vec![], vec![], counts(0, 0, 0, 0)),
    );
    let repo = hallmark::repo::Repo::discover(&env.repo).unwrap();
    hallmark::stamp::strike(
        &repo,
        id.parse().unwrap(),
        &sha,
        1,
        None,
        counts(0, 0, 0, 0),
    )
    .unwrap();
    (id, wt)
}

#[test]
fn the_description_takes_what_why_and_how_from_the_author() {
    let env = Env::initialized();
    bare_remote(&env);
    set_config(&env, "ship", &toml("git push -q -u origin {branch}"));
    let (id, _) = proposed_item(&env);
    assert_eq!(ship_pass(&env)["progress"], "worked");
    let body = std::fs::read_to_string(item_file(&env, &id, "pr-body.md")).unwrap();
    assert!(
        body.starts_with(
            "## What\n\nFailed webhook deliveries are retried with backoff.\n\n\
             ## Why\n\nReceivers that blip lose events today; #88 lost a day of orders.\n\n\
             ## How\n\nA retry loop around `deliver`, with jittered backoff capped at 5 tries.\n"
        ),
        "{body}"
    );
    assert!(
        !body.contains("Hallmarked at") && !body.contains("### Checks"),
        "{body}"
    );
}

#[test]
fn without_the_authors_how_the_commits_are_listed_and_the_request_is_why() {
    let env = Env::initialized();
    bare_remote(&env);
    set_config(&env, "ship", &toml("git push -q -u origin {branch}"));
    let r = env.hall(
        &env.repo,
        &[
            "new",
            "--no-split",
            "--title",
            "Retry webhooks",
            "Receivers lose events when they blip",
        ],
    );
    let id = r.json["id"].as_str().unwrap().to_string();
    let wt = std::path::PathBuf::from(r.json["worktree"].as_str().unwrap());
    commit_file(&wt, "retry.rs", "fn retry() {}\n");
    commit_file(&wt, "backoff.rs", "fn backoff() {}\n");
    assert_eq!(env.hall(&wt, &["ready"]).code, 0);
    let sha = git(&wt, &["rev-parse", "HEAD"]);
    append(&env, &id, "assayer", checks(&sha, 1, true, vec![]));
    append(
        &env,
        &id,
        "assayer",
        verdict(&sha, 1, vec![], vec![], counts(0, 0, 0, 0)),
    );
    let repo = hallmark::repo::Repo::discover(&env.repo).unwrap();
    hallmark::stamp::strike(
        &repo,
        id.parse().unwrap(),
        &sha,
        1,
        None,
        counts(0, 0, 0, 0),
    )
    .unwrap();
    assert_eq!(ship_pass(&env)["progress"], "worked");
    let body = std::fs::read_to_string(item_file(&env, &id, "pr-body.md")).unwrap();
    assert!(
        body.starts_with(
            "## What\n\nRetry webhooks\n\n## Why\n\nReceivers lose events when they blip\n\n\
             ## How\n\n- change retry.rs\n- change backoff.rs\n"
        ),
        "{body}"
    );
}

#[test]
fn the_review_is_posted_as_a_comment_once_the_pr_is_known() {
    let env = Env::initialized();
    bare_remote(&env);
    set_config(
        &env,
        "ship",
        &toml("git push -q origin {branch} && echo Opened https://github.com/acme/app/pull/7"),
    );
    // Portable: the comment command echoes what it was given.
    set_config(
        &env,
        "review_comment",
        &toml("echo posted {pr} {review_file}"),
    );
    let (id, _, _) = hallmarked_item(&env, "Retry webhooks");
    let step = ship_pass(&env);
    assert_eq!(
        step["what"],
        format!("{id} shipped: https://github.com/acme/app/pull/7; the review is in a comment"),
        "{step}"
    );
    let runs = item_file(&env, &id, "runs");
    let log = std::fs::read_dir(&runs)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| {
            e.file_name()
                .to_string_lossy()
                .ends_with("-review-comment.log")
        })
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .expect("the review comment ran");
    assert!(
        log.contains("posted https://github.com/acme/app/pull/7"),
        "{log}"
    );
    assert!(log.contains("review.md"), "{log}");
}

#[test]
fn a_failed_review_comment_doesnt_undo_the_ship() {
    let env = Env::initialized();
    bare_remote(&env);
    set_config(
        &env,
        "ship",
        &toml("git push -q origin {branch} && echo Opened https://github.com/acme/app/pull/7"),
    );
    set_config(
        &env,
        "review_comment",
        &toml("git ls-files missing-file --error-unmatch"),
    );
    let (id, _, _) = hallmarked_item(&env, "Retry webhooks");
    let step = ship_pass(&env);
    let what = step["what"].as_str().unwrap();
    assert!(what.contains("the review comment command"), "{what}");
    assert!(what.contains("review.md"), "{what}");
    assert_eq!(state(&env, &id), "closed");

    // Empty: no comment at all.
    let env = Env::initialized();
    bare_remote(&env);
    set_config(
        &env,
        "ship",
        &toml("git push -q origin {branch} && echo Opened https://github.com/acme/app/pull/7"),
    );
    set_config(&env, "review_comment", &toml(""));
    let (id, _, _) = hallmarked_item(&env, "Retry webhooks");
    assert_eq!(
        ship_pass(&env)["what"],
        format!("{id} shipped: https://github.com/acme/app/pull/7")
    );
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

#[test]
fn a_gh_hallmark_cant_find_is_never_run() {
    // The tests point HALL_GH at a missing file: the default comment
    // command must not reach for another gh on PATH.
    let env = Env::initialized();
    bare_remote(&env);
    set_config(
        &env,
        "ship",
        &toml("git push -q origin {branch} && echo Opened https://github.com/acme/app/pull/7"),
    );
    let (id, _, _) = hallmarked_item(&env, "Retry webhooks");
    let what = ship_pass(&env)["what"].as_str().unwrap().to_string();
    assert!(
        what.contains("gh wasn't found, so the review is in"),
        "{what}"
    );
    assert_eq!(state(&env, &id), "closed");
}

#[test]
fn the_review_comment_says_how_each_finding_was_settled() {
    let env = Env::initialized();
    bare_remote(&env);
    set_config(&env, "ship", &toml("git push -q -u origin {branch}"));
    let (id, wt, _) = fixing_item(&env, "Retry webhooks");
    commit_file(&wt, "retry.rs", "fn retry() { lock(); }\n");
    let fixed = git(&wt, &["rev-parse", "HEAD"]);
    let r = env.hall(
        &wt,
        &[
            "fixed",
            "F1",
            "--dispute",
            "F3",
            "--reason",
            "The name follows the module's convention",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    approve(
        &env,
        &id,
        &wt,
        2,
        vec![("F1", PriorStatus::Fixed), ("F3", PriorStatus::Withdrawn)],
        counts(0, 0, 1, 0),
    );
    assert_eq!(ship_pass(&env)["progress"], "worked");
    let review = std::fs::read_to_string(item_file(&env, &id, "review.md")).unwrap();
    let short = &fixed[..7];
    for s in [
        "after 1 rejected review",
        // Open first, then the settled ones by severity.
        "- **F2** minor · evidence · `retry.rs:3` · No retry test\n  - Still open (minor rides along).",
        &format!(
            "- **F1** major · correctness · `retry.rs:3` · Double delivery\n  - Fixed in `{short}`, confirmed in round 2."
        ),
        "- **F3** nit · repo_rules · `retry.rs:3` · Name the constant\n  - Withdrawn in round 2.\n  - Disputed by the fixer: The name follows the module's convention",
        "- `cargo test` passed (3.1s)",
    ] {
        assert!(review.contains(s), "{s}\nin\n{review}");
    }
    assert!(review.find("**F2**").unwrap() < review.find("**F1**").unwrap());
    let body = std::fs::read_to_string(item_file(&env, &id, "pr-body.md")).unwrap();
    for s in ["F1", "F2", "F3", "Hallmarked", "cargo test"] {
        assert!(!body.contains(s), "{s} in the description:\n{body}");
    }
}
