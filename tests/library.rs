//! The Library (ADR 0021): search, briefings, the librarian, review and
//! retirement, driven through the real `hall` binary with the fake runner.

mod common;

use std::path::{Path, PathBuf};

use common::{Env, git};
use insta::assert_json_snapshot;
use serde_json::{Value, json};

const DIR: &str = ".hallmark/knowledge";

fn learning(id: &str, kind: &str, scope: &[&str], status: &str, title: &str, body: &str) -> String {
    let scope: Vec<String> = scope.iter().map(|s| format!("\"{s}\"")).collect();
    format!(
        "---\nid: {id}\nkind: {kind}\nscope: [{}]\nlens: correctness\nsource: [H-0009/F1]\nstatus: {status}\nused: 0\n---\n# {title}\n\n{body}\n",
        scope.join(", ")
    )
}

/// Writes learning files into `dir`'s knowledge folder and commits them.
fn add_learnings(dir: &Path, files: &[(&str, String)]) {
    std::fs::create_dir_all(dir.join(DIR)).unwrap();
    for (name, text) in files {
        std::fs::write(dir.join(DIR).join(name), text).unwrap();
    }
    git(dir, &["add", DIR]);
    git(dir, &["commit", "-q", "-m", "docs: learnings"]);
}

/// The three learnings most tests start from, accepted on main.
fn seed(env: &Env) {
    add_learnings(
        &env.repo,
        &[
            (
                "L-0001-retries-must-check-the-endpoint.md",
                learning(
                    "L-0001",
                    "rule",
                    &["apps/api/src/webhooks/**"],
                    "accepted",
                    "Retries must check the endpoint still exists",
                    "A deleted endpoint keeps its queued deliveries until the job notices.",
                ),
            ),
            (
                "L-0002-backoff-jitter-lives-in-the-queue.md",
                learning(
                    "L-0002",
                    "false-positive",
                    &["apps/api/src/queue/**"],
                    "accepted",
                    "Backoff jitter lives in the queue",
                    "Don't flag a webhook retry for missing jitter: the queue adds it.",
                ),
            ),
            (
                "L-0003-billing-map.md",
                learning(
                    "L-0003",
                    "map",
                    &["apps/billing/**"],
                    "accepted",
                    "Billing computes invoices nightly",
                    "The invoice job reads usage rows and never writes them.",
                ),
            ),
            (
                "L-0004-proposed-webhook-idea.md",
                learning(
                    "L-0004",
                    "rule",
                    &[],
                    "proposed",
                    "Webhook signatures rotate weekly",
                    "Not approved yet.",
                ),
            ),
            (
                "L-0005-retired-webhook-rule.md",
                learning(
                    "L-0005",
                    "rule",
                    &[],
                    "retired",
                    "Webhooks used to retry forever",
                    "Retired.",
                ),
            ),
        ],
    );
}

fn configure(env: &Env, toml: &str) {
    let p = env.repo.join(".hallmark.toml");
    let mut text = std::fs::read_to_string(&p).unwrap();
    text.push_str(toml);
    std::fs::write(&p, text).unwrap();
    git(&env.repo, &["commit", "-q", "-am", "config: library"]);
}

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

fn verdict(
    round: u32,
    approve: bool,
    escalated: bool,
    findings: Value,
    prior: Value,
    learnings: Value,
) -> Value {
    json!({
        "event": "verdict", "sha": "abc1234", "round": round,
        "verdict": if approve { "approve" } else { "changes" },
        "escalated": escalated, "findings": findings, "prior": prior,
        "counts": {"blocker": 0, "major": 0, "minor": 0, "nit": 0},
        "summary": format!("round {round}"), "learnings": learnings
    })
}

fn finding(id: &str, severity: &str, title: &str) -> Value {
    json!({"id": id, "round": 1, "lens": "correctness", "severity": severity,
           "file": "apps/api/src/webhooks/retry.rs", "line": 42, "title": title,
           "scenario": "A deleted endpoint keeps its deliveries.", "fix": "Check first."})
}

