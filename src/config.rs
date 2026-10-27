//! `.hallmark.toml`: optional, and every key has a default.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, HallError, Result};

pub const FILE_NAME: &str = ".hallmark.toml";

/// `update` as `hall init` wrote it before the review moved to its own
/// comment (ADR 0039). Read as today's default.
const OLD_UPDATE: &str = "git push --force-with-lease={branch}:{pr_head} origin {branch} && gh pr edit {pr} --base {base} && gh pr comment {pr} --body-file {body_file}";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Config {
    /// The branch reviews compare against and the reviewer reads its rules from.
    pub base: String,
    /// Creates an item's worktree. Placeholders: {repo} {id} {branch} {slug} {base}.
    pub worktree: String,
    /// The item's branch name. Placeholders: {id} {slug}.
    pub branch: String,
    /// Ships a hallmarked commit. Placeholders: {branch} {id} {base}
    /// {title} {body_file} (the PR description the shipper writes: What,
    /// Why, How, Testing and How to revert) {review_file} (the review, which `review_comment` posts)
    /// {issue}.
    pub ship: String,
    /// Updates the open PR of an item that already has one (an adopted PR,
    /// or a shipped item reopened for a rebase or a conflict fix), instead
    /// of `ship`. Placeholders: those of `ship`, plus {pr} (the PR's URL)
    /// and {pr_head} (the commit the push replaces: the PR's head when the
    /// item took it over or last shipped, or the branch's head on origin when
    /// this item hallmarked that commit, as after an update that pushed and
    /// then failed; ADR 0042).
    pub update: String,
    /// Posts the review on the PR as a comment, after `ship` opens it and
    /// after every `update`: the description keeps only What, Why, How, Testing and How to revert.
    /// Runs when the PR's URL is known. Placeholders: {pr} (the PR's URL)
    /// {review_file} {branch} {id} {base}. Empty: no comment.
    pub review_comment: String,
    /// The repo's checks, run in order at the frozen commit: a command, or
    /// `{ cmd = "cargo test", timeout = "40m" }` for one with its own time
    /// limit.
    pub gates: Vec<Gate>,
    /// Gates whose results are never cached, such as tests that reach the
    /// network: they run on every check, even on a tree that passed before.
    pub gates_no_cache: Vec<String>,
    /// How long each gate may run before it is killed ("20m").
    /// A gate's own `timeout` overrides it.
    pub gates_timeout: String,
    /// Commands the headless dev runs (the fixer, the Delivery office) may
    /// run besides the gates, as prefixes: a narrower test run, or what
    /// regenerates generated files (snapshots) after a conflict.
    pub dev_commands: Vec<String>,
    /// The fix engine: `claude -p` (the fixer agent), or another command run
    /// in the item's worktree, such as Ralph Orchestrator. Placeholders:
    /// {id} {worktree} {findings_file} (JSON from `hall findings`).
    pub fixer: String,
    /// Extra arguments for every headless `claude -p` run. Flags that would
    /// loosen a run's permissions or sandbox are refused (ADR 0053).
    pub claude_args: Vec<String>,
    /// What every headless `claude -p` run is confined to (ADR 0053).
    pub sandbox: Sandbox,
    /// Rejected reviews before the item is escalated to you.
    pub max_rejects: u32,
    /// Nits a review may record: the reviewer is told the cap, and Hallmark
    /// keeps only the first this many nits of each verdict (ADR 0058).
    pub max_nits: u32,
    /// Failed checks in a row before the item is held for you (kind
    /// `checks`), so a fix that never turns the gates green can't loop.
    /// 0: never.
    pub max_check_failures: u32,
    /// `hall propose-ready` sends the item straight to review when nothing
    /// is left to confirm (no open question, a clean worktree, no open
    /// parts). `false`: it always waits for your call.
    pub auto_ready: bool,
    /// The watchman flags items with no event for this long.
    pub stuck_after: String,
    /// "all", or a list of lens names.
    pub lenses: Lenses,
    /// Paths whose changes get a second, security-only review pass at
    /// full effort, whose findings join the verdict (ADR 0064). Globs;
    /// empty for none.
    pub security_review: Vec<String>,
    /// Files the repo-rules lens reads.
    pub rules: Vec<String>,
    pub capture: Capture,
    pub budget: Budget,
    /// How the Front Desk plans a request.
    pub intake: Intake,
    /// How many items each phase works on at once, per repo.
    pub parallel: Parallel,
    /// The merger: whether shipped PRs merge without you, and how.
    pub merge: Merge,
    /// Notification channels. Usually set in the user config
    /// (`<user data dir>/hallmark/config.toml`), which this overrides.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify: Option<Notify>,
    pub library: Library,
    /// Skill packs by other authors, and the roles that follow them
    /// (`hall extensions`).
    pub extensions: Extensions,
}

impl Config {
    /// What a headless dev run may run: the gates, then `dev_commands`.
    pub fn run_commands(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let gates = self.gates.iter().map(Gate::cmd);
        for c in gates.chain(self.dev_commands.iter().map(String::as_str)) {
            let c = c.trim();
            if !c.is_empty() && !out.iter().any(|o| o == c) {
                out.push(c.to_string());
            }
        }
        out
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            base: "main".into(),
            worktree: "git worktree add ../{repo}-{id} -b {branch}".into(),
            branch: "hm/{id}-{slug}".into(),
            ship: "git push -u origin {branch} && gh pr create --base {base} --head {branch} --title {title} --body-file {body_file}".into(),
            update: "git push --force-with-lease={branch}:{pr_head} origin {branch} && gh pr edit {pr} --base {base}".into(),
            review_comment: "gh pr comment {pr} --body-file {review_file}".into(),
            gates: Vec::new(),
            gates_no_cache: Vec::new(),
            gates_timeout: "20m".into(),
            dev_commands: Vec::new(),
            fixer: "claude -p".into(),
            claude_args: Vec::new(),
            sandbox: Sandbox::default(),
            max_rejects: 3,
            max_nits: 3,
            max_check_failures: 3,
            auto_ready: true,
            stuck_after: "30m".into(),
            lenses: Lenses::All("all".into()),
            security_review: Vec::new(),
            rules: vec!["CLAUDE.md".into(), "AGENTS.md".into()],
            capture: Capture::default(),
            budget: Budget::default(),
            intake: Intake::default(),
            parallel: Parallel::default(),
            merge: Merge::default(),
            notify: None,
            library: Library::default(),
            extensions: Extensions::default(),
        }
    }
}

