//! `hall ps`, `hall kill`, `hall -l` and `hall web -d|-l|--kill` (ADR 0072):
//! the halls that keep running register themselves, are listed, and stop.

mod common;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use common::Env;
use serde_json::{Value, json};

/// A `hall web` in the foreground, killed when dropped.
struct Web {
    child: Child,
    start: Value,
}

impl Drop for Web {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn web(env: &Env) -> Web {
    let mut child = env
        .std_cmd(&env.repo)
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
    Web { child, start }
}

/// Stops a detached server whatever the test does.
struct KillOnDrop<'a>(&'a Env, u32);

impl Drop for KillOnDrop<'_> {
    fn drop(&mut self) {
        let _ = self
            .0
            .cmd(&self.0.repo)
            .args(["kill", &self.1.to_string()])
            .output();
    }
}

fn run_dir(env: &Env) -> PathBuf {
    env.home.join("run")
}

fn files_for(dir: &Path, pid: u64) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(&format!("{pid}.")))
                .collect()
        })
        .unwrap_or_default()
}

fn pids(r: &Value) -> Vec<u64> {
    r["instances"]
        .as_array()
        .unwrap_or_else(|| panic!("{r}"))
        .iter()
        .map(|i| i["pid"].as_u64().unwrap())
        .collect()
}

