//! Workstream D: budgets, resume-on-answer, notifications and the status
//! line. Tests never spend model usage (the fake runner) and never reach
//! the network (HTTP channels point at a local listener; desktop
//! notifications go to `HALL_NOTIFY_LOG`).

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::mpsc;

use common::{Env, commit_file, p};
use hallmark::budget::Level;
use hallmark::item::ItemId;
use hallmark::repo::Repo;
use hallmark::runner::{FakeRunner, Metered, RunRequest, run_metered};
use hallmark::worker::Progress;
use insta::assert_json_snapshot;
use serde_json::{Value, json};

// --- helpers ------------------------------------------------------------

struct Out {
    json: Value,
    code: i32,
    stdout: String,
}

/// Runs `hall` with extra environment and optional stdin.
fn run(env: &Env, dir: &Path, vars: &[(&str, String)], args: &[&str], stdin: Option<&str>) -> Out {
    let mut c = env.cmd(dir);
    for (k, v) in vars {
        c.env(k, v);
    }
    c.args(args);
    if let Some(s) = stdin {
        c.write_stdin(s.to_string());
    }
    let o = c.output().unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout).to_string();
    Out {
        json: serde_json::from_str(stdout.trim()).unwrap_or(Value::Null),
        code: o.status.code().unwrap_or(-1),
        stdout,
    }
}

/// Appends a usage event to a journal, as the runner would record it.
fn add_usage(env: &Env, item: Option<&str>, room: &str, role: &str, source: &str, tokens: u64) {
    let mut v = json!({
        "v": 1, "t": chrono::Utc::now().to_rfc3339(), "actor": "hall", "event": "usage",
        "source": source, "room": room, "role": role, "model": "claude-opus-5",
        "input_tokens": tokens, "output_tokens": 0, "cache_read_tokens": 0,
        "cache_write_tokens": 0, "cost_usd": 1.25, "requests": 1
    });
    let path = match item {
        Some(id) => {
            v["item"] = json!(id);
            env.repo
                .join(".git/hallmark/items")
                .join(id)
                .join("events.jsonl")
        }
        None => env.repo.join(".git/hallmark/events.jsonl"),
    };
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(f, "{v}").unwrap();
}

fn append_config(env: &Env, text: &str) {
    let path = env.repo.join(".hallmark.toml");
    let mut t = std::fs::read_to_string(&path).unwrap();
    t.push_str(text);
    std::fs::write(path, t).unwrap();
}

fn events<'a>(journal: &'a [Value], name: &str) -> Vec<&'a Value> {
    journal.iter().filter(|e| e["event"] == name).collect()
}

fn desktop_log(env: &Env) -> Vec<Value> {
    std::fs::read_to_string(env.root.join("notify.log"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn scrub(env: &Env, v: &Value) -> Value {
    let s = v.to_string().replace(&p(&env.root), "[root]");
    serde_json::from_str(&s).unwrap()
}

#[derive(Debug)]
struct Req {
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A local HTTP listener that records every request and answers `status`.
struct Server {
    url: String,
    rx: mpsc::Receiver<Req>,
}

impl Server {
    fn start(status: u16) -> Server {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { break };
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                if r.read_line(&mut line).is_err() {
                    continue;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let mut headers = Vec::new();
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.trim_end().split_once(':') {
                        headers.push((k.trim().to_string(), v.trim().to_string()));
                    }
                }
                let len: usize = headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, v)| v.parse().ok())
                    .unwrap_or(0);
                let mut body = vec![0u8; len];
                let _ = r.read_exact(&mut body);
                let _ = tx.send(Req {
                    path,
                    headers,
                    body: String::from_utf8_lossy(&body).into_owned(),
                });
                let _ = write!(
                    s,
                    "HTTP/1.1 {status} X\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
                );
            }
        });
        Server { url, rx }
    }

    fn requests(&self) -> Vec<Req> {
        self.rx.try_iter().collect()
    }
}

/// An address nothing listens on.
fn dead_url() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    drop(l);
    url
}

// --- budgets --------------------------------------------------------------

