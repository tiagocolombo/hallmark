//! `hall trace` and `hall watch --trace` (ADR 0039): a run's stream read
//! back as steps. The runs are written here by hand, as the runner writes
//! them: stream-json lines, then one `hall_run_end` line once it exits.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{TimeZone, Utc};
use common::{Env, fixture, p};
use hallmark::trace::RunEnd;
use insta::assert_json_snapshot;
use serde_json::{Value, json};

const SESSION: &str = "91aedf44-f4cd-4efa-8e74-768d46a9dc75";

fn runs_dir(env: &Env, id: &str) -> PathBuf {
    env.repo.join(".git/hallmark/items").join(id).join("runs")
}

fn write_run(env: &Env, id: &str, name: &str, body: &str) -> PathBuf {
    // The fixture's run began here; a resume command cds only into a
    // directory that exists.
    std::fs::create_dir_all("/tmp/spike").unwrap();
    let dir = runs_dir(env, id);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    if name.ends_with(".jsonl") && !body.contains("hall_run_end") {
        hold(env, id);
    }
    path
}

/// The daemon holds the item, since before its runs began: a stream with no
/// end line is going, not interrupted.
fn hold(env: &Env, id: &str) {
    let dir = env.repo.join(".git/hallmark/leases");
    std::fs::create_dir_all(&dir).unwrap();
    let lease = json!({
        "item": id,
        "role": "fixer",
        "holder": "daemon:1",
        "acquired": "2026-10-12T21:00:00Z",
        "expires": "2099-01-01T00:00:00Z",
    });
    std::fs::write(dir.join(format!("{id}.json")), lease.to_string()).unwrap();
}

fn end_line(failure: Option<&str>) -> String {
    RunEnd {
        kind: "hall_run_end".into(),
        t: Utc.with_ymd_and_hms(2026, 10, 12, 21, 51, 52).unwrap(),
        duration_ms: 4_000,
        exit_code: Some(if failure.is_some() { 1 } else { 0 }),
        failure: failure.map(String::from),
    }
    .line()
}

/// The fixture stream, whole, and ended.
fn ended_stream() -> String {
    format!("{}{}", fixture("claude/stream.jsonl"), end_line(None))
}

/// The fixture stream up to its first tool result: still going.
fn running_stream() -> String {
    let text = fixture("claude/stream.jsonl");
    let cut = text.find("{\"type\":\"user\"").unwrap();
    text[..cut].to_string()
}

struct Traced {
    steps: Vec<Value>,
    reply: Value,
    code: i32,
}

/// Runs `hall --json <args>`: the steps, one per line, then the reply.
fn hall_lines(env: &Env, dir: &Path, args: &[&str]) -> Traced {
    let out = env
        .cmd(dir)
        .env("HALL_WATCH_INTERVAL", "100ms")
        .arg("--json")
        .args(args)
        .output()
        .unwrap();
    let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l}: {e}")))
        .collect();
    let (reply, steps) = lines.split_last().expect("a reply");
    Traced {
        steps: steps.to_vec(),
        reply: reply.clone(),
        code: out.status.code().unwrap_or(-1),
    }
}

fn kinds(steps: &[Value]) -> Vec<&str> {
    steps.iter().map(|s| s["kind"].as_str().unwrap()).collect()
}

fn scrub(env: &Env, v: &Value) -> Value {
    serde_json::from_str(&v.to_string().replace(&p(&env.root), "[root]")).unwrap()
}

#[test]
fn trace_prints_the_latest_run_as_steps_then_the_reply() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Usage page");
    write_run(
        &env,
        &id,
        "20261012T213749-reviewer.jsonl",
        &format!(
            "{}{}",
            fixture("claude/stream.jsonl"),
            end_line(Some("timed out"))
        ),
    );
    write_run(&env, &id, "20261012T215146-fixer.jsonl", &ended_stream());
    // Prompts and stderr next to the runs are not runs.
    write_run(&env, &id, "20261012T215146-fixer-r1.prompt.md", "fix it");

    // Inside the worktree the item resolves from the directory.
    let t = hall_lines(&env, &wt, &["trace"]);
    assert_eq!(t.code, 0, "{}", t.reply);
    assert_eq!(
        kinds(&t.steps),
        [
            "start", "tool", "result", "tool", "result", "text", "done", "end"
        ]
    );
    assert_eq!(t.reply["run"]["role"], "fixer");
    assert_eq!(t.reply["run"]["n"], 2);
    assert_eq!(t.reply["total"], 2);
    assert_eq!(t.reply["steps"], 8);
    assert_eq!(t.reply["run"]["ended"], true);
    assert!(t.reply.get("stopped").is_none(), "{}", t.reply);
    let mut lines = t.steps.clone();
    lines.push(t.reply.clone());
    assert_json_snapshot!("trace_lines", scrub(&env, &json!(lines)));
}