#[test]
fn nothing_running_lists_nothing() {
    let env = Env::new();
    for args in [&["ps"][..], &["-l"], &["ps", "--kind", "watch"]] {
        let r = env.hall(&env.repo, args);
        assert_eq!(r.code, 0, "{args:?}: {}", r.stdout);
        assert_eq!(r.json["instances"], json!([]), "{args:?}");
    }
    let r = env.hall(&env.repo, &["web", "-l"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["kind"], "web");
    assert_eq!(r.json["instances"], json!([]));
    assert_eq!(r.json["next"], json!(["hall web -d"]));
}

#[test]
fn a_running_web_is_listed_and_stopped() {
    let env = Env::new();
    let mut w = web(&env);
    let pid = w.child.id() as u64;

    let r = env.hall(&env.repo, &["ps"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(pids(&r.json), vec![pid], "{}", r.stdout);
    let i = &r.json["instances"][0];
    assert_eq!(i["kind"], "web");
    assert_eq!(i["port"], w.start["port"]);
    assert_eq!(i["url"], w.start["url"]);
    assert_eq!(i["args"], json!(["web", "--port", "0", "--no-open"]));
    assert!(i["started_at"].is_string());
    assert_eq!(r.json["next"][0], format!("hall kill {pid}"));

    // The same, by every name; another kind lists nothing.
    assert_eq!(pids(&env.hall(&env.repo, &["-l"]).json), vec![pid]);
    assert_eq!(pids(&env.hall(&env.repo, &["web", "-l"]).json), vec![pid]);
    assert_eq!(
        pids(&env.hall(&env.repo, &["ps", "--kind", "web"]).json),
        vec![pid]
    );
    assert!(pids(&env.hall(&env.repo, &["ps", "--kind", "watch"]).json).is_empty());
    // --kill for another kind's pid stops nothing.
    let r = env.hall(&env.repo, &["kill", &pid.to_string(), "--kind", "watch"]);
    assert_eq!(r.json["changed"], false, "{}", r.stdout);
    assert!(w.child.try_wait().unwrap().is_none(), "still running");

    let r = env.hall(&env.repo, &["web", "--kill", &pid.to_string()]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["changed"], true, "{}", r.stdout);
    assert_eq!(
        r.json["stopped"],
        json!([{"pid": pid, "kind": "web", "stopped": true}])
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while w.child.try_wait().unwrap().is_none() && Instant::now() < deadline {
        hallmark::daemon::nap(Duration::from_millis(50));
    }
    assert!(w.child.try_wait().unwrap().is_some(), "the server exited");
    assert!(pids(&env.hall(&env.repo, &["ps"]).json).is_empty());
    assert!(files_for(&run_dir(&env), pid).is_empty());

    // Stopping it again is safe and says so.
    let r = env.hall(&env.repo, &["web", "--kill", &pid.to_string()]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["changed"], false);
    assert_eq!(r.json["stopped"], json!([]));
    assert!(r.json["note"].as_str().unwrap().contains(&pid.to_string()));
}

#[test]
fn web_kill_without_a_pid_stops_every_web() {
    let env = Env::new();
    let mut a = web(&env);
    let mut b = web(&env);
    let r = env.hall(&env.repo, &["ps"]);
    assert_eq!(pids(&r.json).len(), 2, "{}", r.stdout);

    let r = env.hall(&env.repo, &["web", "--kill"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(
        r.json["stopped"].as_array().unwrap().len(),
        2,
        "{}",
        r.stdout
    );
    assert!(
        r.json["stopped"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["stopped"] == true)
    );
    let _ = a.child.wait();
    let _ = b.child.wait();
    assert!(pids(&env.hall(&env.repo, &["ps"]).json).is_empty());
}

#[test]
fn a_gone_process_is_pruned() {
    let env = Env::new();
    let dir = run_dir(&env);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("web-1-1.log");
    std::fs::write(&log, "").unwrap();
    // An entry whose process is gone: its lock file is there, unlocked.
    let entry = json!({
        "pid": 999_999, "kind": "web", "cwd": "/x", "args": ["web"],
        "port": 4747, "log": log.to_string_lossy(),
    });
    std::fs::write(dir.join("999999.json"), entry.to_string()).unwrap();
    std::fs::write(dir.join("999999.lock"), "").unwrap();
    // And one without a lock file at all.
    let other = json!({"pid": 999_998, "kind": "watch", "cwd": "/x", "args": ["watch"]});
    std::fs::write(dir.join("999998.json"), other.to_string()).unwrap();

    let r = env.hall(&env.repo, &["ps"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["instances"], json!([]), "{}", r.stdout);
    assert!(files_for(&dir, 999_999).is_empty());
    assert!(files_for(&dir, 999_998).is_empty());
    assert!(!log.exists(), "its detached log goes with it");
}

#[test]
fn kill_needs_a_target_and_l_stands_alone() {
    let env = Env::new();
    let r = env.hall(&env.repo, &["kill"]);
    assert_eq!(r.code, 2, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "USAGE");
    assert_eq!(r.json["error"]["next"], json!(["hall ps"]));

    let r = env.hall(&env.repo, &["kill", "4242"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["changed"], false);
    assert!(r.json["note"].as_str().unwrap().contains("4242"));

    let r = env.hall(&env.repo, &["kill", "--all"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["note"], "no hall processes running");

    let r = env.hall(&env.repo, &["kill", "--all", "4242"]);
    assert_eq!(r.code, 2, "--all and pids conflict: {}", r.stdout);

    let r = env.hall(&env.repo, &["-l", "status"]);
    assert_eq!(r.code, 2, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "USAGE");

    let r = env.hall(&env.repo, &["web", "-l", "-d"]);
    assert_eq!(r.code, 2, "{}", r.stdout);
}

#[test]
fn web_d_runs_detached_until_killed() {
    let env = Env::new();
    let r = env.hall(&env.repo, &["web", "-d", "--port", "0", "--no-open"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let pid = r.json["pid"].as_u64().unwrap();
    let _guard = KillOnDrop(&env, pid as u32);
    let url = r.json["url"].as_str().unwrap();
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    assert!(url.contains("#token="), "{url}");
    assert_eq!(
        r.json["next"],
        json!(["hall web -l", format!("hall web --kill {pid}")])
    );
    let log = PathBuf::from(r.json["log"].as_str().unwrap());
    assert!(log.exists());

    // It serves, after the command that started it returned.
    let port = r.json["port"].as_u64().unwrap() as u16;
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());

    let l = env.hall(&env.repo, &["web", "-l"]);
    assert_eq!(pids(&l.json), vec![pid], "{}", l.stdout);
    let i = &l.json["instances"][0];
    assert_eq!(i["url"], r.json["url"]);
    assert_eq!(i["log"], r.json["log"]);

    let k = env.hall(&env.repo, &["kill", "--all"]);
    assert_eq!(k.code, 0, "{}", k.stdout);
    assert_eq!(
        k.json["stopped"],
        json!([{"pid": pid, "kind": "web", "stopped": true}]),
        "{}",
        k.stdout
    );
    assert!(pids(&env.hall(&env.repo, &["ps"]).json).is_empty());
    assert!(!log.exists(), "the log goes with the server");
}

#[test]
fn web_d_reports_a_server_that_does_not_start() {
    let env = Env::new();
    // Take a port, then ask for it.
    let held = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = held.local_addr().unwrap().port().to_string();
    let r = env.hall(&env.repo, &["web", "-d", "--port", &port, "--no-open"]);
    assert_eq!(r.code, 1, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "COMMAND_FAILED");
    let msg = r.json["error"]["message"].as_str().unwrap();
    assert!(msg.contains("can't listen"), "{msg}");
    assert!(pids(&env.hall(&env.repo, &["ps"]).json).is_empty());
    let left: Vec<_> = std::fs::read_dir(run_dir(&env))
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_running_watch_is_listed_while_it_runs() {
    let env = Env::initialized();
    // An item in review keeps `watch` running: it is in motion.
    let (id, wt) = env.new_item("Watched");
    common::commit_file(&wt, "a.txt", "a");
    let r = env.hall(&wt, &["ready"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let mut child = env
        .std_cmd(&env.repo)
        .args(["watch", &id, "--timeout", "1m"])
        .env("HALL_WATCH_INTERVAL", "100ms")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id() as u64;
    let deadline = Instant::now() + Duration::from_secs(10);
    let listed = loop {
        let r = env.hall(&env.repo, &["ps", "--kind", "watch"]);
        if pids(&r.json) == vec![pid] || Instant::now() > deadline {
            break r;
        }
        hallmark::daemon::nap(Duration::from_millis(50));
    };
    assert_eq!(pids(&listed.json), vec![pid], "{}", listed.stdout);
    assert_eq!(listed.json["instances"][0]["kind"], "watch");
    let r = env.hall(&env.repo, &["kill", &pid.to_string()]);
    assert_eq!(r.json["stopped"][0]["stopped"], true, "{}", r.stdout);
    let _ = child.wait();
    assert!(pids(&env.hall(&env.repo, &["ps"]).json).is_empty());
}
