//! `hall update`: install the latest `hall` from the source repository's
//! main branch with `cargo install`, unless the running one was built from
//! that commit. Then refresh what the old binary left behind: the Claude
//! Code plugin, whose hooks name this hall, and a running daemon.
//!
//! Main carries the versions (ADR 0035, ADR 0040): every merge is tagged
//! `vX.Y.Z` on it, so the reply names the installed version and the latest
//! one beside the commits. Every version lookup is remembered in
//! `<user dir>/update.json`, so the SessionStart hook and `hall doctor` can
//! say a new version exists without touching the network;
//! [`check_in_background`] refreshes it once a day.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::setup::{Step, step};
use crate::cli::UpdateArgs;
use crate::error::{ErrorCode, HallError, Result};
use crate::event::short;
use crate::output::{Render, Response};
use crate::paths;

/// Where `hall update` installs from, unless `--source` says otherwise.
pub const SOURCE: &str = concat!(env!("CARGO_PKG_REPOSITORY"), ".git");

/// How long a remembered release lookup stays fresh.
const CHECK_EVERY_HOURS: i64 = 24;

/// The commit this binary was built from, when its build knew it.
pub fn commit() -> Option<&'static str> {
    let c = env!("HALL_COMMIT");
    (!c.is_empty()).then_some(c)
}

/// The version this binary was built as.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// A release: a `vX.Y.Z` tag of the source repository.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Release {
    /// The version, without the `v`: `0.2.0`.
    pub version: String,
    /// The tag: `v0.2.0`.
    pub tag: String,
    /// The commit the tag points at.
    pub commit: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct UpdateOut {
    /// up_to_date, available (with --check) or updated.
    pub status: String,
    /// The version the running hall was built as.
    pub installed_version: String,
    /// The commit the running hall was built from; null when its build
    /// didn't know (built without git).
    pub installed: Option<String>,
    /// The latest commit on `branch`: what is (or would be) installed.
    pub latest: String,
    /// The highest version tagged in the source, without the `v`; null when
    /// there is none yet, or the tags couldn't be read.
    pub latest_version: Option<String>,
    /// That version's tag: `v0.3.0`.
    pub tag: Option<String>,
    /// The version the newly installed hall reports, after an update.
    pub new_version: Option<String>,
    pub source: String,
    pub branch: String,
    /// The hall binary that is (or would be) replaced.
    pub hall: String,
    /// What the update did: the install, the plugin, the daemon.
    pub steps: Vec<Step>,
}

/// `0.2.0 (abc1234)`, or the version alone without a known commit.
fn label(version: &str, commit: Option<&str>) -> String {
    match commit {
        Some(c) => format!("{version} ({})", short(c)),
        None => version.to_string(),
    }
}

