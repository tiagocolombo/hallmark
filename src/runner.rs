//! Every model run is `claude -p` (ADR 0006, ADR 0013). This module owns the
//! command shape, the timeout, failure detection, and recording usage, so
//! the assayer, fixer, intake and librarian only build a [`RunRequest`].
//!
//! ```text
//! claude -p --plugin-dir <dir> --agent <name> --model <m>
//!   --output-format stream-json --verbose --json-schema <schema>
//!   --max-budget-usd <n> -- "<prompt>"   (null stdin)
//! ```
//!
//! A run with an item streams its stdout, line by line as it arrives, into
//! `items/<id>/runs/<stamp>-<role>.jsonl`, and the runner appends one
//! `hall_run_end` line once the process has exited (ADR 0041, `trace`).
//! The result is the stream's last `result` line.
//!
//! Tests never spend model usage: `HALL_RUNNER=fake:<script.json>` swaps in
//! [`FakeRunner`], which replays scripted results and side effects.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, HallError, Result};
use crate::event::{Body, Event};
use crate::item::{HoldKind, ItemId};
use crate::repo::Repo;
use crate::usage::PrintResult;

/// Prompts longer than this go through stdin instead of the command line
/// (Windows caps a command line at 32k characters).
const ARG_PROMPT_MAX: usize = 16_000;

/// One headless run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunRequest {
    /// reviewer, fixer, intake, librarian, resume.
    pub role: String,
    /// assay, repair, front, library, workshop: where usage is booked.
    pub room: String,
    /// A plugin agent (`principal-engineer`, `fixer`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Retried with when `model` is unavailable; `--fallback-model` too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub prompt: String,
    /// The JSON Schema text for `--json-schema`. A run with a schema that
    /// returns no `structured_output` has failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_budget_usd: Option<f64>,
    /// Seconds; 0 means no limit.
    pub timeout_secs: u64,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add_dirs: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// `--resume <session>`: continue a session (answers to questions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<String>,
    /// `--plugin-dir`; defaults to the extracted runtime plugin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_dir: Option<PathBuf>,
    /// From `claude_args` in `.hallmark.toml`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
    /// Extra environment, such as `HALL_ACTOR=fixer`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<(String, String)>,
    /// Environment variables the run doesn't inherit (`[sandbox]
    /// remove_env`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_remove: Vec<String>,
    /// The file stdout streams into as it arrives, ending with a
    /// `hall_run_end` line (ADR 0041). [`run_metered`] sets it for runs with
    /// an item; none streams nowhere.
    #[serde(skip)]
    pub stream: Option<PathBuf>,
}

impl RunRequest {
    /// The `claude` arguments, prompt last after `--` (unless it goes
    /// through stdin).
    pub fn args(&self) -> Vec<String> {
        let mut a: Vec<String> = vec!["-p".into()];
        if let Some(r) = &self.resume {
            a.extend(["--resume".into(), r.clone()]);
        }
        if let Some(p) = &self.plugin_dir {
            a.extend(["--plugin-dir".into(), p.to_string_lossy().into_owned()]);
        }
        if let Some(ag) = &self.agent {
            a.extend(["--agent".into(), ag.clone()]);
        }
        if let Some(m) = &self.model {
            a.extend(["--model".into(), m.clone()]);
        }
        if let Some(f) = &self.fallback_model
            && self.model.as_ref() != Some(f)
        {
            a.extend(["--fallback-model".into(), f.clone()]);
        }
        if let Some(e) = &self.effort {
            a.extend(["--effort".into(), e.clone()]);
        }
        // Line by line as it happens, for `hall trace`; the result is the
        // last `result` line. stream-json needs --verbose with -p.
        a.extend([
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
        ]);
        if let Some(s) = &self.json_schema {
            a.extend(["--json-schema".into(), s.clone()]);
        }
        if let Some(b) = self.max_budget_usd {
            a.extend(["--max-budget-usd".into(), format!("{b}")]);
        }
        if let Some(m) = &self.permission_mode {
            a.extend(["--permission-mode".into(), m.clone()]);
        }
        for d in &self.add_dirs {
            a.extend(["--add-dir".into(), d.to_string_lossy().into_owned()]);
        }
        a.extend(self.extra_args.iter().cloned());
        // Variadic: everything after it until `--` is a tool.
        if !self.allowed_tools.is_empty() {
            a.push("--allowedTools".into());
            a.extend(self.allowed_tools.iter().cloned());
        }
        if !self.prompt_via_stdin() {
            a.push("--".into());
            a.push(self.prompt.clone());
        }
        a
    }

    pub fn prompt_via_stdin(&self) -> bool {
        self.prompt.len() > ARG_PROMPT_MAX
    }
}

/// A request for `role` with everything the config decides filled in: the
/// model, effort, `--max-budget-usd`, timeout, the runtime plugin,
/// `claude_args`, and `HALL_ACTOR`. Callers add the agent, schema, tools
/// and prompt.
pub fn request(
    repo: &Repo,
    role: crate::config::Role,
    agent: Option<&str>,
    prompt: String,
    cwd: &Path,
) -> Result<RunRequest> {
    use crate::config::Role as R;
    let b = &repo.config.budget;
    let actor = match role {
        crate::config::Role::Reviewer => "assayer",
        crate::config::Role::Fixer => "fixer",
        crate::config::Role::Intake => "intake",
        crate::config::Role::Librarian => "librarian",
    };
    // Every run ignores MCP servers; read-only roles (reviewer, intake,
    // librarian) also ignore the checkout's settings and CLAUDE.md, so the
    // branch under review can't steer them (ADR 0013, live check in ADR 0018).
    let read_only = matches!(role, R::Reviewer | R::Intake | R::Librarian);
    let confined = confinement(repo);
    let mut extra_args = confined.args;
    let mut env = vec![
        ("HALL_ACTOR".to_string(), actor.to_string()),
        // Hooks see this and leave usage to the runner (no double count).
        ("HALL_HEADLESS".to_string(), "1".to_string()),
    ];
    if read_only {
        extra_args.extend(["--setting-sources".into(), "user".into()]);
        env.push(("HALL_NO_HOOKS".into(), "1".into()));
        env.push(("CLAUDE_CODE_DISABLE_CLAUDE_MDS".into(), "1".into()));
    }
    Ok(RunRequest {
        role: role.as_str().into(),
        room: role.room().into(),
        // Namespaced: a branch's `.claude/agents/<name>.md` would otherwise
        // replace the plugin's agent of the same bare name.
        agent: agent.map(|a| {
            if a.contains(':') {
                a.to_string()
            } else {
                format!("{}:{a}", crate::plugin::PLUGIN)
            }
        }),
        model: Some(b.model(role)),
        fallback_model: b.fallback(role),
        effort: b.effort(role),
        prompt,
        json_schema: None,
        max_budget_usd: b.max_usd(role),
        timeout_secs: b.timeout_secs(role).max(0) as u64,
        cwd: cwd.to_path_buf(),
        add_dirs: Vec::new(),
        allowed_tools: Vec::new(),
        permission_mode: None,
        resume: None,
        plugin_dir: Some(crate::plugin::runtime_dir()?),
        extra_args,
        env,
        env_remove: confined.env_remove,
        stream: None,
    })
}

/// What confines every run (ADR 0053): `claude_args`, then no MCP servers,
/// the sandbox settings, and no web tools; and the environment it drops.
pub struct Confinement {
    pub args: Vec<String>,
    pub env_remove: Vec<String>,
}

pub fn confinement(repo: &Repo) -> Confinement {
    // A linked worktree's `.hallmark.toml` (read when the main checkout has
    // none) is the agent's to edit, so it can't add arguments or widen the
    // sandbox.
    let trusted = repo
        .config_path
        .as_ref()
        .and_then(|p| p.parent())
        .is_none_or(|d| d == repo.main_root);
    let (claude_args, sandbox) = if trusted {
        (repo.config.claude_args.clone(), repo.config.sandbox.clone())
    } else {
        (Vec::new(), crate::config::Sandbox::default())
    };
    let mut args = claude_args;
    args.push("--strict-mcp-config".into());
    args.extend([
        "--settings".into(),
        settings(&sandbox, cfg!(windows)).to_string(),
        "--disallowedTools".into(),
        "WebFetch,WebSearch".into(),
    ]);
    Confinement {
        args,
        env_remove: sandbox.remove_env,
    }
}

/// The `--settings` for a run. File tools read only inside the run's
/// directories and never `deny_read`; commands run in the sandbox unless
/// `mode = "off"` (or `auto` on native Windows, where Claude Code has none).
/// Without `autoAllowBashIfSandboxed: false` a sandboxed command would skip
/// `--allowedTools`; without `allowUnsandboxedCommands: false` it could
/// retry outside the sandbox.
pub fn settings(s: &crate::config::Sandbox, windows: bool) -> serde_json::Value {
    use crate::config::SandboxMode;
    let deny: Vec<String> = s
        .deny_read
        .iter()
        .flat_map(|p| {
            // A rule's absolute path starts with `//`; `/x` is the project's.
            let p = if p.starts_with('/') {
                format!("/{p}")
            } else {
                p.clone()
            };
            [format!("Read({p})"), format!("Read({p}/**)")]
        })
        .collect();
    let mut v = serde_json::json!({
        "permissions": {
            "blockReadsOutsideWorkingDirectories": true,
            "deny": deny,
        }
    });
    let on = match s.mode {
        SandboxMode::Off => false,
        SandboxMode::Auto => !windows,
        SandboxMode::Required => true,
    };
    if on {
        v["sandbox"] = serde_json::json!({
            "enabled": true,
            // Never fall back to running commands unconfined, in `auto` too.
            "failIfUnavailable": true,
            "autoAllowBashIfSandboxed": false,
            "allowUnsandboxedCommands": false,
            "filesystem": {
                "allowWrite": s.allow_write,
                "denyRead": s.deny_read,
            },
            "network": {
                "allowedDomains": s.domains,
                "allowUnixSockets": s.unix_sockets,
                "allowLocalBinding": s.local_binding,
            },
        });
    }
    v
}

