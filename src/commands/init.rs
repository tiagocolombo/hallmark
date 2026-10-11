//! `hall init`: set up a repository. Safe to repeat; it never overwrites.

use std::fs;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cli::InitArgs;
use crate::config::{self, CaptureMode, Config};
use crate::error::Result;
use crate::index::Index;
use crate::output::{Render, Response};
use crate::paths;
use crate::repo::{Repo, user_dir};
use crate::{detect, git};

pub const MARK_START: &str = "<!-- hallmark:start -->";
pub const MARK_END: &str = "<!-- hallmark:end -->";

pub const CLAUDE_SECTION: &str = "## Hallmark
Changes to this repo's code are tracked with Hallmark.
Only a clear, concise requirement to build or change
something in the app becomes an item: start it with
`hall new`, or `hall adopt` for an existing branch.
Questions, documents, architecture, requirements and
plans are not items: answer or write them directly.
Never push: work reaches GitHub only through review.
`hall help --json` lists every command.";

const REVIEW_STUB: &str = "# Review notes for Hallmark

The principal engineer reads this file from the base branch before every
review. Add what a reviewer new to this repo should know: sensitive paths,
invariants, conventions that aren't written down elsewhere.

## Sensitive paths

<!-- e.g. apps/api/src/billing/** gets the deepest review -->

## Invariants

<!-- e.g. every query goes through the tenant-scoped client -->
";

#[derive(Debug, Serialize, JsonSchema)]
pub struct InitOut {
    pub repo: String,
    pub root: String,
    /// Where the journal lives (never committed).
    pub journal: String,
    pub base: String,
    pub gates: Vec<String>,
    pub capture: CaptureMode,
    /// Files written by this run.
    pub created: Vec<String>,
    /// Files that already existed and were left alone.
    pub unchanged: Vec<String>,
}

impl Render for InitOut {
    fn render(&self) -> String {
        let mut s = format!("Hallmark is set up in {}.\n\n", self.repo);
        s.push_str(&format!("  base     {}\n", self.base));
        s.push_str(&format!("  capture  {}\n", self.capture.as_str()));
        if self.gates.is_empty() {
            s.push_str("  gates    none found; add them to .hallmark.toml\n");
        } else {
            s.push_str(&format!("  gates    {}\n", self.gates.join(" · ")));
        }
        s.push_str(&format!("  journal  {}\n", self.journal));
        for c in &self.created {
            s.push_str(&format!("  wrote    {c}\n"));
        }
        for u in &self.unchanged {
            s.push_str(&format!("  kept     {u}\n"));
        }
        s
    }
}

pub fn init(cwd: &Path, a: InitArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    let root = repo.toplevel.clone();
    let mut created = Vec::new();
    let mut unchanged = Vec::new();

    let config_path = root.join(config::FILE_NAME);
    let (base, gates, capture) = if config_path.is_file() {
        unchanged.push(config::FILE_NAME.to_string());
        let c = Config::parse(&fs::read_to_string(&config_path)?)?;
        (c.base, c.gates, c.capture.mode)
    } else {
        let base = a
            .base
            .clone()
            .unwrap_or_else(|| git::default_branch(&repo.main_root));
        let gates = detect::gates(&root);
        let capture = a.capture.unwrap_or_default();
        fs::write(&config_path, config::render_initial(&base, &gates, capture))?;
        created.push(config::FILE_NAME.to_string());
        (base, gates, capture)
    };

    let review = root.join(".hallmark").join("review.md");
    if review.is_file() {
        unchanged.push(".hallmark/review.md".into());
    } else {
        fs::create_dir_all(review.parent().expect("has parent"))?;
        fs::write(&review, REVIEW_STUB)?;
        created.push(".hallmark/review.md".into());
    }

    if !a.no_claude_md {
        let target = if !root.join("CLAUDE.md").is_file() && root.join("AGENTS.md").is_file() {
            "AGENTS.md"
        } else {
            "CLAUDE.md"
        };
        let path = root.join(target);
        let before = fs::read_to_string(&path).unwrap_or_default();
        let after = upsert_section(&before);
        if after == before {
            unchanged.push(target.into());
        } else {
            fs::write(&path, after)?;
            created.push(target.into());
        }
    }

    repo.hall.create()?;
    Index::synced(&repo.hall)?;
    register(&repo.main_root, &repo.name)?;

    let out = InitOut {
        repo: repo.name.clone(),
        root: paths::display(&root),
        journal: paths::display(&repo.hall.root),
        base,
        gates,
        capture,
        created: created.clone(),
        unchanged,
    };
    let mut next = Vec::new();
    if !created.is_empty() {
        next.push(format!(
            "git add {} && git commit -m \"Adopt Hallmark\"",
            created.join(" ")
        ));
    }
    next.extend(["hall doctor".to_string(), "hall new \"<request>\"".into()]);
    Ok(Response::new(out, next))
}

/// Adds or refreshes the Hallmark section between its markers.
pub fn upsert_section(text: &str) -> String {
    let block = format!("{MARK_START}\n{CLAUDE_SECTION}\n{MARK_END}");
    if let (Some(s), Some(e)) = (text.find(MARK_START), text.find(MARK_END))
        && s < e
    {
        return format!("{}{block}{}", &text[..s], &text[e + MARK_END.len()..]);
    }
    if text.trim().is_empty() {
        return format!("{block}\n");
    }
    let sep = if text.ends_with("\n\n") {
        ""
    } else if text.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    format!("{text}{sep}{block}\n")
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Registry {
    pub repos: Vec<RegisteredRepo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisteredRepo {
    pub name: String,
    pub path: String,
}

/// Records the repo in the user registry, so the terminal app can list
/// every repository that uses Hallmark.
fn register(root: &Path, name: &str) -> Result<()> {
    let dir = user_dir();
    fs::create_dir_all(&dir)?;
    let path = dir.join("repos.json");
    let mut reg: Registry = fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let p = paths::display(root);
    if !reg.repos.iter().any(|r| r.path == p) {
        reg.repos.push(RegisteredRepo {
            name: name.into(),
            path: p,
        });
        fs::write(&path, serde_json::to_string_pretty(&reg)?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_is_added_once_and_refreshed() {
        let once = upsert_section("# Repo\n\nRules.\n");
        assert!(once.starts_with("# Repo\n\nRules.\n\n<!-- hallmark:start -->"));
        assert_eq!(upsert_section(&once), once);
        let stale = once.replace("Never push", "Push freely");
        assert_eq!(upsert_section(&stale), once);
        assert_eq!(
            upsert_section(""),
            format!("{MARK_START}\n{CLAUDE_SECTION}\n{MARK_END}\n")
        );
    }
}