/// An item that went through review (one finding fixed, one withdrawn
/// after a dispute) and shipped.
fn closed_item(env: &Env, title: &str) -> String {
    let (id, _) = env.new_item(title);
    append(
        env,
        &id,
        "you",
        json!({"event": "ready", "sha": "abc1234", "round": 1}),
    );
    append(
        env,
        &id,
        "assayer",
        json!({"event": "checks", "sha": "abc1234", "round": 1, "passed": true, "runs": []}),
    );
    append(
        env,
        &id,
        "assayer",
        verdict(
            1,
            false,
            false,
            json!([
                finding("F1", "major", "Retry ignores deleted endpoints"),
                finding("F2", "minor", "Backoff has no jitter")
            ]),
            json!([]),
            json!([]),
        ),
    );
    append(
        env,
        &id,
        "fixer",
        json!({"event": "fix_submitted", "sha": "def5678", "fixes": ["F1"],
               "disputes": [{"id": "F2", "reason": "the queue adds jitter"}]}),
    );
    append(
        env,
        &id,
        "assayer",
        json!({"event": "checks", "sha": "def5678", "round": 2, "passed": true, "runs": []}),
    );
    append(
        env,
        &id,
        "assayer",
        verdict(
            2,
            true,
            false,
            json!([]),
            json!([{"id": "F1", "status": "fixed"}, {"id": "F2", "status": "withdrawn", "note": "dispute accepted"}]),
            json!([]),
        ),
    );
    append(
        env,
        &id,
        "assayer",
        json!({"event": "hallmarked", "sha": "def5678", "round": 2}),
    );
    append(
        env,
        &id,
        "shipper",
        json!({"event": "shipped", "sha": "def5678", "pr_url": "https://example.invalid/pr/1"}),
    );
    let r = env.hall(&env.repo, &["status", &id]);
    assert_eq!(r.json["state"], "closed", "{}", r.stdout);
    id
}

fn worktree(env: &Env, id: &str) -> PathBuf {
    PathBuf::from(env.journal(id)[0]["worktree"].as_str().unwrap())
}