/// Why a run produced nothing usable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Failure {
    /// The time limit passed and the run was killed.
    Timeout,
    /// `--max-budget-usd` stopped it.
    Budget,
    /// The model isn't available to this account; nothing was spent.
    ModelUnavailable { model: String },
    /// Claude's plan limit: the daemon pauses until `until`.
    RateLimited {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<DateTime<Utc>>,
    },
    /// A schema run without `structured_output` (docs/spikes.md, spike 2).
    NoStructuredOutput,
    /// Anything else: a crash, an error result, unreadable output.
    Error { message: String },
}

impl Failure {
    pub fn describe(&self) -> String {
        match self {
            Failure::Timeout => "timed out".into(),
            Failure::Budget => "reached --max-budget-usd".into(),
            Failure::ModelUnavailable { model } => format!("model {model} is unavailable"),
            Failure::RateLimited { until: Some(u) } => {
                format!("rate limited until {}", u.to_rfc3339())
            }
            Failure::RateLimited { until: None } => "rate limited".into(),
            Failure::NoStructuredOutput => {
                "no structured_output: the result did not match the schema".into()
            }
            Failure::Error { message } => message.clone(),
        }
    }
}

/// What a run returned.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub result: Option<PrintResult>,
    /// stdout as printed: the stream-json lines, without the end line.
    pub raw: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub failure: Option<Failure>,
}

impl RunOutcome {
    pub fn structured(&self) -> Option<&serde_json::Value> {
        self.result.as_ref()?.structured_output.as_ref()
    }

    pub fn session_id(&self) -> Option<&str> {
        self.result.as_ref()?.session_id.as_deref()
    }

    /// The line the runner appends to the stream when the process has
    /// exited (ADR 0041).
    pub fn end(&self) -> crate::trace::RunEnd {
        crate::trace::RunEnd::new(
            self.duration_ms,
            self.exit_code,
            self.failure.as_ref().map(Failure::describe),
        )
    }

    /// What follows `raw` in its stream file: a newline when stdout stopped
    /// mid-line, then the end line.
    pub fn end_text(&self) -> String {
        let nl = if self.raw.is_empty() || self.raw.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        format!("{nl}{}", self.end().line())
    }

    /// The whole stream file for this run: stdout, then the end line. For a
    /// run that streamed nowhere and is saved afterwards (intake).
    pub fn stream_text(&self) -> String {
        format!("{}{}", self.raw, self.end_text())
    }

    fn failed(failure: Failure, raw: String, stderr: String, ms: u64) -> RunOutcome {
        RunOutcome {
            result: PrintResult::parse(&raw).ok(),
            raw,
            stderr,
            exit_code: None,
            duration_ms: ms,
            failure: Some(failure),
        }
    }
}

pub trait Runner: Send + Sync {
    fn run(&self, req: &RunRequest) -> RunOutcome;
}

/// The runner chosen by the environment: `HALL_RUNNER=fake:<script>` for
/// tests, else the real `claude` (`HALL_CLAUDE`, `[tools] claude`, else
/// `claude`).
pub fn from_env() -> Box<dyn Runner> {
    if let Ok(v) = std::env::var("HALL_RUNNER")
        && let Some(script) = v.strip_prefix("fake:")
    {
        return Box::new(FakeRunner::new(PathBuf::from(script)));
    }
    Box::new(ClaudeRunner {
        bin: crate::config::Tool::Claude.command(),
    })
}

/// Runs the real `claude` binary.
pub struct ClaudeRunner {
    pub bin: String,
}

impl Runner for ClaudeRunner {
    fn run(&self, req: &RunRequest) -> RunOutcome {
        let stream = open_stream(req);
        let (mut outcome, stream) = self.exec(req, stream);
        name_model(&mut outcome.failure, req.model.as_deref());
        if let Some(f) = stream {
            end_stream(f, &outcome);
        }
        outcome
    }
}

impl ClaudeRunner {
    /// Runs the process, teeing stdout into `stream` as it arrives; hands
    /// the stream back for its end line.
    fn exec(&self, req: &RunRequest, stream: Option<Stream>) -> (RunOutcome, Option<Stream>) {
        let start = Instant::now();
        let ms = |s: Instant| s.elapsed().as_millis() as u64;
        let mut cmd = crate::process::command(&self.bin);
        cmd.args(req.args())
            .current_dir(&req.cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if req.prompt_via_stdin() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        // A headless run started from inside Claude Code must not think it
        // is nested in that session.
        cmd.env_remove("CLAUDECODE");
        for k in &req.env_remove {
            cmd.env_remove(k);
        }
        for (k, v) in &req.env {
            cmd.env(k, v);
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let o = RunOutcome::failed(
                    Failure::Error {
                        message: format!("could not run {}: {e}", self.bin),
                    },
                    String::new(),
                    String::new(),
                    ms(start),
                );
                return (o, stream);
            }
        };
        if req.prompt_via_stdin()
            && let Some(mut stdin) = child.stdin.take()
        {
            let prompt = req.prompt.clone();
            std::thread::spawn(move || {
                let _ = stdin.write_all(prompt.as_bytes());
            });
        }
        let out = child.stdout.take().map(|o| tee(o, stream));
        let err = child.stderr.take().map(|e| tee(e, None));
        // The limit counts from `start`, as the run's duration does, so a
        // run that timed out lasted at least its limit.
        let waited = if req.timeout_secs == 0 {
            child.wait().map(Some)
        } else {
            use wait_timeout::ChildExt;
            let deadline = start + Duration::from_secs(req.timeout_secs);
            wait_until(deadline, |left| child.wait_timeout(left))
        };
        let status = settle(waited, &self.bin);
        if status.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let (raw, stream) = out.and_then(|h| h.join().ok()).unwrap_or_default();
        let stderr = err
            .and_then(|h| h.join().ok())
            .map(|(s, _)| s)
            .unwrap_or_default();
        let status = match status {
            Ok(s) => s,
            Err(f) => return (RunOutcome::failed(f, raw, stderr, ms(start)), stream),
        };
        let result = PrintResult::parse(&raw).ok();
        let failure = classify(result.as_ref(), &raw, &stderr, req.json_schema.is_some());
        let o = RunOutcome {
            result,
            raw,
            stderr,
            exit_code: status.code(),
            duration_ms: ms(start),
            failure,
        };
        (o, stream)
    }
}

/// What a finished wait means: an exit, a timeout (`Ok(None)`: the deadline
/// passed), or a wait error, which is an error and never a timeout.
fn settle<T>(waited: std::io::Result<Option<T>>, bin: &str) -> std::result::Result<T, Failure> {
    match waited {
        Ok(Some(s)) => Ok(s),
        Ok(None) => Err(Failure::Timeout),
        Err(e) => Err(Failure::Error {
            message: format!("could not wait for {bin}: {e}"),
        }),
    }
}

/// Waits for an exit until `deadline`, asking `wait` (`wait_timeout`) to wait
/// for the time left. `Ok(None)` means the deadline has passed. `wait` may
/// come back early without an exit: on Unix, wait_timeout 0.2 polls for what
/// is left rounded down to whole milliseconds and calls a poll that ends a
/// timeout, so `wait_timeout(1s)` can return after 999ms. An early return is
/// asked again for what is left; an error is the caller's, never a timeout.
fn wait_until<T>(
    deadline: Instant,
    mut wait: impl FnMut(Duration) -> std::io::Result<Option<T>>,
) -> std::io::Result<Option<T>> {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(None);
        }
        // wait_timeout rounds what it is given down to whole milliseconds
        // after spending a few nanoseconds, so asking for exactly `left`
        // under 1ms polls for 0 and spins. One more millisecond makes the
        // call reach the deadline, overshooting by under 1ms.
        if let Some(status) = wait(left + Duration::from_millis(1))? {
            return Ok(Some(status));
        }
    }
}

/// Reads `r` to the end, a line at a time, writing each line to `file` as
/// it arrives so another process can follow it. Returns all of it, and the
/// file.
fn tee<R: Read + Send + 'static>(
    r: R,
    mut file: Option<Stream>,
) -> std::thread::JoinHandle<(String, Option<Stream>)> {
    std::thread::spawn(move || {
        let mut r = BufReader::new(r);
        let mut all = Vec::new();
        let mut line = Vec::new();
        loop {
            line.clear();
            match r.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    // A stream that can't be written loses the trace, never
                    // the run.
                    if let Some(f) = file.as_mut()
                        && f.write_all(&line).and_then(|_| f.flush()).is_err()
                    {
                        file = None;
                    }
                    all.extend_from_slice(&line);
                }
            }
        }
        (String::from_utf8_lossy(&all).into_owned(), file)
    })
}

