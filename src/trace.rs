//! A headless run, step by step (ADR 0041). The runner streams every
//! `claude -p` run as stream-json into the item's `runs/` directory, one
//! file per run, and appends one `hall_run_end` line when the process has
//! exited. This module finds those files and turns their lines into steps:
//! what the agent said, each tool call and its result, and how it ended.
//! `hall trace`, `hall watch --trace`, the terminal app and the now block
//! all read it.
//!
//! ```text
//! .git/hallmark/items/H-0042/runs/20261012T213749-reviewer.jsonl
//! ```
//!
//! Runs from before streaming left one `<stamp>-<role>.json` result; they
//! read as a run with only its end.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::item::ItemId;
use crate::output::clip;
use crate::repo::Repo;

/// The `type` of the line the runner appends when the process has exited.
pub const END: &str = "hall_run_end";

/// How much of a tool's input or result a step keeps.
const CLIP: usize = 200;

/// The line the runner appends after the process exits, timed out or not.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunEnd {
    /// `hall_run_end`.
    #[serde(rename = "type")]
    pub kind: String,
    pub t: DateTime<Utc>,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// `Failure::describe()` when the run failed; none when it succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

impl RunEnd {
    pub fn new(duration_ms: u64, exit_code: Option<i32>, failure: Option<String>) -> RunEnd {
        RunEnd {
            kind: END.into(),
            t: Utc::now(),
            duration_ms,
            exit_code,
            failure,
        }
    }

    /// One JSON line, newline included.
    pub fn line(&self) -> String {
        format!("{}\n", serde_json::to_string(self).unwrap_or_default())
    }
}

/// One run's file in `runs/`.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct RunFile {
    pub path: PathBuf,
    /// reviewer, fixer, intake, librarian, resume…
    pub role: String,
    pub started: DateTime<Utc>,
    /// A `.jsonl` stream; false for an older `.json` result.
    pub streamed: bool,
}

/// Whether the run at `at` in `runs` is a stream that stopped without its end
/// line: its writer's lock is free. A stream with no lock (older) falls back
/// to: a newer run exists, or no live lease began before it. Sets
/// `info.interrupted`.
pub fn mark_interrupted(
    hall: &crate::journal::Hall,
    id: ItemId,
    runs: &[RunFile],
    at: usize,
    info: &mut RunInfo,
) {
    if info.ended {
        return;
    }
    let Some(run) = runs.get(at) else {
        return;
    };
    // Run files are stamped to the second.
    let began = run.started + chrono::Duration::seconds(1);
    // The writer's lock decides when there is one: a run made without an
    // item lease (resume, distill) is still going while it is held.
    info.interrupted = match crate::runner::writer_gone(&run.path) {
        Some(true) => {
            // The end line is written before the lock is freed, so a run
            // that ended after the caller last read shows its end now.
            let mut tail = Tail::default();
            if tail.poll(&run.path).is_ok() && tail.info.ended {
                *info = tail.info;
                false
            } else {
                true
            }
        }
        Some(false) => false,
        None => {
            at + 1 < runs.len()
                || crate::lease::current(hall, id).is_none_or(|l| l.acquired > began)
        }
    };
}

/// The runs of an item, oldest first. Prompts and stderr files are not runs.
pub fn runs(repo: &Repo, id: ItemId) -> Result<Vec<RunFile>> {
    runs_in(&repo.hall.item_runs(id))
}

pub fn runs_in(dir: &Path) -> Result<Vec<RunFile>> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut out: Vec<RunFile> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| run_file(&e.path()))
        .collect();
    out.sort_by(|a, b| (a.started, &a.path).cmp(&(b.started, &b.path)));
    Ok(out)
}