#[test]
fn budget_levels_through_run_metered() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Usage page");
    let script = env.script(json!([{"role": "reviewer"}, {"role": "reviewer"}]));
    let mut repo = Repo::discover(&env.repo).unwrap();
    let runner = FakeRunner::new(script);
    let item: ItemId = id.parse().unwrap();
    let req = RunRequest {
        role: "reviewer".into(),
        room: "assay".into(),
        prompt: "Review".into(),
        cwd: wt.clone(),
        ..Default::default()
    };
    // No caps: the run happens and its usage is recorded: 6,152 tokens,
    // 5,187 weighted by price (cache reads at a tenth, writes at 1.25).
    assert!(matches!(
        run_metered(&repo, &runner, Some(item), &req).unwrap(),
        Metered::Done(_)
    ));

    // The item cap holds the item for you.
    repo.config.budget.per_item = Some("5k".into());
    match run_metered(&repo, &runner, Some(item), &req).unwrap() {
        Metered::Blocked(stop) => {
            assert_eq!(stop.level, Level::Item);
            assert!(matches!(stop.progress(), Progress::Worked { .. }));
        }
        other => panic!("{other:?}"),
    }
    let held = env.journal(&id);
    let held = events(&held, "held");
    assert_eq!(held.len(), 1);
    assert_eq!(held[0]["kind"], "budget");
    let inbox = env.hall(&env.repo, &["inbox"]);
    assert_eq!(
        inbox.json["entries"][0]["kind"], "budget",
        "{}",
        inbox.stdout
    );

    // The room's daily cap closes the Assay Office, not the Repair Shop.
    repo.config.budget.per_item = None;
    repo.config.budget.review = Some(hallmark::config::RoleBudget {
        daily: Some("5k".into()),
        ..Default::default()
    });
    match run_metered(&repo, &runner, Some(item), &req).unwrap() {
        Metered::Blocked(stop) => assert_eq!(stop.level, Level::Room),
        other => panic!("{other:?}"),
    }

    // The daily total pauses the daemon until tomorrow.
    repo.config.budget.daily = Some("5k".into());
    match run_metered(&repo, &runner, None, &req).unwrap() {
        Metered::Blocked(stop) => {
            assert_eq!(stop.level, Level::Daily);
            assert!(matches!(
                stop.progress(),
                Progress::Pause { until: Some(_), .. }
            ));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(env.fake_calls().len(), 1, "blocked runs never start");
}

#[test]
fn retry_grants_a_budget_hold_more_tokens() {
    let env = Env::initialized();
    let cfg = env.repo.join(".hallmark.toml");
    let text = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("# per_item = \"800k\"", "per_item = \"5k\"");
    std::fs::write(&cfg, &text).unwrap();
    let (id, wt) = env.new_item("Usage page");
    let script = env.script(json!([{"role": "reviewer"}, {"role": "reviewer"}]));
    let repo = Repo::discover(&env.repo).unwrap();
    let runner = FakeRunner::new(script);
    let item: ItemId = id.parse().unwrap();
    let req = RunRequest {
        role: "reviewer".into(),
        room: "assay".into(),
        prompt: "Review".into(),
        cwd: wt.clone(),
        ..Default::default()
    };
    // The first run (6152 tokens) crosses the 5k cap; the next one holds.
    assert!(matches!(
        run_metered(&repo, &runner, Some(item), &req).unwrap(),
        Metered::Done(_)
    ));
    match run_metered(&repo, &runner, Some(item), &req).unwrap() {
        Metered::Blocked(stop) => {
            assert_eq!(stop.level, Level::Item);
            assert!(
                stop.reason.contains("`hall retry` grants it 5k more"),
                "{}",
                stop.reason
            );
        }
        other => panic!("{other:?}"),
    }

    let bad = env.hall(
        &env.repo,
        &["retry", &id, "--reason", "x", "--tokens", "lots"],
    );
    assert_eq!(bad.code, 2, "{}", bad.stdout);
    assert_eq!(bad.json["error"]["code"], "USAGE");
    for v in ["inf", "1e19"] {
        let r = env.hall(&env.repo, &["retry", &id, "--reason", "x", "--tokens", v]);
        assert_eq!(r.json["error"]["code"], "USAGE", "{v}: {}", r.stdout);
    }

    // Retrying grants another per_item to this item; the config is untouched.
    let r = env.hall(&env.repo, &["retry", &id, "--reason", "worth finishing"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let note = r.json["note"].as_str().unwrap();
    assert!(note.contains("5k more tokens"), "{note}");
    assert!(note.contains("budget is unchanged"), "{note}");
    let journal = env.journal(&id);
    let released = events(&journal, "released");
    // 5k more from now: the 1152 the first run overshot the cap by comes first.
    assert_eq!(released[0]["granted_tokens"], 6152);
    assert_eq!(std::fs::read_to_string(&cfg).unwrap(), text);
    assert!(matches!(
        run_metered(&repo, &runner, Some(item), &req).unwrap(),
        Metered::Done(_)
    ));
    // 12304 spent of 11152 allowed: held again.
    match run_metered(&repo, &runner, Some(item), &req).unwrap() {
        Metered::Blocked(stop) => {
            assert!(stop.reason.contains("5k token cap plus"), "{}", stop.reason);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(events(&env.journal(&id), "held").len(), 2);

    // --tokens grants a chosen amount.
    let r = env.hall(
        &env.repo,
        &["retry", &id, "--reason", "big one", "--tokens", "1M"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(
        env.journal(&id).last().unwrap()["granted_tokens"],
        1_001_152
    );
    assert_eq!(
        hallmark::budget::check(&repo, Some(item), "reviewer"),
        Ok(())
    );
    let b = env.hall(&env.repo, &["budget"]);
    assert_eq!(b.json["top_items"][0]["granted"], 1_007_304, "{}", b.stdout);
    assert_eq!(b.json["top_items"][0]["over_cap"], false);

    // Other holds are released without a grant, and refuse --tokens.
    let path = env
        .repo
        .join(".git/hallmark/items")
        .join(&id)
        .join("events.jsonl");
    let held = json!({"v": 1, "t": "2026-10-09T12:00:00Z", "actor": "assayer",
                      "item": id, "event": "held", "kind": "run_failed", "reason": "stopped"});
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(f, "{held}").unwrap();
    let r = env.hall(
        &env.repo,
        &["retry", &id, "--reason", "x", "--tokens", "1M"],
    );
    assert_eq!(r.json["error"]["code"], "USAGE", "{}", r.stdout);
    let r = env.hall(&env.repo, &["retry", &id, "--reason", "flaky"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert!(
        env.journal(&id)
            .last()
            .unwrap()
            .get("granted_tokens")
            .is_none()
    );
}

#[test]
fn budget_shows_caps_and_today() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Retry webhooks");
    add_usage(&env, Some(&id), "assay", "reviewer", "headless", 1_200_000);
    add_usage(&env, Some(&id), "repair", "fixer", "headless", 300_000);
    add_usage(&env, Some(&id), "workshop", "dev", "transcript", 2_000_000);
    let r = env.hall(
        &env.repo,
        &[
            "budget",
            "--set",
            "daily=4M per_item=800k review.max_usd=3 review.daily=2M fix.model=sonnet",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["written"], p(&env.repo.join(".hallmark.toml")));
    let toml = std::fs::read_to_string(env.repo.join(".hallmark.toml")).unwrap();
    assert!(
        toml.contains("# Hallmark settings for this repository"),
        "{toml}"
    );
    assert!(toml.contains("daily = \"4M\""), "{toml}");
    assert!(toml.contains("[budget.review]"), "{toml}");
    let mut v = scrub(&env, &r.json);
    v["day"] = json!("[day]");
    assert_json_snapshot!("d_budget_set", v);

    // Idempotent: the same settings write nothing.
    let again = env.hall(&env.repo, &["budget", "--set", "daily=4M"]);
    assert_eq!(again.code, 0);
    assert!(again.json.get("written").is_none(), "{}", again.stdout);
    assert_eq!(
        std::fs::read_to_string(env.repo.join(".hallmark.toml")).unwrap(),
        toml
    );

    let r = env.hall(&env.repo, &["budget"]);
    let caps = r.json["caps"].as_array().unwrap();
    let daily = caps.iter().find(|c| c["level"] == "daily").unwrap();
    assert_eq!(daily["used"], 1_500_000);
    assert_eq!(daily["reached"], false);
    let item = &r.json["top_items"][0];
    assert_eq!(item["item"], id.as_str());
    assert_eq!(item["over_cap"], true);
    assert_eq!(r.json["workshop_today"], 2_000_000);

    // A bad value is a usage error and changes nothing.
    let bad = env.hall(&env.repo, &["budget", "--set", "per_item=lots"]);
    assert_eq!(bad.code, 2);
    assert_eq!(bad.json["error"]["code"], "USAGE");
}

#[test]
fn a_spent_daily_budget_pauses_the_loop_and_tells_the_session() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Usage page");
    add_usage(&env, Some(&id), "assay", "reviewer", "headless", 50_000);
    assert_eq!(
        env.hall(&env.repo, &["budget", "--set", "daily=40k"]).code,
        0
    );
    let r = env.hook(
        "SessionStart",
        &json!({"session_id": "s-1", "cwd": p(&wt), "source": "startup"}),
    );
    let ctx = r.json["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(ctx.contains("the daily budget is spent"), "{ctx}");

    // A question answered for a session: the resume waits for tomorrow.
    let ask = env.hall(
        &wt,
        &[
            "ask",
            "billed or raw?",
            "--options",
            "billed,raw",
            "--session",
            "s-1",
        ],
    );
    assert_eq!(
        env.hall(&wt, &["answer", ask.json["qid"].as_str().unwrap(), "a"])
            .code,
        0
    );
    let script = env.script(json!([{"role": "resume"}]));
    let r = env.hall_fake(
        &env.repo,
        &script,
        &["daemon", "run", "--once", "--only", "resume"],
    );
    let step = &r.json["steps"][0];
    assert_eq!(step["progress"], "pause", "{}", r.stdout);
    assert!(step["until"].is_string());
    assert!(env.fake_calls().is_empty());

    let line = run(&env, &env.repo, &[], &["statusline"], Some("{}"));
    assert!(
        line.stdout.contains("daily budget spent"),
        "{}",
        line.stdout
    );
}

#[test]
fn a_grant_lets_the_loop_go_on_today_without_changing_the_cap() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Usage page");
    add_usage(&env, Some(&id), "assay", "reviewer", "headless", 50_000);
    assert_eq!(
        env.hall(&env.repo, &["budget", "--set", "daily=40k"]).code,
        0
    );
    let toml_before = std::fs::read_to_string(env.repo.join(".hallmark.toml")).unwrap();
    // The daemon paused on the spent budget.
    let now = chrono::Utc::now();
    let journal = p(&env.repo.join(".git").join("hallmark"));
    std::fs::create_dir_all(env.home.join("daemon")).unwrap();
    std::fs::write(
        env.home.join("daemon").join("state.json"),
        json!({
            "pid": 0, "started_at": null, "heartbeat": null, "wanted": false,
            "paused": {
                "reason": "the daily budget is spent (50k of 40k tokens today); the loop waits until 00:00 UTC",
                "until": (now + chrono::Duration::hours(3)).to_rfc3339(),
                "since": now.to_rfc3339(),
                "repo": "app",
                "journals": [journal],
            }
        })
        .to_string(),
    )
    .unwrap();
    let b = env.hall(&env.repo, &["budget"]);
    assert!(
        b.json["next"][0]
            .as_str()
            .unwrap()
            .starts_with("hall budget --grant"),
        "{}",
        b.stdout
    );

    let r = env.hall(
        &env.repo,
        &["budget", "--grant", "--reason", "quota left in Claude"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    // No amount: another `daily`.
    assert_eq!(r.json["grant"]["tokens"], 40_000);
    assert_eq!(r.json["grant"]["changed"], true);
    assert_eq!(r.json["grant"]["resumed"], true);
    let caps = r.json["caps"].as_array().unwrap();
    let daily = caps.iter().find(|c| c["level"] == "daily").unwrap();
    assert_eq!(daily["limit"], "40k");
    assert_eq!(daily["granted"], 40_000);
    assert_eq!(daily["reached"], false);
    // The config is untouched; the grant and the lifted pause are journaled.
    assert_eq!(
        std::fs::read_to_string(env.repo.join(".hallmark.toml")).unwrap(),
        toml_before
    );
    let kinds: Vec<String> = env
        .repo_journal()
        .iter()
        .map(|e| e["event"].as_str().unwrap().to_string())
        .collect();
    let g = kinds.iter().position(|k| k == "budget_granted").unwrap();
    assert!(kinds[g..].contains(&"unpaused".to_string()), "{kinds:?}");
    let state: Value = serde_json::from_str(
        &std::fs::read_to_string(env.home.join("daemon").join("state.json")).unwrap(),
    )
    .unwrap();
    assert!(state["paused"].is_null(), "{state}");

    // Sessions are no longer told the loop waits.
    let h = env.hook(
        "SessionStart",
        &json!({"session_id": "s-1", "cwd": p(&wt), "source": "startup"}),
    );
    let ctx = h.json["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default();
    assert!(!ctx.contains("the daily budget is spent"), "{ctx}");

    // The same grant again changes nothing; a bigger one replaces it.
    let again = env.hall(&env.repo, &["budget", "--grant"]);
    assert_eq!(again.json["grant"]["changed"], false, "{}", again.stdout);
    assert_eq!(again.json["grant"]["resumed"], false);
    let more = env.hall(&env.repo, &["budget", "--grant", "1M"]);
    assert_eq!(more.json["grant"]["tokens"], 1_000_000);
    let grants = env
        .repo_journal()
        .into_iter()
        .filter(|e| e["event"] == "budget_granted")
        .count();
    assert_eq!(grants, 2);
    // Bad amounts are usage errors.
    let bad = env.hall(&env.repo, &["budget", "--grant", "lots"]);
    assert_eq!(bad.code, 2, "{}", bad.stdout);
}

#[test]
fn a_grant_needs_a_daily_cap() {
    let env = Env::initialized();
    let r = env.hall(&env.repo, &["budget", "--grant", "1M"]);
    assert_eq!(r.code, 4, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "WRONG_STATE");
}

// --- answers --------------------------------------------------------------

#[test]
fn a_default_is_used_after_the_wait() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Usage page");
    let ask = env.hall(
        &wt,
        &[
            "ask",
            "billed or raw?",
            "--options",
            "billed,raw",
            "--default",
            "billed",
            "--wait",
            "1s",
        ],
    );
    assert_eq!(ask.code, 0, "{}", ask.stdout);
    let script = env.script(json!([]));
    // Not yet.
    let r = env.hall_fake(
        &env.repo,
        &script,
        &["daemon", "run", "--once", "--only", "resume"],
    );
    assert_eq!(r.json["steps"][0]["progress"], "idle", "{}", r.stdout);
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let r = env.hall_fake(
        &env.repo,
        &script,
        &["daemon", "run", "--once", "--only", "resume"],
    );
    assert_eq!(r.json["steps"][0]["progress"], "worked", "{}", r.stdout);
    let j = env.journal(&id);
    let answers = events(&j, "answer");
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0]["answer"], "billed");
    assert_eq!(answers[0]["defaulted"], true);
    assert_eq!(answers[0]["actor"], "hall");
    // The index agrees: the question left the inbox.
    let inbox = env.hall(&env.repo, &["inbox"]);
    assert_eq!(inbox.json["entries"], json!([]), "{}", inbox.stdout);
    // No session, so nothing to resume; a second tick does nothing.
    let r = env.hall_fake(
        &env.repo,
        &script,
        &["daemon", "run", "--once", "--only", "resume"],
    );
    assert_eq!(r.json["steps"][0]["progress"], "idle");
    assert!(env.fake_calls().is_empty());
    assert_eq!(events(&env.journal(&id), "answer").len(), 1);
}

#[test]
fn an_answer_resumes_the_asking_session() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Usage page");
    let ask = env.hall(
        &wt,
        &[
            "ask",
            "billed or raw?",
            "--options",
            "billed,raw",
            "--session",
            "sess-1",
        ],
    );
    let qid = ask.json["qid"].as_str().unwrap().to_string();
    let script = env.script(json!([{"role": "resume", "session_id": "sess-1"}]));
    // Unanswered: nothing to deliver.
    let r = env.hall_fake(
        &env.repo,
        &script,
        &["daemon", "run", "--once", "--only", "resume"],
    );
    assert_eq!(r.json["steps"][0]["progress"], "idle");
    assert_eq!(env.hall(&env.repo, &["answer", &qid, "raw"]).code, 0);
    let r = env.hall_fake(
        &env.repo,
        &script,
        &["daemon", "run", "--once", "--only", "resume"],
    );
    assert_eq!(r.json["steps"][0]["progress"], "worked", "{}", r.stdout);
    let calls = env.fake_calls();
    assert_eq!(calls.len(), 1);
    let args: Vec<&str> = calls[0]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    let at = args.iter().position(|a| *a == "--resume").unwrap();
    assert_eq!(args[at + 1], "sess-1");
    assert_eq!(
        *args.last().unwrap(),
        format!("The human answered {qid} (\"billed or raw?\"): raw")
    );
    assert_eq!(calls[0]["role"], "resume");
    assert_eq!(calls[0]["room"], "workshop");
    assert_eq!(p(Path::new(calls[0]["cwd"].as_str().unwrap())), p(&wt));
    let j = env.journal(&id);
    let delivered = events(&j, "answer_delivered");
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0]["qid"], qid.as_str());
    assert_eq!(delivered[0]["session"], "sess-1");
    let usage = events(&j, "usage");
    assert_eq!(usage.last().unwrap()["room"], "workshop");
    // Delivered once.
    let r = env.hall_fake(
        &env.repo,
        &script,
        &["daemon", "run", "--once", "--only", "resume"],
    );
    assert_eq!(r.json["steps"][0]["progress"], "idle");
    assert_eq!(env.fake_calls().len(), 1);
}

#[test]
fn a_failed_resume_is_retried_once_then_filed() {
    let env = Env::initialized();
    let ask = env.hall(&env.repo, &["ask", "Which region?", "--session", "sess-9"]);
    let qid = ask.json["qid"].as_str().unwrap().to_string();
    assert_eq!(env.hall(&env.repo, &["answer", &qid, "eu-west"]).code, 0);
    let script = env.script(json!([
        {"role": "resume", "fail": "error"},
        {"role": "resume", "fail": "error"},
        {"role": "resume"}
    ]));
    let tick = || {
        env.hall_fake(
            &env.repo,
            &script,
            &["daemon", "run", "--once", "--only", "resume"],
        )
    };
    let r = tick();
    assert!(
        r.json["steps"][0]["what"]
            .as_str()
            .unwrap()
            .contains("retrying"),
        "{}",
        r.stdout
    );
    let r = tick();
    assert!(
        r.json["steps"][0]["what"]
            .as_str()
            .unwrap()
            .contains("filed"),
        "{}",
        r.stdout
    );
    let j = env.repo_journal();
    assert_eq!(events(&j, "run_failed").len(), 2);
    assert!(events(&j, "answer_delivered").is_empty());
    let n = events(&j, "notification");
    assert_eq!(n[0]["notification_type"], "resume_failed");
    assert_eq!(n[0]["session"], "sess-9");
    let inbox = env.hall(&env.repo, &["inbox"]);
    let e = &inbox.json["entries"][0];
    assert_eq!(e["kind"], "session_waiting", "{}", inbox.stdout);
    assert_eq!(e["answer_with"][0], "claude --resume sess-9");
    // Given up: no third attempt.
    assert_eq!(tick().json["steps"][0]["progress"], "idle");
    assert_eq!(env.fake_calls().len(), 2);
}

// --- notifications --------------------------------------------------------

const NOW: &str = "2026-10-09T10:00:00+02:00";

#[test]
fn a_question_goes_to_the_desktop_and_the_phone_then_reminds_then_stops() {
    let env = Env::initialized();
    let ntfy = Server::start(200);
    append_config(
        &env,
        "\n[notify]\nphone = \"ntfy://ntfy.sh/hallmark-test0topic\"\ndigest = \"\"\n",
    );
    let (id, wt) = env.new_item("Usage page");
    commit_file(&wt, "usage.rs", "const SECRET_CODE_LINE: u32 = 42;");
    let ask = env.hall(
        &wt,
        &[
            "ask",
            "billed usage or raw events?",
            "--options",
            "billed,raw",
            "--default",
            "billed",
        ],
    );
    let qid = ask.json["qid"].as_str().unwrap().to_string();
    let vars = |now: &str| {
        vec![
            ("HALL_NTFY_URL", ntfy.url.clone()),
            ("HALL_NOTIFY_NOW", now.to_string()),
        ]
    };
    let tick = |now: &str| {
        run(
            &env,
            &env.repo,
            &vars(now),
            &["daemon", "run", "--once", "--only", "notify"],
            None,
        )
    };

    let r = tick(NOW);
    assert_eq!(r.json["steps"][0]["progress"], "worked", "{}", r.stdout);
    let reqs = ntfy.requests();
    assert_eq!(reqs.len(), 1);
    let q = &reqs[0];
    assert_eq!(q.path, "/hallmark-test0topic");
    assert_eq!(
        q.header("Title"),
        Some(format!("app: {id} question").as_str())
    );
    assert_eq!(q.header("Priority"), Some("4"));
    assert!(q.header("Tags").unwrap().contains("question"));
    assert!(q.body.contains("billed usage or raw events?"), "{}", q.body);
    assert!(q.body.contains("[a] billed  [b] raw"), "{}", q.body);
    assert!(q.body.contains(&format!("hall answer {qid}")), "{}", q.body);
    assert!(!q.body.contains("SECRET_CODE_LINE"));
    let desk = desktop_log(&env);
    assert_eq!(desk.len(), 1);
    assert_eq!(desk[0]["title"], format!("app: {id} question"));
    let j = env.journal(&id);
    let n = events(&j, "notified");
    assert_eq!(n.len(), 1);
    assert_eq!(n[0]["key"], format!("question:{id}:{qid}"));
    assert_eq!(n[0]["channels"], json!(["desktop", "phone"]));
    let state: Value = serde_json::from_str(
        &std::fs::read_to_string(env.repo.join(".git/hallmark/notify-state.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(state["entries"][format!("question:{id}:{qid}")]["count"], 1);

    // Nothing new an hour later.
    assert_eq!(
        tick("2026-10-09T11:00:00+02:00").json["steps"][0]["progress"],
        "idle"
    );
    assert!(ntfy.requests().is_empty());
    // A reminder after two hours.
    tick("2026-10-09T12:00:00+02:00");
    let reqs = ntfy.requests();
    assert_eq!(reqs.len(), 1);
    assert!(reqs[0].header("Title").unwrap().starts_with("Reminder: "));
    assert_eq!(desktop_log(&env).len(), 2);
    // Answered: forgotten, nothing more.
    assert_eq!(env.hall(&env.repo, &["answer", &qid, "raw"]).code, 0);
    tick("2026-10-09T16:00:00+02:00");
    assert!(ntfy.requests().is_empty());
    let state = std::fs::read_to_string(env.repo.join(".git/hallmark/notify-state.json")).unwrap();
    assert!(!state.contains(&qid), "{state}");
}

#[test]
fn quiet_hours_hold_the_phone_and_failures_retry() {
    let env = Env::initialized();
    append_config(
        &env,
        "\n[notify]\nphone = \"ntfy://ntfy.sh/hallmark-quiet0test\"\nquiet_hours = \"22:00-07:30\"\ndigest = \"\"\n",
    );
    env.hall(
        &env.repo,
        &["ask", "Ship on Friday?", "--options", "yes,no"],
    );
    let ntfy = Server::start(200);
    let tick = |url: &str, now: &str| {
        run(
            &env,
            &env.repo,
            &[
                ("HALL_NTFY_URL", url.to_string()),
                ("HALL_NOTIFY_NOW", now.to_string()),
            ],
            &["daemon", "run", "--once", "--only", "notify"],
            None,
        )
    };
    // 23:00: quiet. The desktop shows it; the phone waits.
    let r = tick(&ntfy.url, "2026-10-09T23:00:00+02:00");
    assert_eq!(r.code, 0);
    assert_eq!(desktop_log(&env).len(), 1);
    assert!(ntfy.requests().is_empty());
    // 07:45: quiet hours are over, but the phone's server is down. The tick
    // still succeeds and the desktop reminds.
    let r = tick(&dead_url(), "2026-10-10T07:45:00+02:00");
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert!(r.json["steps"][0]["error"].is_null(), "{}", r.stdout);
    let state = std::fs::read_to_string(env.repo.join(".git/hallmark/notify-state.json")).unwrap();
    assert!(state.contains("ntfy:"), "the failure is recorded: {state}");
    // Next tick: the phone gets it.
    tick(&ntfy.url, "2026-10-10T07:50:00+02:00");
    assert_eq!(ntfy.requests().len(), 1);
    let j = env.repo_journal();
    let n = events(&j, "notified");
    assert_eq!(n.last().unwrap()["channels"], json!(["phone"]));
}

#[test]
fn the_morning_digest_goes_out_once_a_day() {
    let env = Env::initialized();
    append_config(&env, "\n[notify]\ndigest = \"08:30\"\n");
    let (id, _) = env.new_item("Retry webhooks");
    add_usage(&env, Some(&id), "assay", "reviewer", "headless", 120_000);
    env.hall(&env.repo, &["ask", "Which region?"]);
    let tick = |now: &str| {
        run(
            &env,
            &env.repo,
            &[("HALL_NOTIFY_NOW", now.to_string())],
            &["daemon", "run", "--once", "--only", "notify"],
            None,
        )
    };
    let digests = || {
        desktop_log(&env)
            .into_iter()
            .filter(|d| d["title"].as_str().unwrap().contains("morning digest"))
            .collect::<Vec<_>>()
    };
    tick("2026-10-09T08:00:00+00:00");
    assert!(digests().is_empty(), "too early");
    tick("2026-10-09T09:00:00+00:00");
    let d = digests();
    assert_eq!(d.len(), 1);
    let body = d[0]["body"].as_str().unwrap();
    assert!(body.contains("Waiting for you (1)"), "{body}");
    assert!(body.contains("Which region?"), "{body}");
    tick("2026-10-09T18:00:00+00:00");
    assert_eq!(digests().len(), 1, "once a day");
    tick("2026-10-10T09:00:00+00:00");
    assert_eq!(digests().len(), 2);
    let j = env.repo_journal();
    assert!(events(&j, "notified").iter().any(|n| n["kind"] == "digest"));
}

#[test]
fn notify_test_creates_a_private_topic_and_tests_each_channel() {
    let env = Env::initialized();
    let slack = Server::start(200);
    let r = env.hall(&env.repo, &["notify", "test"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let topic = r.json["created_topic"].as_str().unwrap().to_string();
    assert!(
        topic.starts_with("hallmark-") && topic.len() == 19,
        "{topic}"
    );
    let user = std::fs::read_to_string(env.home.join("config.toml")).unwrap();
    assert!(
        user.contains(&format!("phone = \"ntfy://ntfy.sh/{topic}\"")),
        "{user}"
    );
    let mut v = scrub(&env, &r.json);
    let s = v.to_string().replace(&topic, "[topic]");
    v = serde_json::from_str(&s).unwrap();
    assert_json_snapshot!("d_notify_test_new_topic", v);
    assert_eq!(desktop_log(&env).len(), 1);

    // The topic is kept; asking for the phone by name sends to it.
    let ntfy = Server::start(200);
    let r = run(
        &env,
        &env.repo,
        &[("HALL_NTFY_URL", ntfy.url.clone())],
        &["notify", "test", "--channel", "phone"],
        None,
    );
    assert_eq!(r.json["sent"][0]["ok"], true, "{}", r.stdout);
    assert!(r.json.get("created_topic").is_none());
    assert_eq!(ntfy.requests()[0].path, format!("/{topic}"));

    // Slack, configured in the repo.
    append_config(
        &env,
        &format!(
            "\n[notify]\nphone = \"ntfy://ntfy.sh/{topic}\"\nslack = \"{}/hook\"\n",
            slack.url
        ),
    );
    let r = run(
        &env,
        &env.repo,
        &[("HALL_NTFY_URL", ntfy.url.clone())],
        &["notify", "test", "--channel", "slack"],
        None,
    );
    assert_eq!(r.json["sent"][0]["ok"], true, "{}", r.stdout);
    let req = &slack.requests()[0];
    assert_eq!(req.path, "/hook");
    let body: Value = serde_json::from_str(&req.body).unwrap();
    assert!(body["text"].as_str().unwrap().contains("Hallmark: test"));

    let bad = env.hall(&env.repo, &["notify", "test", "--channel", "pager"]);
    assert_eq!(bad.code, 2);
}

// --- the status line --------------------------------------------------------

#[test]
fn the_status_line() {
    let env = Env::initialized();
    // Outside a repo: nothing at all.
    let outside = env.root.join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    let r = run(&env, &outside, &[], &["statusline"], Some(""));
    assert_eq!(r.code, 0);
    assert_eq!(r.stdout, "");
    let r = run(&env, &env.repo, &[], &["statusline"], Some("not json"));
    assert_eq!(r.stdout, "app\n");

    let (id, wt) = env.new_item("Usage page");
    env.hall(&wt, &["ask", "billed or raw?"]);
    let input = json!({"session_id": "s-7", "cwd": p(&wt), "model": {"id": "x"}}).to_string();
    let r = run(&env, &env.repo, &[], &["statusline"], Some(&input));
    assert_eq!(
        r.stdout,
        format!("app · {id} development · ⚑ 1 needs you\n")
    );
    // workspace.current_dir works too, and stdin's cwd wins over the process's.
    let input = json!({"workspace": {"current_dir": p(&env.repo)}}).to_string();
    let r = run(&env, &wt, &[], &["statusline"], Some(&input));
    assert_eq!(r.stdout, "app · ⚑ 1 needs you\n");
    // A session bound to the item shows it from the main checkout.
    env.hook(
        "SessionStart",
        &json!({"session_id": "s-7", "cwd": p(&wt), "source": "startup"}),
    );
    let input = json!({"session_id": "s-7", "cwd": p(&env.repo)}).to_string();
    let r = run(&env, &env.repo, &[], &["statusline"], Some(&input));
    assert_eq!(
        r.stdout,
        format!("app · {id} development · ⚑ 1 needs you\n")
    );
}

#[test]
fn schemas_are_served() {
    let env = Env::new();
    for name in ["budget", "notify", "statusline"] {
        let r = env.hall(&env.root, &["schema", name]);
        assert_eq!(r.code, 0, "{name}");
        assert!(r.json["schema"].is_object(), "{name}");
    }
}