/// A gate: its command, or a table with the command and its own time limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Gate {
    /// A command, run with `gates_timeout`.
    Cmd(String),
    /// `{ cmd = "cargo test", timeout = "40m" }`.
    Timed(TimedGate),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimedGate {
    /// The command.
    pub cmd: String,
    /// How long it may run ("40m"). Default: `gates_timeout`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
}

impl Gate {
    pub fn cmd(&self) -> &str {
        match self {
            Gate::Cmd(c) => c,
            Gate::Timed(t) => &t.cmd,
        }
    }

    /// The gate's own time limit, as written.
    pub fn timeout(&self) -> Option<&str> {
        match self {
            Gate::Cmd(_) => None,
            Gate::Timed(t) => t.timeout.as_deref(),
        }
    }
}

impl std::fmt::Display for Gate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.cmd())
    }
}

impl From<&str> for Gate {
    fn from(c: &str) -> Self {
        Gate::Cmd(c.into())
    }
}

impl Config {
    /// The gates' commands, in order.
    pub fn gate_commands(&self) -> Vec<String> {
        self.gates.iter().map(|g| g.cmd().to_string()).collect()
    }

    /// How long `gate` may run, in seconds: its own
    /// `timeout`, else `gates_timeout`, else 20 minutes. Never the
    /// reviewer's limit: a slow test suite isn't a slow review.
    pub fn gate_timeout_secs(&self, gate: &Gate) -> u64 {
        gate.timeout()
            .and_then(parse_duration)
            .or_else(|| parse_duration(&self.gates_timeout))
            .unwrap_or(1200)
            .max(0) as u64
    }

    /// The longest any gate may run, in seconds (0: no gates, or none has a
    /// limit). A lease held while the gates run must outlast it.
    pub fn longest_gate_secs(&self) -> u64 {
        self.gates
            .iter()
            .map(|g| self.gate_timeout_secs(g))
            .max()
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Lenses {
    All(String),
    List(Vec<String>),
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, clap::ValueEnum,
)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    /// Edits outside an item's worktree are denied with instructions.
    #[default]
    Gate,
    /// Claude Code asks you before such an edit.
    Ask,
    /// Only `/hallmark` starts items.
    Off,
}

impl CaptureMode {
    pub fn as_str(self) -> &'static str {
        match self {
            CaptureMode::Gate => "gate",
            CaptureMode::Ask => "ask",
            CaptureMode::Off => "off",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Capture {
    pub mode: CaptureMode,
    /// Paths that need no item: edits and commits that touch only these are
    /// never gated. Globs relative to the repo root. By default, docs and
    /// Markdown: questions, documents, architecture and requirements aren't
    /// development work, so they don't become items.
    pub free: Vec<String>,
    /// How the push gate treats commands it can't read. `strict`: inside an
    /// item's worktree, it also denies those that may push or write a note
    /// (scripts such as `make push`, unknown git subcommands, `gh` commands
    /// that change GitHub). `parse`: only what it reads as a push.
    pub push_guard: PushGuard,
    /// Commands the strict push guard lets through, as prefixes
    /// (`make release`). A command the gate reads as a push is still gated.
    pub push_allow: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PushGuard {
    /// Inside an item's worktree, deny what may push but can't be read too.
    #[default]
    Strict,
    /// Deny only what the gate reads as a push or a note write.
    Parse,
}

impl Default for Capture {
    fn default() -> Self {
        Capture {
            mode: CaptureMode::default(),
            free: vec!["docs/".into(), "*.md".into()],
            push_guard: PushGuard::default(),
            push_allow: Vec::new(),
        }
    }
}

impl Capture {
    /// `push_allow` as word prefixes, for the gate.
    pub fn push_allow_prefixes(&self) -> Vec<Vec<String>> {
        self.push_allow
            .iter()
            .map(|c| crate::gate::words(c))
            .filter(|w| !w.is_empty())
            .collect()
    }

    /// Whether `rel` (a path relative to the repo root, `/`-separated) matches
    /// `free`. Use [`Config::is_free`], which also protects review inputs.
    pub fn matches_free(&self, rel: &str) -> bool {
        crate::assayer::base::globset(&self.free).is_match(rel)
    }
}

impl Config {
    /// Whether `rel` (a path relative to the repo root, `/`-separated) needs
    /// no item. Review inputs never do: this file, `.hallmark/` and the rules
    /// files steer the reviewer, so changing them is development work.
    pub fn is_free(&self, rel: &str) -> bool {
        let rel = rel.trim_start_matches("./");
        if rel == FILE_NAME
            || rel.starts_with(".hallmark/")
            || self.rules.iter().any(|r| {
                let r = r.split('#').next().unwrap_or(r);
                r.trim_start_matches("./") == rel
            })
        {
            return false;
        }
        self.capture.matches_free(rel)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Budget {
    /// Daily cap for the whole review loop, in tokens or dollars ("4M",
    /// "$15"). Default "4M"; "none" for no cap.
    #[serde(deserialize_with = "cap_or_none", serialize_with = "cap_or_none_str")]
    pub daily: Option<String>,
    /// Cap per item, in tokens or dollars ("800k", "$5"). Default "800k";
    /// "none" for no cap.
    #[serde(deserialize_with = "cap_or_none", serialize_with = "cap_or_none_str")]
    pub per_item: Option<String>,
    pub review_model: String,
    pub fix_model: String,
    pub intake_model: String,
    pub librarian_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<RoleBudget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<RoleBudget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intake: Option<RoleBudget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub librarian: Option<RoleBudget>,
}

/// `[budget] daily` when the repo doesn't set it.
pub const DEFAULT_DAILY: &str = "4M";
/// `[budget] per_item` when the repo doesn't set it.
pub const DEFAULT_PER_ITEM: &str = "800k";

/// Whether a cap's value turns the cap off.
pub fn is_no_cap(s: &str) -> bool {
    s.trim().eq_ignore_ascii_case("none")
}

/// A cap as written: `"none"` (any case) is no cap.
fn cap_or_none<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<String>, D::Error> {
    let s: Option<String> = Option::deserialize(d)?;
    Ok(s.filter(|s| !is_no_cap(s)))
}

/// No cap is written `"none"`: a missing key means the default cap.
fn cap_or_none_str<S: serde::Serializer>(
    v: &Option<String>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_str(v.as_deref().unwrap_or("none"))
}

/// The four budgeted model roles, and the room each one works in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Reviewer,
    Fixer,
    Intake,
    Librarian,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Reviewer => "reviewer",
            Role::Fixer => "fixer",
            Role::Intake => "intake",
            Role::Librarian => "librarian",
        }
    }

    /// The per-run cap in USD when `max_usd` is unset. A run that needs
    /// more than this is rare; real reviews of sensitive changes at max
    /// effort cost over $5.
    pub fn default_max_usd(self) -> f64 {
        match self {
            Role::Reviewer => 6.0,
            Role::Fixer => 2.0,
            Role::Intake => 0.10,
            Role::Librarian => 0.20,
        }
    }

    /// The room usage is booked to.
    pub fn room(self) -> &'static str {
        match self {
            Role::Reviewer => "assay",
            Role::Fixer => "repair",
            Role::Intake => "front",
            Role::Librarian => "library",
        }
    }
}

impl Budget {
    pub fn role(&self, r: Role) -> Option<&RoleBudget> {
        match r {
            Role::Reviewer => self.review.as_ref(),
            Role::Fixer => self.fix.as_ref(),
            Role::Intake => self.intake.as_ref(),
            Role::Librarian => self.librarian.as_ref(),
        }
    }

