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
    /// Ships a hallmarked commit. Placeholders: {branch} {id} {base}.
    pub ship: String,
    /// The repo's checks, run in order at the frozen commit.
    pub gates: Vec<String>,
    /// The command that runs the reviewer.
    pub reviewer: String,
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
    /// Notification channels (Stage 3). Kept as written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify: Option<serde_json::Value>,
    /// Library settings (Stage 2). Kept as written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub library: Option<serde_json::Value>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            base: "main".into(),
            worktree: "git worktree add ../{repo}-{id} -b {branch}".into(),
            branch: "hm/{id}-{slug}".into(),
            ship: "git push -u origin {branch} && gh pr create --fill".into(),
            gates: Vec::new(),
            reviewer: "claude -p".into(),
            max_rejects: 3,
            stuck_after: "30m".into(),
            lenses: Lenses::All("all".into()),
            rules: vec!["CLAUDE.md".into(), "AGENTS.md".into()],
            capture: Capture::default(),
            budget: Budget::default(),
            notify: None,
            library: None,
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
        }
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
