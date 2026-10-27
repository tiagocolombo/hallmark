//! Workstream A: the daemon. One pass over every registered repo, pauses,
//! the watchman, and start/stop/status of the background loop. No test
//! spends model usage: the fake runner is set everywhere, and no worker
//! here has model work to do.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use common::{Env, Reply, commit_file, git, p};
use insta::assert_json_snapshot;
use serde_json::{Value, json};

/// Replaces the temp directory with `[root]` everywhere.
fn scrub(env: &Env, v: &Value) -> Value {
    let s = v.to_string().replace(&p(&env.root), "[root]");
    serde_json::from_str(&s).unwrap()
}

/// A second repository under the same home, set up with `hall init`.
fn second_repo(env: &Env, name: &str) -> PathBuf {
    let dir = env.root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("README.md"), format!("# {name}\n")).unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    let r = env.hall(&dir, &["init"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "Adopt Hallmark"]);
    dir
}

fn state_path(env: &Env) -> PathBuf {
    env.home.join("daemon").join("state.json")
}

fn write_state(env: &Env, v: Value) {
    std::fs::create_dir_all(env.home.join("daemon")).unwrap();
    std::fs::write(state_path(env), v.to_string()).unwrap();
}

fn read_state(env: &Env) -> Value {
    serde_json::from_str(&std::fs::read_to_string(state_path(env)).unwrap()).unwrap()
}

fn hall_root(repo: &Path) -> String {
    repo.join(".git")
        .join("hallmark")
        .to_string_lossy()
        .to_string()
}