impl Render for UpdateOut {
    fn render(&self) -> String {
        let installed = label(&self.installed_version, self.installed.as_deref());
        let latest = short(&self.latest);
        let from = format!("{} of {}", self.branch, self.source);
        let release = match (&self.latest_version, &self.tag) {
            (Some(v), Some(t)) if newer(v, &self.installed_version) => {
                format!(" Version {v} ({t}) is out.")
            }
            (Some(v), _) => format!(" The latest version is {v}."),
            _ => String::new(),
        };
        let mut s = match self.status.as_str() {
            "up_to_date" => {
                format!("hall {installed} is up to date: {latest} is the latest on {from}.\n")
            }
            "available" => {
                format!("An update is available: {installed} → {latest} ({from}).{release}\n")
            }
            _ => {
                let new = label(
                    self.new_version.as_deref().unwrap_or("?"),
                    Some(&self.latest),
                );
                format!("Updated {}: {installed} → {new} ({from}).\n", self.hall)
            }
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
    // Releases are tags on main: they name versions, main is what installs.
    // A source whose tags can't be read still updates.
    let release = match latest_release(cwd, &a.source) {
        Ok(r) => {
            remember(&a.source, r.as_ref());
            r
        }
        Err(_) => None,
    };
    let mut out = UpdateOut {
        status: "up_to_date".into(),
        installed_version: version().into(),
        installed: commit().map(String::from),
        latest,
        latest_version: release.as_ref().map(|r| r.version.clone()),
        tag: release.as_ref().map(|r| r.tag.clone()),
        new_version: None,
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
    let cargo = crate::config::Tool::Cargo.command();
    // Build into a staging root, so the installed hall is only out of place
    // for the renames in `swap_in`, not for the whole build.
    let staging = root.join(".hall-update");
    let _ = std::fs::remove_dir_all(&staging);
    let installed = cargo_install(&cargo, cwd, &a, &out.latest, &staging)
        .and_then(|()| swap_in(&staging, &exe, &a.source));
    let _ = std::fs::remove_dir_all(&staging);
    installed?;
    out.status = "updated".into();
    out.new_version = run_new(&exe, &["help"])
        .ok()
        .and_then(|v| v["version"].as_str().map(String::from));
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

/// True when version `a` is newer than `b`. A version that doesn't parse
/// is never newer, and nothing is newer than one that doesn't.
pub fn newer(a: &str, b: &str) -> bool {
    match (semver::Version::parse(a), semver::Version::parse(b)) {
        (Ok(a), Ok(b)) => a > b,
        _ => false,
    }
}

/// `git ls-remote <flags> <source> <patterns>`, which uses your git
/// credentials and never prompts for them.
fn ls_remote(
    cwd: &Path,
    flags: &[&str],
    source: &str,
    patterns: &[&str],
) -> Result<std::process::Output> {
    let mut c = Command::new("git");
    c.current_dir(cwd)
        .arg("ls-remote")
        .args(flags)
        .arg(source)
        .args(patterns)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    for v in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
        c.env_remove(v);
    }
    c.output().map_err(|e| {
        HallError::new(ErrorCode::CommandFailed, format!("could not run git: {e}"))
            .next(["hall doctor"])
    })
}

/// The highest release tagged in `source`: `vX.Y.Z` tags, pre-releases
/// left out. None when it has no release yet.
pub fn latest_release(cwd: &Path, source: &str) -> Result<Option<Release>> {
    let out = ls_remote(cwd, &["--tags"], source, &["v*"])?;
    if !out.status.success() {
        return Err(HallError::new(
            ErrorCode::CommandFailed,
            format!(
                "could not read the releases of {source}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        )
        .next([
            format!("git ls-remote --tags {}", quote(source)),
            "hall update --source <git url>".into(),
        ]));
    }
    Ok(pick_release(&String::from_utf8_lossy(&out.stdout)))
}

/// The highest release in `git ls-remote --tags` output. An annotated
/// tag's `^{}` line names the commit, which is what gets built.
fn pick_release(ls_remote: &str) -> Option<Release> {
    let mut found: Vec<(semver::Version, Release)> = Vec::new();
    for line in ls_remote.lines() {
        let Some((sha, name)) = line.split_once('\t') else {
            continue;
        };
        let name = name.trim().trim_start_matches("refs/tags/");
        let (tag, peeled) = match name.strip_suffix("^{}") {
            Some(t) => (t, true),
            None => (name, false),
        };
        let Some(v) = tag
            .strip_prefix('v')
            .and_then(|v| semver::Version::parse(v).ok())
        else {
            continue;
        };
        if !v.pre.is_empty() {
            continue;
        }
        match found.iter_mut().find(|(_, r)| r.tag == tag) {
            Some((_, r)) if peeled => r.commit = sha.trim().to_string(),
            Some(_) => {}
            None => found.push((
                v.clone(),
                Release {
                    version: v.to_string(),
                    tag: tag.to_string(),
                    commit: sha.trim().to_string(),
                },
            )),
        }
    }
    found
        .into_iter()
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, r)| r)
}

/// The last release lookup, in `<user dir>/update.json`.
#[derive(Debug, Serialize, Deserialize)]
struct Seen {
    checked_at: DateTime<Utc>,
    source: String,
    latest: Option<Release>,
}

fn seen_path() -> PathBuf {
    crate::repo::user_dir().join("update.json")
}

fn load_seen() -> Option<Seen> {
    serde_json::from_slice(&std::fs::read(seen_path()).ok()?).ok()
}

/// Remembers a release lookup. Best effort: a notice is only a hint.
fn remember(source: &str, latest: Option<&Release>) {
    let seen = Seen {
        checked_at: Utc::now(),
        source: source.to_string(),
        latest: latest.cloned(),
    };
    let path = seen_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec_pretty(&seen) {
        let _ = std::fs::write(path, json);
    }
}

/// Where `hall update` looks by default: `HALL_UPDATE_SOURCE`, else
/// [`SOURCE`].
fn default_source() -> String {
    std::env::var("HALL_UPDATE_SOURCE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| SOURCE.to_string())
}

/// A newer release than this hall, as last seen (no network): the
/// release, when there is one.
pub fn available() -> Option<Release> {
    let seen = load_seen()?;
    let r = seen.latest.filter(|_| seen.source == default_source())?;
    newer(&r.version, version()).then_some(r)
}

/// One line that says a newer release exists, for the SessionStart hook
/// and `hall doctor`.
pub fn notice() -> Option<String> {
    let r = available()?;
    Some(format!(
        "hall {} is available (this is {}): run `hall update`.",
        r.version,
        version()
    ))
}

/// Refreshes `update.json` with a detached `hall update --check` when the
/// last lookup is a day old, so the next session can say what's new.
/// `HALL_UPDATE_CHECK=off` turns it off (tests, offline machines).
pub fn check_in_background() {
    if matches!(
        std::env::var("HALL_UPDATE_CHECK").as_deref(),
        Ok("off" | "0" | "false")
    ) {
        return;
    }
    let source = default_source();
    let fresh = load_seen().is_some_and(|s| {
        s.source == source && Utc::now() - s.checked_at < chrono::Duration::hours(CHECK_EVERY_HOURS)
    });
    if fresh {
        return;
    }
    // Mark it checked now, keeping what was seen, so a failing lookup
    // (offline) is tried once a day, not on every session.
    let last = load_seen()
        .filter(|s| s.source == source)
        .and_then(|s| s.latest);
    remember(&source, last.as_ref());
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut c = Command::new(exe);
    c.args(["update", "--check", "--json"])
        .current_dir(crate::repo::user_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::daemon::spawn::detach(&mut c);
    let _ = c.spawn();
}

/// The commit `branch` points at in `source`.
fn latest_commit(cwd: &Path, source: &str, branch: &str) -> Result<String> {
    let refname = format!("refs/heads/{branch}");
    let out = ls_remote(cwd, &["--exit-code"], source, &[&refname])?;
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
            "install Rust (https://rustup.rs), or `hall config set --user tools.cargo /path/to/cargo`",
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

/// Moves the freshly built hall from `staging` over `exe`. Windows can't
/// replace a running program but can rename one, so this hall goes aside
/// first and comes back if the move fails.
fn swap_in(staging: &Path, exe: &Path, source: &str) -> Result<()> {
    let name = exe.file_name().unwrap_or_default();
    let built = staging.join("bin").join(name);
    if !built.is_file() {
        return Err(HallError::new(
            ErrorCode::CommandFailed,
            format!(
                "cargo install succeeded but didn't build {}; does {source} build a `hall` binary?",
                name.to_string_lossy()
            ),
        )
        .next(["hall update"]));
    }
    let aside = set_aside(exe);
    if let Err(e) = std::fs::rename(&built, exe) {
        if let Some(old) = &aside {
            let _ = std::fs::rename(old, exe);
        }
        return Err(HallError::new(
            ErrorCode::CommandFailed,
            format!("could not install over {}: {e}", paths::display(exe)),
        )
        .next(["hall update"]));
    }
    if let Some(old) = &aside {
        // Windows keeps a running program's file; the next update removes it.
        let _ = std::fs::remove_file(old);
    }
    Ok(())
}

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

    fn available(latest_version: &str) -> UpdateOut {
        UpdateOut {
            status: "available".into(),
            installed_version: "0.2.0".into(),
            installed: Some("a".repeat(40)),
            latest: "b".repeat(40),
            latest_version: Some(latest_version.into()),
            tag: Some(format!("v{latest_version}")),
            new_version: None,
            source: "https://github.com/tiagocolombo/hallmark".into(),
            branch: "main".into(),
            hall: "/bin/hall".into(),
            steps: vec![],
        }
    }

    #[test]
    fn names_the_latest_version_tagged_on_main() {
        // Every merge is tagged (ADR 0040): a tag is a version, not a release.
        assert_eq!(
            available("0.2.0").render(),
            "An update is available: 0.2.0 (aaaaaaa) → bbbbbbb (main of \
             https://github.com/tiagocolombo/hallmark). The latest version is 0.2.0.\n"
        );
        assert_eq!(
            available("0.3.0").render(),
            "An update is available: 0.2.0 (aaaaaaa) → bbbbbbb (main of \
             https://github.com/tiagocolombo/hallmark). Version 0.3.0 (v0.3.0) is out.\n"
        );
    }

    #[test]
    fn picks_the_highest_release_and_its_commit() {
        let out = "\
aaa\trefs/tags/v0.2.0
bbb\trefs/tags/v0.10.0
ccc\trefs/tags/v0.10.0^{}
ddd\trefs/tags/v1.0.0-rc.1
eee\trefs/tags/v0.9.9
fff\trefs/tags/vnext
";
        let r = pick_release(out).unwrap();
        // 0.10 beats 0.9 (semver, not text); the rc is a pre-release; the
        // annotated tag's peeled line is the commit.
        assert_eq!(
            r,
            Release {
                version: "0.10.0".into(),
                tag: "v0.10.0".into(),
                commit: "ccc".into(),
            }
        );
        assert_eq!(pick_release(""), None);
        assert_eq!(pick_release("aaa\trefs/tags/nightly\n"), None);
    }

    #[test]
    fn compares_versions_by_semver() {
        assert!(newer("0.10.0", "0.9.0"));
        assert!(newer("1.0.0", "1.0.0-rc.1"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
        assert!(!newer("garbage", "0.1.0"));
        assert!(!newer("0.2.0", "garbage"));
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
