//! `hall web` (ADR 0052): the page, the token and host checks, and the API
//! that runs hall's own commands for it.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Stdio};
use std::time::Duration;

use common::{Env, commit_file};
use serde_json::{Value, json};

/// A running `hall web`, stopped when dropped.
struct Web {
    child: Child,
    port: u16,
    token: String,
    start: Value,
}

impl Drop for Web {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(env: &Env) -> Web {
    start_with(env, &[])
}

/// Starts `hall web` with more variables in its own environment.
fn start_with(env: &Env, vars: &[(&str, &str)]) -> Web {
    let mut cmd = env.std_cmd(&env.repo);
    for (k, v) in vars {
        cmd.env(k, v);
    }
    let mut child = cmd
        .args(["web", "--port", "0", "--no-open"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let start: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}"));
    let url = start["url"].as_str().unwrap().to_string();
    let token = url.split("#token=").nth(1).unwrap().to_string();
    Web {
        child,
        port: start["port"].as_u64().unwrap() as u16,
        token,
        start,
    }
}

/// One HTTP/1.1 request; (status, body).
fn http(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\n");
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        req.push_str(&format!("Host: 127.0.0.1:{port}\r\n"));
    }
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = out.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    (status, body.to_string())
}

impl Web {
    fn get(&self, path: &str) -> (u16, Value) {
        let (code, body) = http(self.port, "GET", path, &[("X-Hall-Token", &self.token)], "");
        (code, serde_json::from_str(&body).unwrap_or(Value::Null))
    }

    /// `hall <args>` through the API, in `repo`: (status, {exit, reply, lines}).
    fn run(&self, repo: &str, args: &[&str]) -> (u16, Value) {
        let body = json!({ "repo": repo, "args": args }).to_string();
        let (code, out) = http(
            self.port,
            "POST",
            "/api/run",
            &[
                ("X-Hall-Token", &self.token),
                ("Content-Type", "application/json"),
            ],
            &body,
        );
        (code, serde_json::from_str(&out).unwrap_or(Value::Null))
    }
}

fn path(env: &Env) -> String {
    common::p(&env.repo)
}

#[test]
fn it_prints_where_it_listens_and_serves_the_page() {
    let env = Env::initialized();
    let web = start(&env);
    assert_eq!(web.token.len(), 32);
    assert!(
        web.start["url"]
            .as_str()
            .unwrap()
            .starts_with(&format!("http://127.0.0.1:{}/#token=", web.port))
    );
    let repos = web.start["repos"].as_array().unwrap();
    assert!(repos.iter().any(|r| r["path"] == path(&env)), "{repos:?}");

    let (code, html) = http(web.port, "GET", "/", &[], "");
    assert_eq!(code, 200);
    assert!(html.contains("<title>Hallmark</title>"));
    for asset in ["/app.js", "/app.css", "/icon.svg"] {
        assert_eq!(http(web.port, "GET", asset, &[], "").0, 200, "{asset}");
    }
    assert_eq!(http(web.port, "GET", "/nothing", &[], "").0, 404);
    let (code, ping) = http(web.port, "GET", "/api/ping", &[], "");
    assert_eq!(code, 200);
    assert!(ping.contains("\"ok\":true"));
}

#[test]
fn the_api_needs_the_token_and_this_address() {
    let env = Env::initialized();
    let web = start(&env);
    let (code, body) = http(web.port, "GET", "/api/repos", &[], "");
    assert_eq!(code, 401);
    assert!(body.contains("\"code\":\"USAGE\""), "{body}");
    let wrong = "0".repeat(32);
    assert_eq!(
        http(
            web.port,
            "GET",
            "/api/repos",
            &[("X-Hall-Token", &wrong)],
            ""
        )
        .0,
        401
    );
    // A page that rebinds its own name to 127.0.0.1 still sends its own Host.
    let (code, _) = http(
        web.port,
        "GET",
        "/api/repos",
        &[("Host", "evil.example:80"), ("X-Hall-Token", &web.token)],
        "",
    );
    assert_eq!(code, 403);
    assert_eq!(
        http(web.port, "GET", "/", &[("Host", "evil.example")], "").0,
        403
    );

    let (code, repos) = web.get("/api/repos");
    assert_eq!(code, 200);
    assert_eq!(repos["here"], 0);
    assert_eq!(repos["repos"][0]["path"], path(&env));
}

#[test]
fn it_runs_hall_commands_in_a_repo_as_you() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Retry webhooks");
    let web = start(&env);
    let (code, r) = web.run(&path(&env), &["status"]);
    assert_eq!(code, 200, "{r}");
    assert_eq!(r["exit"], 0);
    assert_eq!(r["reply"]["items"][0]["id"], id.as_str());
    assert!(r["reply"]["next"].is_array());