/// `<stamp>-<role>.jsonl` or `<stamp>-<role>.json`.
fn run_file(path: &Path) -> Option<RunFile> {
    let name = path.file_name()?.to_str()?;
    let (stem, streamed) = if let Some(s) = name.strip_suffix(".jsonl") {
        (s, true)
    } else {
        (name.strip_suffix(".json")?, false)
    };
    let (stamp, role) = stem.split_once('-')?;
    let started = NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%S")
        .ok()?
        .and_utc();
    if role.is_empty() {
        return None;
    }
    Some(RunFile {
        path: path.to_path_buf(),
        role: role.into(),
        started,
        streamed,
    })
}

/// What a step is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    /// The run began: model and working directory.
    Start,
    /// What the agent said.
    Text,
    /// A tool call: `name` and a line of its input.
    Tool,
    /// A tool's result, clipped.
    Result,
    /// Claude's result: done, or failed.
    Done,
    /// The process exited (the runner's `hall_run_end`).
    End,
}

/// One thing that happened in a run.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Step {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub t: Option<DateTime<Utc>>,
    pub kind: StepKind,
    /// The tool's name, for `tool`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// One line.
    pub text: String,
    /// A failed tool result or run.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub error: bool,
    /// Inside a subagent (`parent_tool_use_id` is set).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub nested: bool,
}

impl Step {
    fn new(t: Option<DateTime<Utc>>, kind: StepKind, text: String) -> Step {
        Step {
            t,
            kind,
            name: None,
            text,
            error: false,
            nested: false,
        }
    }

    /// For terminals, in local time: `21:51:47  ▸ Bash  wc -c note.txt`.
    pub fn render(&self) -> String {
        self.render_in(&chrono::Local)
    }

    pub fn render_in<Tz: TimeZone>(&self, tz: &Tz) -> String
    where
        Tz::Offset: std::fmt::Display,
    {
        let t = self
            .t
            .map(|t| t.with_timezone(tz).format("%H:%M:%S").to_string())
            .unwrap_or_else(|| " ".repeat(8));
        let indent = if self.nested { "  " } else { "" };
        match self.kind {
            StepKind::Start => format!("{t}  {indent}● {}", self.text),
            StepKind::Text => format!("{t}  {indent}» {}", self.text),
            StepKind::Tool => format!(
                "{t}  {indent}▸ {}  {}",
                self.name.as_deref().unwrap_or("tool"),
                self.text
            ),
            StepKind::Result => {
                let mark = if self.error { "✗" } else { "↳" };
                format!("{}  {indent}  {mark} {}", " ".repeat(8), self.text)
            }
            StepKind::Done | StepKind::End => {
                let mark = if self.error { "✗" } else { "■" };
                format!("{t}  {indent}{mark} {}", self.text)
            }
        }
    }
}

/// What the stream says about the run as a whole.
#[derive(Debug, Clone, Default, PartialEq, Serialize, JsonSchema)]
pub struct RunInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Where it ran; `claude --resume` must start there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The process exited: the runner's `hall_run_end`, or an older
    /// `.json` result. A run without it is still going, or the daemon died.
    pub ended: bool,
    /// No end line, and nothing is writing the stream any more: the daemon
    /// died or was stopped mid-run. Set by [`mark_interrupted`], not read
    /// from the stream.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub interrupted: bool,
    /// The failure, when it ended with one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

impl RunInfo {
    /// The command that opens the run in Claude Code, once it has ended.
    pub fn resume_command(&self) -> Option<String> {
        let sid = self.session_id.as_deref()?;
        if !self.ended && !self.interrupted {
            return None;
        }
        Some(match &self.cwd {
            // A reviewer's checkout is removed when its run ends; Claude
            // finds the session by ID from any directory.
            Some(c) if c.is_dir() => format!("cd {} && claude --resume {sid}", quote(c)),
            _ => format!("claude --resume {sid}"),
        })
    }

