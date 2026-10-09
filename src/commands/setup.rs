//! `hall setup claude`: install the plugin for Claude Code at user level,
//! through a local marketplace (docs/decisions/0009).

use std::process::Command;

use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::{SetupArgs, SetupTarget};
use crate::error::{ErrorCode, HallError, Result};
use crate::output::{Render, Response};
use crate::paths;
use crate::plugin::{self, MARKETPLACE, PLUGIN};
use crate::repo::user_dir;

#[derive(Debug, Serialize, JsonSchema)]
pub struct SetupOut {
    pub target: String,
    pub marketplace_dir: String,
    /// The `hall` binary the hooks call.
    pub hall: String,
    pub dry_run: bool,
    pub steps: Vec<Step>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Step {
    pub command: String,
    /// done, skipped, failed, planned
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Render for SetupOut {
    fn render(&self) -> String {
        let mut s = format!(
            "{} the Hallmark plugin for Claude Code (hooks call {}).\n\n",
            if self.dry_run {
                "Would install"
            } else {
                "Installed"
            },
            self.hall
        );
        for st in &self.steps {
            s.push_str(&format!("  [{}] {}\n", st.status, st.command));
            if let Some(d) = &st.detail {
                s.push_str(&format!("        {d}\n"));
            }
        }
        s
    }
}

fn claude_bin() -> String {
    std::env::var("HALL_CLAUDE").unwrap_or_else(|_| "claude".into())
}

pub fn setup(a: SetupArgs) -> Result<Response> {
    let SetupTarget::Claude = a.target;
    let exe = std::env::current_exe()?;
    let exe = paths::normalize(&exe);
    let dir = user_dir().join("claude-plugin");
    let claude = claude_bin();
    let dir_s = dir.to_string_lossy().to_string();
    let plan: Vec<Vec<String>> = vec![
        vec!["plugin", "marketplace", "add", &dir_s, "--scope", "user"],
        vec![
            "plugin",
            "install",
            &format!("{PLUGIN}@{MARKETPLACE}"),
            "--scope",
            "user",
        ],
    ]
    .into_iter()
    .map(|v| v.into_iter().map(String::from).collect())
    .collect();

    let mut steps = vec![Step {
        command: format!("write the plugin to {}", paths::display(&dir)),
        status: if a.dry_run { "planned" } else { "done" }.into(),
        detail: None,
    }];
    if !a.dry_run {
        plugin::extract(&dir, &exe)?;
    }
    for args in &plan {
        let shown = format!("{claude} {}", args.join(" "));
        if a.dry_run {
            steps.push(Step {
                command: shown,
                status: "planned".into(),
                detail: None,
            });
            continue;
        }
        let out = Command::new(&claude).args(args).output().map_err(|e| {
            HallError::new(
                ErrorCode::CommandFailed,
                format!("could not run {claude}: {e}"),
            )
            .next([
                "install Claude Code: https://claude.com/claude-code",
                "hall doctor",
            ])
        })?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let already = text.to_ascii_lowercase().contains("already");
        if out.status.success() || already {
            steps.push(Step {
                command: shown,
                status: if already { "skipped" } else { "done" }.into(),
                detail: already.then(|| "already present".into()),
            });
        } else {
            return Err(HallError::new(
                ErrorCode::CommandFailed,
                format!("`{shown}` failed: {}", text.trim()),
            )
            .next(["hall setup claude --dry-run", "hall doctor"]));
        }
    }
    // An existing install keeps the old copy until the marketplace refreshes.
    if !a.dry_run {
        for args in [
            vec![
                "plugin".to_string(),
                "marketplace".into(),
                "update".into(),
                MARKETPLACE.into(),
            ],
            vec![
                "plugin".to_string(),
                "update".into(),
                format!("{PLUGIN}@{MARKETPLACE}"),
                "--scope".into(),
                "user".into(),
            ],
        ] {
            let shown = format!("{claude} {}", args.join(" "));
            let ok = Command::new(&claude)
                .args(&args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            steps.push(Step {
                command: shown,
                status: if ok { "done" } else { "skipped" }.into(),
                detail: None,
            });
        }
    }
    let out = SetupOut {
        target: "claude".into(),
        marketplace_dir: paths::display(&dir),
        hall: paths::display(&exe),
        dry_run: a.dry_run,
        steps,
    };
    let next = vec![
        "hall doctor".into(),
        "hall init".into(),
        "restart Claude Code to load the hooks".into(),
    ];
    Ok(Response::new(out, next))
}