/// The run's stream file, opened for appending. None when the run has no
/// item, or the file can't be opened: the run goes on without a trace.
fn open_stream(req: &RunRequest) -> Option<Stream> {
    let p = req.stream.as_ref()?;
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let file = OpenOptions::new().create(true).append(true).open(p).ok()?;
    // The writer holds a lock on a sidecar for as long as it lives; the OS
    // frees it if the process dies. Readers tell a stream still being
    // written from an abandoned one by whether they can take it. A reader
    // holds it only for the instant of its check, and each run has its own
    // name (`claim_stream`), so waiting for it is brief; giving up on the
    // first try would leave the run reading as interrupted for its life.
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path(p))
        .ok()
        .filter(|l| fs4::FileExt::lock(l).is_ok());
    Some(Stream { file, _lock: lock })
}

/// `<stamp>-<role>.lock`, next to the stream.
pub fn lock_path(stream: &Path) -> PathBuf {
    stream.with_extension("lock")
}

/// Whether nothing is writing `stream` any more: its writer's lock can be
/// taken. None when it has no lock (a stream from before locks, or one whose
/// lock couldn't be made).
pub fn writer_gone(stream: &Path) -> Option<bool> {
    let l = OpenOptions::new()
        .write(true)
        .open(lock_path(stream))
        .ok()?;
    let free = fs4::FileExt::try_lock(&l).is_ok();
    let _ = fs4::FileExt::unlock(&l);
    Some(free)
}