    /// Folds one line into what is known.
    pub fn observe(&mut self, v: &Value) {
        if self.session_id.is_none()
            && let Some(s) = v.get("session_id").and_then(Value::as_str)
        {
            self.session_id = Some(s.into());
        }
        match v.get("type").and_then(Value::as_str) {
            Some("system") if v.get("subtype").and_then(Value::as_str) == Some("init") => {
                self.cwd = v.get("cwd").and_then(Value::as_str).map(PathBuf::from);
                self.model = v.get("model").and_then(Value::as_str).map(String::from);
            }
            Some(END) => {
                self.ended = true;
                self.failure = v.get("failure").and_then(Value::as_str).map(String::from);
            }
            _ => {}
        }
    }
}

fn quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    if s.contains(char::is_whitespace) {
        format!("\"{s}\"")
    } else {
        s.into_owned()
    }
}

/// The steps in one stream-json line (none for most system lines).
pub fn steps(line: &str) -> Vec<Step> {
    let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
        return Vec::new();
    };
    steps_of(&v)
}

fn steps_of(v: &Value) -> Vec<Step> {
    let t = v
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));
    let nested = v.get("parent_tool_use_id").is_some_and(|p| !p.is_null());
    let mut out = match v.get("type").and_then(Value::as_str) {
        Some("system") if v.get("subtype").and_then(Value::as_str) == Some("init") => {
            let model = v.get("model").and_then(Value::as_str).unwrap_or("?");
            let cwd = v.get("cwd").and_then(Value::as_str).unwrap_or("?");
            vec![Step::new(t, StepKind::Start, format!("{model} in {cwd}"))]
        }
        Some("assistant") | Some("user") => content(v, t),
        Some("result") => vec![done(v, t)],
        Some(END) => vec![end(v)],
        _ => Vec::new(),
    };
    for s in &mut out {
        s.nested = nested;
    }
    out
}

fn content(v: &Value, t: Option<DateTime<Utc>>) -> Vec<Step> {
    let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(|b| match b.get("type").and_then(Value::as_str)? {
            "text" => {
                let text = b.get("text").and_then(Value::as_str)?;
                (!text.trim().is_empty())
                    .then(|| Step::new(t, StepKind::Text, clip(text, CLIP * 2)))
            }
            "tool_use" => {
                let mut s = Step::new(t, StepKind::Tool, tool_input(b.get("input")));
                s.name = b.get("name").and_then(Value::as_str).map(String::from);
                Some(s)
            }
            "tool_result" => {
                let mut s = Step::new(t, StepKind::Result, clip(&result_text(b), CLIP));
                s.error = b.get("is_error").and_then(Value::as_bool) == Some(true);
                Some(s)
            }
            // Thinking arrives empty in headless runs; nothing to show.
            _ => None,
        })
        .collect()
}

/// The line a person would want: the description, else the command, path,
/// pattern, else the input itself.
fn tool_input(input: Option<&Value>) -> String {
    let Some(i) = input else {
        return String::new();
    };
    for k in [
        "description",
        "command",
        "file_path",
        "notebook_path",
        "pattern",
        "url",
        "query",
        "prompt",
    ] {
        if let Some(s) = i.get(k).and_then(Value::as_str) {
            return clip(s, CLIP);
        }
    }
    clip(&i.to_string(), CLIP)
}