#[test]
fn role_and_run_pick_another_run_and_list_numbers_them() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Usage page");
    write_run(
        &env,
        &id,
        "20261012T213749-reviewer.jsonl",
        &format!(
            "{}{}",
            fixture("claude/stream.jsonl"),
            end_line(Some("timed out"))
        ),
    );
    write_run(&env, &id, "20261012T215146-fixer.jsonl", &ended_stream());
    write_run(
        &env,
        &id,
        "20261012T220000-reviewer.jsonl",
        &running_stream(),
    );

    let t = hall_lines(&env, &env.repo, &["trace", &id, "--role", "fixer"]);
    assert_eq!(t.reply["run"]["role"], "fixer");
    assert_eq!(t.reply["run"]["n"], 2);

    let t = hall_lines(&env, &env.repo, &["trace", &id, "--role", "reviewer"]);
    assert_eq!(t.reply["run"]["n"], 3, "the latest reviewer run");
    assert_eq!(t.reply["run"]["ended"], false);
    assert_eq!(kinds(&t.steps), ["start", "tool"]);
    assert_eq!(
        t.reply["next"][0],
        format!("hall trace {id} --run 3 --follow")
    );

    let t = hall_lines(&env, &env.repo, &["trace", &id, "--run", "1"]);
    assert_eq!(t.reply["run"]["role"], "reviewer");
    assert_eq!(t.reply["run"]["failure"], "timed out");
    assert_eq!(t.steps.last().unwrap()["error"], true);

    let t = hall_lines(&env, &env.repo, &["trace", &id, "--run", "9"]);
    assert_eq!(t.code, 2);
    assert_eq!(t.reply["error"]["code"], "USAGE");
    assert_eq!(
        t.reply["error"]["next"][0],
        format!("hall trace {id} --list")
    );

    let t = hall_lines(&env, &env.repo, &["trace", &id, "--list"]);
    assert_eq!(t.code, 0);
    assert!(t.steps.is_empty());
    let runs: Vec<(u64, &str, bool)> = t.reply["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["n"].as_u64().unwrap(),
                r["role"].as_str().unwrap(),
                r["ended"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        runs,
        [
            (1, "reviewer", true),
            (2, "fixer", true),
            (3, "reviewer", false)
        ]
    );

    let t = hall_lines(
        &env,
        &env.repo,
        &["trace", &id, "--list", "--role", "fixer"],
    );
    assert_eq!(t.reply["runs"].as_array().unwrap().len(), 1);
    assert_eq!(t.reply["runs"][0]["n"], 2, "numbered among all runs");
}

#[test]
fn an_older_json_result_reads_as_an_ended_run() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Usage page");
    write_run(
        &env,
        &id,
        "20261012T213749-reviewer.json",
        &fixture("claude/print_result.json"),
    );
    let t = hall_lines(&env, &env.repo, &["trace", &id]);
    assert_eq!(t.code, 0, "{}", t.reply);
    assert_eq!(kinds(&t.steps), ["done"]);
    assert_eq!(t.reply["run"]["streamed"], false);
    assert_eq!(t.reply["run"]["ended"], true);

    let t = hall_lines(&env, &env.repo, &["trace", &id, "--open"]);
    assert_eq!(t.code, 0, "{}", t.reply);
    let sid = t.reply["run"]["session_id"].as_str().unwrap();
    assert_eq!(
        t.reply["run"]["resume"],
        format!("claude --resume {sid}"),
        "an older result has no working directory"
    );
}

#[test]
fn open_gives_the_resume_command_only_once_the_run_has_ended() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Usage page");
    write_run(&env, &id, "20261012T215146-fixer.jsonl", &ended_stream());

    let t = hall_lines(&env, &env.repo, &["trace", &id, "--open"]);
    assert_eq!(t.code, 0, "{}", t.reply);
    assert!(t.steps.is_empty());
    let cmd = format!("cd /tmp/spike && claude --resume {SESSION}");
    assert_eq!(t.reply["run"]["resume"], cmd);
    assert_eq!(t.reply["next"][0], cmd);

    write_run(
        &env,
        &id,
        "20261012T220000-reviewer.jsonl",
        &running_stream(),
    );
    let t = hall_lines(&env, &env.repo, &["trace", &id, "--open"]);
    assert_eq!(t.code, 4);
    assert_eq!(t.reply["error"]["code"], "WRONG_STATE");
    assert!(
        t.reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("still going"),
        "{}",
        t.reply
    );
    assert_eq!(
        t.reply["error"]["next"][0],
        format!("hall trace {id} --run 2 --follow")
    );

    // The ended one still opens by its number.
    let t = hall_lines(&env, &env.repo, &["trace", &id, "--run", "1", "--open"]);
    assert_eq!(t.reply["run"]["resume"], cmd);
}

