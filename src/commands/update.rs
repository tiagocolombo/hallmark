//! `hall update`: install the latest `hall` from the source repository's
//! main branch with `cargo install`, unless the running one was built from
//! that commit. Then refresh what the old binary left behind: the Claude
//! Code plugin, whose hooks name this hall, and a running daemon.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use super::setup::{Step, step};
use crate::cli::UpdateArgs;
use crate::error::{ErrorCode, HallError, Result};
use crate::event::short;
use crate::output::{Render, Response};
use crate::paths;

/// Where `hall update` installs from, unless `--source` says otherwise.
pub const SOURCE: &str = concat!(env!("CARGO_PKG_REPOSITORY"), ".git");

/// The commit this binary was built from, when its build knew it.
pub fn commit() -> Option<&'static str> {
    let c = env!("HALL_COMMIT");
    (!c.is_empty()).then_some(c)
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct UpdateOut {
    /// up_to_date, available (with --check) or updated.
    pub status: String,
    /// The commit the running hall was built from; null when its build
    /// didn't know (built without git), which never counts as up to date.
    pub installed: Option<String>,
    /// The latest commit on `branch`.
    pub latest: String,
    pub source: String,
    pub branch: String,
    /// The hall binary that is (or would be) replaced.
    pub hall: String,
    /// What the update did: the install, the plugin, the daemon.
    pub steps: Vec<Step>,
}

impl Render for UpdateOut {
    fn render(&self) -> String {
        let installed = self.installed.as_deref().map_or("an unknown commit", short);
        let latest = short(&self.latest);
        let from = format!("{} of {}", self.branch, self.source);
        let mut s = match self.status.as_str() {
            "up_to_date" => {
                format!("hall is already up to date: {latest} is the latest on {from}.\n")
            }
            "available" => format!("An update is available: {installed} → {latest} ({from}).\n"),
            _ => format!("Updated {}: {installed} → {latest} ({from}).\n", self.hall),
        };
        if !self.steps.is_empty() {
            s.push('\n');
        }
        for st in &self.steps {
            s.push_str(&format!("  [{}] {}\n", st.status, st.command));
            if let Some(d) = &st.detail {
                s.push_str(&format!("        {d}\n"));
            }
        }
        s
    }
}

pub fn update(cwd: &Path, a: UpdateArgs) -> Result<Response> {
    let exe = paths::normalize(&std::env::current_exe()?);
    let latest = latest_commit(cwd, &a.source, &a.branch)?;
    let mut out = UpdateOut {
        status: "up_to_date".into(),
        installed: commit().map(String::from),
        latest,
        source: a.source.clone(),
        branch: a.branch.clone(),
        hall: paths::display(&exe),
        steps: Vec::new(),
    };
    if out.installed.as_deref() == Some(out.latest.as_str()) {
        return Ok(Response::new(out, vec!["hall doctor".into()]));
    }
    if a.check {
        out.status = "available".into();
        return Ok(Response::new(out, vec![again(&a)]));
    }

    let root = install_root(&exe)?;
    let cargo = std::env::var("HALL_CARGO").unwrap_or_else(|_| "cargo".into());
    let aside = set_aside(&exe);
    let installed = cargo_install(&cargo, cwd, &a, &out.latest, &root).and_then(|()| {
        if exe.exists() {
            return Ok(());
        }
        Err(HallError::new(
            ErrorCode::CommandFailed,
            format!(
                "cargo install succeeded but didn't write {}; does {} build a `hall` binary?",
                paths::display(&exe),
                a.source
            ),
        )
        .next(["hall update"]))
    });
    if let Err(e) = installed {
        if let Some(old) = &aside
            && !exe.exists()
        {
            let _ = std::fs::rename(old, &exe);
        }
        return Err(e);
    }
    if let Some(old) = &aside {
        // Windows keeps a running program's file; the next update removes it.
        let _ = std::fs::remove_file(old);
    }
    out.status = "updated".into();
    out.steps.push(step(
        format!(
            "cargo install --git {} --rev {}",
            a.source,
            short(&out.latest)
        ),
        "done",
        Some(format!("installed {}", paths::display(&exe))),
    ));
    if a.no_setup {
        out.steps
            .push(step("hall setup claude".into(), "skipped", None));
        out.steps
            .push(step("restart the daemon".into(), "skipped", None));
    } else {
        out.steps.push(refresh_plugin(&exe));
        out.steps.push(restart_daemon(&exe));
    }
    let mut next = vec![
        "restart Claude Code to load the new plugin".to_string(),
        "hall doctor".to_string(),
    ];
    if out.steps.iter().any(|s| s.status == "failed") {
        next.insert(0, "hall setup".into());
    }
    Ok(Response::new(out, next))
}