fn result_text(b: &Value) -> String {
    match b.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn done(v: &Value, t: Option<DateTime<Utc>>) -> Step {
    let error = v.get("is_error").and_then(Value::as_bool) == Some(true)
        || v.get("subtype").and_then(Value::as_str) != Some("success");
    let secs = v.get("duration_ms").and_then(Value::as_u64).unwrap_or(0) / 1000;
    let cost = v
        .get("total_cost_usd")
        .and_then(Value::as_f64)
        .map(|c| format!(", ${c:.2}"))
        .unwrap_or_default();
    let what = if error {
        let sub = v.get("subtype").and_then(Value::as_str).unwrap_or("error");
        format!("failed ({sub})")
    } else {
        "done".into()
    };
    let mut s = Step::new(t, StepKind::Done, format!("{what} in {secs}s{cost}"));
    s.error = error;
    s
}

fn end(v: &Value) -> Step {
    let e: RunEnd = serde_json::from_value(v.clone()).unwrap_or_default();
    let secs = e.duration_ms / 1000;
    let (text, error) = match &e.failure {
        Some(f) => (format!("run failed after {secs}s: {f}"), true),
        None => (format!("run ended after {secs}s"), false),
    };
    let mut s = Step::new(Some(e.t), StepKind::End, text);
    s.error = error;
    s
}

/// Reads a run file a piece at a time, for following it while it grows.
/// Only whole lines are consumed; a half-written last line waits.
#[derive(Debug, Clone, Default)]
pub struct Tail {
    pub offset: u64,
    pub info: RunInfo,
}

impl Tail {
    /// The steps written since the last call.
    pub fn poll(&mut self, path: &Path) -> Result<Vec<Step>> {
        let mut f = fs::File::open(path)?;
        let len = f.metadata()?.len();
        if len < self.offset {
            // Rewritten: start over.
            *self = Tail::default();
        }
        f.seek(SeekFrom::Start(self.offset))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        let streamed = path.extension().and_then(|e| e.to_str()) == Some("jsonl");
        let upto = if streamed {
            match buf.iter().rposition(|&b| b == b'\n') {
                Some(i) => i + 1,
                None => return Ok(Vec::new()),
            }
        } else {
            buf.len()
        };
        self.offset += upto as u64;
        let text = String::from_utf8_lossy(&buf[..upto]);
        if !streamed {
            // An older `.json` result: one object, and the run is over.
            let Ok(v) = serde_json::from_str::<Value>(text.trim()) else {
                return Ok(Vec::new());
            };
            self.info.observe(&v);
            self.info.ended = true;
            return Ok(vec![done(&v, None)]);
        }
        let mut out = Vec::new();
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            self.info.observe(&v);
            out.extend(steps_of(&v));
        }
        Ok(out)
    }
}

