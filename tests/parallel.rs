//! Each phase runs several items at once, per `[parallel]` (ADR 0026).

mod common;

use std::path::{Path, PathBuf};

use common::{Env, commit_file, git};
use serde_json::json;

/// Sets the gates (a top-level key) and appends a `[parallel]` table.
fn set_config(repo: &Path, gates: &str, parallel: &str) {
    let cfg = repo.join(".hallmark.toml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    let kept: Vec<&str> = text.lines().filter(|l| !l.starts_with("gates")).collect();
    std::fs::write(
        &cfg,
        format!("{gates}\n{}\n\n[parallel]\n{parallel}\n", kept.join("\n")),
    )
    .unwrap();
    git(repo, &["commit", "-qam", "config"]);
}

fn ready(env: &Env, title: &str) -> (String, PathBuf) {
    let (id, wt) = env.new_item(title);
    commit_file(&wt, "work.txt", title);
    assert_eq!(env.hall(&wt, &["ready"]).code, 0);
    (id, wt)
}

#[test]
fn each_phase_takes_up_to_its_parallel_count() {
    let env = Env::initialized();
    set_config(&env.repo, "gates = []", "checks = 2\nreview = 1");
    let ids: Vec<String> = (1..=3)
        .map(|n| ready(&env, &format!("Item {n}")).0)
        .collect();
    let claim = |holder: &str| {
        env.hall(
            &env.repo,
            &["next", "--role", "assayer", "--holder", holder],
        )
        .json["work"]["id"]
            .as_str()
            .map(String::from)
    };
    // Two checks at once, oldest first; the third waits for a slot.
    assert_eq!(claim("a").as_deref(), Some(ids[0].as_str()));
    assert_eq!(claim("b").as_deref(), Some(ids[1].as_str()));
    assert_eq!(claim("c"), None, "checks = 2");
    // A slot frees up: the oldest unclaimed item takes it.
    env.hall(
        &env.repo,
        &[
            "next",
            "--role",
            "assayer",
            "--holder",
            "a",
            "--item",
            &ids[0],
            "--release",
        ],
    );
    assert_eq!(claim("c").as_deref(), Some(ids[0].as_str()));
    assert_eq!(claim("d"), None, "still two at once");
}

// Unix only: the gate sleeps, and the timing shows the checks overlap.
#[cfg(unix)]
#[test]
fn the_daemon_runs_checks_side_by_side() {
    let env = Env::initialized();
    set_config(&env.repo, "gates = [\"sleep 3\"]", "checks = 2");
    let (a, _) = ready(&env, "First");
    let (b, _) = ready(&env, "Second");
    let script = env.script(json!([]));
    let started = std::time::Instant::now();
    let out = env
        .cmd(&env.root)
        .env("HALL_RUNNER", format!("fake:{}", script.display()))
        .env("HALL_DAEMON_POLL", "200ms")
        .args(["daemon", "start"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let state = |id: &str| {
        env.hall(&env.repo, &["status", id]).json["state"]
            .as_str()
            .unwrap_or("")
            .to_string()
    };
    let deadline = started + std::time::Duration::from_secs(12);
    while (state(&a) == "checks" || state(&b) == "checks") && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let took = started.elapsed();
    let _ = env.cmd(&env.root).args(["daemon", "stop"]).output();
    assert_ne!(state(&a), "checks");
    assert_ne!(state(&b), "checks");
    // One after the other would take over 6s; side by side, about 3.
    assert!(
        took < std::time::Duration::from_millis(5500),
        "took {took:?}"
    );
    // Each ran in its own slot of the assay pool.
    let parent = env.repo.parent().unwrap();
    assert!(parent.join("app-assay").is_dir());
    assert!(parent.join("app-assay-2").is_dir());
}