/// `hall update` with the same source and branch.
fn again(a: &UpdateArgs) -> String {
    let mut c = "hall update".to_string();
    if a.source != SOURCE {
        c.push_str(&format!(" --source {}", quote(&a.source)));
    }
    if a.branch != "main" {
        c.push_str(&format!(" --branch {}", quote(&a.branch)));
    }
    c
}

fn quote(s: &str) -> String {
    shlex::try_quote(s).map_or_else(|_| s.to_string(), |q| q.into_owned())
}

/// The commit `branch` points at in `source`, by `git ls-remote`, which
/// uses your git credentials and never prompts for them.
fn latest_commit(cwd: &Path, source: &str, branch: &str) -> Result<String> {
    let refname = format!("refs/heads/{branch}");
    let mut c = Command::new("git");
    c.current_dir(cwd)
        .args(["ls-remote", "--exit-code", source, &refname])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    for v in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
        c.env_remove(v);
    }
    let out = c.output().map_err(|e| {
        HallError::new(ErrorCode::CommandFailed, format!("could not run git: {e}"))
            .next(["hall doctor"])
    })?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let sha = stdout.lines().find_map(|l| {
        let (sha, name) = l.split_once('\t')?;
        (name.trim() == refname).then(|| sha.trim().to_string())
    });
    match sha {
        Some(sha) if out.status.success() => Ok(sha),
        // --exit-code: the repository answered, without that branch.
        _ if out.status.code() == Some(2) => Err(HallError::new(
            ErrorCode::CommandFailed,
            format!("{source} has no branch {branch}"),
        )
        .next([format!("git ls-remote --heads {}", quote(source))])),
        _ => Err(HallError::new(
            ErrorCode::CommandFailed,
            format!(
                "could not read {branch} from {source}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        )
        .next([
            format!("git ls-remote {} {refname}", quote(source)),
            "hall update --source <git url>".into(),
        ])),
    }
}

/// The `--root` for `cargo install`: the directory above the `bin` that
/// this hall runs from, so the new one replaces it. Copies a package
/// manager owns are its to update.
fn install_root(exe: &Path) -> Result<PathBuf> {
    let shown = paths::display(exe);
    let unix = shown.replace('\\', "/");
    let refuse = |why: &str, how: &[&str]| {
        Err(HallError::new(ErrorCode::CommandFailed, format!("{shown} {why}")).next(how.to_vec()))
    };
    if unix.starts_with("/nix/store/") {
        return refuse(
            "was installed with Nix; upgrade it with Nix",
            &[
                "nix profile list",
                "nix profile upgrade <the hallmark entry>",
            ],
        );
    }
    if unix.contains("/Cellar/") {
        return refuse("was installed with Homebrew", &["brew upgrade hall"]);
    }
    if unix.contains("/target/debug/") || unix.contains("/target/release/") {
        return refuse(
            "is a build directory, not an installed hall",
            &["cargo install --path . --locked", "hall setup"],
        );
    }
    match exe.parent() {
        Some(bin) if bin.file_name().is_some_and(|n| n == "bin") => {
            Ok(bin.parent().unwrap_or(bin).to_path_buf())
        }
        _ => refuse(
            "isn't in a `bin` directory, so cargo can't install over it",
            &["cargo install --git <source> --locked --force"],
        ),
    }
}

/// Builds and installs `rev`. Cargo's progress goes to stderr, so stdout
/// stays this command's reply.
fn cargo_install(cargo: &str, cwd: &Path, a: &UpdateArgs, rev: &str, root: &Path) -> Result<()> {
    let mut c = Command::new(cargo);
    c.current_dir(cwd)
        .args([
            "install", "--locked", "--force", "--git", &a.source, "--rev", rev,
        ])
        .arg("--root")
        .arg(root)
        // The new binary knows its commit, and cargo fetches with your git
        // and its credentials, never prompting for them.
        .env("HALL_BUILD_COMMIT", rev)
        .env("CARGO_NET_GIT_FETCH_WITH_CLI", "true")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()));
    let status = c.status().map_err(|e| {
        HallError::new(
            ErrorCode::CommandFailed,
            format!("could not run {cargo}: {e}"),
        )
        .next([
            "install Rust (https://rustup.rs), or set HALL_CARGO=/path/to/cargo",
            "hall update",
        ])
    })?;
    if status.success() {
        return Ok(());
    }
    Err(HallError::new(
        ErrorCode::CommandFailed,
        format!(
            "cargo install failed ({status}); its output is on stderr. The installed hall is unchanged"
        ),
    )
    .next([again(a), "hall doctor".into()]))
}

