//! Introspection: what an item is doing now (`hall status <id>`), one inbox
//! entry explained (`hall inbox <n>`), `hall watch`, and the hooks telling
//! Claude what waits for the human.

mod common;

use std::path::Path;

use common::{Env, commit_file, fixture, p};
use serde_json::{Value, json};

fn ready(env: &Env, title: &str) -> String {
    let (id, wt) = env.new_item(title);
    commit_file(&wt, "work.txt", title);
    assert_eq!(env.hall(&wt, &["ready"]).code, 0);
    id
}

fn now(env: &Env, id: &str) -> Value {
    let r = env.hall(&env.repo, &["status", id]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    r.json["now"].clone()
}

fn live(env: &Env, line: &str) {
    use std::io::Write;
    let path = env.repo.join(".git/hallmark/live.log");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(
        f,
        "{} {line}",
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
    )
    .unwrap();
}

#[test]
fn now_says_where_an_item_waits_and_what_runs() {
    let env = Env::initialized();
    let first = ready(&env, "First");
    let second = ready(&env, "Second");

    // Queued, behind the first, and nothing runs them: say so.
    let n = now(&env, &second);
    assert_eq!(n["status"], "queued", "{n}");
    assert!(
        n["doing"]
            .as_str()
            .unwrap()
            .contains("the daemon isn't running"),
        "{n}"
    );
    assert!(
        n["detail"][0]
            .as_str()
            .unwrap()
            .starts_with(&format!("ahead: {first} (checks")),
        "{n}"
    );
    let commands: Vec<&str> = n["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["command"].as_str().unwrap())
        .collect();
    assert!(commands.contains(&"hall daemon start"), "{commands:?}");
    assert!(n["actions"][0]["does"].is_string());

    // The assayer takes the first one: running, with the gate it is on.
    let r = env.hall(&env.repo, &["next", "--role", "assayer", "--holder", "t"]);
    assert_eq!(r.json["work"]["id"], first.as_str());
    let n = now(&env, &first);
    assert_eq!(n["status"], "running", "{n}");
    assert!(n["doing"].as_str().unwrap().contains("preparing"), "{n}");
    live(
        &env,
        &format!("{first} checks 2/3: cargo test --all-targets"),
    );
    let n = now(&env, &first);
    assert!(
        n["doing"]
            .as_str()
            .unwrap()
            .contains("running gate 2/3: cargo test --all-targets"),
        "{n}"
    );
    // The second is still queued behind it, now marked as running.
    let n = now(&env, &second);
    assert!(
        n["detail"][0].as_str().unwrap().contains("running for"),
        "{n}"
    );
}

#[test]
fn an_inbox_entry_explains_itself() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Usage page");
    env.hall(
        &wt,
        &[
            "ask",
            "billed usage or raw events?",
            "--options",
            "billed,raw",
            "--default",
            "billed",
            "--wait",
            "4h",
        ],
    );
    env.hall(
        &wt,
        &[
            "propose-ready",
            "--ask",
            "--summary",
            "usage page with tests",
        ],
    );

    let list = env.hall(&env.repo, &["inbox"]);
    assert_eq!(list.json["next"][0], "hall inbox 1");

    // By number, by question ID and by item ID.
    let q = env.hall(&env.repo, &["inbox", "1"]);
    assert_eq!(q.code, 0, "{}", q.stdout);
    assert_eq!(q.json["kind"], "question");
    assert_eq!(q.json["number"], 1);
    assert!(q.json["why"].as_str().unwrap().contains("resumes"));
    let evidence: Vec<&str> = q.json["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap())
        .collect();
    assert!(evidence.contains(&"[a] billed"), "{evidence:?}");
    assert!(
        evidence.iter().any(|e| e.starts_with("default: billed")),
        "{evidence:?}"
    );
    assert_eq!(q.json["actions"][0]["command"], "hall answer Q-1 a");
    assert_eq!(q.json["next"][0], "hall answer Q-1 a");
    assert_eq!(
        env.hall(&env.repo, &["inbox", "Q-1"]).json["kind"],
        "question"
    );

    let ready = env.hall(&env.repo, &["inbox", "2"]);
    assert_eq!(ready.json["kind"], "ready", "{}", ready.stdout);
    assert_eq!(
        ready.json["actions"][0]["command"],
        format!("hall ready {id}")
    );
    assert_eq!(
        ready.json["actions"][0]["does"],
        "send it to checks and review"
    );
    assert!(
        ready.json["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e == "summary: usage page with tests"),
        "{}",
        ready.stdout
    );

    let missing = env.hall(&env.repo, &["inbox", "9"]);
    assert_eq!(missing.code, 3);
    assert_eq!(missing.json["error"]["code"], "ITEM_NOT_FOUND");
}

/// `hall watch` prints progress lines, then the reply as the last line.
fn watch(env: &Env, dir: &Path, id: &str) -> (Vec<Value>, Value) {
    let out = env
        .cmd(dir)
        .env("HALL_WATCH_INTERVAL", "100ms")
        .args(["--json", "watch", id, "--timeout", "2s"])
        .output()
        .unwrap();
    let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let (last, progress) = lines.split_last().unwrap();
    (progress.to_vec(), last.clone())
}

#[test]
fn watch_follows_until_the_item_leaves_the_loop_or_needs_you() {
    let env = Env::initialized();
    let (dev, _) = env.new_item("In development");
    let (progress, reply) = watch(&env, &env.repo, &dev);
    assert_eq!(progress.len(), 1);
    assert_eq!(progress[0]["state"], "development");
    assert_eq!(reply["stopped"], "left_loop");

    // Queued with nothing to run it: it times out, having said so once.
    let queued = ready(&env, "Queued");
    let (progress, reply) = watch(&env, &env.repo, &queued);
    assert_eq!(progress.len(), 1, "{progress:?}");
    assert_eq!(progress[0]["status"], "queued");
    assert_eq!(reply["stopped"], "timeout");
}

fn hook_input(name: &str, cwd: &Path, session: &str) -> Value {
    let mut v: Value = serde_json::from_str(&fixture(&format!("hooks/{name}.json"))).unwrap();
    v["cwd"] = json!(p(cwd));
    v["session_id"] = json!(session);
    v
}

fn context(r: &common::Reply) -> String {
    r.json["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

#[test]
fn claude_is_told_what_waits_once() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Usage page");
    env.hall(&wt, &["propose-ready", "--ask", "--summary", "done"]);

    let start = env.hook(
        "session-start",
        &hook_input("session_start", &env.repo, "w1"),
    );
    let text = context(&start);
    assert!(text.contains("waiting for the human"), "{text}");
    assert!(
        text.contains(&format!("`hall ready {id}` (send it to checks and review)")),
        "{text}"
    );
    assert!(text.contains("Details: `hall inbox 1`"), "{text}");

    // Already told: the next prompt doesn't repeat it…
    let prompt = env.hook(
        "user-prompt-submit",
        &hook_input("user_prompt_submit", &env.repo, "w1"),
    );
    assert!(
        !context(&prompt).contains("waiting for the human"),
        "{}",
        prompt.stdout
    );

    // …but a new entry is announced on the next prompt.
    env.hall(&wt, &["ask", "billed or raw?", "--options", "billed,raw"]);
    let prompt = env.hook(
        "user-prompt-submit",
        &hook_input("user_prompt_submit", &env.repo, "w1"),
    );
    let text = context(&prompt);
    assert!(text.contains("new entries are waiting"), "{text}");
    assert!(text.contains("billed or raw?"), "{text}");
    assert!(!text.contains("looks done"), "only the new one: {text}");
}

#[test]
fn an_entry_that_leaves_and_returns_is_announced_again() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Usage page");
    env.hall(&wt, &["propose-ready", "--ask", "--summary", "done"]);
    let start = env.hook(
        "session-start",
        &hook_input("session_start", &env.repo, "w2"),
    );
    assert!(context(&start).contains("waiting for the human"));

    // It leaves the inbox, and the session sees an empty inbox...
    env.hall(&env.repo, &["pull-back", &id]);
    let prompt = env.hook(
        "user-prompt-submit",
        &hook_input("user_prompt_submit", &env.repo, "w2"),
    );
    assert!(!context(&prompt).contains("waiting for the human"));

    // ...then comes back: it is a new occurrence.
    hallmark::daemon::nap(std::time::Duration::from_millis(20));
    env.hall(&wt, &["propose-ready", "--ask", "--summary", "done again"]);
    let prompt = env.hook(
        "user-prompt-submit",
        &hook_input("user_prompt_submit", &env.repo, "w2"),
    );
    let text = context(&prompt);
    assert!(text.contains("new entries are waiting"), "{text}");
}

#[test]
fn the_inbox_clears_and_entries_return_when_they_change() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Usage page");
    env.hall(&wt, &["ask", "billed or raw?", "--options", "billed,raw"]);
    env.hall(&wt, &["propose-ready", "--ask", "--summary", "first pass"]);
    let count = |all: bool| {
        let args: &[&str] = if all { &["inbox", "--all"] } else { &["inbox"] };
        env.hall(&env.repo, args).json["entries"]
            .as_array()
            .unwrap()
            .len()
    };
    assert_eq!(count(false), 2);

    // One entry, from its explanation: the last action dismisses it.
    let detail = env.hall(&env.repo, &["inbox", "Q-1"]);
    let actions = detail.json["actions"].as_array().unwrap();
    let dismiss = actions.last().unwrap()["command"].as_str().unwrap();
    let key = dismiss
        .strip_prefix("hall inbox ")
        .and_then(|s| s.strip_suffix(" --dismiss"))
        .unwrap()
        .to_string();
    assert!(key.starts_with("question:Q-1:"), "{dismiss}");
    let r = env.hall(&env.repo, &["inbox", &key, "--dismiss"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["dismissed"][0]["qid"], "Q-1");
    assert_eq!(r.json["remaining"], 1);
    assert_eq!(count(false), 1);
    assert_eq!(count(true), 2, "--all still shows it");
    // Nothing was answered.
    let q = env.hall(&env.repo, &["inbox", "--all"]);
    assert_eq!(q.json["entries"][0]["kind"], "question");

    // Everything at once; repeating it changes nothing.
    let r = env.hall(&env.repo, &["inbox", "--clear"]);
    assert_eq!(r.json["dismissed"].as_array().unwrap().len(), 1);
    assert_eq!(r.json["remaining"], 0);
    assert_eq!(count(false), 0);
    let again = env.hall(&env.repo, &["inbox", "--clear"]);
    assert_eq!(again.json["dismissed"].as_array().unwrap().len(), 0);

    // The item changes (a new "looks done"): it comes back.
    env.hall(&wt, &["pull-back"]);
    env.hall(&wt, &["propose-ready", "--ask", "--summary", "second pass"]);
    let inbox = env.hall(&env.repo, &["inbox"]);
    assert_eq!(inbox.json["entries"][0]["item"], id.as_str());
    assert_eq!(inbox.json["entries"][0]["kind"], "ready");

    // --dismiss needs an entry; --clear takes none.
    assert_eq!(env.hall(&env.repo, &["inbox", "--dismiss"]).code, 2);
    assert_eq!(env.hall(&env.repo, &["inbox", "1", "--clear"]).code, 2);
}