    let (_, r) = web.run(&path(&env), &["drop", &id, "--reason", "from the web"]);
    assert_eq!(r["exit"], 0, "{r}");
    let dropped = env
        .journal(&id)
        .into_iter()
        .find(|e| e["event"] == "dropped")
        .expect("a dropped event");
    assert_eq!(dropped["reason"], "from the web");
    assert_ne!(dropped["actor"], "agent");

    // A command's own error is its reply, with its exit code.
    let (code, r) = web.run(&path(&env), &["status", "H-0999"]);
    assert_eq!(code, 200);
    assert_eq!(r["exit"], 3);
    assert_eq!(r["reply"]["error"]["code"], "ITEM_NOT_FOUND");
}

#[test]
fn page_actions_are_yours_whatever_session_started_the_server() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Retry webhooks");
    let web = start_with(
        &env,
        &[
            ("CLAUDECODE", "1"),
            ("HALL_ACTOR", "fixer"),
            ("HALL_HEADLESS", "1"),
        ],
    );
    let (_, r) = web.run(&path(&env), &["config", "set", "budget.daily", "4M"]);
    assert_eq!(r["exit"], 0, "{r}");
    let (_, r) = web.run(&path(&env), &["drop", &id, "--reason", "from the web"]);
    assert_eq!(r["exit"], 0, "{r}");
    let dropped = env
        .journal(&id)
        .into_iter()
        .find(|e| e["event"] == "dropped")
        .expect("a dropped event");
    assert_eq!(dropped["actor"], "you");
}

#[test]
fn a_setting_saved_from_the_page_is_just_its_value() {
    let env = Env::initialized();
    let web = start(&env);
    // The page's Save sends `config set <key> -- <value>`.
    let (_, r) = web.run(
        &path(&env),
        &["config", "set", "budget.per_item", "--", "800k"],
    );
    assert_eq!(r["exit"], 0, "{r}");
    assert_eq!(r["reply"]["entries"][0]["value"], "800k");
    let (_, r) = web.run(&path(&env), &["config", "set", "gates", "--", "cargo test"]);
    assert_eq!(r["exit"], 0, "{r}");
    assert_eq!(r["reply"]["entries"][0]["value"], json!(["cargo test"]));
}

#[test]
fn a_runs_steps_come_back_as_lines_before_the_reply() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Traced");
    common::make_spike_dir(&env);
    let runs = env.repo.join(".git/hallmark/items").join(&id).join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    let end = hallmark::trace::RunEnd {
        kind: "hall_run_end".into(),
        t: chrono::Utc::now(),
        duration_ms: 4_000,
        exit_code: Some(0),
        failure: None,
    }
    .line();
    std::fs::write(
        runs.join("20261012T215146-fixer.jsonl"),
        format!("{}{end}", common::fixture("claude/stream.jsonl")),
    )
    .unwrap();
    let web = start(&env);
    let (_, r) = web.run(&path(&env), &["trace", &id, "--list"]);
    assert_eq!(r["exit"], 0, "{r}");
    assert_eq!(r["reply"]["runs"][0]["role"], "fixer");
    let (_, r) = web.run(&path(&env), &["trace", &id, "--run", "1"]);
    assert_eq!(r["exit"], 0, "{r}");
    let lines = r["lines"].as_array().unwrap();
    assert!(!lines.is_empty());
    assert!(lines.iter().all(|s| s["kind"].is_string()), "{lines:?}");
    assert_eq!(r["reply"]["run"]["n"], 1);
}