/// A whole run: its steps and what is known about it.
pub fn read(path: &Path) -> Result<(Vec<Step>, RunInfo)> {
    let mut tail = Tail::default();
    let steps = tail.poll(path)?;
    Ok((steps, tail.info))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> String {
        fs::read_to_string(format!(
            "{}/tests/fixtures/claude/stream.jsonl",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn a_stream_reads_as_steps() {
        let all: Vec<Step> = fixture().lines().flat_map(steps).collect();
        let kinds: Vec<StepKind> = all.iter().map(|s| s.kind).collect();
        use StepKind::*;
        assert_eq!(kinds, [Start, Tool, Result, Tool, Result, Text, Done]);
        assert_eq!(all[1].name.as_deref(), Some("Read"));
        assert_eq!(all[3].text, "Count bytes in note.txt");
        assert_eq!(all[4].text, "6 note.txt");
        assert!(all[5].text.contains("6 bytes"));
        assert!(!all[6].error);
        insta::assert_snapshot!(
            all.iter()
                .map(|s| s.render_in(&Utc))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn a_tail_takes_whole_lines_and_learns_the_session() {
        // The fixture's run began in /tmp/spike. A resume command cds only
        // into a directory that exists, so the run moves to one this test
        // owns: a sandboxed run may write only its own temp dir.
        let dir = tempfile::tempdir().unwrap();
        let spike = dir.path().join("spike");
        fs::create_dir_all(&spike).unwrap();
        let cwd = crate::paths::display(&spike);
        let p = dir.path().join("20261012T215146-reviewer.jsonl");
        let text = fixture().replace("/tmp/spike", &cwd);
        let (first, _) = text.split_at(text.find("\"type\":\"user\"").unwrap());
        fs::write(&p, first).unwrap();
        let mut tail = Tail::default();
        let a = tail.poll(&p).unwrap();
        assert_eq!(a.len(), 2, "start and the first tool call");
        assert_eq!(tail.info.cwd.as_deref(), Some(Path::new(&cwd)));
        assert!(!tail.info.ended);
        assert_eq!(tail.info.resume_command(), None, "not while it runs");

        let end = RunEnd::new(4_000, Some(0), None).line();
        fs::write(&p, format!("{text}{end}")).unwrap();
        let b = tail.poll(&p).unwrap();
        assert_eq!(b.first().map(|s| s.kind), Some(StepKind::Result));
        assert_eq!(b.last().map(|s| s.kind), Some(StepKind::End));
        assert!(tail.info.ended);
        assert_eq!(
            tail.info.resume_command(),
            Some(format!(
                "cd {cwd} && claude --resume 91aedf44-f4cd-4efa-8e74-768d46a9dc75"
            ))
        );
    }

    #[test]
    fn a_resume_command_only_enters_a_directory_that_exists() {
        // A reviewer's checkout is removed once its run ends.
        let dir = tempfile::tempdir().unwrap();
        let mut info = RunInfo {
            session_id: Some("s1".into()),
            cwd: Some(dir.path().join("app-review-H-0042")),
            ended: true,
            ..RunInfo::default()
        };
        assert_eq!(info.resume_command().as_deref(), Some("claude --resume s1"));
        info.cwd = Some(dir.path().to_path_buf());
        assert!(
            info.resume_command()
                .is_some_and(|c| c.starts_with("cd ") && c.ends_with("&& claude --resume s1"))
        );
    }

    #[test]
    fn a_failed_end_is_an_error() {
        let line = RunEnd::new(600_000, None, Some("timed out".into())).line();
        let s = steps(&line);
        assert_eq!(s.len(), 1);
        assert!(s[0].error);
        assert_eq!(s[0].text, "run failed after 600s: timed out");
    }

    #[test]
    fn runs_are_streams_and_older_results() {
        let dir = tempfile::tempdir().unwrap();
        for f in [
            "20261012T215146-fixer.jsonl",
            "20261012T213749-reviewer-r1.prompt.md",
            "20261012T213749-reviewer.json",
            "20261012T213749-reviewer.stderr",
            "notes.txt",
        ] {
            fs::write(dir.path().join(f), "").unwrap();
        }
        let r = runs_in(dir.path()).unwrap();
        let got: Vec<(&str, bool)> = r.iter().map(|r| (r.role.as_str(), r.streamed)).collect();
        assert_eq!(got, [("reviewer", false), ("fixer", true)]);
        assert!(runs_in(&dir.path().join("missing")).unwrap().is_empty());
    }

    #[test]
    fn an_older_result_reads_as_ended() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("20261012T213749-reviewer.json");
        let raw = fs::read_to_string(format!(
            "{}/tests/fixtures/claude/print_result.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        fs::write(&p, raw).unwrap();
        let (steps, info) = read(&p).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].kind, StepKind::Done);
        assert!(info.ended);
        assert!(info.session_id.is_some());
    }

    #[test]
    fn a_run_that_ends_after_it_was_read_is_not_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("20261012T215146-reviewer.jsonl");
        let text = fixture();
        fs::write(&p, &text).unwrap();
        // Read while it ran: no end line yet.
        let (_, mut info) = read(&p).unwrap();
        assert!(!info.ended);
        // Then it ends, and its writer frees the lock.
        let end = RunEnd::new(4_000, Some(0), None).line();
        fs::write(&p, format!("{text}{end}")).unwrap();
        fs::write(crate::runner::lock_path(&p), "").unwrap();
        let hall = crate::journal::Hall::new(dir.path());
        let runs = runs_in(dir.path()).unwrap();
        mark_interrupted(&hall, ItemId(1), &runs, 0, &mut info);
        assert!(info.ended, "the end line is read again");
        assert!(!info.interrupted);
    }
}
