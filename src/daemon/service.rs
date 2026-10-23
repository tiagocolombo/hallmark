//! `hall daemon start|stop --at-login`: a user-level login service labelled
//! `dev.hallmark.daemon` that runs `<this binary> daemon run`.
//!
//! - macOS: a launchd agent (`~/Library/LaunchAgents`), through
//!   service-manager.
//! - Linux: a `systemd --user` unit, through service-manager.
//! - Windows: service-manager only knows system services there, so a Task
//!   Scheduler task (`schtasks /SC ONLOGON`) instead.
//!
//! It never restarts the daemon on its own: `hall daemon stop` means
//! stopped. `HALL_DAEMON_SERVICE_DRY=1` reports the definition without
//! installing anything (tests and CI must never install a real service).

use std::ffi::OsString;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Serialize;

use crate::error::{ErrorCode, HallError, Result};

use super::state;

pub const LABEL: &str = "dev.hallmark.daemon";

/// Passed to the service, when set: launchd and systemd start it with a
/// bare environment, where `claude`, `git` and `gh` may not be found.
const PASS_ENV: &[&str] = &["PATH", "HALL_HOME", "HALL_CLAUDE", "HALL_GH"];

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ServiceOut {
    pub label: String,
    /// launchd, systemd, schtasks, or another service-manager kind.
    pub manager: String,
    /// install or uninstall.
    pub action: String,
    /// True when `HALL_DAEMON_SERVICE_DRY` is set: nothing was touched.
    pub dry_run: bool,
    /// Whether the service was added or removed by this call.
    pub changed: bool,
    pub program: String,
    pub args: Vec<String>,
    /// The environment variables passed to the service (names only).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    /// What gets installed, in the manager's words.
    pub definition: String,
}

pub fn dry_run() -> bool {
    std::env::var("HALL_DAEMON_SERVICE_DRY").is_ok_and(|v| !v.is_empty() && v != "0")
}

fn manager_name() -> String {
    if cfg!(windows) {
        return "schtasks".into();
    }
    match service_manager::ServiceManagerKind::native() {
        Ok(k) => format!("{k:?}").to_lowercase(),
        Err(_) => "none".into(),
    }
}

fn program() -> Result<PathBuf> {
    Ok(dunce::canonicalize(std::env::current_exe()?)?)
}

fn args() -> Vec<String> {
    vec!["daemon".into(), "run".into()]
}

fn env() -> Vec<(String, String)> {
    PASS_ENV
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect()
}

fn definition(manager: &str, program: &str) -> String {
    let cmd = format!("{program} daemon run");
    match manager {
        "launchd" => format!(
            "launchd agent {LABEL} (~/Library/LaunchAgents/{LABEL}.plist): RunAtLoad, no KeepAlive, runs `{cmd}`"
        ),
        "systemd" => format!(
            "systemd --user unit hallmark-daemon.service: WantedBy=default.target, Restart=no, runs `{cmd}`"
        ),
        "schtasks" => format!(
            "schtasks /Create /SC ONLOGON /TN {LABEL} /TR \"\\\"{program}\\\" daemon run\" /F"
        ),
        other => format!("{other} service {LABEL}: runs `{cmd}`"),
    }
}

fn out(action: &str, changed: bool) -> Result<ServiceOut> {
    let manager = manager_name();
    let program = crate::paths::display(&program()?);
    Ok(ServiceOut {
        label: LABEL.into(),
        definition: definition(&manager, &program),
        manager,
        action: action.into(),
        dry_run: dry_run(),
        changed,
        program,
        args: args(),
        env: env().into_iter().map(|(k, _)| k).collect(),
    })
}

fn failed(what: &str, e: impl std::fmt::Display) -> HallError {
    HallError::new(
        ErrorCode::CommandFailed,
        format!("could not {what} the login service {LABEL}: {e}"),
    )
    .next(["hall daemon start", "hall doctor"])
}