#[test]
fn follow_prints_steps_as_they_arrive_until_the_run_ends() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Usage page");
    let path = write_run(&env, &id, "20261012T215146-fixer.jsonl", &running_stream());
    let writer = std::thread::spawn(move || {
        use std::io::Write;
        hallmark::daemon::nap(Duration::from_millis(400));
        let rest = ended_stream()[running_stream().len()..].to_string();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(rest.as_bytes()).unwrap();
    });
    let t = hall_lines(
        &env,
        &env.repo,
        &["trace", &id, "--follow", "--timeout", "20s"],
    );
    writer.join().unwrap();
    assert_eq!(t.code, 0, "{}", t.reply);
    assert_eq!(t.reply["stopped"], "ended");
    assert_eq!(
        kinds(&t.steps),
        [
            "start", "tool", "result", "tool", "result", "text", "done", "end"
        ],
        "each step once"
    );
    assert_eq!(t.reply["run"]["ended"], true);
    assert_eq!(
        t.reply["next"][0],
        format!("hall trace {id} --run 1 --open")
    );
}

#[test]
fn follow_stops_at_the_timeout_while_the_run_is_going() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Usage page");
    write_run(&env, &id, "20261012T215146-fixer.jsonl", &running_stream());
    let t = hall_lines(
        &env,
        &env.repo,
        &["trace", &id, "--follow", "--timeout", "1s"],
    );
    assert_eq!(t.code, 0, "{}", t.reply);
    assert_eq!(t.reply["stopped"], "timeout");
    assert_eq!(t.reply["run"]["ended"], false);
    assert_eq!(kinds(&t.steps), ["start", "tool"]);
}

#[test]
fn a_run_without_a_lease_is_going_while_its_writer_holds_the_lock() {
    // A resume or distill run: nothing leases the item, but the runner
    // holds the stream's lock for as long as it lives.
    let env = Env::initialized();
    let (id, _) = env.new_item("Usage page");
    let dir = runs_dir(&env, &id);
    std::fs::create_dir_all(&dir).unwrap();
    let stream = dir.join("20261012T215146-resume.jsonl");
    std::fs::write(&stream, running_stream()).unwrap();
    let lock = std::fs::File::create(hallmark::runner::lock_path(&stream)).unwrap();
    fs4::FileExt::try_lock(&lock).unwrap();
    // A newer run exists too: the older one is still going.
    std::fs::write(dir.join("20261012T215147-librarian.jsonl"), ended_stream()).unwrap();

    let t = hall_lines(&env, &env.repo, &["trace", &id, "--list"]);
    assert_ne!(t.reply["runs"][0]["interrupted"], true, "{}", t.reply);
    assert_eq!(t.reply["runs"][0]["ended"], false, "{}", t.reply);
    assert!(t.reply["runs"][0].get("resume").is_none(), "{}", t.reply);
    let t = hall_lines(
        &env,
        &env.repo,
        &["trace", &id, "--run", "1", "--follow", "--timeout", "1s"],
    );
    assert_eq!(t.reply["stopped"], "timeout", "{}", t.reply);

    // The writer dies: the OS frees the lock.
    drop(lock);
    let t = hall_lines(&env, &env.repo, &["trace", &id, "--list"]);
    assert_eq!(t.reply["runs"][0]["interrupted"], true, "{}", t.reply);
}

#[test]
fn a_run_the_daemon_left_without_an_end_reads_as_interrupted() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Usage page");
    // No live lease, and no end line: nothing is writing it any more.
    let dir = runs_dir(&env, &id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("20261012T215146-fixer.jsonl"), running_stream()).unwrap();

    let t = hall_lines(&env, &env.repo, &["trace", &id, "--follow"]);
    assert_eq!(t.code, 0, "{}", t.reply);
    assert_eq!(t.reply["stopped"], "interrupted", "it doesn't wait");
    assert_eq!(t.reply["run"]["interrupted"], true);

    let t = hall_lines(&env, &env.repo, &["trace", &id, "--list"]);
    assert_eq!(t.reply["runs"][0]["interrupted"], true);

    let t = hall_lines(&env, &env.repo, &["trace", &id, "--open"]);
    assert_eq!(t.code, 0, "{}", t.reply);
    assert!(
        t.reply["run"]["resume"]
            .as_str()
            .unwrap()
            .ends_with(&format!("claude --resume {SESSION}")),
        "{}",
        t.reply
    );
}

