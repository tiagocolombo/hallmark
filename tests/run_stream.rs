//! Every headless run streams into the item's `runs/` (ADR 0039): one
//! `<stamp>-<role>.jsonl` per run, ended by the runner's `hall_run_end`
//! line, read back through `hallmark::trace`. The fake runner stands in for
//! `claude`, except where a shell script does (Unix).

mod common;

use std::path::{Path, PathBuf};

use common::Env;
use hallmark::item::ItemId;
use hallmark::repo::Repo;
use hallmark::runner::{FakeRunner, Metered, RunRequest, run_metered};
use hallmark::trace::{self, RunFile};
use serde_json::json;

fn runs_dir(env: &Env, id: &str) -> PathBuf {
    env.repo.join(".git/hallmark/items").join(id).join("runs")
}

fn names(dir: &Path) -> Vec<String> {
    let mut n: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    // The writer's `.lock` sidecar is not a run file.
    n.retain(|f| !f.ends_with(".lock"));
    n.sort();
    n
}

fn reviewer(cwd: &Path) -> RunRequest {
    RunRequest {
        role: "reviewer".into(),
        room: "assay".into(),
        prompt: "Review".into(),
        cwd: cwd.to_path_buf(),
        ..Default::default()
    }
}

#[test]
fn a_metered_run_streams_into_the_items_runs() {
    let env = Env::initialized();
    let (id, wt) = env.new_item("Usage page");
    let script = env.script(json!([{"role": "reviewer", "result": "fine"}]));
    let repo = Repo::discover(&env.repo).unwrap();
    let item: ItemId = id.parse().unwrap();
    let out = run_metered(&repo, &FakeRunner::new(script), Some(item), &reviewer(&wt)).unwrap();
    let Metered::Done(o) = out else {
        panic!("{out:?}")
    };
    assert_eq!(o.session_id(), Some("fake-session"));

    // One stream, and no `.json` result beside it.
    let dir = runs_dir(&env, &id);
    let n = names(&dir);
    assert_eq!(n.len(), 1, "{n:?}");
    assert!(n[0].ends_with("-reviewer.jsonl"), "{n:?}");
    let runs: Vec<RunFile> = trace::runs_in(&dir).unwrap();
    assert_eq!(runs[0].role, "reviewer");
    assert!(runs[0].streamed);
    let (steps, info) = trace::read(&runs[0].path).unwrap();
    assert!(info.ended && info.failure.is_none(), "{info:?}");
    assert_eq!(info.session_id.as_deref(), Some("fake-session"));
    assert_eq!(info.cwd.as_deref(), Some(wt.as_path()));
    assert_eq!(steps.last().unwrap().kind, trace::StepKind::End);

    // The request the runner saw carried no stream path into its log.
    assert!(env.fake_calls()[0].get("stream").is_none());
}

#[test]
fn a_run_without_an_item_streams_nowhere() {
    let env = Env::initialized();
    let script = env.script(json!([{"role": "reviewer"}]));
    let repo = Repo::discover(&env.repo).unwrap();
    let out = run_metered(&repo, &FakeRunner::new(script), None, &reviewer(&env.repo)).unwrap();
    assert!(matches!(out, Metered::Done(_)), "{out:?}");
    let items = env.repo.join(".git/hallmark/items");
    assert!(names(&items).is_empty(), "{:?}", names(&items));
}

#[test]
fn the_fallback_retry_streams_into_a_file_of_its_own() {
    // The model is unavailable (an API error before any model ran), so the
    // runner retries with the fallback: a second run, a second session, a
    // second file, stamped after the first.
    let env = Env::initialized();
    let (id, wt) = env.new_item("Usage page");
    let script = env.script(json!([
        {"role": "reviewer", "session_id": "first", "print": {
            "type": "result", "is_error": true, "subtype": "error_during_execution",
            "terminal_reason": "api_error", "modelUsage": {}, "session_id": "first",
            "result": "[claude-code:unrecognized_model] claude-x"
        }},
        {"role": "reviewer", "session_id": "second", "result": "fine"}
    ]));
    let repo = Repo::discover(&env.repo).unwrap();
    let item: ItemId = id.parse().unwrap();
    let mut req = reviewer(&wt);
    req.model = Some("claude-x".into());
    req.fallback_model = Some("claude-y".into());
    let out = run_metered(&repo, &FakeRunner::new(script), Some(item), &req).unwrap();
    assert!(matches!(out, Metered::Done(_)), "{out:?}");
    let calls = env.fake_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1]["model"], "claude-y");

    let runs = trace::runs_in(&runs_dir(&env, &id)).unwrap();
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert!(runs[0].started < runs[1].started, "{runs:?}");
    let (_, first) = trace::read(&runs[0].path).unwrap();
    assert!(first.ended);
    assert_eq!(first.session_id.as_deref(), Some("first"));
    assert_eq!(
        first.failure.as_deref(),
        Some("model claude-x is unavailable")
    );
    let (_, second) = trace::read(&runs[1].path).unwrap();
    assert!(second.ended && second.failure.is_none(), "{second:?}");
    assert_eq!(second.session_id.as_deref(), Some("second"));
}

/// The real runner, with a shell script for `claude`: stderr is kept next to
/// the stream, under the same stamp.
#[cfg(unix)]
#[test]
fn stderr_is_kept_next_to_its_stream() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::initialized();
    let (id, wt) = env.new_item("Usage page");
    let bin = env.root.join("claude");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\ncat '{}'\necho 'a warning' >&2\n",
            common::p(Path::new(&format!(
                "{}/tests/fixtures/claude/stream.jsonl",
                env!("CARGO_MANIFEST_DIR")
            )))
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let runner = hallmark::runner::ClaudeRunner {
        bin: bin.to_string_lossy().into_owned(),
    };
    let repo = Repo::discover(&env.repo).unwrap();
    let item: ItemId = id.parse().unwrap();
    let out = run_metered(&repo, &runner, Some(item), &reviewer(&wt)).unwrap();
    assert!(matches!(out, Metered::Done(_)), "{out:?}");
    let n = names(&runs_dir(&env, &id));
    assert_eq!(n.len(), 2, "{n:?}");
    let stem = n[0].strip_suffix(".jsonl").unwrap();
    assert_eq!(n[1], format!("{stem}.stderr"));
    let stderr = std::fs::read_to_string(runs_dir(&env, &id).join(&n[1])).unwrap();
    assert_eq!(stderr.trim(), "a warning");
    let (_, info) = trace::read(&runs_dir(&env, &id).join(&n[0])).unwrap();
    assert!(info.ended && info.failure.is_none(), "{info:?}");
}
