//! Every model run is `claude -p` (ADR 0006, ADR 0013). This module owns the
//! command shape, the timeout, failure detection, and recording usage, so
//! the assayer, fixer, intake and librarian only build a [`RunRequest`].
//!
//! ```text
//! claude -p --plugin-dir <dir> --agent <name> --model <m> --output-format json
//!   --json-schema <schema> --max-budget-usd <n> -- "<prompt>"   (null stdin)
//! ```
//!
//! Tests never spend model usage: `HALL_RUNNER=fake:<script.json>` swaps in
//! [`FakeRunner`], which replays scripted results and side effects.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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
        a.extend(["--output-format".into(), "json".into()]);
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
    let mut extra_args = repo.config.claude_args.clone();
    extra_args.push("--strict-mcp-config".into());
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
    })
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
    /// stdout as printed, for the item's `runs/` directory.
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
/// tests, else the real `claude` (`HALL_CLAUDE`, else `claude`).
pub fn from_env() -> Box<dyn Runner> {
    if let Ok(v) = std::env::var("HALL_RUNNER")
        && let Some(script) = v.strip_prefix("fake:")
    {
        return Box::new(FakeRunner::new(PathBuf::from(script)));
    }
    Box::new(ClaudeRunner {
        bin: std::env::var("HALL_CLAUDE").unwrap_or_else(|_| "claude".into()),
    })
}

/// Runs the real `claude` binary.
pub struct ClaudeRunner {
    pub bin: String,
}

impl Runner for ClaudeRunner {
    fn run(&self, req: &RunRequest) -> RunOutcome {
        let start = Instant::now();
        let ms = |s: Instant| s.elapsed().as_millis() as u64;
        let mut cmd = Command::new(&self.bin);
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
        for (k, v) in &req.env {
            cmd.env(k, v);
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return RunOutcome::failed(
                    Failure::Error {
                        message: format!("could not run {}: {e}", self.bin),
                    },
                    String::new(),
                    String::new(),
                    ms(start),
                );
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
        let out = child.stdout.take().map(reader);
        let err = child.stderr.take().map(reader);
        let status = if req.timeout_secs == 0 {
            child.wait().ok()
        } else {
            use wait_timeout::ChildExt;
            match child.wait_timeout(Duration::from_secs(req.timeout_secs)) {
                Ok(Some(s)) => Some(s),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    None
                }
            }
        };
        let raw = out.and_then(|h| h.join().ok()).unwrap_or_default();
        let stderr = err.and_then(|h| h.join().ok()).unwrap_or_default();
        let Some(status) = status else {
            return RunOutcome::failed(Failure::Timeout, raw, stderr, ms(start));
        };
        let result = PrintResult::parse(&raw).ok();
        let failure = classify(result.as_ref(), &raw, &stderr, req.json_schema.is_some());
        RunOutcome {
            result,
            raw,
            stderr,
            exit_code: status.code(),
            duration_ms: ms(start),
            failure,
        }
    }
}

fn reader<R: Read + Send + 'static>(mut r: R) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = r.read_to_end(&mut s);
        String::from_utf8_lossy(&s).into_owned()
    })
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
    if failed {
        let text = format!(
            "{} {}",
            result.and_then(|r| r.result.as_deref()).unwrap_or(raw),
            stderr
        );
        if let Some(until) = rate_limit(&text) {
            return Some(Failure::RateLimited { until });
        }
    }
    let Some(r) = result else {
        return Some(Failure::Error {
            message: format!(
                "unreadable claude -p output: {}",
                crate::output::clip(if raw.trim().is_empty() { stderr } else { raw }, 200)
            ),
        });
    };
    // An API error before any model ran: an unknown or unavailable model
    // ("[claude-code:unrecognized_model] …"). It costs nothing.
    if r.terminal_reason.as_deref() == Some("api_error")
        && r.model_usage.is_empty()
        && format!("{raw} {stderr}")
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