    /// `[budget.review] model`, else `review_model`, and so on.
    pub fn model(&self, r: Role) -> String {
        if let Some(m) = self.role(r).and_then(|b| b.model.clone()) {
            return m;
        }
        match r {
            Role::Reviewer => self.review_model.clone(),
            Role::Fixer => self.fix_model.clone(),
            Role::Intake => self.intake_model.clone(),
            Role::Librarian => self.librarian_model.clone(),
        }
    }

    /// The per-run cap passed as `--max-budget-usd`: `[budget.<role>]
    /// max_usd`, else the role's default; `"none"` means no cap.
    pub fn max_usd(&self, r: Role) -> Option<f64> {
        match self.role(r).and_then(|b| b.max_usd) {
            Some(MaxUsd::Usd(u)) => Some(u),
            Some(MaxUsd::None) => None,
            None => Some(r.default_max_usd()),
        }
    }

    /// The model to use when the role's model is unavailable or overloaded.
    pub fn fallback(&self, r: Role) -> Option<String> {
        self.role(r).and_then(|b| b.fallback.clone())
    }

    pub fn effort(&self, r: Role) -> Option<String> {
        self.role(r).and_then(|b| b.effort.clone())
    }

    /// The per-run time limit in seconds (default 20 minutes; intake 2).
    pub fn timeout_secs(&self, r: Role) -> i64 {
        self.role(r)
            .and_then(|b| b.timeout.as_deref())
            .and_then(parse_duration)
            .unwrap_or(match r {
                Role::Intake => 120,
                _ => 1200,
            })
    }
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            daily: Some(DEFAULT_DAILY.into()),
            per_item: Some(DEFAULT_PER_ITEM.into()),
            review_model: "opus".into(),
            fix_model: "sonnet".into(),
            intake_model: "haiku".into(),
            librarian_model: "haiku".into(),
            review: None,
            fix: None,
            intake: None,
            librarian: None,
        }
    }
}

/// `[parallel]`: how many items each phase works on at once, per repo
/// (ADR 0026). Checks share build caches and sometimes ports, so each check
/// slot has its own assay worktree; set `checks = 1` when tests can't run
/// side by side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Parallel {
    pub checks: u32,
    pub review: u32,
    pub fix: u32,
    pub ship: u32,
}

impl Default for Parallel {
    fn default() -> Self {
        Parallel {
            checks: 2,
            review: 4,
            fix: 4,
            ship: 2,
        }
    }
}

impl Parallel {
    /// Slots for a daemon worker: the assayer covers checks and review.
    pub fn slots(&self, worker: &str) -> u32 {
        match worker {
            "assayer" => self.checks.max(1) + self.review.max(1),
            "fixer" => self.fix.max(1),
            "shipper" => self.ship.max(1),
            _ => 1,
        }
    }
}

/// Whether the merger merges shipped PRs: the loop's light/dark switch
/// (ADR 0048).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MergeMode {
    /// A person merges every PR. `hall queue` still shows the order.
    #[default]
    Off,
    /// The merger merges each ready PR, one at a time per base, in order.
    Auto,
}

impl MergeMode {
    pub fn as_str(self) -> &'static str {
        match self {
            MergeMode::Off => "off",
            MergeMode::Auto => "auto",
        }
    }
}

/// When the merger brings a PR up to date before it merges (ADR 0051).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MergeUpdate {
    /// Whenever the base moved on a file the PR touches: CI tests exactly
    /// what lands, one PR after another on shared files.
    #[default]
    Files,
    /// Only when GitHub says it conflicts: a clean PR with green CI merges
    /// as it is, and the base's own CI catches a bad combination.
    Conflicts,
}

impl MergeUpdate {
    pub fn as_str(self) -> &'static str {
        match self {
            MergeUpdate::Files => "files",
            MergeUpdate::Conflicts => "conflicts",
        }
    }
}

/// `[sandbox]`: what a headless `claude -p` run may reach (ADR 0053). Its
/// commands run in Claude Code's sandbox; its file tools stay in the
/// directories it runs in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Sandbox {
    pub mode: SandboxMode,
    /// Paths a run's commands may write besides its working directory, the
    /// temp dir and the repo's git directory, such as tool caches
    /// ("~/.cargo").
    pub allow_write: Vec<String>,
    /// Paths a run may never read, with its tools or its commands, in every
    /// mode.
    pub deny_read: Vec<String>,
    /// Hosts a run's commands may reach ("crates.io"). Empty: none.
    pub domains: Vec<String>,
    /// Unix sockets a run's commands may connect to, such as the Nix daemon's.
    pub unix_sockets: Vec<String>,
    /// A run's commands may listen on localhost, for tests that start a server.
    pub local_binding: bool,
    /// Environment variables removed from every run, in every mode.
    pub remove_env: Vec<String>,
}