fn files(wt: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(wt.join(DIR))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn read_learning(wt: &Path, prefix: &str) -> String {
    let name = files(wt)
        .into_iter()
        .find(|f| f.starts_with(prefix))
        .unwrap_or_else(|| panic!("no {prefix} in {}", wt.display()));
    std::fs::read_to_string(wt.join(DIR).join(name)).unwrap()
}

fn scrub(env: &Env, v: &Value) -> Value {
    let s = v.to_string().replace(&common::p(&env.root), "[root]");
    serde_json::from_str(&s).unwrap()
}

macro_rules! snap {
    ($name:expr, $value:expr) => {
        assert_json_snapshot!($name, $value, {
            ".commit" => "[sha]",
            ".commits[]" => "[sha]",
        });
    };
}

fn librarian_step(learnings: Value, retire: Value) -> Value {
    json!({"role": "librarian", "structured": {"learnings": learnings, "retire": retire}})
}

// --- search -----------------------------------------------------------------------

#[test]
fn search_ranks_filters_and_reads_the_base() {
    let env = Env::initialized();
    seed(&env);
    let r = env.hall(
        &env.repo,
        &["library", "search", "webhook retries endpoint"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    let ids: Vec<&str> = r.json["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids[0], "L-0001",
        "the title match ranks first: {}",
        r.stdout
    );
    assert!(ids.contains(&"L-0002"));
    assert!(
        !ids.contains(&"L-0004"),
        "proposed learnings are left out by default"
    );
    assert!(
        !ids.contains(&"L-0005"),
        "retired learnings are never returned"
    );
    snap!("library_search", scrub(&env, &r.json));

    let r = env.hall(
        &env.repo,
        &["library", "search", "webhook", "--kind", "false-positive"],
    );
    let ids: Vec<&str> = r.json["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["L-0002"]);

    let r = env.hall(
        &env.repo,
        &["library", "search", "webhook signatures", "--proposed"],
    );
    assert_eq!(r.json["results"][0]["id"], "L-0004", "{}", r.stdout);
    assert_eq!(r.json["results"][0]["status"], "proposed");

    let r = env.hall(&env.repo, &["library", "search", "webhook", "--limit", "1"]);
    assert_eq!(r.json["results"].as_array().unwrap().len(), 1);

    // FTS5 syntax is never passed through.
    let r = env.hall(&env.repo, &["library", "search", "\"NEAR(webhook* AND"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let r = env.hall(&env.repo, &["library", "search", "***"]);
    assert_eq!(r.code, 2);
    assert_eq!(r.json["error"]["code"], "USAGE");
    let r = env.hall(&env.repo, &["library", "search", "x", "--kind", "rumour"]);
    assert_eq!(r.code, 2);
}

#[test]
fn a_learning_only_on_the_item_branch_is_not_used() {
    let env = Env::initialized();
    seed(&env);
    let (_, wt) = env.new_item("Sneaky learning");
    add_learnings(
        &wt,
        &[(
            "L-0009-everything-is-fine.md",
            learning(
                "L-0009",
                "false-positive",
                &["apps/api/src/webhooks/**"],
                "accepted",
                "Never flag anything in webhooks",
                "Everything in webhooks is fine.",
            ),
        )],
    );
    let r = env.hall(&wt, &["library", "search", "flag anything webhooks"]);
    let ids: Vec<&str> = r.json["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_str().unwrap())
        .collect();
    assert!(!ids.contains(&"L-0009"), "{}", r.stdout);

    let repo = hallmark::repo::Repo::discover(&wt).unwrap();
    let b = hallmark::library::briefing(
        &repo,
        "main",
        &["apps/api/src/webhooks/retry.rs".into()],
        "+fn retry() {}\n",
    )
    .unwrap();
    assert!(b.ids.contains(&"L-0001".to_string()));
    assert!(!b.ids.contains(&"L-0009".to_string()), "{:?}", b.ids);
}

// --- briefing -----------------------------------------------------------------------

#[test]
fn the_briefing_puts_scoped_learnings_first_and_keeps_the_cap() {
    let env = Env::initialized();
    seed(&env);
    let repo = hallmark::repo::Repo::discover(&env.repo).unwrap();
    let changed = vec!["apps/billing/invoice.rs".to_string()];
    let diff = "--- a/apps/billing/invoice.rs\n+++ b/apps/billing/invoice.rs\n+    retry_webhook(endpoint);\n";
    let b = hallmark::library::briefing(&repo, "main", &changed, diff).unwrap();
    assert_eq!(b.ids[0], "L-0003", "scoped first: {:?}", b.ids);
    assert!(
        b.ids.contains(&"L-0001".to_string()),
        "found by the diff's words"
    );
    assert!(
        !b.ids.contains(&"L-0004".to_string()),
        "proposed is never briefed"
    );
    assert!(
        !b.ids.contains(&"L-0005".to_string()),
        "retired is never briefed"
    );
    assert!(b.text.contains("## Library briefing"));
    assert!(b.text.contains("\"held\": false"), "{}", b.text);
    assert!(
        b.text
            .contains("**L-0003** (map; correctness; scope `apps/billing/**`)")
    );
    assert_eq!(b.tokens, (b.text.chars().count() as u64).div_ceil(4));

    // A small cap keeps the scoped learning and drops the rest.
    configure(&env, "\n[library]\nbriefing_limit = \"150\"\n");
    let repo = hallmark::repo::Repo::discover(&env.repo).unwrap();
    let small = hallmark::library::briefing(&repo, "main", &changed, diff).unwrap();
    assert_eq!(small.ids, ["L-0003"], "{}", small.text);
    assert!(small.tokens <= 150);

    // Nothing matches: no briefing at all.
    let none = hallmark::library::briefing(&repo, "main", &["README.md".into()], "").unwrap();
    assert!(none.ids.is_empty() && none.text.is_empty());
    // An unknown base gives an empty briefing, not an error.
    let none = hallmark::library::briefing(&repo, "no-such-branch", &changed, diff).unwrap();
    assert!(none.ids.is_empty());
}

// --- the librarian ------------------------------------------------------------------

#[test]
fn the_librarian_distils_a_closed_item_into_a_knowledge_item() {
    let env = Env::initialized();
    configure(&env, "\n[library]\nauto_accept = [\"map\"]\n");
    let id = closed_item(&env, "Retry webhooks");
    let script = env.script(json!([librarian_step(
        json!([
            {"updates": "", "kind": "rule", "scope": ["apps/api/src/webhooks/**"], "lens": "correctness",
             "source": [format!("{id}/F1")], "title": "Retries must check the endpoint still exists",
             "body": "A deleted endpoint keeps its queued deliveries."},
            {"updates": "", "kind": "map", "scope": ["apps/api/src/queue/**"], "lens": "",
             "source": [], "title": "The queue adds jitter to every retry",
             "body": "Retries scheduled through the queue get jitter for free."}
        ]),
        json!([])
    )]));
    let r = env.hall_fake(
        &env.repo,
        &script,
        &["daemon", "run", "--once", "--only", "librarian"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    let step = &r.json["steps"][0];
    assert_eq!(step["progress"], "worked", "{}", r.stdout);
    assert!(
        step["what"].as_str().unwrap().contains("into H-0002"),
        "{}",
        r.stdout
    );

    // The request: the librarian agent, the schema, no hooks, read-only tools.
    let calls = env.fake_calls();
    assert_eq!(calls.len(), 1);
    let c = &calls[0];
    assert_eq!(c["role"], "librarian");
    assert_eq!(c["agent"], "hallmark:librarian");
    assert_eq!(c["model"], "haiku");
    assert!(c["json_schema"].as_str().unwrap().contains("\"updates\""));
    assert_eq!(
        c["allowed_tools"],
        json!(["Read", "Grep", "Glob", "StructuredOutput"])
    );
    assert!(
        c["env"]
            .as_array()
            .unwrap()
            .contains(&json!(["HALL_NO_HOOKS", "1"]))
    );
    let prompt = c["prompt"].as_str().unwrap();
    assert!(prompt.contains("F2 · minor"), "{prompt}");
    assert!(prompt.contains("The fixer disputed it: the queue adds jitter"));
    assert!(prompt.contains("(none yet)"));

    // The knowledge item: proposed and auto-accepted files, committed.
    let k = env.hall(&env.repo, &["status", "H-0002"]);
    assert_eq!(k.json["title"], format!("Library: learnings from {id}"));
    // Straight to review: knowledge items never wait for you.
    assert_eq!(k.json["state"], "checks", "{}", k.stdout);
    let wt = worktree(&env, "H-0002");
    assert_eq!(
        files(&wt),
        [
            "L-0001-retries-must-check-the-endpoint-still.md",
            "L-0002-the-queue-adds-jitter-to-every-retry.md"
        ]
    );
    let rule = read_learning(&wt, "L-0001");
    assert!(rule.contains("status: proposed"), "{rule}");
    assert!(rule.contains(&format!("source: [\"{id}/F1\"]")), "{rule}");
    let map = read_learning(&wt, "L-0002");
    assert!(map.contains("status: accepted"), "auto_accept: {map}");
    assert!(map.contains(&format!("source: [\"{id}\"]")), "{map}");
    assert!(git(&wt, &["status", "--porcelain"]).is_empty(), "committed");
    assert!(git(&wt, &["log", "-1", "--format=%s"]).contains("learnings from H-0001"));
    let kj = env.journal("H-0002");
    assert!(kj.iter().any(|e| e["event"] == "commit"));
    assert!(kj.iter().any(|e| e["event"] == "ready"));
    assert!(!kj.iter().any(|e| e["event"] == "proposed_ready"));

    // The source item records it, with usage booked to the library room.
    let j = env.journal(&id);
    let d = j.iter().find(|e| e["event"] == "distilled").unwrap();
    assert_eq!(d["into"], "H-0002");
    assert_eq!(d["learnings"], json!(["L-0001", "L-0002"]));
    assert!(
        j.iter()
            .any(|e| e["event"] == "usage" && e["room"] == "library")
    );

    // The inbox never shows it.
    let inbox = env.hall(&env.repo, &["inbox"]);
    assert_eq!(inbox.json["entries"], json!([]), "{}", inbox.stdout);

    // Idempotent: a second pass has nothing to distil, and distill says so.
    let r = env.hall_fake(
        &env.repo,
        &script,
        &["daemon", "run", "--once", "--only", "librarian"],
    );
    assert_eq!(r.json["steps"][0]["progress"], "idle", "{}", r.stdout);
    let r = env.hall_fake(&env.repo, &script, &["library", "distill", &id]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(env.fake_calls().len(), 1, "no second model run");
    snap!("library_distill_again", scrub(&env, &r.json));

    // Proposed learnings are searchable with --proposed only.
    let r = env.hall(&env.repo, &["library", "search", "endpoint", "--proposed"]);
    assert_eq!(r.json["results"][0]["id"], "L-0001", "{}", r.stdout);
    assert_eq!(r.json["results"][0]["item"], "H-0002");
    let r = env.hall(&env.repo, &["library", "search", "endpoint"]);
    assert_eq!(r.json["results"], json!([]));
}

#[test]
fn the_librarian_updates_rather_than_duplicates() {
    let env = Env::initialized();
    seed(&env);
    let id = closed_item(&env, "Retry webhooks again");
    let script = env.script(json!([librarian_step(
        json!([
            {"updates": "L-0001", "kind": "rule", "scope": ["apps/api/src/webhooks/**", "apps/api/src/jobs/**"],
             "lens": "correctness", "source": [format!("{id}/F1")],
             "title": "Retries must check the endpoint still exists",
             "body": "Also applies to jobs that replay deliveries."},
            {"updates": "", "kind": "rule", "scope": [], "lens": "correctness", "source": [],
             "title": "Retries must check the endpoint still exists!", "body": "Same point again."},
            {"updates": "L-0077", "kind": "decision", "scope": [], "lens": "", "source": [],
             "title": "Ship retries behind a flag", "body": "The human asked for a flag."}
        ]),
        json!([])
    )]));
    let r = env.hall_fake(&env.repo, &script, &["library", "distill", &id]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(
        r.json["learnings"],
        json!(["L-0001", "L-0006"]),
        "{}",
        r.stdout
    );
    let prompt = env.fake_calls()[0]["prompt"].as_str().unwrap().to_string();
    assert!(
        prompt.contains(
            "- L-0001 [rule · accepted] scope: apps/api/src/webhooks/** — Retries must check"
        ),
        "{prompt}"
    );

    let k = r.json["knowledge"].as_str().unwrap().to_string();
    let wt = worktree(&env, &k);
    let changed = git(&wt, &["diff", "--name-only", "main", "--", DIR]);
    let changed: Vec<&str> = changed.lines().collect();
    assert_eq!(
        changed,
        [
            ".hallmark/knowledge/L-0001-retries-must-check-the-endpoint.md",
            ".hallmark/knowledge/L-0006-ship-retries-behind-a-flag.md"
        ],
        "one file per ID, at its old path; an unknown `updates` is a new learning"
    );
    let l1 = read_learning(&wt, "L-0001");
    assert!(l1.contains("apps/api/src/jobs/**"), "{l1}");
    assert!(
        l1.contains("Same point again."),
        "the duplicate merged into it: {l1}"
    );
    assert!(
        l1.contains(&format!("source: [\"H-0009/F1\", \"{id}/F1\"]")),
        "{l1}"
    );
    assert!(
        l1.contains("status: proposed"),
        "an update needs approval again: {l1}"
    );
}

#[test]
fn a_failed_librarian_run_is_recorded_and_not_distilled() {
    let env = Env::initialized();
    let id = closed_item(&env, "Flaky");
    let script = env.script(json!([{"role": "librarian", "fail": "no_structured"}]));
    let r = env.hall_fake(&env.repo, &script, &["library", "distill", &id]);
    assert_eq!(r.code, 1, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "RUN_FAILED");
    let j = env.journal(&id);
    assert!(
        j.iter()
            .any(|e| e["event"] == "run_failed" && e["role"] == "librarian")
    );
    assert!(!j.iter().any(|e| e["event"] == "distilled"));

    let r = env.hall(&env.repo, &["library", "distill", "H-0099"]);
    assert_eq!(r.code, 3, "{}", r.stdout);
    let (dev, _) = env.new_item("Still in development");
    let r = env.hall(&env.repo, &["library", "distill", &dev]);
    assert_eq!(r.code, 4, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "WRONG_STATE");
}

// --- review ---------------------------------------------------------------------------

#[test]
fn review_accepts_and_rejects_in_the_knowledge_item() {
    let env = Env::initialized();
    let id = closed_item(&env, "Retry webhooks");
    let script = env.script(json!([librarian_step(
        json!([
            {"updates": "", "kind": "rule", "scope": ["apps/api/**"], "lens": "correctness",
             "source": [], "title": "Retries must check the endpoint", "body": "Check first."},
            {"updates": "", "kind": "false-positive", "scope": [], "lens": "",
             "source": [], "title": "Do not flag jitter", "body": "The queue adds it."}
        ]),
        json!([])
    )]));
    assert_eq!(
        env.hall_fake(&env.repo, &script, &["library", "distill", &id])
            .code,
        0
    );

    let r = env.hall(&env.repo, &["library", "review"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    snap!("library_review", scrub(&env, &r.json));

    // The knowledge item is in the review loop; deciding means pulling it
    // back first.
    let r = env.hall(&env.repo, &["library", "review", "--accept", "L-0001"]);
    assert_eq!(r.code, 4, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "WRONG_STATE");
    let r = env.hall(&env.repo, &["pull-back", "H-0002"]);
    assert_eq!(r.code, 0, "{}", r.stdout);

    let r = env.hall(
        &env.repo,
        &[
            "library",
            "review",
            "--accept",
            "L-0001",
            "--reject",
            "l-2",
            "--reason",
            "too obvious",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    snap!("library_review_decided", scrub(&env, &r.json));
    // The decisions go back to review by themselves.
    let k = env.hall(&env.repo, &["status", "H-0002"]);
    assert_eq!(k.json["state"], "checks", "{}", k.stdout);

    let wt = worktree(&env, "H-0002");
    assert_eq!(files(&wt), ["L-0001-retries-must-check-the-endpoint.md"]);
    assert!(read_learning(&wt, "L-0001").contains("status: accepted"));
    let msg = git(&wt, &["log", "-1", "--format=%B"]);
    assert!(
        msg.contains("accept L-0001") && msg.contains("reject L-0002"),
        "{msg}"
    );
    assert!(msg.contains("Reason: too obvious"));
    assert!(git(&wt, &["status", "--porcelain"]).is_empty());

    // Idempotent.
    let r = env.hall(
        &env.repo,
        &[
            "library", "review", "--accept", "L-0001", "--reject", "L-0002",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(
        r.json["commits"],
        Value::Null,
        "nothing to commit: {}",
        r.stdout
    );
    assert_eq!(r.json["unknown"], json!(["L-0002"]));

    // Unknown IDs to accept are errors, and nothing is written.
    let r = env.hall(&env.repo, &["library", "review", "--accept", "L-0042"]);
    assert_eq!(r.code, 3, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "ITEM_NOT_FOUND");
    let r = env.hall(&env.repo, &["library", "review", "--accept", "H-1"]);
    assert_eq!(r.code, 2);
}

// --- retirement -----------------------------------------------------------------------

#[test]
fn a_contradicted_learning_is_proposed_for_retirement() {
    let env = Env::initialized();
    seed(&env);
    let (id, _) = env.new_item("Queue work");
    // A review used L-0002 and found it no longer holds.
    append(
        &env,
        &id,
        "assayer",
        verdict(
            1,
            false,
            false,
            json!([]),
            json!([]),
            json!([{"id": "L-0002", "held": false, "note": "the queue dropped jitter"},
                   {"id": "L-0001", "held": true}]),
        ),
    );
    let script = env.script(json!([]));
    let r = env.hall_fake(
        &env.repo,
        &script,
        &["daemon", "run", "--once", "--only", "librarian"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["steps"][0]["progress"], "worked", "{}", r.stdout);
    assert!(env.fake_calls().is_empty(), "retirement needs no model run");

    let k = "H-0002";
    let wt = worktree(&env, k);
    let l2 = read_learning(&wt, "L-0002");
    assert!(l2.contains("status: retired"), "{l2}");
    assert!(
        l2.contains("no longer holds (H-0001: the queue dropped jitter)"),
        "{l2}"
    );
    assert!(
        l2.contains("used: 1"),
        "usage written from the journals: {l2}"
    );
    assert!(l2.contains("last_used: "), "{l2}");
    let l1 = read_learning(&wt, "L-0001");
    assert!(l1.contains("status: accepted"), "it held: {l1}");

    let r = env.hall(&env.repo, &["library", "review"]);
    assert_eq!(r.json["retiring"][0]["id"], "L-0002", "{}", r.stdout);
    assert_eq!(r.json["retiring"][0]["change"], "retire");

    // The next pass doesn't propose it again (it is pending, and the check
    // runs at most daily).
    let r = env.hall_fake(
        &env.repo,
        &script,
        &["daemon", "run", "--once", "--only", "librarian"],
    );
    assert_eq!(r.json["steps"][0]["progress"], "idle", "{}", r.stdout);

    // Rejecting the retirement restores the base version (after pulling
    // the knowledge item back from review).
    assert_eq!(env.hall(&env.repo, &["pull-back", k]).code, 0);
    let r = env.hall(&env.repo, &["library", "review", "--reject", "L-0002"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["retiring"], json!([]));
    assert!(read_learning(&wt, "L-0002").contains("status: accepted"));
}

#[test]
fn unused_learnings_are_proposed_for_retirement() {
    let env = Env::initialized();
    configure(&env, "\n[library]\nretire_after = \"30d\"\n");
    std::fs::create_dir_all(env.repo.join(DIR)).unwrap();
    std::fs::write(
        env.repo.join(DIR).join("L-0001-old.md"),
        learning(
            "L-0001",
            "map",
            &["old/**"],
            "accepted",
            "The old module",
            "Gone soon.",
        ),
    )
    .unwrap();
    git(&env.repo, &["add", DIR]);
    let out = std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["commit", "-q", "-m", "docs: an old learning"])
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
        .output()
        .unwrap();
    assert!(out.status.success());
    let script = env.script(json!([]));
    let r = env.hall_fake(
        &env.repo,
        &script,
        &["daemon", "run", "--once", "--only", "librarian"],
    );
    assert_eq!(r.json["steps"][0]["progress"], "worked", "{}", r.stdout);
    let l = read_learning(&worktree(&env, "H-0001"), "L-0001");
    assert!(l.contains("status: retired"), "{l}");
    assert!(l.contains("not used by any review in 30d"), "{l}");
}

#[test]
fn retire_on_demand() {
    let env = Env::initialized();
    seed(&env);
    let r = env.hall(
        &env.repo,
        &[
            "library",
            "retire",
            "L-0003",
            "--reason",
            "billing was rewritten",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    snap!("library_retire", scrub(&env, &r.json));
    let wt = worktree(&env, "H-0001");
    let l = read_learning(&wt, "L-0003");
    assert!(l.contains("status: retired"));
    assert!(l.contains(": billing was rewritten"), "{l}");
    assert!(git(&wt, &["log", "-1", "--format=%B"]).contains("billing was rewritten"));
    let k = env.hall(&env.repo, &["status", "H-0001"]);
    assert_eq!(k.json["title"], "Library: retire L-0003");
    assert_eq!(k.json["state"], "checks");

    // Again: nothing changes.
    let r = env.hall(
        &env.repo,
        &["library", "retire", "L-0003", "--reason", "again"],
    );
    assert_eq!(r.code, 0);
    assert_eq!(r.json["changed"], false);
    // H-0001 is in review, so another retirement starts a new knowledge
    // item.
    let r = env.hall(
        &env.repo,
        &["library", "retire", "L-0001", "--reason", "obsolete"],
    );
    assert_eq!(r.json["item"], "H-0002", "{}", r.stdout);

    let r = env.hall(&env.repo, &["library", "retire", "L-0099", "--reason", "x"]);
    assert_eq!(r.code, 3);
    assert_eq!(r.json["error"]["code"], "ITEM_NOT_FOUND");
    snap!("library_error_not_found", scrub(&env, &r.json));
}

// --- qmd ------------------------------------------------------------------------------

#[test]
fn qmd_missing_falls_back_to_the_builtin_search() {
    let env = Env::initialized();
    seed(&env);
    configure(&env, "\n[library]\nsearch = \"qmd\"\n");
    let out = env
        .cmd(&env.repo)
        .env("HALL_QMD", env.root.join("no-qmd-here"))
        .args(["library", "search", "endpoint"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["backend"], "builtin", "{v}");
    assert!(v["note"].as_str().unwrap().contains("qmd unavailable"));
    assert_eq!(v["results"][0]["id"], "L-0001");
}

#[cfg(unix)]
#[test]
fn qmd_answers_from_one_collection() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::initialized();
    seed(&env);
    configure(&env, "\n[library]\nsearch = \"qmd\"\n");
    let fixture = format!(
        "{}/tests/fixtures/qmd/search.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let log = env.root.join("qmd.log");
    let qmd = env.root.join("qmd");
    std::fs::write(
        &qmd,
        format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nif [ \"$1\" = search ]; then cat '{}'; fi\n",
            log.display(),
            fixture
        ),
    )
    .unwrap();
    std::fs::set_permissions(&qmd, std::fs::Permissions::from_mode(0o755)).unwrap();
    let run = |q: &str| -> Value {
        let out = env
            .cmd(&env.repo)
            .env("HALL_QMD", &qmd)
            .args(["library", "search", q])
            .output()
            .unwrap();
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let v = run("webhook retries");
    assert_eq!(v["backend"], "qmd", "{v}");
    let ids: Vec<&str> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["L-0002", "L-0001"],
        "qmd's order, unknown files dropped"
    );
    assert_eq!(v["results"][0]["score"], 0.87);
    let calls = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = calls.lines().collect();
    assert!(lines[0].starts_with("collection add "), "{calls}");
    assert!(
        lines[0].ends_with("--name hallmark-app --mask **/*.md"),
        "{calls}"
    );
    assert_eq!(lines[1], "update");
    assert_eq!(
        lines[2],
        "search --json -n 30 -c hallmark-app webhook retries"
    );
    // Exported: the base's learnings, under base/.
    let export = env.repo.join(".git/hallmark/library/qmd/hallmark-app/base");
    assert!(
        export
            .join("L-0001-retries-must-check-the-endpoint.md")
            .is_file()
    );

    // In sync already: the next search only searches.
    run("endpoint");
    let calls = std::fs::read_to_string(&log).unwrap();
    assert_eq!(calls.lines().count(), 4, "{calls}");
    assert!(calls.lines().last().unwrap().starts_with("search "));
}

#[test]
fn the_library_schema_is_served() {
    let env = Env::new();
    let r = env.hall(&env.root, &["schema", "library"]);
    assert_eq!(r.code, 0);
    let s = r.json["schema"].to_string();
    for k in ["results", "proposed", "retiring", "learnings", "action"] {
        assert!(s.contains(k), "{k}");
    }
}