/// Windows can't replace a running program, but it can rename one: move
/// this hall aside so cargo can write the new one in its place.
#[cfg(windows)]
fn set_aside(exe: &Path) -> Option<PathBuf> {
    let old = exe.with_extension("exe.old");
    // A previous update's copy; still locked if that hall still runs.
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).ok().map(|_| old)
}

#[cfg(not(windows))]
fn set_aside(_exe: &Path) -> Option<PathBuf> {
    None
}

/// The plugin's hooks are written by the hall that installs them: have the
/// new one rewrite them, if the plugin is installed at all.
fn refresh_plugin(exe: &Path) -> Step {
    let command = "hall setup claude".to_string();
    if !crate::repo::user_dir().join("claude-plugin").is_dir() {
        return step(
            command,
            "skipped",
            Some("the plugin isn't installed; `hall setup` installs it".into()),
        );
    }
    match run_new(exe, &["setup", "claude"]) {
        Ok(v) => {
            let failed = v["steps"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|s| s["status"] == "failed");
            match failed {
                Some(s) => step(command, "failed", s["detail"].as_str().map(String::from)),
                None => step(command, "done", None),
            }
        }
        Err(e) => step(command, "failed", Some(e)),
    }
}

/// A running daemon still runs the old code: stop it and start the new one.
fn restart_daemon(exe: &Path) -> Step {
    let command = "restart the daemon".to_string();
    if !crate::daemon::state::instance_running() {
        return step(command, "skipped", Some("not running".into()));
    }
    match run_new(exe, &["daemon", "stop"]) {
        Ok(v) if v["running"] == true => {
            return step(
                command,
                "warn",
                Some(
                    "it is finishing its current pass on the old hall; \
                     run `hall daemon start` once it exits"
                        .into(),
                ),
            );
        }
        Ok(_) => {}
        Err(e) => return step(command, "failed", Some(e)),
    }
    match run_new(exe, &["daemon", "start"]) {
        Ok(_) => step(command, "done", Some("running the new hall".into())),
        Err(e) => step(command, "failed", Some(e)),
    }
}

/// Runs the newly installed hall and returns its JSON reply, or the error
/// it reported.
fn run_new(exe: &Path, args: &[&str]) -> std::result::Result<Value, String> {
    let out = Command::new(exe)
        .args(args)
        .arg("--json")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run {}: {e}", paths::display(exe)))?;
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    if out.status.success() {
        return Ok(v);
    }
    Err(v["error"]["message"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| String::from_utf8_lossy(&out.stderr).trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(source: &str, branch: &str) -> UpdateArgs {
        UpdateArgs {
            check: false,
            source: source.into(),
            branch: branch.into(),
            no_setup: false,
        }
    }

    #[test]
    fn installs_over_the_bin_it_runs_from() {
        let root = install_root(Path::new("/home/me/.cargo/bin/hall")).unwrap();
        assert_eq!(root, Path::new("/home/me/.cargo"));
    }

    #[test]
    fn leaves_package_managers_and_builds_alone() {
        for exe in [
            "/nix/store/abc-hallmark-0.1.0/bin/hall",
            "/opt/homebrew/Cellar/hall/0.1.0/bin/hall",
            "/src/hallmark/target/release/hall",
            "/opt/hall",
        ] {
            let e = install_root(Path::new(exe)).unwrap_err();
            assert_eq!(e.code, ErrorCode::CommandFailed, "{exe}");
            assert!(!e.next.is_empty(), "{exe}");
        }
    }

    #[test]
    fn repeats_only_the_flags_that_differ() {
        assert_eq!(again(&args(SOURCE, "main")), "hall update");
        assert_eq!(
            again(&args("/tmp/my fork.git", "next")),
            "hall update --source '/tmp/my fork.git' --branch next"
        );
    }

    #[test]
    fn source_is_the_package_repository() {
        assert_eq!(SOURCE, "https://github.com/tiagocolombo/hallmark.git");
    }
}