/// A run's stream file and the lock that says its writer is alive.
struct Stream {
    file: File,
    _lock: Option<File>,
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// Appends the `hall_run_end` line, on a line of its own.
fn end_stream(mut f: Stream, o: &RunOutcome) {
    let _ = f.write_all(o.end_text().as_bytes());
}

/// An unavailable model is the one the run asked for.
fn name_model(failure: &mut Option<Failure>, asked: Option<&str>) {
    if let Some(Failure::ModelUnavailable { model }) = failure
        && model.is_empty()
    {
        *model = asked.unwrap_or_default().to_string();
    }
}

/// Reads a finished run: rate limits, budget stops, errors, and schema runs
/// without `structured_output`.
pub fn classify(
    result: Option<&PrintResult>,
    raw: &str,
    stderr: &str,
    schema: bool,
) -> Option<Failure> {
    // A rate limit only ever ends a run: a run that completed may well talk
    // about rate limits in its result (a review of the code that handles
    // them did, and paused the daemon for nothing).
    let failed = result.is_none_or(|r| {
        r.is_error
            || r.subtype.as_deref().is_some_and(|s| s.starts_with("error"))
            || r.terminal_reason
                .as_deref()
                .is_some_and(|t| t != "completed")
    });
    // Only the result and stderr: a stream's own events mention models and
    // rate limits in every run (`rate_limit_event` … `"allowed"`, ADR 0041).
    let said = said(raw);
    if failed {
        let text = format!(
            "{} {}",
            result
                .and_then(|r| r.result.as_deref())
                .unwrap_or(said.as_str()),
            stderr
        );
        if let Some(until) = rate_limit(&text) {
            return Some(Failure::RateLimited { until });
        }
    }
    let Some(r) = result else {
        let detail = if !stderr.trim().is_empty() {
            stderr.trim()
        } else if !said.trim().is_empty() {
            said.trim()
        } else {
            "no output"
        };
        return Some(Failure::Error {
            message: format!(
                "unreadable claude -p output: {}",
                crate::output::clip(detail, 200)
            ),
        });
    };
    // An API error before any model ran: an unknown or unavailable model
    // ("[claude-code:unrecognized_model] …"). It costs nothing.
    if r.terminal_reason.as_deref() == Some("api_error")
        && r.model_usage.is_empty()
        && format!("{said} {stderr}")
            .to_ascii_lowercase()
            .contains("model")
    {
        return Some(Failure::ModelUnavailable {
            model: String::new(),
        });
    }
    let subtype = r.subtype.as_deref().unwrap_or("");
    if subtype.contains("budget") {
        return Some(Failure::Budget);
    }
    if r.is_error || subtype.starts_with("error") {
        return Some(Failure::Error {
            message: format!(
                "claude -p failed ({}): {}",
                if subtype.is_empty() { "error" } else { subtype },
                crate::output::clip(r.result.as_deref().unwrap_or(""), 200)
            ),
        });
    }
    if schema && r.structured_output.is_none() {
        return Some(Failure::NoStructuredOutput);
    }
    None
}

/// stdout without the stream's event lines (`system`, `assistant`, `user`,
/// `rate_limit_event`, …): the result line and anything that isn't JSON.
fn said(raw: &str) -> String {
    raw.lines()
        .filter(|l| {
            let kind = serde_json::from_str::<serde_json::Value>(l.trim())
                .ok()
                .and_then(|v| v.get("type")?.as_str().map(String::from));
            !kind.is_some_and(|k| crate::usage::STREAM_EVENTS.contains(&k.as_str()))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Claude's plan limit, as worded in its results: "Claude AI usage limit
/// reached|1760000000", "You've hit your limit · resets 3pm", "You've hit
/// your session limit · resets 11:10pm (America/Toronto)", "rate limit".
/// Returns the reset time when one is given: a Unix timestamp after `|`,
/// or a clock time after "resets", read in this machine's time zone (Claude
/// prints the account's zone, normally the same one).
pub fn rate_limit(text: &str) -> Option<Option<DateTime<Utc>>> {
    rate_limit_at(text, chrono::Local::now())
}

/// [`rate_limit`] with the clock given, for tests.
pub fn rate_limit_at(
    text: &str,
    now: DateTime<chrono::Local>,
) -> Option<Option<DateTime<Utc>>> {
    let t = text.to_ascii_lowercase();
    let hit = [
        "usage limit",
        "rate limit",
        "rate_limit",
        "hit your limit",
        "session limit",
        "weekly limit",
        "limit reached",
    ]
    .iter()
    .any(|p| t.contains(p))
        || (t.contains("limit") && t.contains("resets"));
    if !hit {
        return None;
    }
    let stamp = text.split('|').nth(1).and_then(|s| {
        let digits: String = s.trim().chars().take_while(char::is_ascii_digit).collect();
        DateTime::from_timestamp(digits.parse().ok()?, 0)
    });
    Some(stamp.or_else(|| resets_at(&t, now)))
}

/// "3pm", "11:10pm" or "23:10" (lower-case).
fn clock_word(word: &str) -> Option<chrono::NaiveTime> {
    let (clock, pm) = match word.strip_suffix("am") {
        Some(c) => (c, Some(false)),
        None => match word.strip_suffix("pm") {
            Some(c) => (c, Some(true)),
            None => (word, None),
        },
    };
    let (h, m) = match clock.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (clock.parse::<u32>().ok()?, 0),
    };
    let h = match pm {
        Some(true) if h < 12 => h + 12,
        Some(false) if h == 12 => 0,
        _ => h,
    };
    chrono::NaiveTime::from_hms_opt(h, m, 0)
}

/// What follows "resets" (lower-cased): a clock time ("11:10pm", "3pm",
/// "23:10"), a date ("Oct 30", "Oct 30, 9am", "Oct 30 at 9am") or a weekday
/// ("Mon 9am"): the next such instant in `now`'s zone, plus a minute of
/// slack. A date or weekday without a time means the start of that day.
/// None when the text gives none of these.
fn resets_at(lower: &str, now: DateTime<chrono::Local>) -> Option<DateTime<Utc>> {
    use chrono::{Datelike, NaiveDate, TimeZone};
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    let rest = lower.split("resets").nth(1)?;
    let rest = rest.split('(').next().unwrap_or("");
    let words: Vec<&str> = rest
        .split(|c: char| c.is_whitespace() || c == ',' || c == '·')
        .filter(|w| !w.is_empty() && !matches!(*w, "at" | "on"))
        .collect();
    let first = *words.first()?;
    let time_in = |ws: &[&str]| ws.iter().find_map(|w| clock_word(w));
    let midnight = chrono::NaiveTime::MIN;
    let prefix = first.get(..3).unwrap_or("");
    let at = if let Some(month) = MONTHS.iter().position(|m| *m == prefix) {
        let day: u32 = words
            .get(1)?
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .ok()?;
        let time = time_in(&words[2..]).unwrap_or(midnight);
        let on = |year| NaiveDate::from_ymd_opt(year, month as u32 + 1, day);
        let this = on(now.year())?.and_time(time);
        if this > now.naive_local() {
            this
        } else {
            on(now.year() + 1)?.and_time(time)
        }
    } else if let Some(wd) = DAYS.iter().position(|d| *d == prefix) {
        let time = time_in(&words[1..]).unwrap_or(midnight);
        (0..=7)
            .map(|n| (now.date_naive() + chrono::Duration::days(n)).and_time(time))
            .find(|at| {
                at.weekday().num_days_from_monday() as usize == wd && *at > now.naive_local()
            })?
    } else {
        let time = clock_word(first)?;
        let mut at = now.date_naive().and_time(time);
        if at <= now.naive_local() {
            at += chrono::Duration::days(1);
        }
        at
    };
    let at = now.timezone().from_local_datetime(&at).earliest()?;
    Some(at.with_timezone(&Utc) + chrono::Duration::minutes(1))
}

// --- the fake runner, for tests ---------------------------------------------

/// A scripted step for [`FakeRunner`]:
///
/// ```json
/// {"role": "reviewer", "structured": {…}, "files": {"src/a.rs": "…"},
///  "run": ["git commit -qam fix", "{hall} fixed F1"], "fail": "timeout"}
/// ```
///
/// Like the real runner, it writes the run's stream file when the request
/// has one: the lines before the result (`stream`, else an init line and an
/// assistant line) before the step's files and commands, so a command can
/// see the run going, then the result line and the `hall_run_end` line.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FakeStep {
    /// Which runs this step answers.
    pub role: String,
    /// `structured_output`.
    pub structured: Option<serde_json::Value>,
    /// The `result` text.
    pub result: Option<String>,
    /// A whole `claude -p` JSON result; overrides the two above.
    pub print: Option<serde_json::Value>,
    /// A fixture file (relative to the script) holding a whole result, or
    /// a whole stream-json run whose last line is the result.
    pub fixture: Option<String>,
    /// stream-json lines printed before the result, in place of the
    /// default init and assistant lines.
    pub stream: Option<Vec<serde_json::Value>>,
    /// Files to write, relative to the run's cwd.
    pub files: std::collections::BTreeMap<String, String>,
    /// Shell commands run in the cwd after writing files. `{hall}` is this
    /// binary, `{cwd}` the run's directory.
    pub run: Vec<String>,
    /// timeout, budget, rate_limit, error, no_structured.
    pub fail: Option<String>,
    pub model: Option<String>,
    pub cost_usd: Option<f64>,
    pub session_id: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FakeScript {
    pub steps: Vec<FakeStep>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FakeState {
    used: Vec<usize>,
}

/// Replays a script of steps. Each run takes the first unused step for its
/// role. Every request is appended to `<script>.calls.jsonl`, so tests can
/// check the command that would have run.
pub struct FakeRunner {
    pub script: PathBuf,
}

impl FakeRunner {
    pub fn new(script: PathBuf) -> FakeRunner {
        FakeRunner { script }
    }

    fn state_path(&self) -> PathBuf {
        self.script.with_extension("state.json")
    }

    pub fn calls_path(&self) -> PathBuf {
        self.script.with_extension("calls.jsonl")
    }

    fn take(&self, role: &str) -> std::result::Result<FakeStep, String> {
        let script: FakeScript = serde_json::from_str(
            &std::fs::read_to_string(&self.script)
                .map_err(|e| format!("fake runner: {}: {e}", self.script.display()))?,
        )
        .map_err(|e| format!("fake runner: bad script: {e}"))?;
        let sp = self.state_path();
        let mut st: FakeState = std::fs::read_to_string(&sp)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let (i, step) = script
            .steps
            .into_iter()
            .enumerate()
            .find(|(i, s)| s.role == role && !st.used.contains(i))
            .ok_or_else(|| format!("fake runner: no step left for role {role}"))?;
        st.used.push(i);
        let _ = std::fs::write(&sp, serde_json::to_string(&st).unwrap_or_default());
        Ok(step)
    }
}

impl Runner for FakeRunner {
    fn run(&self, req: &RunRequest) -> RunOutcome {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.calls_path())
        {
            let mut v = serde_json::to_value(req).unwrap_or_default();
            v["args"] = serde_json::json!(req.args());
            let _ = writeln!(f, "{v}");
        }
        let mut stream = open_stream(req);
        let step = match self.take(&req.role) {
            Ok(s) => s,
            Err(message) => {
                let o =
                    RunOutcome::failed(Failure::Error { message }, String::new(), String::new(), 0);
                if let Some(f) = stream {
                    end_stream(f, &o);
                }
                return o;
            }
        };
        let (lead, result_line) = self.print(&step, req);
        let mut raw = String::new();
        let mut say = |stream: &mut Option<Stream>, line: &str| {
            let line = format!("{line}\n");
            if let Some(f) = stream.as_mut() {
                let _ = f.write_all(line.as_bytes());
            }
            raw.push_str(&line);
        };
        for l in &lead {
            say(&mut stream, l);
        }
        for (rel, body) in &step.files {
            let p = req.cwd.join(rel);
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(&p, body);
        }
        let hall = std::env::current_exe()
            .map(|p| crate::commands::new::shell_quote(&crate::paths::display(&p)))
            .unwrap_or_else(|_| "hall".into());
        for cmd in &step.run {
            let cmd = cmd
                .replace("{hall}", &hall)
                .replace("{cwd}", &crate::paths::display(&req.cwd));
            let mut c = crate::commands::new::shell(&cmd);
            c.current_dir(&req.cwd);
            for (k, v) in &req.env {
                c.env(k, v);
            }
            c.env_remove("CLAUDECODE");
            let _ = c.output();
        }
        say(&mut stream, &result_line);
        let failure = match step.fail.as_deref() {
            Some("timeout") => Some(Failure::Timeout),
            Some("budget") => Some(Failure::Budget),
            Some("rate_limit") => Some(Failure::RateLimited {
                until: Some(Utc::now() + chrono::Duration::hours(1)),
            }),
            Some("no_structured") => Some(Failure::NoStructuredOutput),
            Some(other) => Some(Failure::Error {
                message: format!("fake failure: {other}"),
            }),
            None => None,
        };
        let result = PrintResult::parse(&raw).ok();
        let mut failure =
            failure.or_else(|| classify(result.as_ref(), &raw, "", req.json_schema.is_some()));
        name_model(&mut failure, req.model.as_deref());
        let o = RunOutcome {
            result,
            raw,
            stderr: String::new(),
            exit_code: Some(0),
            duration_ms: 1200,
            failure,
        };
        if let Some(f) = stream {
            end_stream(f, &o);
        }
        o
    }
}

impl FakeRunner {
    /// The step's stream: the lines before the result, and the result line.
    fn print(&self, step: &FakeStep, req: &RunRequest) -> (Vec<String>, String) {
        let model = step
            .model
            .clone()
            .unwrap_or_else(|| "claude-haiku-5-5".into());
        let print = if let Some(p) = &step.print {
            p.clone()
        } else if let Some(f) = &step.fixture {
            let path = self.script.parent().unwrap_or(Path::new(".")).join(f);
            let text = std::fs::read_to_string(path).unwrap_or_default();
            match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(v) => v,
                // A whole stream-json run: replayed as it is.
                Err(_) => {
                    let mut lines: Vec<String> = text
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .map(String::from)
                        .collect();
                    let last = lines.pop().unwrap_or_default();
                    return (lines, last);
                }
            }
        } else {
            let cost = step.cost_usd.unwrap_or(0.001);
            let mut v = serde_json::json!({
                "type": "result", "subtype": "success", "is_error": false,
                "result": step.result.clone().unwrap_or_else(|| "done".into()),
                "session_id": step.session_id.clone().unwrap_or_else(|| "fake-session".into()),
                "num_turns": 3, "duration_ms": 1200, "total_cost_usd": cost,
                "modelUsage": { &model: {
                    "inputTokens": 12, "outputTokens": 340,
                    "cacheReadInputTokens": 2100, "cacheCreationInputTokens": 3700,
                    "costUSD": cost, "costBasis": "list"
                }}
            });
            if let Some(s) = &step.structured {
                v["structured_output"] = s.clone();
            }
            v
        };
        let lead = match &step.stream {
            Some(lines) => lines.iter().map(|l| l.to_string()).collect(),
            None => {
                let sid = print
                    .get("session_id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("fake-session");
                let said = print
                    .get("result")
                    .and_then(|s| s.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or("done");
                vec![
                    serde_json::json!({
                        "type": "system", "subtype": "init",
                        "cwd": crate::paths::display(&req.cwd),
                        "session_id": sid, "model": model,
                    })
                    .to_string(),
                    serde_json::json!({
                        "type": "assistant",
                        "message": {"role": "assistant", "content": [{"type": "text", "text": said}]},
                        "parent_tool_use_id": null, "session_id": sid,
                        "timestamp": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    })
                    .to_string(),
                ]
            }
        };
        (lead, print.to_string())
    }
}

// --- metered runs -------------------------------------------------------------

/// The result of [`run_metered`].
#[derive(Debug)]
pub enum Metered {
    /// The run finished; usage is recorded.
    Done(RunOutcome),
    /// The run failed; the failure (and usage, if any) is recorded. After
    /// the second failure in a row for this role, the item is held for you.
    Failed {
        outcome: RunOutcome,
        failure: Failure,
        held: bool,
    },
    /// A budget cap stopped it before it started.
    Blocked(crate::budget::Stop),
}

/// Checks the budgets, runs (streaming into the item's `runs/`), records
/// usage, and handles failures: one retry (the next tick), then a hold.
pub fn run_metered(
    repo: &Repo,
    runner: &dyn Runner,
    item: Option<ItemId>,
    req: &RunRequest,
) -> Result<Metered> {
    // Events about the run carry the run's actor (fixer, assayer…), not
    // whoever happens to run the daemon.
    let actor = req
        .env
        .iter()
        .find(|(k, _)| k == "HALL_ACTOR")
        .map(|(_, v)| v.clone())
        .unwrap_or_else(crate::repo::actor);
    let actor = actor.as_str();
    if let Err(stop) = crate::budget::check(repo, item, &req.role) {
        if let (Some(id), crate::budget::Level::Item) = (item, stop.level) {
            record(
                repo,
                Some(id),
                actor,
                Body::Held {
                    kind: HoldKind::Budget,
                    reason: stop.reason.clone(),
                },
            )?;
        }
        return Ok(Metered::Blocked(stop));
    }
    // Each run streams into a file of its own in the item's `runs/`
    // (ADR 0041); a run with no item streams nowhere.
    let dir = match item {
        Some(id) => {
            let d = repo.hall.item_runs(id);
            std::fs::create_dir_all(&d)?;
            Some(d)
        }
        None => None,
    };
    let mut first = req.clone();
    first.stream = dir.as_deref().and_then(|d| claim_stream(d, &req.role));
    repo.hall
        .live(&format!("{} {} run started", label(item), req.role));
    let mut outcome = runner.run(&first);
    keep_stderr(&first, &outcome);
    // The role's model isn't available: retry once with its fallback.
    if matches!(outcome.failure, Some(Failure::ModelUnavailable { .. }))
        && let Some(fb) = req.fallback_model.clone()
        && req.model.as_ref() != Some(&fb)
    {
        repo.hall.live(&format!(
            "{} {} model {} unavailable; retrying with {fb}",
            label(item),
            req.role,
            req.model.as_deref().unwrap_or("(default)")
        ));
        // A second run, with its own session: it gets its own stream file,
        // stamped after the first, so the first keeps its end line (and its
        // failure) and the retry reads as the latest run.
        let mut again = req.clone();
        again.model = Some(fb);
        again.fallback_model = None;
        again.stream = dir.as_deref().and_then(|d| claim_stream(d, &req.role));
        outcome = runner.run(&again);
        keep_stderr(&again, &outcome);
    }
    if let Some(Failure::ModelUnavailable { model }) = &mut outcome.failure
        && model.is_empty()
    {
        *model = req.model.clone().unwrap_or_default();
    }
    if let Some(r) = &outcome.result {
        for u in r.usage(&req.room, &req.role) {
            record(repo, item, actor, Body::Usage(u))?;
        }
    }
    let Some(failure) = outcome.failure.clone() else {
        repo.hall.live(&format!(
            "{} {} run finished in {}s",
            label(item),
            req.role,
            outcome.duration_ms / 1000
        ));
        return Ok(Metered::Done(outcome));
    };
    repo.hall.live(&format!(
        "{} {} run failed: {}",
        label(item),
        req.role,
        failure.describe()
    ));
    // A rate limit isn't the item's fault: the daemon pauses instead.
    if matches!(failure, Failure::RateLimited { .. }) {
        return Ok(Metered::Failed {
            outcome,
            failure,
            held: false,
        });
    }
    let attempt = match item {
        Some(id) => failures_in_a_row(&repo.hall.read_item(id)?, &req.role) + 1,
        None => 1,
    };
    record(
        repo,
        item,
        actor,
        Body::RunFailed {
            role: req.role.clone(),
            reason: failure.describe(),
            attempt,
        },
    )?;
    let held = attempt >= 2 && item.is_some();
    if held {
        record(
            repo,
            item,
            actor,
            Body::Held {
                kind: HoldKind::RunFailed,
                reason: match (&failure, req.max_budget_usd) {
                    (Failure::Budget, Some(usd)) => {
                        let key = crate::budget::role_of(&req.role)
                            .map(crate::budget::table_key)
                            .unwrap_or(&req.role);
                        format!(
                            "the {role} run failed twice: reached its ${usd:.2} per-run cap; \
                             raise it with `hall budget --set {key}.max_usd=<usd>` \
                             ([budget.{key}] max_usd)",
                            role = req.role
                        )
                    }
                    _ => format!("the {} run failed twice: {}", req.role, failure.describe()),
                },
            },
        )?;
    }
    Ok(Metered::Failed {
        outcome,
        failure,
        held,
    })
}

/// Claims `<dir>/<stamp>-<role>.jsonl` for a run starting now: created
/// empty, so the run is listed from its start. A name already taken (the
/// fallback retry, within the same second) moves to the next free second,
/// so runs sort in the order they ran. None when it can't be created.
pub fn claim_stream(dir: &Path, role: &str) -> Option<PathBuf> {
    let now = Utc::now();
    (0..60).find_map(|s| {
        let stamp = (now + chrono::Duration::seconds(s)).format("%Y%m%dT%H%M%S");
        let p = dir.join(format!("{stamp}-{role}.jsonl"));
        match OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(_) => Some(Some(p)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => None,
            Err(_) => Some(None),
        }
    })?
}

/// The run's stderr, when there is any, next to its stream:
/// `<stamp>-<role>.stderr`.
fn keep_stderr(req: &RunRequest, o: &RunOutcome) {
    if let Some(p) = &req.stream
        && !o.stderr.trim().is_empty()
    {
        let _ = std::fs::write(p.with_extension("stderr"), &o.stderr);
    }
}

/// `run_failed` events for `role` since the item last changed state, was
/// released, or finished a run of that role.
pub fn failures_in_a_row(events: &[Event], role: &str) -> u32 {
    let mut n = 0;
    for e in events {
        match &e.body {
            Body::RunFailed { role: r, .. } if r == role => n += 1,
            Body::Released { .. } => n = 0,
            b if b.action().is_some() => n = 0,
            _ => {}
        }
    }
    n
}

fn label(item: Option<ItemId>) -> String {
    item.map(|i| i.to_string()).unwrap_or_else(|| "repo".into())
}

fn record(repo: &Repo, item: Option<ItemId>, actor: &str, body: Body) -> Result<()> {
    let e = Event::new(actor, item, body);
    match item {
        Some(id) => repo.hall.append_item(id, &e),
        None => repo.hall.append(&repo.hall.repo_events(), &e),
    }
}

/// An error for a run that produced nothing usable.
pub fn run_error(role: &str, f: &Failure) -> HallError {
    HallError::new(
        ErrorCode::RunFailed,
        format!("the {role} run failed: {}", f.describe()),
    )
    .next(["hall log --kind run_failed", "hall doctor"])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(p: &str) -> String {
        std::fs::read_to_string(format!("{}/tests/fixtures/{p}", env!("CARGO_MANIFEST_DIR")))
            .unwrap()
    }

    #[test]
    fn the_command_shape() {
        let req = RunRequest {
            role: "reviewer".into(),
            agent: Some("principal-engineer".into()),
            model: Some("opus".into()),
            prompt: "Review H-0042".into(),
            json_schema: Some("{}".into()),
            max_budget_usd: Some(3.0),
            allowed_tools: vec!["Read".into(), "Bash(git diff:*)".into()],
            plugin_dir: Some(PathBuf::from("/p")),
            ..Default::default()
        };
        let a = req.args();
        assert_eq!(a[0], "-p");
        let s = a.join(" ");
        assert!(s.contains("--plugin-dir /p --agent principal-engineer --model opus"));
        assert!(
            s.contains("--output-format stream-json --verbose --json-schema {} --max-budget-usd 3")
        );
        // `--` comes after the variadic --allowedTools, then the prompt.
        assert!(s.ends_with("--allowedTools Read Bash(git diff:*) -- Review H-0042"));
        let long = RunRequest {
            prompt: "x".repeat(ARG_PROMPT_MAX + 1),
            ..Default::default()
        };
        assert!(long.prompt_via_stdin());
        assert!(!long.args().contains(&"--".to_string()));
    }

    #[test]
    fn a_completed_run_that_mentions_rate_limits_is_no_rate_limit() {
        // H-0007's review (live, 2026-10-10): it succeeded, and its verdict
        // discussed the daemon's rate-limit pauses.
        let ok: PrintResult = serde_json::from_value(serde_json::json!({
            "type": "result", "subtype": "success", "is_error": false,
            "terminal_reason": "completed",
            "result": "{\"summary\":\"A rate limit pauses the daemon until it resets; the usage limit reached path is tested.\"}",
            "structured_output": {"summary": "fine"}
        }))
        .unwrap();
        assert_eq!(classify(Some(&ok), "", "", true), None);
        // The same words in a failed run are a rate limit.
        let failed: PrintResult = serde_json::from_value(serde_json::json!({
            "is_error": true, "result": "Claude AI usage limit reached|1760000000"
        }))
        .unwrap();
        assert!(matches!(
            classify(Some(&failed), "", "", true),
            Some(Failure::RateLimited { .. })
        ));
    }

    /// H-0094's review (live, 2026-10-27): the run ended with the session
    /// limit, and was read as "model unavailable" because its own result
    /// line mentions `modelUsage`. It is a rate limit, with its reset time.
    #[test]
    fn a_session_limit_is_a_rate_limit_with_its_reset_time() {
        let line = r#"{"type":"result","subtype":"success","is_error":true,"terminal_reason":"api_error","result":"You've hit your session limit · resets 11:10pm (America/Toronto)","modelUsage":{},"num_turns":1,"total_cost_usd":0}"#;
        let r: PrintResult = serde_json::from_str(line).unwrap();
        let Some(Failure::RateLimited { until: Some(until) }) = classify(Some(&r), line, "", true)
        else {
            panic!("not a rate limit: {:?}", classify(Some(&r), line, "", true));
        };
        assert!(until > Utc::now());
        // The same, mid-run and with tokens spent: still a rate limit.
        let spent = line.replace(
            r#""modelUsage":{}"#,
            r#""modelUsage":{"m":{"costUSD":3.5}}"#,
        );
        let r: PrintResult = serde_json::from_str(&spent).unwrap();
        assert!(matches!(
            classify(Some(&r), &spent, "", true),
            Some(Failure::RateLimited { .. })
        ));
        assert_eq!(rate_limit("the limit of this function is three"), None);
        use chrono::TimeZone;
        let now = chrono::Local
            .with_ymd_and_hms(2026, 10, 27, 23, 10, 30)
            .single()
            .unwrap();
        let at = |s: &str| rate_limit_at(s, now);
        let slack = chrono::Duration::minutes(1);
        let local = |y, mo, d, h| {
            chrono::Local
                .with_ymd_and_hms(y, mo, d, h, 0, 0)
                .single()
                .unwrap()
                .with_timezone(&Utc)
                + slack
        };
        // Only the "limit … resets" catch-all matches this one; a near miss
        // without "limit" does not.
        assert_eq!(
            at("Opus limit · resets 3pm"),
            Some(Some(local(2026, 10, 28, 15)))
        );
        assert_eq!(at("the build resets 3pm"), None);
        // "resets 11:10pm", just past: tomorrow, exactly.
        assert_eq!(
            at("You've hit your session limit · resets 11:10pm (America/Toronto)"),
            Some(Some(
                chrono::Local
                    .with_ymd_and_hms(2026, 10, 28, 23, 10, 0)
                    .single()
                    .unwrap()
                    .with_timezone(&Utc)
                    + slack
            ))
        );
        // A weekly limit names a date, or a weekday.
        assert_eq!(
            at("You've hit your weekly limit · resets Oct 30"),
            Some(Some(local(2026, 10, 30, 0)))
        );
        assert_eq!(
            at("You've hit your weekly limit · resets Oct 30, 9am (UTC)"),
            Some(Some(local(2026, 10, 30, 9)))
        );
        // 2026-10-27 is a Tuesday: Friday is the 30th.
        assert_eq!(
            at("You've hit your weekly limit · resets Fri 9am"),
            Some(Some(local(2026, 10, 30, 9)))
        );
        // A date already past is next year's.
        assert_eq!(
            at("weekly limit · resets Jan 5"),
            Some(Some(local(2027, 1, 5, 0)))
        );
    }

    #[test]
    fn a_reset_time_is_the_next_such_clock_time_here() {
        use chrono::TimeZone;
        let local = |h: u32, m: u32| {
            chrono::Local
                .with_ymd_and_hms(2026, 10, 27, h, m, 0)
                .single()
                .unwrap()
        };
        let now = local(0, 28);
        let at = |s: &str| resets_at(s, now).unwrap();
        let slack = chrono::Duration::minutes(1);
        assert_eq!(
            at("hit your session limit · resets 11:10pm (america/toronto)"),
            local(23, 10) + slack
        );
        assert_eq!(at("resets 3pm"), local(15, 0) + slack);
        assert_eq!(
            at("resets 12am"),
            local(0, 0) + chrono::Duration::days(1) + slack
        );
        assert_eq!(at("resets 23:10"), local(23, 10) + slack);
        // A time already past today is tomorrow's.
        assert_eq!(
            resets_at("resets 3pm", local(16, 0)).unwrap(),
            local(15, 0) + chrono::Duration::days(1) + slack
        );
        assert_eq!(resets_at("resets soon", now), None);
        assert_eq!(resets_at("no reset here", now), None);
    }

    #[test]
    fn a_streamed_run_is_no_rate_limit() {
        // Every stream has `{"type":"rate_limit_event",…,"status":"allowed"}`
        // lines (ADR 0041): only the result and stderr say a run hit a limit.
        let raw = fixture("claude/stream.jsonl");
        assert!(raw.contains("rate_limit_event"));
        let ok = PrintResult::parse(&raw).unwrap();
        assert_eq!(classify(Some(&ok), &raw, "", false), None);
        // Killed before its result: a crash, not a rate limit.
        let cut: String = raw
            .lines()
            .filter(|l| !l.contains("\"type\":\"result\""))
            .map(|l| format!("{l}\n"))
            .collect();
        assert!(matches!(
            classify(None, &cut, "", false),
            Some(Failure::Error { .. })
        ));
        // An error result with no text of its own: still no rate limit.
        let errored = |result: Option<&str>| {
            let mut v: serde_json::Value =
                serde_json::from_str(raw.lines().last().unwrap()).unwrap();
            v["is_error"] = true.into();
            v["subtype"] = "error_during_execution".into();
            v["terminal_reason"] = "error".into();
            match result {
                Some(r) => v["result"] = r.into(),
                None => {
                    v.as_object_mut().unwrap().remove("result");
                }
            }
            format!("{cut}{v}\n")
        };
        let text = errored(None);
        let r = PrintResult::parse(&text).unwrap();
        assert!(matches!(
            classify(Some(&r), &text, "", false),
            Some(Failure::Error { .. })
        ));
        // A real limit, in the result or on stderr, still pauses the daemon.
        let text = errored(Some("Claude AI usage limit reached|1760000000"));
        let r = PrintResult::parse(&text).unwrap();
        match classify(Some(&r), &text, "", false) {
            Some(Failure::RateLimited { until: Some(u) }) => assert_eq!(u.timestamp(), 1760000000),
            other => panic!("{other:?}"),
        }
        let text = errored(None);
        let r = PrintResult::parse(&text).unwrap();
        assert!(matches!(
            classify(Some(&r), &text, "You've hit your limit · resets 3pm", false),
            Some(Failure::RateLimited { until: Some(_) })
        ));
    }

    #[test]
    fn classifies_results() {
        let ok = PrintResult::parse(&fixture("claude/print_structured.json")).unwrap();
        assert_eq!(classify(Some(&ok), "", "", true), None);
        let mismatch = PrintResult::parse(&fixture("claude/print_schema_mismatch.json")).unwrap();
        assert_eq!(
            classify(Some(&mismatch), "", "", true),
            Some(Failure::NoStructuredOutput)
        );
        assert_eq!(classify(Some(&mismatch), "", "", false), None);
        assert!(matches!(
            classify(None, "garbage", "", false),
            Some(Failure::Error { .. })
        ));
        let limited: PrintResult = serde_json::from_value(serde_json::json!({
            "is_error": true, "result": "Claude AI usage limit reached|1760000000"
        }))
        .unwrap();
        match classify(Some(&limited), "", "", false) {
            Some(Failure::RateLimited { until: Some(u) }) => assert_eq!(u.timestamp(), 1760000000),
            other => panic!("{other:?}"),
        }
        let budget: PrintResult = serde_json::from_value(
            serde_json::json!({"is_error": true, "subtype": "error_max_budget_usd"}),
        )
        .unwrap();
        assert_eq!(classify(Some(&budget), "", "", true), Some(Failure::Budget));
    }

    #[test]
    fn an_unavailable_model_is_recognized() {
        // What Claude Code 2.1.296 printed for an unknown model (live, $0).
        let raw = "[claude-code:unrecognized_model] {\"model\":\"claude-x\"}\n\
            {\"total_cost_usd\":0,\"modelUsage\":{},\"terminal_reason\":\"api_error\",\
            \"session_id\":\"s\"}";
        let r = PrintResult::parse(raw).unwrap();
        assert!(matches!(
            classify(Some(&r), raw, "", true),
            Some(Failure::ModelUnavailable { .. })
        ));
        let req = RunRequest {
            model: Some("claude-fable-5-1".into()),
            fallback_model: Some("claude-opus-5-5".into()),
            ..Default::default()
        };
        assert!(
            req.args()
                .join(" ")
                .contains("--fallback-model claude-opus-5-5")
        );
    }

    #[test]
    fn the_fake_runner_replays_steps_by_role() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("script.json");
        std::fs::write(
            &script,
            serde_json::json!({"steps": [
                {"role": "reviewer", "structured": {"summary": "a"}},
                {"role": "fixer", "files": {"out.txt": "fixed"}},
                {"role": "reviewer", "fail": "timeout"}
            ]})
            .to_string(),
        )
        .unwrap();
        let r = FakeRunner::new(script.clone());
        let req = |role: &str| RunRequest {
            role: role.into(),
            cwd: dir.path().to_path_buf(),
            json_schema: (role == "reviewer").then(|| "{}".into()),
            ..Default::default()
        };
        let a = r.run(&req("reviewer"));
        assert_eq!(a.failure, None);
        assert_eq!(a.structured().unwrap()["summary"], "a");
        assert_eq!(
            a.result.unwrap().usage("assay", "reviewer")[0].output_tokens,
            340
        );
        let f = r.run(&req("fixer"));
        assert_eq!(f.failure, None);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("out.txt")).unwrap(),
            "fixed"
        );
        assert_eq!(r.run(&req("reviewer")).failure, Some(Failure::Timeout));
        assert!(matches!(
            r.run(&req("reviewer")).failure,
            Some(Failure::Error { .. })
        ));
        let calls = std::fs::read_to_string(r.calls_path()).unwrap();
        assert_eq!(calls.lines().count(), 4);
    }

    #[test]
    fn failures_count_until_the_item_moves() {
        let ev = |b: Body| Event::new("hall", Some(ItemId(1)), b);
        let fail = || {
            ev(Body::RunFailed {
                role: "reviewer".into(),
                reason: "timeout".into(),
                attempt: 1,
            })
        };
        let mut events = vec![fail()];
        assert_eq!(failures_in_a_row(&events, "reviewer"), 1);
        assert_eq!(failures_in_a_row(&events, "fixer"), 0);
        events.push(fail());
        assert_eq!(failures_in_a_row(&events, "reviewer"), 2);
        events.push(ev(Body::Released {
            reason: None,
            granted_tokens: None,
        }));
        assert_eq!(failures_in_a_row(&events, "reviewer"), 0);
        events.push(fail());
        events.push(ev(Body::PulledBack { reason: None }));
        assert_eq!(failures_in_a_row(&events, "reviewer"), 0);
    }

    // --- confinement (ADR 0053) -------------------------------------------

    #[test]
    fn the_settings_sandbox_commands_and_keep_reads_in() {
        use crate::config::{Sandbox, SandboxMode};
        let s = Sandbox {
            allow_write: vec!["~/.cargo".into()],
            deny_read: vec!["~/.ssh".into(), "/etc/secret".into()],
            domains: vec!["crates.io".into()],
            unix_sockets: vec!["/run/nix.sock".into()],
            ..Sandbox::default()
        };
        let v = settings(&s, false);
        assert_eq!(
            v["permissions"]["blockReadsOutsideWorkingDirectories"],
            true
        );
        // `~` paths as given; an absolute one gets the `//` a rule needs.
        assert_eq!(
            v["permissions"]["deny"],
            serde_json::json!([
                "Read(~/.ssh)",
                "Read(~/.ssh/**)",
                "Read(//etc/secret)",
                "Read(//etc/secret/**)"
            ])
        );
        let sb = &v["sandbox"];
        assert_eq!(sb["enabled"], true);
        // Without these, a sandboxed command skips --allowedTools, or
        // retries outside the sandbox.
        assert_eq!(sb["autoAllowBashIfSandboxed"], false);
        assert_eq!(sb["allowUnsandboxedCommands"], false);
        assert_eq!(sb["failIfUnavailable"], true, "auto");
        assert_eq!(
            sb["filesystem"]["allowWrite"],
            serde_json::json!(["~/.cargo"])
        );
        assert_eq!(
            sb["filesystem"]["denyRead"],
            serde_json::json!(["~/.ssh", "/etc/secret"])
        );
        assert_eq!(
            sb["network"]["allowedDomains"],
            serde_json::json!(["crates.io"])
        );
        assert_eq!(
            sb["network"]["allowUnixSockets"],
            serde_json::json!(["/run/nix.sock"])
        );
        assert_eq!(sb["network"]["allowLocalBinding"], true);

        // Native Windows has no sandbox: `auto` leaves it out there,
        // `required` asks for it anyway, so the run fails.
        assert!(settings(&s, true).get("sandbox").is_none());
        let required = Sandbox {
            mode: SandboxMode::Required,
            ..s.clone()
        };
        for windows in [false, true] {
            let sb = &settings(&required, windows)["sandbox"];
            assert_eq!(sb["enabled"], true);
            assert_eq!(sb["failIfUnavailable"], true);
        }
        // `off`: no sandbox, but reads stay in and credentials out.
        let off = settings(
            &Sandbox {
                mode: SandboxMode::Off,
                ..s.clone()
            },
            false,
        );
        assert!(off.get("sandbox").is_none());
        assert_eq!(off["permissions"], v["permissions"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_removed_variable_never_reaches_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("env.txt");
        let runner = claude(
            dir.path(),
            &format!(
                "echo \"home=$HOME path=${{PATH:+set}}\" > \"{}\"",
                out.display()
            ),
        );
        runner.run(&RunRequest {
            role: "fixer".into(),
            cwd: dir.path().to_path_buf(),
            timeout_secs: 60,
            env_remove: vec!["HOME".into()],
            ..Default::default()
        });
        assert_eq!(
            std::fs::read_to_string(&out).unwrap().trim(),
            "home= path=set"
        );
    }

    // --- streaming (ADR 0041) ---------------------------------------------

    fn lines(p: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(p)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or(serde_json::Value::String(l.into())))
            .collect()
    }

    fn end_of(p: &Path) -> crate::trace::RunEnd {
        let last = std::fs::read_to_string(p).unwrap();
        let last = last.lines().last().unwrap().to_string();
        let e: crate::trace::RunEnd = serde_json::from_str(&last).unwrap();
        assert_eq!(e.kind, crate::trace::END);
        e
    }

    /// A stand-in `claude`: a shell script that ignores its arguments.
    #[cfg(unix)]
    fn claude(dir: &Path, body: &str) -> ClaudeRunner {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("claude");
        std::fs::write(
            &bin,
            format!("#!/bin/sh\n[ -n \"$WARM\" ] && exit 0\n{body}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        // On Linux, exec fails with "Text file busy" while any process still
        // holds the script open for writing: a test on another thread can
        // fork just as it is written, and its child keeps that descriptor
        // until it execs. Run it once, doing nothing, until that has passed.
        for _ in 0..50 {
            match crate::process::command(&bin).env("WARM", "1").status() {
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    crate::daemon::nap(Duration::from_millis(100));
                }
                r => {
                    r.unwrap();
                    break;
                }
            }
        }
        ClaudeRunner {
            bin: bin.to_string_lossy().into_owned(),
        }
    }

    #[cfg(unix)]
    fn streaming(dir: &Path, timeout_secs: u64) -> RunRequest {
        RunRequest {
            role: "reviewer".into(),
            cwd: dir.to_path_buf(),
            timeout_secs,
            stream: Some(dir.join("runs/20261012T215146-reviewer.jsonl")),
            env: vec![
                (
                    "FIX".into(),
                    format!(
                        "{}/tests/fixtures/claude/stream.jsonl",
                        env!("CARGO_MANIFEST_DIR")
                    ),
                ),
                ("GO".into(), dir.join("go").to_string_lossy().into_owned()),
            ],
            ..Default::default()
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_run_streams_line_by_line_while_it_runs() {
        let dir = tempfile::tempdir().unwrap();
        // One line, then wait for the test before the rest.
        let runner = claude(
            dir.path(),
            "head -n 1 \"$FIX\"\nwhile [ ! -f \"$GO\" ]; do sleep 0.02; done\ntail -n +2 \"$FIX\"",
        );
        let req = streaming(dir.path(), 60);
        let stream = req.stream.clone().unwrap();
        let running = std::thread::spawn(move || runner.run(&req));
        let deadline = Instant::now() + Duration::from_secs(20);
        while std::fs::read_to_string(&stream).map_or(true, |s| s.is_empty()) {
            assert!(Instant::now() < deadline, "the first line never arrived");
            crate::daemon::nap(Duration::from_millis(20));
        }
        // The process is still running, and its first line is readable.
        let fix = fixture("claude/stream.jsonl");
        let first = format!("{}\n", fix.lines().next().unwrap());
        assert_eq!(std::fs::read_to_string(&stream).unwrap(), first);
        assert!(!running.is_finished());
        let mut tail = crate::trace::Tail::default();
        let steps = tail.poll(&stream).unwrap();
        assert_eq!(steps[0].kind, crate::trace::StepKind::Start);
        assert!(!tail.info.ended);
        // The runner holds the writer's lock for as long as the run goes.
        assert_eq!(writer_gone(&stream), Some(false));

        std::fs::write(dir.path().join("go"), "").unwrap();
        let o = running.join().unwrap();
        assert_eq!(o.failure, None, "{o:?}");
        assert_eq!(o.raw, fix, "raw is stdout, without the end line");
        assert_eq!(o.session_id(), Some("91aedf44-f4cd-4efa-8e74-768d46a9dc75"));
        let text = std::fs::read_to_string(&stream).unwrap();
        assert_eq!(text.lines().count(), fix.lines().count() + 1);
        assert!(text.starts_with(&fix));
        let e = end_of(&stream);
        assert_eq!((e.exit_code, e.failure), (Some(0), None));
        tail.poll(&stream).unwrap();
        assert!(tail.info.ended);
        assert!(tail.info.resume_command().is_some());
        // Nothing else: no `.json` result, no stderr when it is empty; the
        // writer's lock is free again.
        let mut names: Vec<_> = std::fs::read_dir(dir.path().join("runs"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "20261012T215146-reviewer.jsonl",
                "20261012T215146-reviewer.lock"
            ]
        );
        assert_eq!(writer_gone(&stream), Some(true));
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_run_ends_its_stream_on_a_line_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let runner = claude(
            dir.path(),
            "printf '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\\nhalf a li'\necho oops >&2\nexit 3",
        );
        let req = streaming(dir.path(), 60);
        let stream = req.stream.clone().unwrap();
        let o = runner.run(&req);
        assert!(matches!(o.failure, Some(Failure::Error { .. })), "{o:?}");
        assert_eq!(o.stderr.trim(), "oops");
        let l = lines(&stream);
        assert_eq!(l.len(), 3, "{l:?}");
        assert_eq!(l[0]["subtype"], "init");
        assert_eq!(l[1], "half a li");
        let e = end_of(&stream);
        assert_eq!(e.exit_code, Some(3));
        assert!(
            e.failure
                .as_deref()
                .unwrap()
                .starts_with("unreadable claude -p output"),
            "{e:?}"
        );
        assert!(
            e.failure.as_deref().unwrap().contains("oops"),
            "the failure says what stderr said: {e:?}"
        );
        assert!(
            !e.failure.as_deref().unwrap().contains("session_id"),
            "{e:?}"
        );
        let (_, info) = crate::trace::read(&stream).unwrap();
        assert!(info.ended && info.failure.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn a_timed_out_run_still_ends_its_stream() {
        let dir = tempfile::tempdir().unwrap();
        let runner = claude(dir.path(), "head -n 1 \"$FIX\"\nexec sleep 30");
        // (`claude` has run it once already: macOS checks a new executable on
        // its first run, which can take longer than the time limit.)
        let req = streaming(dir.path(), 1);
        let stream = req.stream.clone().unwrap();
        let o = runner.run(&req);
        assert_eq!(o.failure, Some(Failure::Timeout));
        let l = lines(&stream);
        assert_eq!(l.len(), 2, "{l:?}");
        assert_eq!(l[0]["subtype"], "init");
        let e = end_of(&stream);
        assert_eq!(e.exit_code, None);
        assert_eq!(e.failure.as_deref(), Some("timed out"));
        assert!(e.duration_ms >= 1000);
    }

    #[test]
    fn an_early_wake_is_not_a_timeout() {
        // A wait that comes back before the deadline without an exit, as
        // wait_timeout does when it rounds what is left down to whole
        // milliseconds, is asked again for the time left.
        let limit = Duration::from_millis(200);
        let start = Instant::now();
        let mut asked = Vec::new();
        let r = wait_until(start + limit, |left| {
            // The first wait wakes at once, early; the next ones sleep out
            // what is left, so a slow runner can't skip the second ask.
            if !asked.is_empty() {
                crate::daemon::nap(left);
            }
            asked.push(left);
            Ok::<_, std::io::Error>(None::<u8>)
        });
        assert!(matches!(r, Ok(None)), "{r:?}");
        assert!(start.elapsed() >= limit, "{:?}", start.elapsed());
        assert!(asked.len() > 1, "{asked:?}");
        assert!(
            asked.iter().all(|d| *d >= Duration::from_millis(1)),
            "never a zero wait, which would spin: {asked:?}"
        );
        // An exit after an early wake is the exit.
        let mut calls = 0;
        let r = wait_until(Instant::now() + limit, |_| {
            calls += 1;
            Ok::<_, std::io::Error>((calls == 3).then_some(7u8))
        });
        assert_eq!(r.unwrap(), Some(7));
        assert_eq!(calls, 3);
    }

    #[test]
    fn a_wait_error_is_not_a_timeout() {
        let mut calls = 0;
        let r = wait_until(Instant::now() + Duration::from_secs(60), |_| {
            calls += 1;
            Err::<Option<u8>, _>(std::io::Error::other("no child"))
        });
        assert_eq!(r.unwrap_err().to_string(), "no child");
        assert_eq!(calls, 1, "an error ends the wait at once");
    }

    #[test]
    fn settle_tells_an_exit_a_timeout_and_an_error_apart() {
        assert_eq!(settle(Ok(Some(3u8)), "claude").unwrap(), 3);
        assert!(matches!(
            settle::<u8>(Ok(None), "claude"),
            Err(Failure::Timeout)
        ));
        match settle::<u8>(Err(std::io::Error::other("no child")), "claude") {
            Err(Failure::Error { message }) => {
                assert_eq!(message, "could not wait for claude: no child")
            }
            other => panic!("{other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_timed_out_wait_lasts_its_limit() {
        // wait_timeout alone can come back a millisecond early (see
        // `wait_until`); this is the real thing, a few times over.
        use wait_timeout::ChildExt;
        for _ in 0..10 {
            let mut child = crate::process::command("sleep").arg("30").spawn().unwrap();
            let limit = Duration::from_millis(50);
            let start = Instant::now();
            let r = wait_until(start + limit, |left| child.wait_timeout(left));
            let took = start.elapsed();
            let _ = child.kill();
            let _ = child.wait();
            assert!(matches!(r, Ok(None)), "{r:?}");
            assert!(took >= limit, "{took:?}");
        }
    }

    #[test]
    fn a_run_that_cannot_start_still_ends_its_stream() {
        let dir = tempfile::tempdir().unwrap();
        let stream = dir.path().join("runs/20261012T215146-fixer.jsonl");
        let runner = ClaudeRunner {
            bin: dir
                .path()
                .join("no-claude-here")
                .to_string_lossy()
                .into_owned(),
        };
        let o = runner.run(&RunRequest {
            role: "fixer".into(),
            cwd: dir.path().to_path_buf(),
            stream: Some(stream.clone()),
            ..Default::default()
        });
        assert!(matches!(o.failure, Some(Failure::Error { .. })));
        assert_eq!(lines(&stream).len(), 1);
        let e = end_of(&stream);
        assert!(e.failure.unwrap().starts_with("could not run"));
        // Without a stream path, a run writes nothing.
        let quiet = tempfile::tempdir().unwrap();
        runner.run(&RunRequest {
            cwd: quiet.path().to_path_buf(),
            ..Default::default()
        });
        assert_eq!(std::fs::read_dir(quiet.path()).unwrap().count(), 0);
    }

    fn fake(dir: &Path, steps: serde_json::Value) -> FakeRunner {
        let script = dir.join("script.json");
        std::fs::write(&script, serde_json::json!({ "steps": steps }).to_string()).unwrap();
        FakeRunner::new(script)
    }

    fn fake_req(dir: &Path, n: u32) -> RunRequest {
        RunRequest {
            role: "reviewer".into(),
            cwd: dir.to_path_buf(),
            stream: Some(dir.join(format!("runs/20261012T21514{n}-reviewer.jsonl"))),
            ..Default::default()
        }
    }

    #[test]
    fn the_fake_runner_streams_like_claude() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(
            format!(
                "{}/tests/fixtures/claude/stream.jsonl",
                env!("CARGO_MANIFEST_DIR")
            ),
            dir.path().join("stream.jsonl"),
        )
        .unwrap();
        let r = fake(
            dir.path(),
            serde_json::json!([
                {"role": "reviewer", "result": "Looks fine", "session_id": "s1"},
                {"role": "reviewer", "stream": [
                    {"type": "assistant", "message": {"content": [
                        {"type": "tool_use", "name": "Bash", "input": {"command": "cargo test"}}
                    ]}}
                ]},
                {"role": "reviewer", "fixture": "stream.jsonl"},
                {"role": "reviewer", "fail": "timeout"}
            ]),
        );
        use crate::trace::StepKind::*;
        let kinds = |p: &Path| -> Vec<crate::trace::StepKind> {
            crate::trace::read(p)
                .unwrap()
                .0
                .iter()
                .map(|s| s.kind)
                .collect()
        };

        // A plausible stream: init, what it said, the result, the end.
        let req = fake_req(dir.path(), 1);
        let o = r.run(&req);
        let stream = req.stream.unwrap();
        assert_eq!(o.failure, None);
        assert_eq!(o.session_id(), Some("s1"));
        assert_eq!(o.raw.lines().count(), 3, "raw has no end line");
        assert_eq!(kinds(&stream), [Start, Text, Done, End]);
        let (steps, info) = crate::trace::read(&stream).unwrap();
        assert_eq!(steps[1].text, "Looks fine");
        assert_eq!(info.session_id.as_deref(), Some("s1"));
        assert_eq!(info.cwd.as_deref(), Some(dir.path()));
        assert!(info.ended && info.failure.is_none());

        // The script's own lines before the result.
        let req = fake_req(dir.path(), 2);
        r.run(&req);
        let stream = req.stream.unwrap();
        assert_eq!(kinds(&stream), [Tool, Done, End]);
        assert_eq!(crate::trace::read(&stream).unwrap().0[0].text, "cargo test");

        // A recorded stream, replayed.
        let req = fake_req(dir.path(), 3);
        let o = r.run(&req);
        let stream = req.stream.unwrap();
        assert_eq!(o.failure, None);
        assert_eq!(o.session_id(), Some("91aedf44-f4cd-4efa-8e74-768d46a9dc75"));
        assert_eq!(
            kinds(&stream),
            [Start, Tool, Result, Tool, Result, Text, Done, End]
        );

        // A scripted failure ends the stream with it.
        let req = fake_req(dir.path(), 4);
        r.run(&req);
        let e = end_of(req.stream.as_ref().unwrap());
        assert_eq!(e.failure.as_deref(), Some("timed out"));

        // No step left: the stream is only its end.
        let req = fake_req(dir.path(), 5);
        r.run(&req);
        let stream = req.stream.unwrap();
        assert_eq!(lines(&stream).len(), 1);
        assert!(end_of(&stream).failure.unwrap().contains("no step left"));
    }

    #[cfg(unix)]
    #[test]
    fn the_fake_runner_streams_before_its_commands() {
        // A step's commands see the run going: started, not ended.
        let dir = tempfile::tempdir().unwrap();
        let req = fake_req(dir.path(), 1);
        let stream = req.stream.clone().unwrap();
        let mid = dir.path().join("mid.jsonl");
        let r = fake(
            dir.path(),
            serde_json::json!([{"role": "reviewer", "run": [
                format!("cp '{}' '{}'", stream.display(), mid.display())
            ]}]),
        );
        r.run(&req);
        let (steps, info) = crate::trace::read(&mid).unwrap();
        assert_eq!(steps.len(), 2, "{steps:?}");
        assert!(!info.ended);
        assert!(crate::trace::read(&stream).unwrap().1.ended);
    }
}