/// Claude's plan limit, as worded in its results: "Claude AI usage limit
/// reached|1760000000", "You've hit your limit · resets 3pm", "rate limit".
/// Returns the reset time when one is given as a Unix timestamp.
pub fn rate_limit(text: &str) -> Option<Option<DateTime<Utc>>> {
    let t = text.to_ascii_lowercase();
    let hit = [
        "usage limit",
        "rate limit",
        "rate_limit",
        "hit your limit",
        "limit reached",
    ]
    .iter()
    .any(|p| t.contains(p));
    if !hit {
        return None;
    }
    let until = text.split('|').nth(1).and_then(|s| {
        let digits: String = s.trim().chars().take_while(char::is_ascii_digit).collect();
        DateTime::from_timestamp(digits.parse().ok()?, 0)
    });
    Some(until)
}

// --- the fake runner, for tests ---------------------------------------------

/// A scripted step for [`FakeRunner`]:
///
/// ```json
/// {"role": "reviewer", "structured": {…}, "files": {"src/a.rs": "…"},
///  "run": ["git commit -qam fix", "{hall} fixed F1"], "fail": "timeout"}
/// ```
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
    /// A fixture file (relative to the script) holding a whole result.
    pub fixture: Option<String>,
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
        let step = match self.take(&req.role) {
            Ok(s) => s,
            Err(message) => {
                return RunOutcome::failed(
                    Failure::Error { message },
                    String::new(),
                    String::new(),
                    0,
                );
            }
        };
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
        let print = if let Some(p) = &step.print {
            p.clone()
        } else if let Some(f) = &step.fixture {
            let path = self.script.parent().unwrap_or(Path::new(".")).join(f);
            std::fs::read_to_string(path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            let model = step
                .model
                .clone()
                .unwrap_or_else(|| "claude-haiku-5-5".into());
            let cost = step.cost_usd.unwrap_or(0.001);
            let mut v = serde_json::json!({
                "type": "result", "subtype": "success", "is_error": false,
                "result": step.result.clone().unwrap_or_else(|| "done".into()),
                "session_id": step.session_id.clone().unwrap_or_else(|| "fake-session".into()),
                "num_turns": 3, "duration_ms": 1200, "total_cost_usd": cost,
                "modelUsage": { model: {
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
        let raw = print.to_string();
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
        let failure =
            failure.or_else(|| classify(result.as_ref(), &raw, "", req.json_schema.is_some()));
        RunOutcome {
            result,
            raw,
            stderr: String::new(),
            exit_code: Some(0),
            duration_ms: 1200,
            failure,
        }
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

/// Checks the budgets, runs, records usage and the raw result, and handles
/// failures: one retry (the next tick), then a hold.
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
    repo.hall
        .live(&format!("{} {} run started", label(item), req.role));
    let mut outcome = runner.run(req);
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
        let mut again = req.clone();
        again.model = Some(fb);
        again.fallback_model = None;
        outcome = runner.run(&again);
    }
    if let Some(Failure::ModelUnavailable { model }) = &mut outcome.failure
        && model.is_empty()
    {
        *model = req.model.clone().unwrap_or_default();
    }
    if let Some(id) = item {
        let dir = repo.hall.item_runs(id);
        std::fs::create_dir_all(&dir)?;
        let stamp = Utc::now().format("%Y%m%dT%H%M%S");
        let _ = std::fs::write(dir.join(format!("{stamp}-{}.json", req.role)), &outcome.raw);
        if !outcome.stderr.trim().is_empty() {
            let _ = std::fs::write(
                dir.join(format!("{stamp}-{}.stderr", req.role)),
                &outcome.stderr,
            );
        }
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
                reason: format!("the {} run failed twice: {}", req.role, failure.describe()),
            },
        )?;
    }
    Ok(Metered::Failed {
        outcome,
        failure,
        held,
    })
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
        assert!(s.contains("--output-format json --json-schema {} --max-budget-usd 3"));
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
        events.push(ev(Body::Released { reason: None }));
        assert_eq!(failures_in_a_row(&events, "reviewer"), 0);
        events.push(fail());
        events.push(ev(Body::PulledBack { reason: None }));
        assert_eq!(failures_in_a_row(&events, "reviewer"), 0);
    }
}