impl Default for Sandbox {
    fn default() -> Self {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect();
        Sandbox {
            mode: SandboxMode::Auto,
            allow_write: Vec::new(),
            deny_read: s(&[
                "~/.ssh",
                "~/.aws",
                "~/.azure",
                "~/.config/gcloud",
                "~/.config/gh",
                "~/.docker/config.json",
                "~/.gnupg",
                "~/.kube",
                "~/.netrc",
                "~/.npmrc",
                "~/.claude.json",
                "~/.claude/.credentials.json",
            ]),
            domains: Vec::new(),
            unix_sockets: Vec::new(),
            local_binding: true,
            remove_env: s(&[
                "SSH_AUTH_SOCK",
                "GH_TOKEN",
                "GITHUB_TOKEN",
                "GH_ENTERPRISE_TOKEN",
                "GITHUB_ENTERPRISE_TOKEN",
                "AWS_ACCESS_KEY_ID",
                "AWS_SECRET_ACCESS_KEY",
                "AWS_SESSION_TOKEN",
            ]),
        }
    }
}

/// Whether a run's commands go through Claude Code's sandbox (Seatbelt on
/// macOS, bubblewrap on Linux and WSL2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SandboxMode {
    /// Sandboxed, or the run fails where the sandbox can't start; native
    /// Windows runs without it.
    #[default]
    Auto,
    /// Sandboxed, or the run fails.
    Required,
    /// No sandbox. `deny_read` and `remove_env` still apply.
    Off,
}

/// `claude_args` flags that would loosen a run's permissions or sandbox,
/// or bring in code that runs outside it.
const LOOSENING_ARGS: &[&str] = &[
    "--dangerously-skip-permissions",
    "--allow-dangerously-skip-permissions",
    "--permission-mode",
    "--permission-prompt-tool",
    "--settings",
    "--setting-sources",
    "--add-dir",
    "--allowedTools",
    "--allowed-tools",
    "--tools",
    "--mcp-config",
    "--plugin-dir",
];

/// The first `claude_args` entry that would loosen a run, if any.
pub fn loosening_arg(args: &[String]) -> Option<&str> {
    args.iter().map(String::as_str).find(|a| {
        LOOSENING_ARGS
            .iter()
            .any(|f| a == f || a.strip_prefix(f).is_some_and(|r| r.starts_with('=')))
    })
}

/// `[merge]`: the merger (ADR 0048).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Merge {
    pub mode: MergeMode,
    /// Merges one PR, run in the main checkout. Placeholders: {pr} (the
    /// PR's URL) {pr_head} (the hallmarked head it must still be at)
    /// {branch} {base} {id} {title}.
    pub command: String,
    /// Wait for the PR's CI to pass before merging. `false`: merge once the
    /// PR is hallmarked, mergeable and up to date, whatever CI says.
    pub require_ci: bool,
    /// When a PR is rebased before it merges: `files` (the base moved on a
    /// file it touches) or `conflicts` (only when GitHub says it conflicts).
    pub update: MergeUpdate,
    /// Paths a person merges: a PR that touches one waits for you (a `lit`
    /// hold). Globs relative to the repo root.
    pub lit: Vec<String>,
}

