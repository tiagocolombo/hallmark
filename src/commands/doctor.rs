//! `hall doctor`: each failed check names the command that fixes it.

use std::path::Path;
use std::process::Command;

use schemars::JsonSchema;
use serde::Serialize;

use crate::error::Result;
use crate::output::{Render, Response};
use crate::repo::{Repo, user_dir};
use crate::{gh, git, paths};

#[derive(Debug, Serialize, JsonSchema)]
pub struct DoctorOut {
    /// True when every check passed.
    pub healthy: bool,
    pub checks: Vec<Check>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    /// The command that fixes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Render for DoctorOut {
    fn render(&self) -> String {
        let mut s = String::new();
        for c in &self.checks {
            let mark = if c.ok { "✓" } else { "✗" };
            s.push_str(&format!("{mark} {}: {}", c.name, c.detail));
            if let Some(f) = &c.fix {
                s.push_str(&format!("  →  {f}"));
            }
            s.push('\n');
        }
        s
    }
}

fn check(name: &str, ok: bool, detail: impl Into<String>, fix: Option<&str>) -> Check {
    Check {
        name: name.into(),
        ok,
        detail: detail.into(),
        fix: if ok { None } else { fix.map(String::from) },
    }
}

fn claude_version(claude: &str) -> Option<String> {
    let out = Command::new(claude).arg("--version").output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The repo's skills (ADR 0037) as the reviewer and the fixer read them:
/// from the base branch. None when the repo has none anywhere.
fn skills_check(repo: &Repo) -> Option<Check> {
    use crate::skills::{DIR, KNOWN, list};
    let base = crate::assayer::base::Base::resolve(repo, &repo.config.base).ok()?;
    let listing = list(&repo.main_root, &base.sha);
    if listing.is_empty() {
        return repo.main_root.join(DIR).is_dir().then(|| {
            check(
                "skills",
                true,
                format!("{DIR} isn't on {} yet; it applies once merged", base.name),
                None,
            )
        });
    }
    let found: Vec<String> = listing
        .found
        .iter()
        .map(|(n, m)| format!("{n} ({})", m.as_str()))
        .collect();
    let mut detail = if found.is_empty() {
        format!("none in use on {}", base.name)
    } else {
        format!("{} on {}", found.join(", "), base.name)
    };
    if listing.problems.is_empty() {
        return Some(check("skills", true, detail, None));
    }
    detail.push_str("; ");
    detail.push_str(&listing.problems.join("; "));
    let known: Vec<&str> = KNOWN.iter().map(|(k, _)| *k).collect();
    let fix = format!(
        "keep only {DIR}/<name>/SKILL.md, with <name> one of {}",
        known.join(", ")
    );
    Some(check("skills", false, detail, Some(&fix)))
}

/// The extensions turned on (ADR 0038), with their credit, and for the
/// reviewer, whether the base branch has the setting yet. None when none is.
fn extensions_check(repo: &Repo) -> Option<Check> {
    use crate::extensions::{CATALOG, Role};
    let base = crate::assayer::base::Base::resolve(repo, &repo.config.base).ok();
    let reviews = |id: &str| {
        base.as_ref()
            .map_or(&repo.config, |b| &b.config)
            .extensions
            .roles(id)
            .contains(&Role::Reviewer)
    };
    let mut parts = Vec::new();
    for e in CATALOG {
        let mut roles: Vec<String> = repo
            .config
            .extensions
            .roles(e.id)
            .iter()
            .filter(|r| **r != Role::Reviewer)
            .map(|r| r.as_str().to_string())
            .collect();
        let here = repo.config.extensions.roles(e.id).contains(&Role::Reviewer);
        let name = base.as_ref().map_or("the base", |b| b.name.as_str());
        match (here, reviews(e.id)) {
            (true, true) => roles.push("reviewer".into()),
            (true, false) => roles.push(format!("reviewer once .hallmark.toml is on {name}")),
            (false, true) => roles.push(format!("reviewer until .hallmark.toml is on {name}")),
            (false, false) => {}
        }
        if !roles.is_empty() {
            parts.push(format!("{} ({}): {}", e.id, e.credit(), roles.join(", ")));
        }
    }
    (!parts.is_empty()).then(|| check("extensions", true, parts.join("; "), None))
}

pub fn doctor(cwd: &Path) -> Result<Response> {
    let mut checks = Vec::new();

    // Being behind isn't unhealthy: the detail says what's newer.
    let mut hall = env!("HALL_VERSION").to_string();
    if let Some(note) = crate::commands::update::notice() {
        hall = format!("{hall}; {note}");
    }
    checks.push(check("hall", true, hall, None));

    match git::version() {
        Some(v) => checks.push(check("git", true, v, None)),
        None => checks.push(check("git", false, "not found", Some("install git"))),
    }
    match gh::auth_status() {
        Some((p, true)) => checks.push(check(
            "gh",
            true,
            format!("logged in ({})", paths::display(&p)),
            None,
        )),
        Some((p, false)) => checks.push(check(
            "gh",
            false,
            format!("not logged in ({})", paths::display(&p)),
            Some("gh auth login"),
        )),
        None => checks.push(check(
            "gh",
            false,
            "not found; needed for --issue and the default ship command",
            Some("install gh, or `hall config set --user tools.gh /path/to/gh`"),
        )),
    }
    let claude = crate::config::Tool::Claude.command();
    let claude_ok = match claude_version(&claude) {
        Some(v) => {
            checks.push(check("claude", true, v, None));
            true
        }
        None => {
            checks.push(check(
                "claude",
                false,
                "Claude Code not found",
                Some(
                    "install Claude Code, or `hall config set --user tools.claude /path/to/claude`",
                ),
            ));
            false
        }
    };

    let plugin_dir = user_dir()
        .join("claude-plugin")
        .join("plugins")
        .join("hallmark");
    let hooks = plugin_dir.join("hooks").join("hooks.json");
    let installed = claude_ok
        && Command::new(&claude)
            .args(["plugin", "list", "--json"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("hallmark@hallmark"))
            .unwrap_or(false);
    checks.push(check(
        "plugin",
        installed,
        if installed {
            "hallmark (user)".to_string()
        } else {
            "not installed".to_string()
        },
        Some("hall setup claude"),
    ));
    let exe = std::env::current_exe().map(|p| paths::normalize(&p)).ok();
    let hooks_ok = match (std::fs::read_to_string(&hooks), &exe) {
        (Ok(h), Some(e)) => h == crate::plugin::hooks_json(e),
        _ => false,
    };
    checks.push(check(
        "hooks",
        hooks_ok,
        if hooks_ok {
            "SessionStart UserPromptSubmit PreToolUse PostToolUse Stop Notification → this hall"
                .to_string()
        } else if hooks.is_file() {
            "installed, but they call a different hall binary".to_string()
        } else {
            "not installed".to_string()
        },
        Some("hall setup claude"),
    ));

    match Repo::discover(cwd) {
        Err(_) => checks.push(check(
            "repo",
            true,
            "not in a git repository; repo checks skipped",
            None,
        )),
        Ok(repo) => {
            if repo.initialized() {
                let gates = if repo.config.gates.is_empty() {
                    "no gates configured".to_string()
                } else {
                    format!("gates {}", repo.config.gate_commands().join(", "))
                };
                checks.push(check("repo", true, format!("{}: {gates}", repo.name), None));
                let probe = repo.hall.root.join(".doctor");
                let writable =
                    std::fs::write(&probe, b"ok").is_ok() && std::fs::remove_file(&probe).is_ok();
                checks.push(check(
                    "journal",
                    writable,
                    paths::display(&repo.hall.root),
                    Some("check the permissions of the .git directory"),
                ));
                match repo.index() {
                    Ok(_) => checks.push(check("index", true, "in sync with the journal", None)),
                    Err(e) => checks.push(check("index", false, e.message, Some("hall reindex"))),
                }
                if repo.config.gates.is_empty() {
                    checks.push(check(
                        "gates",
                        false,
                        "no checks run before review",
                        Some("add gates to .hallmark.toml"),
                    ));
                }
                checks.extend(skills_check(&repo));
                checks.extend(extensions_check(&repo));
            } else {
                checks.push(check(
                    "repo",
                    false,
                    format!("{} doesn't use Hallmark", repo.name),
                    Some("hall init"),
                ));
            }
        }
    }
    // You type `hall`; hooks and the daemon use the absolute path.
    let exe = crate::paths::normalize(&std::env::current_exe()?);
    let on_path = crate::gh::which("hall").map(|p| crate::paths::normalize(&p));
    checks.push(check(
        "path",
        on_path.as_deref() == Some(exe.as_path()),
        match &on_path {
            Some(p) if p == &exe => "hall is on PATH".to_string(),
            Some(p) => format!(
                "`hall` on PATH is {}, not {}",
                paths::display(p),
                paths::display(&exe)
            ),
            None => format!(
                "{} isn't on PATH",
                paths::display(exe.parent().unwrap_or(&exe))
            ),
        },
        Some("hall setup"),
    ));
    let running = crate::daemon::state::instance_running();
    let paused = crate::daemon::state::load().and_then(|s| s.paused);
    checks.push(check(
        "daemon",
        running,
        match (&paused, running) {
            (Some(p), true) => format!("running, paused: {}", p.reason),
            (None, true) => "running".to_string(),
            (_, false) => "not running; reviews wait in the queue".to_string(),
        },
        Some("hall daemon start --at-login"),
    ));

    let healthy = checks.iter().all(|c| c.ok);
    let mut next: Vec<String> = checks.iter().filter_map(|c| c.fix.clone()).collect();
    if crate::commands::update::available().is_some() {
        next.push("hall update".into());
    }
    Ok(Response::new(DoctorOut { healthy, checks }, next))
}