/// Installs (or refreshes) the login service.
pub fn install() -> Result<ServiceOut> {
    let was = installed()?;
    if !dry_run() {
        imp::install(&program()?, &env()).map_err(|e| failed("install", e))?;
    }
    let _ = std::fs::create_dir_all(state::dir());
    let _ = std::fs::write(installed_marker(), LABEL);
    out("install", !was)
}

/// Removes the login service. Removing nothing is fine.
pub fn uninstall() -> Result<ServiceOut> {
    let was = installed()?;
    if was && !dry_run() {
        imp::uninstall().map_err(|e| failed("remove", e))?;
    }
    let _ = std::fs::remove_file(installed_marker());
    out("uninstall", was)
}

/// Is the login service installed? (`hall daemon status`.)
pub fn installed() -> Result<bool> {
    if dry_run() {
        return Ok(installed_marker().exists());
    }
    imp::installed().map_err(|e| failed("query", e))
}

/// A note in the daemon directory that `--at-login` was used. Dry runs use
/// it as their whole record, so start/stop stay idempotent in tests too.
fn installed_marker() -> PathBuf {
    state::dir().join("at-login")
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    use service_manager::{
        RestartPolicy, ServiceInstallCtx, ServiceLabel, ServiceLevel, ServiceManager,
        ServiceStatus, ServiceStatusCtx, ServiceUninstallCtx,
    };

    fn label() -> std::io::Result<ServiceLabel> {
        LABEL.parse()
    }

    fn manager() -> std::io::Result<Box<dyn ServiceManager>> {
        let mut m = <dyn ServiceManager>::native()?;
        m.set_level(ServiceLevel::User)?;
        if !m.available()? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "no user-level service manager is available",
            ));
        }
        Ok(m)
    }

    pub fn install(program: &std::path::Path, env: &[(String, String)]) -> std::io::Result<()> {
        manager()?.install(ServiceInstallCtx {
            label: label()?,
            program: program.to_path_buf(),
            args: super::args().into_iter().map(OsString::from).collect(),
            contents: None,
            username: None,
            working_directory: Some(state::dir()),
            environment: Some(env.to_vec()),
            autostart: true,
            restart_policy: RestartPolicy::Never,
        })
    }

    pub fn uninstall() -> std::io::Result<()> {
        manager()?.uninstall(ServiceUninstallCtx { label: label()? })
    }

    pub fn installed() -> std::io::Result<bool> {
        let s = manager()?.status(ServiceStatusCtx { label: label()? })?;
        Ok(s != ServiceStatus::NotInstalled)
    }
}

#[cfg(windows)]
mod imp {
    use super::*;

    fn schtasks(args: &[OsString]) -> std::io::Result<std::process::Output> {
        crate::process::command("schtasks").args(args).output()
    }

    fn check(o: std::process::Output) -> std::io::Result<()> {
        if o.status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(
                String::from_utf8_lossy(&o.stderr).trim().to_string(),
            ))
        }
    }

    pub fn install(program: &std::path::Path, _env: &[(String, String)]) -> std::io::Result<()> {
        let tr = format!("\"{}\" daemon run", program.display());
        let args: Vec<OsString> = ["/Create", "/SC", "ONLOGON", "/TN", LABEL, "/TR"]
            .iter()
            .map(OsString::from)
            .chain([OsString::from(tr), OsString::from("/F")])
            .collect();
        check(schtasks(&args)?)
    }

    pub fn uninstall() -> std::io::Result<()> {
        let args: Vec<OsString> = ["/Delete", "/TN", LABEL, "/F"]
            .iter()
            .map(OsString::from)
            .collect();
        check(schtasks(&args)?)
    }

    pub fn installed() -> std::io::Result<bool> {
        let args: Vec<OsString> = ["/Query", "/TN", LABEL]
            .iter()
            .map(OsString::from)
            .collect();
        Ok(schtasks(&args)?.status.success())
    }
}
