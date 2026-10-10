//! `.hallmark.toml`: optional, and every key has a default.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::Result;

pub const FILE_NAME: &str = ".hallmark.toml";

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
    /// {title} {body_file} (the PR body the shipper writes) {issue}.
    pub ship: String,
    /// The repo's checks, run in order at the frozen commit.
    pub gates: Vec<String>,
    /// The command that runs the reviewer.
    pub reviewer: String,
    /// The fix engine: `claude -p` (the fixer agent), or another command run
    /// in the item's worktree, such as Ralph Orchestrator. Placeholders:
    /// {id} {worktree} {findings_file} (JSON from `hall findings`).
    pub fixer: String,
    /// Extra arguments for every headless `claude -p` run.
    pub claude_args: Vec<String>,
    /// Rejected reviews before the item is escalated to you.
    pub max_rejects: u32,
    /// The watchman flags items with no event for this long.
    pub stuck_after: String,
    /// "all", or a list of lens names.
    pub lenses: Lenses,
    /// Files the repo-rules lens reads.
    pub rules: Vec<String>,
    pub capture: Capture,
    pub budget: Budget,
    /// How the Front Desk plans a request.
    pub intake: Intake,
    /// Notification channels. Usually set in the user config
    /// (`<user data dir>/hallmark/config.toml`), which this overrides.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify: Option<Notify>,
    pub library: Library,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            base: "main".into(),
            worktree: "git worktree add ../{repo}-{id} -b {branch}".into(),
            branch: "hm/{id}-{slug}".into(),
            ship: "git push -u origin {branch} && gh pr create --base {base} --head {branch} --title {title} --body-file {body_file}".into(),
            gates: Vec::new(),
            reviewer: "claude -p".into(),
            fixer: "claude -p".into(),
            claude_args: Vec::new(),
            max_rejects: 3,
            stuck_after: "30m".into(),
            lenses: Lenses::All("all".into()),
            rules: vec!["CLAUDE.md".into(), "AGENTS.md".into()],
            capture: Capture::default(),
            budget: Budget::default(),
            intake: Intake::default(),
            notify: None,
            library: Library::default(),
        }
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

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Capture {
    pub mode: CaptureMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Budget {
    /// Daily cap for the whole review loop, in tokens ("4M"). None: no cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daily: Option<String>,
    /// Cap per item, in tokens ("800k"). None: no cap.
    #[serde(skip_serializing_if = "Option::is_none")]
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

    /// The per-run cap passed as `--max-budget-usd`.
    pub fn max_usd(&self, r: Role) -> Option<f64> {
        self.role(r).and_then(|b| b.max_usd)
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
            daily: None,
            per_item: None,
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
            "ready" => &self.ready,
            "budget" | "run_failed" | "ship_failed" | "held" => &self.budget,
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
    /// Unused learnings are proposed for retirement after this long.
    pub retire_after: String,
    /// Where learnings live, relative to the repo root.
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
    /// Per run, passed as `--max-budget-usd`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_usd: Option<f64>,
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

impl Config {
    pub fn parse(text: &str) -> Result<Config> {
        Ok(toml::from_str(text)?)
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

    /// The first plain command words of the ship command, used by the push
    /// gate (`dev submit --pr` → `dev submit`).
    pub fn ship_prefixes(&self) -> Vec<Vec<String>> {
        crate::gate::split_segments(&self.ship)
            .iter()
            .filter_map(|seg| {
                let words: Vec<String> = crate::gate::words(seg)
                    .into_iter()
                    .take_while(|w| !w.starts_with('-') && !w.contains('{'))
                    .collect();
                (words.len() >= 2).then_some(words)
            })
            .collect()
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
{gates_toml}
max_rejects = 3
rules = ["CLAUDE.md", "AGENTS.md"]

[capture]
mode = {capture}   # gate: edits need an item · ask: Claude asks you · off

[budget]           # no caps unless you set them
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
        capture = toml_str(capture.as_str()),
    )
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
        assert_eq!(c.library.search, SearchBackend::Qmd);
        assert_eq!(c.library.briefing_limit, "6k");
        assert_eq!(c.intake.split, SplitMode::Off);
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
    fn initial_file_round_trips() {
        let text = render_initial(
            "trunk",
            &["cargo test".into(), "make \"lint\"".into()],
            CaptureMode::Gate,
        );
        let c = Config::parse(&text).unwrap();
        assert_eq!(c.base, "trunk");
        assert_eq!(
            c.gates,
            vec!["cargo test".to_string(), "make \"lint\"".to_string()]
        );
        assert_eq!(c.worktree, Config::default().worktree);
    }

    #[test]
    fn ship_prefixes() {
        let d = Config::default();
        assert_eq!(
            d.ship_prefixes(),
            vec![vec!["git", "push"], vec!["gh", "pr", "create"]]
        );
        let c = Config {
            ship: "dev submit --pr".into(),
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
}