#[test]
fn an_item_without_runs_says_so() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Usage page");
    let t = hall_lines(&env, &env.repo, &["trace", &id]);
    assert_eq!(t.code, 0, "{}", t.reply);
    assert!(t.steps.is_empty());
    assert!(t.reply.get("run").is_none(), "{}", t.reply);
    assert_eq!(t.reply["total"], 0);
    assert_eq!(t.reply["next"][0], format!("hall status {id}"));

    let t = hall_lines(&env, &env.repo, &["trace", &id, "--follow"]);
    assert_eq!(t.code, 0, "nothing to follow: it returns at once");

    let t = hall_lines(&env, &env.repo, &["trace", &id, "--open"]);
    assert_eq!(t.code, 3);
    assert_eq!(t.reply["error"]["code"], "ITEM_NOT_FOUND");
    assert!(!t.reply["error"]["next"].as_array().unwrap().is_empty());

    let t = hall_lines(&env, &env.repo, &["trace", &id, "--run", "1"]);
    assert_eq!(t.code, 2);
    assert_eq!(t.reply["error"]["code"], "USAGE");
}

#[test]
fn trace_schemas_are_served() {
    let env = Env::new();
    let r = env.hall(&env.root, &["schema", "trace"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let schema = &r.json["schema"];
    let required = schema["required"].as_array().unwrap();
    for k in ["id", "total", "steps", "next"] {
        assert!(required.iter().any(|r| r == k), "{k}: {schema}");
    }
    let r = env.hall(&env.root, &["schema", "trace-line"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert!(r.json["schema"]["properties"]["kind"].is_object());
}

#[test]
fn watch_trace_prints_the_running_runs_steps() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Usage page");
    // In development, watch stops after its first line.
    let watch = |trace: bool| {
        let mut args = vec!["watch", id.as_str(), "--timeout", "2s"];
        if trace {
            args.push("--trace");
        }
        let t = hall_lines(&env, &env.repo, &args);
        assert_eq!(t.code, 0, "{}", t.reply);
        t.steps
    };

    write_run(&env, &id, "20261012T215146-fixer.jsonl", &ended_stream());
    let lines = watch(true);
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].get("steps").is_none(),
        "a run that had ended before watching is not shown: {}",
        lines[0]
    );

    write_run(
        &env,
        &id,
        "20261012T220000-reviewer.jsonl",
        &running_stream(),
    );
    let lines = watch(true);
    assert_eq!(lines.len(), 1);
    assert_eq!(
        kinds(lines[0]["steps"].as_array().unwrap()),
        ["start", "tool"]
    );

    let lines = watch(false);
    assert!(lines[0].get("steps").is_none(), "only with --trace");
}

#[test]
fn watch_trace_prints_each_step_once() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Usage page");
    assert_eq!(env.hall(&wt, &["ready"]).code, 0);
    write_run(
        &env,
        &id,
        "20261012T220000-reviewer.jsonl",
        &running_stream(),
    );
    // The assayer holds it: the now block follows the run.
    let lease = env.repo.join(format!(".git/hallmark/leases/{id}.json"));
    let held = std::fs::read_to_string(&lease).unwrap();
    std::fs::write(&lease, held.replace("\"fixer\"", "\"assayer\"")).unwrap();
    let t = hall_lines(
        &env,
        &env.repo,
        &["watch", id.as_str(), "--trace", "--timeout", "1s"],
    );
    assert_eq!(t.code, 0, "{}", t.reply);
    // Without --trace the now block's log carries the run's last steps.
    let plain = hall_lines(&env, &env.repo, &["watch", id.as_str(), "--timeout", "1s"]);
    let logged: Vec<&Value> = plain
        .steps
        .iter()
        .flat_map(|l| l["log"].as_array().into_iter().flatten())
        .collect();
    assert!(
        logged.iter().any(|l| l.as_str().unwrap().contains("Read")),
        "the log shows the run: {logged:?}"
    );
    // With it, the steps print as steps and the log leaves them out.
    let steps: Vec<&Value> = t
        .steps
        .iter()
        .flat_map(|l| l["steps"].as_array().into_iter().flatten())
        .collect();
    assert_eq!(
        kinds(&steps.into_iter().cloned().collect::<Vec<_>>()),
        ["start", "tool"]
    );
    for l in &t.steps {
        for line in l["log"].as_array().into_iter().flatten() {
            assert!(
                !line.as_str().unwrap().contains("Read"),
                "a step printed twice: {line}"
            );
        }
    }
}