fn events(journal: &[Value]) -> Vec<String> {
    journal
        .iter()
        .map(|e| e["event"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn run_once_covers_every_registered_repo() {
    let env = Env::initialized();
    let api = second_repo(&env, "api");
    // A registered repo that has since been deleted is skipped, not fatal.
    let reg = env.home.join("repos.json");
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&reg).unwrap()).unwrap();
    v["repos"].as_array_mut().unwrap().push(json!({
        "name": "gone",
        "path": p(&env.root.join("gone")),
    }));
    std::fs::write(&reg, v.to_string()).unwrap();

    let script = env.script(json!([]));
    // Outside any repository, a pass covers the whole registry.
    let r = env.hall_fake(&env.root, &script, &["daemon", "run", "--once"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let steps = r.json["steps"].as_array().unwrap();
    assert_eq!(
        steps.len(),
        2 * hallmark::worker::ORDER.len(),
        "{}",
        r.stdout
    );
    for name in ["app", "api"] {
        assert_eq!(
            steps.iter().filter(|s| s["repo"] == name).count(),
            hallmark::worker::ORDER.len(),
            "{name}"
        );
    }
    let repos = r.json["repos"].as_array().unwrap();
    assert_eq!(repos.len(), 3);
    assert_eq!(repos[0]["status"], "ok");
    assert_eq!(repos[0]["last_pass"]["result"], "idle");
    assert_eq!(repos[2]["name"], "gone");
    assert_eq!(repos[2]["status"], "missing");
    // `run --once` is not the daemon: it never marks it wanted.
    assert!(
        !state_path(&env).exists() || read_state(&env)["wanted"] == false,
        "run --once must not ask for a daemon"
    );

    // --repo picks one, by registered name or by path.
    let r = env.hall_fake(
        &env.root,
        &script,
        &["daemon", "run", "--once", "--repo", "api"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    let steps = r.json["steps"].as_array().unwrap();
    assert!(steps.iter().all(|s| s["repo"] == "api"), "{}", r.stdout);
    let r = env.hall_fake(
        &env.root,
        &script,
        &[
            "daemon",
            "run",
            "--once",
            "--repo",
            &p(&api),
            "--only",
            "watchman",
        ],
    );
    assert_eq!(r.json["steps"].as_array().unwrap().len(), 1);
    // Inside a repository, only that one.
    let r = env.hall_fake(&env.repo, &script, &["daemon", "run", "--once"]);
    let steps = r.json["steps"].as_array().unwrap();
    assert!(steps.iter().all(|s| s["repo"] == "app"), "{}", r.stdout);

    // A worker that doesn't exist is a usage error.
    let r = env.hall_fake(
        &env.repo,
        &script,
        &["daemon", "run", "--once", "--only", "janitor"],
    );
    assert_eq!(r.code, 2, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "USAGE");
    // An unregistered, uninitialized repo named with --repo is an error.
    let bare = env.root.join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "-b", "main"]);
    let r = env.hall_fake(
        &env.root,
        &script,
        &["daemon", "run", "--once", "--repo", &p(&bare)],
    );
    assert_eq!(r.code, 3, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "NOT_INITIALIZED");
}

#[test]
fn the_watchman_flags_a_silent_item_once() {
    let env = Env::initialized();
    // The main checkout's settings are the ones that count.
    let cfg = env.repo.join(".hallmark.toml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(&cfg, format!("stuck_after = \"10m\"\n{text}")).unwrap();
    let (id, wt) = env.new_item("Retry webhooks");
    commit_file(&wt, "retry.rs", "fn retry() {}");
    assert_eq!(env.hall(&wt, &["ready"]).code, 0);
    let script = env.script(json!([]));
    let watchman = ["daemon", "run", "--once", "--only", "watchman"];
    // The workers' clock is pinned, counted from the item's last event, so
    // a slow machine can't make "too soon" late.
    let quiet_since: DateTime<Utc> = env.journal(&id).last().unwrap()["t"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let after = |d: chrono::Duration| quiet_since + d;

    // Too soon: nothing to flag.
    let r = env.hall_fake_at(
        &env.repo,
        &script,
        after(chrono::Duration::seconds(599)),
        &watchman,
    );
    assert_eq!(r.json["steps"][0]["progress"], "idle", "{}", r.stdout);

    let r = env.hall_fake_at(
        &env.repo,
        &script,
        after(chrono::Duration::minutes(10)),
        &watchman,
    );
    assert_eq!(r.json["steps"][0]["progress"], "worked", "{}", r.stdout);
    assert!(
        r.json["steps"][0]["what"].as_str().unwrap().contains(&id),
        "{}",
        r.stdout
    );
    let journal = env.journal(&id);
    let last = journal.last().unwrap();
    assert_eq!(last["event"], "stuck");
    assert_eq!(last["actor"], "watchman");
    assert_eq!(last["state"], "checks");

    // Flagged once: the next patrol leaves it alone.
    let r = env.hall_fake_at(
        &env.repo,
        &script,
        after(chrono::Duration::minutes(30)),
        &watchman,
    );
    assert_eq!(r.json["steps"][0]["progress"], "idle", "{}", r.stdout);
    assert_eq!(
        events(&env.journal(&id))
            .iter()
            .filter(|e| *e == "stuck")
            .count(),
        1
    );

    // A notice, not an escalation: your office stays empty.
    let inbox = env.hall(&env.repo, &["inbox"]);
    assert_eq!(
        inbox.json["entries"],
        serde_json::json!([]),
        "{}",
        inbox.stdout
    );
    let inbox = env.hall(&env.repo, &["inbox", "--notices"]);
    let entries = inbox.json["entries"].as_array().unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e["kind"] == "stuck" && e["item"] == id.as_str()),
        "{}",
        inbox.stdout
    );
    // The live log says so.
    let live = std::fs::read_to_string(env.repo.join(".git/hallmark/live.log")).unwrap();
    assert!(live.contains("stuck in checks"), "{live}");
}

#[test]
fn a_pause_skips_the_model_workers_until_it_ends() {
    let env = Env::initialized();
    let script = env.script(json!([]));
    let now = Utc::now();
    // A pause from an earlier daemon (it survives restarts in state.json).
    write_state(
        &env,
        json!({
            "pid": 0, "started_at": null, "heartbeat": null, "wanted": false,
            "paused": {
                "reason": "rate limited",
                "until": (now + chrono::Duration::minutes(10)).to_rfc3339(),
                "since": now.to_rfc3339(),
                "repo": "app",
                "journals": [hall_root(&env.repo)],
            }
        }),
    );
    let r = env.hall_fake(&env.repo, &script, &["daemon", "run", "--once"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let steps = r.json["steps"].as_array().unwrap();
    assert_eq!(steps.len(), hallmark::worker::ORDER.len());
    for s in steps {
        let model =
            ["resume", "assayer", "fixer", "librarian"].contains(&s["worker"].as_str().unwrap());
        assert_eq!(s["skipped"].is_string(), model, "{s}");
    }
    assert!(
        r.json["next"]
            .as_array()
            .unwrap()
            .contains(&json!("hall daemon status"))
    );
    // Naming a worker drives it by hand, pause or not.
    let r = env.hall_fake(
        &env.repo,
        &script,
        &["daemon", "run", "--once", "--only", "assayer"],
    );
    assert!(r.json["steps"][0]["skipped"].is_null(), "{}", r.stdout);
    // status shows it.
    let st = env.hall(&env.repo, &["daemon", "status"]);
    assert_eq!(st.json["state"]["paused"]["reason"], "rate limited");

    // Once its time has passed, the next pass ends it and says so in the
    // journal that recorded it.
    let mut s = read_state(&env);
    s["paused"]["until"] = json!((now - chrono::Duration::seconds(1)).to_rfc3339());
    write_state(&env, s);
    let r = env.hall_fake(&env.repo, &script, &["daemon", "run", "--once"]);
    let steps = r.json["steps"].as_array().unwrap();
    assert!(steps.iter().all(|s| s["skipped"].is_null()), "{}", r.stdout);
    assert!(read_state(&env).get("paused").is_none());
    let repo_journal = env.repo_journal();
    let last = repo_journal.last().unwrap();
    assert_eq!(last["event"], "unpaused");
    assert_eq!(last["actor"], "daemon");
}

#[test]
fn the_inbox_flags_a_daemon_that_died_until_you_stop_it() {
    let env = Env::initialized();
    let old = Utc::now() - chrono::Duration::minutes(10);
    write_state(
        &env,
        json!({
            "pid": 999_999, "started_at": old.to_rfc3339(),
            "heartbeat": old.to_rfc3339(), "wanted": true,
        }),
    );
    let flagged = |env: &Env| {
        env.hall(&env.repo, &["inbox"]).json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "daemon_stopped")
    };
    assert!(flagged(&env));
    let st = env.hall(&env.repo, &["daemon", "status"]);
    assert_eq!(st.json["running"], false, "{}", st.stdout);
    assert_eq!(st.json["next"][0], "hall daemon start");
    // `stop` says you don't want it, which settles the flag.
    let r = env.hall(&env.repo, &["daemon", "stop"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["changed"], true);
    assert!(!flagged(&env));
    let r = env.hall(&env.repo, &["daemon", "stop"]);
    assert_eq!(r.json["changed"], false, "stop is idempotent");
}

/// Always stops the background daemon, even when an assertion fails.
#[cfg(unix)]
struct StopOnDrop<'a>(&'a Env);

#[cfg(unix)]
impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        let _ = self.0.cmd(&self.0.root).args(["daemon", "stop"]).output();
    }
}

