//! The Gazette (ADR 0053): the librarian prints a newspaper page for each
//! finished item, `hall gazette` reads or reprints it, and `hall attach`
//! adds plates. Driven through the CLI with the fake runner.

mod common;

use std::path::PathBuf;

use common::Env;
use serde_json::{Value, json};

/// Appends a journal event to an item.
fn append(env: &Env, id: &str, actor: &str, mut body: Value) {
    body["v"] = json!(1);
    body["t"] = json!(chrono::Utc::now().to_rfc3339());
    body["actor"] = json!(actor);
    body["item"] = json!(id);
    let p = env
        .repo
        .join(".git/hallmark/items")
        .join(id)
        .join("events.jsonl");
    let mut text = std::fs::read_to_string(&p).unwrap();
    text.push_str(&format!("{body}\n"));
    std::fs::write(&p, text).unwrap();
}

fn finding(id: &str, severity: &str, lens: &str, title: &str) -> Value {
    json!({"id": id, "round": 1, "lens": lens, "severity": severity,
           "file": "src/web/mod.rs", "line": 42, "title": title,
           "scenario": "Another account reads /proc/<pid>/cmdline.", "fix": "Write a 0600 page."})
}

/// An item that went through two rounds of review (F1 fixed, F2 withdrawn
/// after a dispute, F3 left open) and shipped.
fn closed_item(env: &Env) -> String {
    let (id, _) = env.new_item("hall web: a local web app");
    let ev = |actor: &str, body: Value| append(env, &id, actor, body);
    ev(
        "agent",
        json!({"event": "proposed_ready", "summary": "Adds hall web",
               "why": "People asked for the building in a browser.",
               "how": "Every button runs a hall command."}),
    );
    ev(
        "you",
        json!({"event": "ready", "sha": "abc1234", "round": 1}),
    );
    ev(
        "assayer",
        json!({"event": "checks", "sha": "abc1234", "round": 1, "passed": true,
               "runs": [{"command": "nix develop --command cargo test", "exit_code": 0, "duration_ms": 10}]}),
    );
    ev(
        "assayer",
        json!({"event": "verdict", "sha": "abc1234", "round": 1, "verdict": "changes",
        "escalated": false, "summary": "round 1",
        "counts": {"blocker": 0, "major": 1, "minor": 2, "nit": 0},
        "findings": [
            finding("F1", "major", "security_tenancy", "The token URL is passed in argv"),
            finding("F2", "minor", "correctness", "Backoff has no jitter"),
            finding("F3", "minor", "evidence", "No test pins launch_dir")
        ]}),
    );
    ev(
        "fixer",
        json!({"event": "fix_submitted", "sha": "def5678", "fixes": ["F1"],
               "disputes": [{"id": "F2", "reason": "the queue adds jitter"}]}),
    );
    ev(
        "assayer",
        json!({"event": "checks", "sha": "def5678", "round": 2, "passed": true,
               "runs": [{"command": "nix develop --command cargo test", "exit_code": 0, "duration_ms": 10}]}),
    );
    ev(
        "assayer",
        json!({"event": "verdict", "sha": "def5678", "round": 2, "verdict": "approve",
               "escalated": false, "summary": "round 2",
               "counts": {"blocker": 0, "major": 0, "minor": 1, "nit": 0},
               "findings": [],
               "prior": [{"id": "F1", "status": "fixed"},
                         {"id": "F2", "status": "withdrawn", "note": "dispute accepted"},
                         {"id": "F3", "status": "open"}]}),
    );
    ev(
        "assayer",
        json!({"event": "hallmarked", "sha": "def5678aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "round": 2}),
    );
    ev(
        "shipper",
        json!({"event": "shipped", "sha": "def5678", "pr_url": "https://github.com/o/r/pull/110"}),
    );
    ev(
        "hook",
        json!({"event": "usage", "role": "dev", "room": "workshop", "model": "claude-opus-5-5",
               "source": "transcript", "input_tokens": 1, "output_tokens": 1,
               "cache_read_tokens": 0, "cache_write_tokens": 0, "cost_usd": 18.5, "requests": 1}),
    );
    let r = env.hall(&env.repo, &["status", &id]);
    assert_eq!(r.json["state"], "closed", "{}", r.stdout);
    id
}

fn page(env: &Env, id: &str) -> PathBuf {
    env.repo
        .join(".git/hallmark/items")
        .join(id)
        .join("gazette.html")
}

fn read(env: &Env, id: &str) -> String {
    std::fs::read_to_string(page(env, id)).unwrap()
}

fn prose() -> Value {
    json!({
        "kicker": "The web app",
        "headline": "Hallmark's whole floor, now in a browser tab",
        "deck": "After two rounds of review, `hall web` shipped.",
        "story": ["You asked for the floor in a browser.", "Every button runs a hall command."],
        "rounds": [{"round": 1, "line": "Not safe to ship: the key could leak."}],
        "findings": [
            {"id": "F1", "headline": "Other people on a shared computer could read the key",
             "text": "It now goes through a private file."},
            {"id": "F3", "headline": "No test checks where the page is written", "text": ""}
        ],
        "diagram": {
            "caption": "How a click becomes a command <script>",
            "nodes": [
                {"id": "b", "label": "Your browser", "note": "the page"},
                {"id": "w", "label": "hall web", "note": "127.0.0.1 only"},
                {"id": "c", "label": "hall <command>", "note": ""}
            ],
            "edges": [
                {"from": "b", "to": "w", "label": "key + command"},
                {"from": "w", "to": "c", "label": "as you"},
                {"from": "c", "to": "b", "label": "the reply"}
            ]
        }
    })
}

#[test]
fn the_librarian_prints_the_gazette_with_its_words() {
    let env = Env::initialized();
    let id = closed_item(&env);
    let script = env.script(json!([{"role": "librarian", "structured": {
        "learnings": [{"updates": "", "kind": "rule", "scope": ["src/web/**"], "lens": "security_tenancy",
                       "source": [format!("{id}/F1")], "title": "Never put a token in argv",
                       "body": "Other accounts read argv."}],
        "retire": [],
        "gazette": prose()
    }}]));
    let r = env.hall_fake(
        &env.repo,
        &script,
        &["daemon", "run", "--once", "--only", "librarian"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);

    // The prompt asks for the Gazette, and the schema has a place for it.
    let calls = env.fake_calls();
    let c = &calls[0];
    assert!(c["json_schema"].as_str().unwrap().contains("\"gazette\""));
    let prompt = c["prompt"].as_str().unwrap();
    assert!(prompt.contains("`gazette`"), "{prompt}");
    assert!(prompt.contains("People asked for the building in a browser."));

    // The page: the librarian's words, Hallmark's facts.
    let html = read(&env, &id);
    for want in [
        "<title>Hallmark Gazette ",
        "Hallmark&#39;s whole floor, now in a browser tab",
        "After two rounds of review, <span class=\"mono\">hall web</span> shipped.",
        "Not safe to ship: the key could leak.",
        // Round 2 had no line from the librarian: Hallmark writes one.
        "Approved, with 1 minor left open.",
        "Other people on a shared computer could read the key",
        "Fixed in round 2",
        // F2 has no words from the librarian: its own title.
        "Backoff has no jitter",
        "Withdrawn in round 2",
        // F3 shipped open.
        "<h2>Corrections</h2>",
        "No test checks where the page is written.",
        "Never put a token in argv",
        "HALLMARKED",
        "def5678",
        "Pull request #110 opened.",
        "cargo test",
        "$18.50",
        "Dev agent",
        "Written by the librarian",
    ] {
        assert!(html.contains(want), "missing {want:?}");
    }
    // The diagram is drawn, escaped, with the edge back to the browser
    // dashed.
    assert!(html.contains("<svg viewBox"), "a diagram");
    assert!(html.contains("class=\"edge back\""));
    assert!(html.contains("hall &lt;command&gt;"));
    assert!(!html.contains("<script>"), "nothing gets through unescaped");
    // Self-contained: no scripts, nothing from the network.
    assert!(!html.contains("<script"));
    assert!(!html.contains("<link"));

    // The prose is kept for reprints; reading the page doesn't print it.
    let saved = env
        .repo
        .join(".git/hallmark/items")
        .join(&id)
        .join("gazette.json");
    assert!(saved.exists());
    let r = env.hall(&env.repo, &["gazette", &id]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["printed"], false, "{}", r.stdout);
    assert_eq!(r.json["prose"], true);
    assert!(r.json.get("html").is_none());

    // Nothing new: the next tick is idle. The PR merges: the next tick
    // reprints the page from code, without a model run.
    let r = env.hall_fake(
        &env.repo,
        &script,
        &["daemon", "run", "--once", "--only", "librarian"],
    );
    assert_eq!(r.json["steps"][0]["progress"], "idle", "{}", r.stdout);
    append(
        &env,
        &id,
        "watchman",
        json!({"event": "delivered", "pr_url": "https://github.com/o/r/pull/110", "outcome": "merged"}),
    );
    let r = env.hall_fake(
        &env.repo,
        &script,
        &["daemon", "run", "--once", "--only", "librarian"],
    );
    assert_eq!(r.json["steps"][0]["progress"], "worked", "{}", r.stdout);
    assert!(
        r.json["steps"][0]["what"]
            .as_str()
            .unwrap()
            .contains(&format!("reprinted the Gazette for {id}")),
        "{}",
        r.stdout
    );
    assert_eq!(env.fake_calls().len(), 1, "no second model run");
    let html = read(&env, &id);
    assert!(html.contains("<span>Merged</span>"), "the dateline");
    assert!(html.contains("The pull request merged."));
    assert!(html.contains("Other people on a shared computer could read the key"));

    // The list of editions.
    let r = env.hall(&env.repo, &["gazette", "--list"]);
    assert_eq!(r.json["editions"][0]["id"], json!(id), "{}", r.stdout);
}

#[test]
fn hall_gazette_prints_any_finished_item_in_its_own_words() {
    let env = Env::initialized();
    let id = closed_item(&env);
    let r = env.hall(&env.repo, &["gazette", &id, "--html"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["printed"], true);
    assert_eq!(r.json["prose"], false);
    let html = r.json["html"].as_str().unwrap();
    assert_eq!(html, read(&env, &id));
    for want in [
        "hall web: a local web app",
        "Adds hall web",
        "People asked for the building in a browser.",
        "The token URL is passed in argv",
        "Sent back: 1 major, 2 minor.",
        "Set from the item's own words",
    ] {
        assert!(html.contains(want), "missing {want:?}");
    }
    // Idempotent: a second read prints nothing.
    let r = env.hall(&env.repo, &["gazette", &id]);
    assert_eq!(r.json["printed"], false, "{}", r.stdout);
    // A new event makes it stale; the next read prints it again.
    append(
        &env,
        &id,
        "watchman",
        json!({"event": "pr_checks", "pr_url": "https://github.com/o/r/pull/110", "passed": true}),
    );
    let r = env.hall(&env.repo, &["gazette", &id]);
    assert_eq!(r.json["printed"], true, "{}", r.stdout);
    assert!(read(&env, &id).contains("CI passed on the pull request."));
}

#[test]
fn an_item_in_progress_has_no_gazette() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Retry webhooks");
    let r = env.hall(&env.repo, &["gazette", &id]);
    assert_eq!(r.code, 4, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "WRONG_STATE");
    assert!(!page(&env, &id).exists());
    let r = env.hall(&env.repo, &["gazette", "--list"]);
    assert_eq!(r.json["editions"], json!([]), "{}", r.stdout);
}

#[test]
fn attachments_become_plates() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("hall web: a local web app");
    // A 1×1 PNG.
    let png: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, 0x49, 0x48, 0x44, 0x52, 0,
        0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89,
    ];
    std::fs::write(wt.join("control room.png"), png).unwrap();
    let r = env.hall(
        &wt,
        &[
            "attach",
            "control room.png",
            "--caption",
            "The control room, redrawn",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["id"], json!(id));
    assert_eq!(r.json["path"], "attachments/1-control-room.png");
    assert_eq!(r.json["kind"], "image");
    assert_eq!(r.json["added"], true);
    // Again: nothing new.
    let r = env.hall(
        &wt,
        &[
            "attach",
            "control room.png",
            "--caption",
            "The control room, redrawn",
        ],
    );
    assert_eq!(r.json["added"], false, "{}", r.stdout);
    assert_eq!(r.json["path"], "attachments/1-control-room.png");
    let r = env.hall(
        &env.repo,
        &[
            "attach",
            "https://claude.ai/artifact/abc",
            "--item",
            &id,
            "--caption",
            "The design",
        ],
    );
    assert_eq!(r.json["kind"], "link", "{}", r.stdout);
    let r = env.hall(&wt, &["attach", "missing.png"]);
    assert_eq!(r.code, 2, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "USAGE");
    let attached: Vec<Value> = env
        .journal(&id)
        .into_iter()
        .filter(|e| e["event"] == "attached")
        .collect();
    assert_eq!(attached.len(), 2);

    append(
        &env,
        &id,
        "you",
        json!({"event": "dropped", "reason": "a test", "worktree_removed": false}),
    );
    let r = env.hall(&env.repo, &["gazette", &id]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["attachments"], 2);
    let html = read(&env, &id);
    assert!(
        html.contains("<img src=\"data:image/png;base64,iVBORw0KGgo"),
        "inlined"
    );
    assert!(html.contains("<b>Plate I.</b> The control room, redrawn"));
    assert!(html.contains("href=\"https://claude.ai/artifact/abc\""));
    assert!(html.contains("<span>Dropped</span>"));
}