impl Default for Merge {
    fn default() -> Self {
        Merge {
            mode: MergeMode::Off,
            command: "gh pr merge {pr} --squash --match-head-commit {pr_head}".into(),
            require_ci: true,
            update: MergeUpdate::Files,
            lit: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SplitMode {
    /// The Front Desk splits a request into parallel parts when it can.
    #[default]
    Auto,
    /// Every request is one item.
    Off,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Intake {
    pub split: SplitMode,
    /// At most this many parallel parts.
    pub max_parts: u32,
}

impl Default for Intake {
    fn default() -> Self {
        Intake {
            split: SplitMode::Auto,
            max_parts: 4,
        }
    }
}

/// `[notify]`: where Hallmark tells you something waits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Notify {
    /// Desktop notifications (macOS, Linux, Windows).
    pub desktop: bool,
    /// `ntfy://ntfy.sh/<topic>` (the default phone channel; the topic is
    /// random) or `telegram://<bot-token>@<chat-id>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    /// A Slack incoming-webhook URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slack: Option<String>,
    /// "22:00-07:30": the phone waits; the desktop still shows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quiet_hours: Option<String>,
    /// Repeat an unanswered entry this often.
    pub remind: String,
    /// The local time of the morning digest; empty for none.
    pub digest: String,
    /// Channels per inbox kind: desktop, phone, slack, digest.
    pub route: Route,
}

impl Default for Notify {
    fn default() -> Self {
        Notify {
            desktop: true,
            phone: None,
            slack: None,
            quiet_hours: None,
            remind: "2h".into(),
            digest: "08:30".into(),
            route: Route::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Route {
    pub question: Vec<String>,
    pub escalated: Vec<String>,
    pub stuck: Vec<String>,
    pub ready: Vec<String>,
    pub budget: Vec<String>,
    pub session_waiting: Vec<String>,
    pub learnings: Vec<String>,
}

impl Default for Route {
    fn default() -> Self {
        let v = |c: &[&str]| c.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        Route {
            question: v(&["desktop", "phone"]),
            escalated: v(&["desktop", "phone"]),
            stuck: v(&["desktop", "phone"]),
            ready: v(&["desktop"]),
            budget: v(&["desktop"]),
            session_waiting: v(&["desktop"]),
            learnings: v(&["digest"]),
        }
    }
}

impl Route {
    /// The channels for an inbox kind.
    pub fn channels(&self, kind: &str) -> &[String] {
        match kind {
            "question" => &self.question,
            "escalated" => &self.escalated,
            "stuck" | "daemon_stopped" => &self.stuck,
            "ci_failed" | "conflict" | "feedback" | "merge_failed" | "lit" | "checks" => {
                &self.escalated
            }
            "ready" => &self.ready,
            "budget" | "run_failed" | "ship_failed" | "held" | "paused" => &self.budget,
            "session_waiting" => &self.session_waiting,
            "learnings" => &self.learnings,
            _ => &self.ready,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchBackend {
    /// SQLite FTS5, built in.
    #[default]
    Builtin,
    /// tobi/qmd, when installed: one collection per repo.
    Qmd,
}

/// `[extensions]`: for each extension, the roles that follow its skills
/// (planner, developer, reviewer, fixer). Empty: off. `hall extensions`
/// lists them with their credits and what each role gets.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Extensions {
    /// Matt Pocock's skills (https://github.com/mattpocock/skills, MIT):
    /// to-tickets for the planner; tdd, diagnosing-bugs and codebase-design
    /// for the developer; code-review for the reviewer; tdd and
    /// diagnosing-bugs for the fixer.
    pub mattpocock: Vec<crate::extensions::Role>,
}

impl Extensions {
    /// The roles that follow extension `id`.
    pub fn roles(&self, id: &str) -> &[crate::extensions::Role] {
        match id {
            "mattpocock" => &self.mattpocock,
            _ => &[],
        }
    }
}

/// `[library]`: the Library of learnings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Library {
    pub search: SearchBackend,
    /// The qmd collection; defaults to `hallmark-<repo>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    /// Tokens of learnings sent to each review.
    pub briefing_limit: String,
    /// Kinds that skip your approval: rule, false-positive, map, decision.
    pub auto_accept: Vec<String>,
    /// Unused learnings are retired after this long.
    pub retire_after: String,
    /// Where learnings were committed before they moved to the local
    /// store (ADR 0027), relative to the repo root: what
    /// `hall library import` reads from the base branch.
    pub dir: String,
}

impl Default for Library {
    fn default() -> Self {
        Library {
            search: SearchBackend::Builtin,
            collection: None,
            briefing_limit: "6k".into(),
            auto_accept: Vec::new(),
            retire_after: "90d".into(),
            dir: ".hallmark/knowledge".into(),
        }
    }
}

/// `<user data dir>/hallmark/config.toml`: settings that belong to you
/// rather than to a repository (your phone's topic, for one).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct UserConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify: Option<Notify>,
    /// Where the programs Hallmark runs live.
    pub tools: Tools,
}

/// `[tools]`: the programs Hallmark runs, for when they aren't on PATH. The
/// daemon's login service starts with a bare PATH, so an absolute path here
/// is the dependable way to point it at them. `HALL_CLAUDE`, `HALL_GH`,
/// `HALL_QMD` and `HALL_CARGO` override these.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Tools {
    /// Claude Code (`claude`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude: Option<String>,
    /// The GitHub CLI (`gh`), for issues, PRs and the default ship command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gh: Option<String>,
    /// tobi/qmd, for `[library] search = "qmd"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qmd: Option<String>,
    /// `cargo`, which `hall update` installs with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cargo: Option<String>,
}

/// A program Hallmark runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Claude,
    Gh,
    Qmd,
    Cargo,
}

impl Tool {
    pub const ALL: [Tool; 4] = [Tool::Claude, Tool::Gh, Tool::Qmd, Tool::Cargo];

    /// The program's name, and its key under `[tools]`.
    pub fn name(self) -> &'static str {
        match self {
            Tool::Claude => "claude",
            Tool::Gh => "gh",
            Tool::Qmd => "qmd",
            Tool::Cargo => "cargo",
        }
    }

    /// The environment variable that overrides `[tools]`.
    pub fn env(self) -> &'static str {
        match self {
            Tool::Claude => "HALL_CLAUDE",
            Tool::Gh => "HALL_GH",
            Tool::Qmd => "HALL_QMD",
            Tool::Cargo => "HALL_CARGO",
        }
    }

    /// `HALL_<TOOL>`, else `[tools] <tool>` in the user config.
    pub fn configured(self) -> Option<String> {
        if let Ok(v) = std::env::var(self.env())
            && !v.trim().is_empty()
        {
            return Some(v);
        }
        let t = UserConfig::load().ok()?.tools;
        match self {
            Tool::Claude => t.claude,
            Tool::Gh => t.gh,
            Tool::Qmd => t.qmd,
            Tool::Cargo => t.cargo,
        }
        .filter(|v| !v.trim().is_empty())
    }

    /// The command to run: [`Tool::configured`], else the bare name, found
    /// on PATH.
    pub fn command(self) -> String {
        self.configured().unwrap_or_else(|| self.name().into())
    }
}

impl UserConfig {
    pub fn path() -> PathBuf {
        crate::repo::user_dir().join("config.toml")
    }

    /// The user config, or the defaults when the file is missing.
    pub fn load() -> Result<UserConfig> {
        match std::fs::read_to_string(Self::path()) {
            Ok(t) => Ok(toml::from_str(&t)?),
            Err(_) => Ok(UserConfig::default()),
        }
    }
}

impl Config {
    /// The repo's `[notify]`, else the user's, else the defaults.
    pub fn notify(&self) -> Notify {
        self.notify
            .clone()
            .or_else(|| UserConfig::load().ok().and_then(|u| u.notify))
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct RoleBudget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Per run, passed as `--max-budget-usd`. Unset: review 6.00, fix
    /// 2.00, intake 0.10, librarian 0.20; "none": no per-run cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_usd: Option<MaxUsd>,
    /// Per room, per day, in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily: Option<String>,
    /// Per run time limit ("20m").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    /// Used when `model` is unavailable (a run that fails fast without
    /// tokens is retried once with it) or overloaded (`--fallback-model`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
}

/// `max_usd`: a per-run cap in USD, or `"none"` for no cap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MaxUsd {
    Usd(f64),
    None,
}

impl Serialize for MaxUsd {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            MaxUsd::Usd(u) => s.serialize_f64(*u),
            MaxUsd::None => s.serialize_str("none"),
        }
    }
}