#[cfg(unix)]
fn bg(env: &Env, script: &Path, args: &[&str]) -> Reply {
    let out = env
        .cmd(&env.root)
        .env("HALL_RUNNER", format!("fake:{}", script.display()))
        .env("HALL_DAEMON_POLL", "200ms")
        .env("HALL_DAEMON_SERVICE_DRY", "1")
        .args(args)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    Reply {
        json: serde_json::from_str(stdout.trim()).unwrap_or(Value::Null),
        code: out.status.code().unwrap_or(-1),
        stdout,
    }
}

// Unix only: it runs a real background process, and process lifetime and
// file locks on Windows CI are too slow to make this test reliable.
#[cfg(unix)]
#[test]
fn start_status_stop_in_the_background() {
    let env = Env::initialized();
    let script = env.script(json!([]));
    let _guard = StopOnDrop(&env);

    let r = bg(&env, &script, &["daemon", "status"]);
    assert_eq!(r.json["running"], false, "{}", r.stdout);

    let r = bg(&env, &script, &["daemon", "start", "--at-login"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["running"], true);
    assert_eq!(r.json["changed"], true);
    assert_eq!(r.json["state"]["wanted"], true);
    assert!(r.json["state"]["pid"].as_u64().unwrap() > 0);
    assert!(r.json["state"]["heartbeat"].is_string());
    let svc = &r.json["service"];
    assert_eq!(svc["dry_run"], true, "{}", r.stdout);
    assert_eq!(svc["action"], "install");
    assert_eq!(svc["changed"], true);
    assert_eq!(svc["label"], "dev.hallmark.daemon");
    assert_eq!(svc["args"], json!(["daemon", "run"]));

    // Idempotent.
    let again = bg(&env, &script, &["daemon", "start", "--at-login"]);
    assert_eq!(again.json["changed"], false, "{}", again.stdout);
    assert_eq!(again.json["state"]["pid"], r.json["state"]["pid"]);
    // One instance only.
    let second = bg(&env, &script, &["daemon", "run"]);
    assert_eq!(second.code, 7, "{}", second.stdout);
    assert_eq!(second.json["error"]["code"], "CLAIMED");

    // It runs passes over the registered repo.
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        let s = bg(&env, &script, &["daemon", "status"]);
        if s.json["repos"][0]["last_pass"].is_object() || Instant::now() > deadline {
            break s;
        }
        hallmark::daemon::nap(Duration::from_millis(100));
    };
    assert_eq!(status.json["running"], true, "{}", status.stdout);
    assert_eq!(status.json["at_login"], true);
    assert_eq!(status.json["repos"][0]["name"], "app");
    assert_eq!(
        status.json["repos"][0]["last_pass"]["result"], "idle",
        "{}",
        status.stdout
    );
    let inbox = env.hall(&env.repo, &["inbox"]);
    assert!(!inbox.stdout.contains("daemon_stopped"), "{}", inbox.stdout);

    let r = bg(&env, &script, &["daemon", "stop", "--at-login"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["running"], false, "{}", r.stdout);
    assert_eq!(r.json["changed"], true);
    assert_eq!(r.json["service"]["action"], "uninstall");
    assert_eq!(r.json["service"]["changed"], true);
    assert_eq!(r.json["state"]["wanted"], false);
    assert!(r.json["state"]["heartbeat"].is_null(), "{}", r.stdout);
    let again = bg(&env, &script, &["daemon", "stop", "--at-login"]);
    assert_eq!(again.json["changed"], false, "{}", again.stdout);

    let st = bg(&env, &script, &["daemon", "status"]);
    assert_json_snapshot!("daemon_status_stopped", scrub(&env, &st.json), {
        ".state.pid" => "[pid]",
        ".state.started_at" => "[time]",
        ".state.stopped_at" => "[time]",
        ".state.passes" => "[passes]",
        ".repos[].last_pass.at" => "[time]",
    });
    let inbox = env.hall(&env.repo, &["inbox"]);
    assert!(!inbox.stdout.contains("daemon_stopped"));
    let log = std::fs::read_to_string(env.home.join("daemon").join("daemon.log")).unwrap();
    assert!(log.contains("daemon started"), "{log}");
    assert!(log.contains("daemon stopped"), "{log}");
}

/// Replaces the `gates = []` line `hall init` wrote.
#[cfg(unix)]
fn set_gates(repo: &Path, gate: &str) {
    let cfg = repo.join(".hallmark.toml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    let text: Vec<String> = text
        .lines()
        .map(|l| {
            if l.starts_with("gates") {
                format!("gates = [\"{gate}\"]")
            } else {
                l.to_string()
            }
        })
        .collect();
    std::fs::write(&cfg, text.join("\n")).unwrap();
    git(repo, &["commit", "-qam", "gates"]);
}

// Unix only: a real background daemon and a gate that sleeps.
#[cfg(unix)]
#[test]
fn a_slow_repo_does_not_hold_the_others() {
    let env = Env::initialized();
    let api = second_repo(&env, "api");
    set_gates(&env.repo, "sleep 8");
    set_gates(&api, "git --version");
    let item = |repo: &Path, title: &str| {
        let r = env.hall(repo, &["new", "--title", title]);
        assert_eq!(r.code, 0, "{}", r.stdout);
        let wt = PathBuf::from(r.json["worktree"].as_str().unwrap());
        commit_file(&wt, "work.txt", title);
        assert_eq!(env.hall(&wt, &["ready"]).code, 0);
        r.json["id"].as_str().unwrap().to_string()
    };
    let slow = item(&env.repo, "Slow gate");
    let fast = item(&api, "Fast gate");
    let verdict = json!({"summary": "Fine.", "findings": [], "prior": [], "learnings": []});
    let script = env.script(json!([{"role": "reviewer", "structured": verdict}]));
    let _guard = StopOnDrop(&env);
    let r = bg(&env, &script, &["daemon", "start"]);
    assert_eq!(r.code, 0, "{}", r.stdout);

    // While app's gate sleeps, api's item goes through its checks and, in a
    // later pass of its own, its review. The old daemon held api's second
    // pass until app's pass ended.
    let state = |repo: &Path, id: &str| {
        env.hall(repo, &["status", id]).json["state"]
            .as_str()
            .unwrap_or("")
            .to_string()
    };
    let reviewed = |repo: &Path, id: &str| {
        let journal = repo
            .join(".git/hallmark/items")
            .join(id)
            .join("events.jsonl");
        std::fs::read_to_string(journal)
            .unwrap_or_default()
            .contains("\"event\":\"verdict\"")
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
    while !reviewed(&api, &fast) && std::time::Instant::now() < deadline {
        hallmark::daemon::nap(std::time::Duration::from_millis(100));
    }
    assert!(reviewed(&api, &fast), "api waited for app's slow gate");
    assert_eq!(
        state(&env.repo, &slow),
        "checks",
        "app's gate is still running"
    );
}

#[test]
fn resume_lifts_a_pause_before_its_time() {
    let env = Env::initialized();
    let now = Utc::now();
    write_state(
        &env,
        json!({
            "pid": 0, "started_at": null, "heartbeat": null, "wanted": false,
            "paused": {
                "reason": "H-0007 review: Claude's plan limit",
                "until": (now + chrono::Duration::minutes(30)).to_rfc3339(),
                "since": now.to_rfc3339(),
                "repo": "app",
                "journals": [hall_root(&env.repo)],
            }
        }),
    );
    let r = env.hall(&env.repo, &["daemon", "resume"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["changed"], true);
    assert!(r.json["note"].as_str().unwrap().contains("plan limit"));
    assert!(read_state(&env)["paused"].is_null(), "the pause is gone");
    assert_eq!(events(&env.repo_journal()).last().unwrap(), "unpaused");
    // Nothing left to lift.
    let again = env.hall(&env.repo, &["daemon", "resume"]);
    assert_eq!(again.json["changed"], false);
    assert_eq!(again.json["note"], "not paused");
}

/// A pause (Claude's usage or session limit, the daily budget) is in the
/// inbox with the way to retry now, until it ends or `hall daemon resume`.
#[test]
fn a_pause_is_in_the_inbox_until_resumed() {
    let env = Env::initialized();
    let now = Utc::now();
    write_state(
        &env,
        json!({
            "pid": std::process::id(), "started_at": now.to_rfc3339(),
            "heartbeat": now.to_rfc3339(), "wanted": true,
            "paused": {
                "reason": "rate limited",
                "until": (now + chrono::Duration::hours(1)).to_rfc3339(),
                "since": now.to_rfc3339(),
            },
        }),
    );
    let r = env.hall(&env.repo, &["inbox"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let entries = r.json["entries"].as_array().unwrap();
    let paused = entries
        .iter()
        .find(|e| e["kind"] == "paused")
        .unwrap_or_else(|| panic!("{}", r.stdout));
    assert!(
        paused["text"]
            .as_str()
            .unwrap()
            .starts_with("The loop is paused: rate limited, until "),
        "{paused}"
    );
    assert_eq!(paused["answer_with"][0], "hall daemon resume");
    let n = entries.iter().position(|e| e["kind"] == "paused").unwrap() + 1;
    let r = env.hall(&env.repo, &["inbox", &n.to_string()]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["actions"][0]["command"], "hall daemon resume");
    assert!(r.json["why"].as_str().unwrap().contains("session limit"));

    let r = env.hall(&env.repo, &["daemon", "resume"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let r = env.hall(&env.repo, &["inbox"]);
    assert!(
        !r.json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "paused"),
        "{}",
        r.stdout
    );
}