#[test]
fn it_refuses_what_a_person_does_not_run_from_a_page() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Guarded");
    let web = start(&env);
    for args in [
        vec!["hook", "stop"],
        vec!["verdict", id.as_str(), "--file", "-"],
        vec!["next", "--role", "fixer"],
        vec!["fixed", "F1"],
        vec!["watch", id.as_str()],
        vec!["trace", id.as_str(), "--follow"],
        vec!["new", "x", "--open"],
        vec!["adopt"],
        vec!["setup"],
        vec!["init"],
        vec!["web"],
        vec!["daemon", "run", "--once"],
        vec!["update", "--source", "https://example.com/other.git"],
        vec!["propose-ready", "--summary", "done"],
    ] {
        let (code, r) = web.run(&path(&env), &args);
        assert_eq!(code, 400, "{args:?}: {r}");
        assert_eq!(r["error"]["code"], "USAGE", "{args:?}: {r}");
    }
    // Only a repo the app shows.
    let elsewhere = env.root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let (code, r) = web.run(&common::p(&elsewhere), &["status"]);
    assert_eq!(code, 400);
    assert_eq!(r["error"]["code"], "NOT_A_REPO");
    let (code, r) = web.run(&path(&env), &["no-such-command"]);
    assert_eq!(code, 400);
    assert_eq!(r["error"]["code"], "USAGE");
}

#[test]
fn the_diff_is_the_items_branch_against_its_base() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Diffed");
    commit_file(&wt, "webhook.txt", "retry 6 times\n");
    let web = start(&env);
    let repo = path(&env)
        .replace('/', "%2F")
        .replace(':', "%3A")
        .replace('\\', "%5C");
    let (code, d) = web.get(&format!("/api/diff?repo={repo}&id={id}"));
    assert_eq!(code, 200, "{d}");
    assert_eq!(d["range"], format!("main...hm/{}-diffed", id).as_str());
    let lines: Vec<&str> = d["lines"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(lines.contains(&"+retry 6 times"), "{lines:?}");
}

#[test]
fn a_taken_port_is_an_error_when_you_asked_for_it() {
    let env = Env::initialized();
    let web = start(&env);
    let port = web.port.to_string();
    let r = env.hall(&env.repo, &["web", "--port", &port, "--no-open"]);
    assert_eq!(r.code, 2, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "USAGE");
}

#[cfg(unix)]
#[test]
fn a_restart_comes_back_on_the_same_port_and_token() {
    let env = Env::initialized();
    let web = start(&env);
    let (code, _) = http(
        web.port,
        "POST",
        "/api/restart",
        &[("X-Hall-Token", &web.token)],
        "{}",
    );
    assert_eq!(code, 200);
    let mut back = false;
    for _ in 0..50 {
        hallmark::daemon::nap(Duration::from_millis(200));
        if TcpStream::connect(("127.0.0.1", web.port)).is_ok() {
            let (code, _) = web.get("/api/repos");
            if code == 200 {
                back = true;
                break;
            }
        }
    }
    assert!(back, "hall web didn't come back on port {}", web.port);
}

#[test]
fn its_start_line_and_reply_have_schemas() {
    let env = Env::new();
    for name in ["web", "web-line"] {
        let r = env.hall(&env.repo, &["schema", name]);
        assert_eq!(r.code, 0, "{name}: {}", r.stdout);
    }
}