impl<'de> Deserialize<'de> for MaxUsd {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = MaxUsd;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(r#"an amount in USD, such as 3.0, or "none""#)
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<MaxUsd, E> {
                Ok(MaxUsd::Usd(v))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<MaxUsd, E> {
                Ok(MaxUsd::Usd(v as f64))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<MaxUsd, E> {
                Ok(MaxUsd::Usd(v as f64))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<MaxUsd, E> {
                if v.trim().eq_ignore_ascii_case("none") {
                    Ok(MaxUsd::None)
                } else {
                    Err(E::invalid_value(serde::de::Unexpected::Str(v), &self))
                }
            }
        }
        d.deserialize_any(V)
    }
}

impl JsonSchema for MaxUsd {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "MaxUsd".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "An amount in USD, or \"none\" for no cap",
            "anyOf": [
                { "type": "number", "minimum": 0 },
                { "const": "none" }
            ]
        })
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Config> {
        let mut c: Config = toml::from_str(text)?;
        if let Some(a) = loosening_arg(&c.claude_args) {
            return Err(HallError::new(
                ErrorCode::Usage,
                format!(
                    ".hallmark.toml: claude_args can't include {a}: it would loosen what every headless run is confined to (ADR 0053)"
                ),
            )
            .next(["hall schema config"]));
        }
        // `hall init` wrote the old default, which posted the PR body as a
        // comment; the review comment does that job now (ADR 0039).
        if c.update == OLD_UPDATE {
            c.update = Config::default().update;
        }
        Ok(c)
    }

    /// Reads the first `.hallmark.toml` among `dirs`, or the defaults.
    pub fn load(dirs: &[&Path]) -> Result<(Config, Option<PathBuf>)> {
        for d in dirs {
            let p = d.join(FILE_NAME);
            if p.is_file() {
                let text = std::fs::read_to_string(&p)?;
                return Ok((Config::parse(&text)?, Some(p)));
            }
        }
        Ok((Config::default(), None))
    }

    /// The first plain command words of the ship and update commands, used
    /// by the push gate (`dev submit --pr` → `dev submit`).
    pub fn ship_prefixes(&self) -> Vec<Vec<String>> {
        let mut out: Vec<Vec<String>> = Vec::new();
        for seg in [&self.ship, &self.update]
            .into_iter()
            .flat_map(|c| crate::gate::split_segments(c))
        {
            let words: Vec<String> = crate::gate::words(&seg)
                .into_iter()
                .take_while(|w| !w.starts_with('-') && !w.contains('{'))
                .collect();
            // Commenting on or editing a PR puts no commits on GitHub.
            let no_commits = words.len() >= 3
                && words[0] == "gh"
                && words[1] == "pr"
                && matches!(words[2].as_str(), "comment" | "edit");
            if words.len() >= 2 && !no_commits && !out.contains(&words) {
                out.push(words);
            }
        }
        out
    }
}

/// Parses "800k", "4M", "1.5M", "1200" into a token count.
pub fn parse_tokens(s: &str) -> Option<u64> {
    let t = s.trim().to_ascii_lowercase();
    let (num, mult) = match t.chars().last()? {
        'k' => (&t[..t.len() - 1], 1_000.0),
        'm' => (&t[..t.len() - 1], 1_000_000.0),
        _ => (&t[..], 1.0),
    };
    let n: f64 = num.trim().parse().ok()?;
    (n >= 0.0).then(|| (n * mult).round() as u64)
}

/// Parses "30m", "4h", "2d", "90s", "1w" into seconds.
pub fn parse_duration(s: &str) -> Option<i64> {
    let t = s.trim().to_ascii_lowercase();
    let unit = t.chars().last()?;
    let mult = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86_400,
        'w' => 604_800,
        _ => return None,
    };
    let n: i64 = t[..t.len() - 1].trim().parse().ok()?;
    (n >= 0).then_some(n * mult)
}

/// The `.hallmark.toml` that `hall init` writes.
pub fn render_initial(base: &str, gates: &[String], capture: CaptureMode) -> String {
    let d = Config::default();
    let gates_toml = if gates.is_empty() {
        "gates = []                       # add the commands that must pass before review".into()
    } else {
        let list: Vec<String> = gates
            .iter()
            .map(|g| format!("  {},", toml_str(g)))
            .collect();
        format!("gates = [\n{}\n]", list.join("\n"))
    };
    format!(
        r#"# Hallmark settings for this repository. Every key is optional; the
# values below are the defaults unless `hall init` detected something.
# `hall schema config` describes every key.

base     = {base}
worktree = {worktree}
branch   = {branch}
ship     = {ship}
update   = {update}
review_comment = {review_comment}   # the review goes in a PR comment
{gates_toml}
max_rejects = 3
auto_ready  = true   # done, with nothing left to confirm: straight to review
rules = ["CLAUDE.md", "AGENTS.md"]

[capture]
mode = {capture}   # gate: edits need an item · ask: Claude asks you · off
free = ["docs/", "*.md"]   # questions, docs, designs and plans need no item

[budget]           # runs, 800k per item and 4M a day are capped by default; "none" for no cap
review_model = "opus"
fix_model    = "sonnet"
intake_model = "haiku"
# daily    = "4M"
# per_item = "800k"
"#,
        base = toml_str(base),
        worktree = toml_str(&d.worktree),
        branch = toml_str(&d.branch),
        ship = toml_str(&d.ship),
        update = toml_str(&d.update),
        review_comment = toml_str(&d.review_comment),
        capture = toml_str(capture.as_str()),
    )
}

/// The user config `hall setup` writes when you have none: the defaults,
/// with the keys people set as comments.
pub fn render_initial_user() -> String {
    r#"# Hallmark settings for you, on every repository: how you are notified and
# where your tools live. Every key is optional. `hall config list --user`
# shows them, `hall config set --user <key> <value>` changes one, and
# `hall setup --interactive` asks about them again.

[notify]
desktop = true   # macOS, Linux and Windows notifications
# phone = "ntfy://ntfy.sh/<topic>"   # `hall notify test` creates a private topic
# quiet_hours = "22:00-07:30"         # the phone waits; the desktop still shows

[tools]   # programs that aren't on the daemon's PATH, as absolute paths
# claude = "/path/to/claude"
# gh = "/path/to/gh"
"#
    .into()
}

fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_gives_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn budgets_cap_by_default_and_none_opts_out() {
        // No [budget] at all, or one without caps: 800k an item, 4M a day.
        for text in ["", "[budget]\nfix_model = \"opus\"\n"] {
            let c = Config::parse(text).unwrap();
            assert_eq!(c.budget.per_item.as_deref(), Some("800k"), "{text}");
            assert_eq!(c.budget.daily.as_deref(), Some("4M"), "{text}");
        }
        assert_eq!(Config::default().max_check_failures, 3);
        // "none" (any case) turns a cap off, and survives a round trip.
        let c = Config::parse("[budget]\nper_item = \"none\"\ndaily = \"None\"\n").unwrap();
        assert_eq!(c.budget.per_item, None);
        assert_eq!(c.budget.daily, None);
        let back = toml::to_string(&c).unwrap();
        assert!(back.contains("per_item = \"none\""), "{back}");
        assert_eq!(Config::parse(&back).unwrap().budget, c.budget);
        let c = Config::parse("max_check_failures = 5\n[budget]\nper_item = \"$5\"\n").unwrap();
        assert_eq!(c.budget.per_item.as_deref(), Some("$5"));
        assert_eq!(c.budget.daily.as_deref(), Some("4M"));
        assert_eq!(c.max_check_failures, 5);
    }

    #[test]
    fn typed_sections() {
        let c = Config::parse(
            r#"
[budget]
daily = "4M"
per_item = "800k"
[budget.review]
model = "opus"
effort = "high"
max_usd = 3.0
daily = "2M"
[budget.fix]
timeout = "5m"
[budget.intake]
model = "claude-fable-5-1"
fallback = "claude-opus-5-5"
[notify]
phone = "ntfy://ntfy.sh/hallmark-7f3k2q"
quiet_hours = "22:00-07:30"
[notify.route]
ready = ["desktop", "phone"]
[library]
search = "qmd"
auto_accept = ["map"]
[intake]
split = "off"
"#,
        )
        .unwrap();
        assert_eq!(c.budget.model(Role::Reviewer), "opus");
        assert_eq!(c.budget.model(Role::Fixer), "sonnet");
        assert_eq!(c.budget.max_usd(Role::Reviewer), Some(3.0));
        assert_eq!(c.budget.timeout_secs(Role::Fixer), 300);
        assert_eq!(c.budget.timeout_secs(Role::Reviewer), 1200);
        assert_eq!(c.budget.model(Role::Intake), "claude-fable-5-1");
        assert_eq!(
            c.budget.fallback(Role::Intake).as_deref(),
            Some("claude-opus-5-5")
        );
        assert_eq!(c.budget.fallback(Role::Fixer), None);
        let n = c.notify();
        assert!(n.desktop);
        assert_eq!(n.route.channels("ready"), ["desktop", "phone"]);
        assert_eq!(n.route.channels("question"), ["desktop", "phone"]);
        assert_eq!(n.route.channels("checks"), ["desktop", "phone"]);
        // By default a checks hold takes the escalated route; `ready` is
        // the desktop only.
        let d = Route::default();
        assert_eq!(d.channels("ready"), ["desktop"]);
        assert_eq!(d.channels("checks"), d.channels("escalated"));
        assert_eq!(d.channels("checks"), ["desktop", "phone"]);
        // A pause takes the budget route, whatever it is set to.
        let custom = Route {
            budget: vec!["phone".into()],
            ..Route::default()
        };
        assert_eq!(custom.channels("paused"), ["phone"]);
        assert_eq!(custom.channels("paused"), custom.channels("budget"));
        assert_ne!(custom.channels("paused"), custom.channels("ready"));
        assert_eq!(c.library.search, SearchBackend::Qmd);
        assert_eq!(c.library.briefing_limit, "6k");
        assert_eq!(c.intake.split, SplitMode::Off);
    }

    #[test]
    fn per_run_caps_have_defaults_by_role() {
        let c = Config::parse("").unwrap();
        assert_eq!(c.budget.max_usd(Role::Reviewer), Some(6.0));
        assert_eq!(c.budget.max_usd(Role::Fixer), Some(2.0));
        assert_eq!(c.budget.max_usd(Role::Intake), Some(0.10));
        assert_eq!(c.budget.max_usd(Role::Librarian), Some(0.20));
        // A table without max_usd keeps the default; "none" opts out.
        let c = Config::parse(
            r#"
[budget.review]
max_usd = "none"
[budget.fix]
max_usd = 5
[budget.intake]
effort = "low"
[budget.librarian]
max_usd = 0.5
"#,
        )
        .unwrap();
        assert_eq!(c.budget.max_usd(Role::Reviewer), None);
        assert_eq!(c.budget.max_usd(Role::Fixer), Some(5.0));
        assert_eq!(c.budget.max_usd(Role::Intake), Some(0.10));
        assert_eq!(c.budget.max_usd(Role::Librarian), Some(0.5));
        // "none" round-trips as written.
        let back = toml::to_string(&c.budget).unwrap();
        assert!(back.contains(r#"max_usd = "none""#), "{back}");
        assert!(Config::parse("[budget.review]\nmax_usd = \"lots\"\n").is_err());
    }

    #[test]
    fn overrides_merge_with_defaults() {
        let c = Config::parse(
            r#"
worktree = "dev wt new {branch}"
ship     = "dev submit --pr"
gates    = ["dev test", "dev run build"]
[capture]
mode = "ask"
"#,
        )
        .unwrap();
        assert_eq!(c.worktree, "dev wt new {branch}");
        assert_eq!(c.gates.len(), 2);
        assert_eq!(c.capture.mode, CaptureMode::Ask);
        assert_eq!(c.max_rejects, 3);
        assert_eq!(c.base, "main");
    }

    #[test]
    fn gates_have_their_own_timeout() {
        // Not the reviewer's: `[budget.review] timeout` doesn't reach them.
        let c = Config::parse(
            r#"
gates = ["cargo clippy", { cmd = "cargo test", timeout = "40m" }, { cmd = "make" }]
[budget.review]
timeout = "5m"
"#,
        )
        .unwrap();
        let cmds: Vec<&str> = c.gates.iter().map(Gate::cmd).collect();
        assert_eq!(cmds, ["cargo clippy", "cargo test", "make"]);
        let secs: Vec<u64> = c.gates.iter().map(|g| c.gate_timeout_secs(g)).collect();
        assert_eq!(secs, [1200, 2400, 1200]);
        // Leases held while the gates run last as long as the slowest one.
        assert_eq!(c.longest_gate_secs(), 2400);
        assert_eq!(Config::default().longest_gate_secs(), 0);
        let c = Config::parse("gates_timeout = \"90s\"\ngates = [\"make\"]\n").unwrap();
        assert_eq!(c.gate_timeout_secs(&c.gates[0]), 90);
        // A table needs its command, and nothing it doesn't know.
        assert!(Config::parse("gates = [{ timeout = \"1m\" }]").is_err());
        assert!(Config::parse("gates = [{ cmd = \"make\", timout = \"1m\" }]").is_err());
    }

    #[test]
    fn initial_file_round_trips() {
        let text = render_initial(
            "trunk",
            &["cargo test".into(), "make \"lint\"".into()],
            CaptureMode::Gate,
        );
        let c = Config::parse(&text).unwrap();
        assert_eq!(c.base, "trunk");
        assert_eq!(c.gates, vec!["cargo test".into(), "make \"lint\"".into()]);
        assert_eq!(c.worktree, Config::default().worktree);
        assert_eq!(c.review_comment, Config::default().review_comment);
    }

    #[test]
    fn the_update_init_used_to_write_reads_as_todays() {
        // It posted the PR body as a comment; the review comment does now.
        let c = Config::parse(&format!("update = {}\n", toml_str(OLD_UPDATE))).unwrap();
        assert_eq!(c.update, Config::default().update);
        assert!(!c.update.contains("gh pr comment"), "{}", c.update);
        assert_eq!(
            c.review_comment,
            "gh pr comment {pr} --body-file {review_file}"
        );
        // A changed one is kept as written.
        let own = format!("{OLD_UPDATE} && notify-team");
        let c = Config::parse(&format!("update = {}\n", toml_str(&own))).unwrap();
        assert_eq!(c.update, own);
    }

    #[test]
    fn ship_prefixes() {
        let d = Config::default();
        // `update` is gated like `ship` where it moves commits; commenting on
        // or editing a PR pushes nothing.
        assert_eq!(
            d.ship_prefixes(),
            vec![vec!["git", "push"], vec!["gh", "pr", "create"]]
        );
        let c = Config {
            ship: "dev submit --pr".into(),
            update: "dev submit --update".into(),
            ..Config::default()
        };
        assert_eq!(c.ship_prefixes(), vec![vec!["dev", "submit"]]);
    }

    #[test]
    fn units() {
        assert_eq!(parse_tokens("800k"), Some(800_000));
        assert_eq!(parse_tokens("1.5M"), Some(1_500_000));
        assert_eq!(parse_tokens("12"), Some(12));
        assert_eq!(parse_tokens("x"), None);
        assert_eq!(parse_duration("30m"), Some(1800));
        assert_eq!(parse_duration("4h"), Some(14_400));
        assert_eq!(parse_duration("7d"), Some(604_800));
        assert_eq!(parse_duration("4"), None);
    }

    #[test]
    fn claude_args_cant_loosen_a_run() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            loosening_arg(&args(&["--model", "opus", "--verbose"])),
            None
        );
        assert_eq!(loosening_arg(&args(&[])), None);
        for (given, found) in [
            (
                &["--dangerously-skip-permissions"][..],
                "--dangerously-skip-permissions",
            ),
            (
                &["--permission-mode", "bypassPermissions"][..],
                "--permission-mode",
            ),
            (
                &["--permission-mode=bypassPermissions"][..],
                "--permission-mode=bypassPermissions",
            ),
            (&["--model", "x", "--settings", "{}"][..], "--settings"),
            (&["--add-dir", "/"][..], "--add-dir"),
            (&["--allowed-tools", "Bash"][..], "--allowed-tools"),
            (&["--mcp-config", "m.json"][..], "--mcp-config"),
        ] {
            assert_eq!(loosening_arg(&args(given)), Some(found), "{given:?}");
        }
        for flag in [
            "--dangerously-skip-permissions",
            "--allow-dangerously-skip-permissions",
            "--permission-mode",
            "--permission-prompt-tool",
            "--settings",
            "--setting-sources",
            "--add-dir",
            "--allowedTools",
            "--allowed-tools",
            "--tools",
            "--mcp-config",
            "--plugin-dir",
        ] {
            let eq = format!("{flag}=x");
            for given in [vec![flag.to_string()], vec![eq.clone()]] {
                assert_eq!(loosening_arg(&given), Some(given[0].as_str()), "{flag}");
                let toml = format!("claude_args = [{:?}]\n", given[0]);
                let e = Config::parse(&toml).unwrap_err();
                assert_eq!(e.code, ErrorCode::Usage, "{flag}");
            }
        }
        // A flag that only shares a prefix is someone else's.
        assert_eq!(loosening_arg(&args(&["--settingsfile"])), None);

        let e =
            Config::parse("claude_args = [\"--verbose\", \"--dangerously-skip-permissions\"]\n")
                .unwrap_err();
        assert_eq!(e.code, ErrorCode::Usage);
        assert!(
            e.message.contains("--dangerously-skip-permissions"),
            "{}",
            e.message
        );
        assert!(Config::parse("claude_args = [\"--verbose\"]\n").is_ok());
    }

    #[test]
    fn the_sandbox_is_on_and_keeps_credentials_out_by_default() {
        let s = Config::default().sandbox;
        assert_eq!(s.mode, SandboxMode::Auto);
        assert!(
            s.domains.is_empty(),
            "no network unless the repo names hosts"
        );
        assert!(s.allow_write.is_empty());
        for p in ["~/.ssh", "~/.aws", "~/.config/gh", "~/.claude.json"] {
            assert!(s.deny_read.iter().any(|d| d == p), "{p}");
        }
        for v in ["SSH_AUTH_SOCK", "GH_TOKEN", "GITHUB_TOKEN"] {
            assert!(s.remove_env.iter().any(|e| e == v), "{v}");
        }

        let c = Config::parse(
            r#"
[sandbox]
mode = "required"
allow_write = ["~/.cargo"]
domains = ["crates.io"]
unix_sockets = ["/private/var/run/nix-daemon.socket"]
local_binding = false
"#,
        )
        .unwrap();
        assert_eq!(c.sandbox.mode, SandboxMode::Required);
        assert_eq!(c.sandbox.allow_write, ["~/.cargo"]);
        assert_eq!(c.sandbox.domains, ["crates.io"]);
        assert!(!c.sandbox.local_binding);
        // Keys left out keep their defaults.
        assert_eq!(c.sandbox.deny_read, s.deny_read);
        assert!(Config::parse("[sandbox]\nmode = \"sometimes\"\n").is_err());
    }
}
