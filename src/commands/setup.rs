//! `hall setup`: every step of setting Hallmark up, in one command (see
//! [`all`]). `hall setup claude`: install the plugin for Claude Code at user level,
//! through a local marketplace (docs/decisions/0009), and with
//! `--statusline`, point Claude Code's status line at `hall statusline`
//! (docs/decisions/0023).

use std::path::{Path, PathBuf};
use std::process::Command;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

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
    /// The files written under `marketplace_dir`, relative to it: the
    /// plugin's hooks, skills, agents and schemas, and the marketplace.
    pub plugin_files: Vec<String>,
    /// With `--statusline`: the settings file and the command written to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statusline: Option<StatusLineOut>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Serialize, serde::Deserialize, JsonSchema)]
pub struct StatusLineOut {
    /// Claude Code's user settings file.
    pub settings: String,
    /// The `statusLine.command` Hallmark sets.
    pub command: String,
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
        s.push_str(&format!(
            "\nPlugin files ({}) in {}:\n",
            self.plugin_files.len(),
            self.marketplace_dir
        ));
        for f in &self.plugin_files {
            s.push_str(&format!("  {f}\n"));
        }
        if let Some(sl) = &self.statusline {
            s.push_str(&format!(
                "\nStatus line: {} (in {})\n",
                sl.command, sl.settings
            ));
        }
        s
    }
}

fn claude_bin() -> String {
    std::env::var("HALL_CLAUDE").unwrap_or_else(|_| "claude".into())
}

fn step(command: String, status: &str, detail: Option<String>) -> Step {
    Step {
        command,
        status: status.into(),
        detail,
    }
}

pub fn setup(cwd: &Path, a: SetupArgs) -> Result<Response> {
    match a.target {
        Some(SetupTarget::Claude) => {
            if a.force && !a.statusline {
                return Err(HallError::usage("--force goes with --statusline")
                    .next(["hall setup claude --statusline --force"]));
            }
            claude(&a)
        }
        None => all(cwd, &a),
    }
}

fn claude(a: &SetupArgs) -> Result<Response> {
    let exe = paths::normalize(&std::env::current_exe()?);
    let dir = user_dir().join("claude-plugin");
    let claude = claude_bin();
    let dir_s = dir.to_string_lossy().to_string();

    // Decide about the status line first, so a refusal changes nothing.
    let statusline = if a.statusline {
        let settings = settings_path()?;
        let command = statusline_command(&exe);
        let plan = plan_statusline(&settings, &command, a.force)?;
        Some((settings, command, plan))
    } else {
        None
    };

    let mut plugin_files: Vec<String> = plugin::FILES
        .iter()
        .map(|(rel, _)| format!("plugins/{PLUGIN}/{rel}"))
        .collect();
    plugin_files.push(".claude-plugin/marketplace.json".into());

    let mut steps = vec![step(
        format!("write the plugin to {}", paths::display(&dir)),
        if a.dry_run { "planned" } else { "done" },
        None,
    )];
    if !a.dry_run {
        // Start clean, so files a newer plugin dropped don't linger.
        let plugin_dir = dir.join("plugins").join(PLUGIN);
        if plugin_dir.exists() {
            std::fs::remove_dir_all(&plugin_dir)?;
        }
        plugin::extract(&dir, &exe)?;
    }

    // (arguments, required). `add` and `install` report "already" when there
    // is nothing to do; the updates refresh an existing install and may fail
    // harmlessly on a fresh one (ADR 0009).
    let plan: [(Vec<String>, bool); 4] = [
        (
            args(&["plugin", "marketplace", "add", &dir_s, "--scope", "user"]),
            true,
        ),
        (
            args(&["plugin", "marketplace", "update", MARKETPLACE]),
            false,
        ),
        (
            args(&[
                "plugin",
                "install",
                &format!("{PLUGIN}@{MARKETPLACE}"),
                "--scope",
                "user",
            ]),
            true,
        ),
        (
            args(&[
                "plugin",
                "update",
                &format!("{PLUGIN}@{MARKETPLACE}"),
                "--scope",
                "user",
            ]),
            false,
        ),
    ];
    for (argv, required) in &plan {
        let shown = format!("{claude} {}", argv.join(" "));
        if a.dry_run {
            steps.push(step(shown, "planned", None));
            continue;
        }
        let out = Command::new(&claude).args(argv).output();
        if !required {
            let ok = out.map(|o| o.status.success()).unwrap_or(false);
            steps.push(step(shown, if ok { "done" } else { "skipped" }, None));
            continue;
        }
        let out = out.map_err(|e| {
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
            steps.push(step(
                shown,
                if already { "skipped" } else { "done" },
                already.then(|| "already present".into()),
            ));
        } else {
            return Err(HallError::new(
                ErrorCode::CommandFailed,
                format!("`{shown}` failed: {}", text.trim()),
            )
            .next(["hall setup claude --dry-run", "hall doctor"]));
        }
    }

    let statusline = match statusline {
        None => None,
        Some((settings, command, plan)) => {
            let shown = format!("set statusLine in {}", paths::display(&settings));
            match plan {
                StatusPlan::Unchanged => {
                    steps.push(step(shown, "skipped", Some("already set".into())))
                }
                StatusPlan::Write { text, replaced } => {
                    let detail = replaced.map(|old| format!("replaces `{old}`"));
                    if a.dry_run {
                        steps.push(step(shown, "planned", detail));
                    } else {
                        write_atomic(&settings, &text)?;
                        steps.push(step(shown, "done", detail));
                    }
                }
            }
            Some(StatusLineOut {
                settings: paths::display(&settings),
                command,
            })
        }
    };

    let out = SetupOut {
        target: "claude".into(),
        marketplace_dir: paths::display(&dir),
        hall: paths::display(&exe),
        dry_run: a.dry_run,
        plugin_files,
        statusline,
        steps,
    };
    let mut next: Vec<String> = Vec::new();
    if a.dry_run {
        next.push(
            match (a.statusline, a.force) {
                (true, true) => "hall setup claude --statusline --force",
                (true, false) => "hall setup claude --statusline",
                _ => "hall setup claude",
            }
            .into(),
        );
    }
    next.extend(
        [
            "hall init",
            "hall daemon start --at-login",
            "hall notify test",
            "hall doctor",
            "restart Claude Code to load the plugin",
        ]
        .map(String::from),
    );
    Ok(Response::new(out, next))
}

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

// ── the status line ────────────────────────────────────────────────────────

/// Claude Code's user settings: `HALL_CLAUDE_SETTINGS`, else
/// `$CLAUDE_CONFIG_DIR/settings.json`, else `~/.claude/settings.json`.
fn settings_path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("HALL_CLAUDE_SETTINGS") {
        return Ok(PathBuf::from(p));
    }
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Ok(PathBuf::from(d).join("settings.json"));
    }
    dirs::home_dir()
        .map(|h| h.join(".claude").join("settings.json"))
        .ok_or_else(|| {
            HallError::internal("no home directory to find Claude Code's settings in")
                .next(["HALL_CLAUDE_SETTINGS=<path> hall setup claude --statusline"])
        })
}

/// `<abs path to hall> statusline`, quoted like the hook commands.
fn statusline_command(exe: &Path) -> String {
    let exe = exe.to_string_lossy();
    if exe.contains(' ') {
        format!("\"{exe}\" statusline")
    } else {
        format!("{exe} statusline")
    }
}

/// Whether a status line command is Hallmark's, from any install path.
fn is_hall_statusline(cmd: &str) -> bool {
    let Some(bin) = cmd.trim().strip_suffix(" statusline") else {
        return false;
    };
    let bin = bin.trim().trim_matches('"');
    let name = bin.rsplit(['/', '\\']).next().unwrap_or(bin);
    name == "hall" || name.eq_ignore_ascii_case("hall.exe")
}

#[derive(Debug, PartialEq)]
enum StatusPlan {
    Unchanged,
    /// The new settings file, and the command it replaces, if any.
    Write {
        text: String,
        replaced: Option<String>,
    },
}

fn plan_statusline(settings: &Path, command: &str, force: bool) -> Result<StatusPlan> {
    let text = match std::fs::read_to_string(settings) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let shown = paths::display(settings);
    let invalid = |why: String| {
        HallError::internal(format!("{shown} {why}; it was left unchanged"))
            .next([format!("fix {shown}"), "hall setup claude".into()])
    };
    let desired = serde_json::json!({ "type": "command", "command": command });
    if text.trim().is_empty() {
        let new = format!(
            "{}\n",
            serde_json::to_string_pretty(&serde_json::json!({ "statusLine": desired }))?
        );
        return Ok(StatusPlan::Write {
            text: new,
            replaced: None,
        });
    }
    let old: Value =
        serde_json::from_str(&text).map_err(|e| invalid(format!("is not valid JSON ({e})")))?;
    let Some(obj) = old.as_object() else {
        return Err(invalid("is not a JSON object".into()));
    };
    let mut replaced = None;
    if let Some(cur) = obj.get("statusLine") {
        let cur_cmd = cur.get("command").and_then(Value::as_str).unwrap_or("");
        if cur.get("type").and_then(Value::as_str) == Some("command") && cur_cmd == command {
            return Ok(StatusPlan::Unchanged);
        }
        if !force && !is_hall_statusline(cur_cmd) {
            return Err(HallError::new(
                ErrorCode::Usage,
                format!("{shown} already has a status line: {cur}"),
            )
            .next([
                "hall setup claude --statusline --force",
                "hall setup claude",
            ]));
        }
        replaced = Some(if cur_cmd.is_empty() {
            cur.to_string()
        } else {
            cur_cmd.to_string()
        });
    }
    let new =
        merge_status_line(&text, command).ok_or_else(|| invalid("could not be edited".into()))?;
    // Everything but statusLine must survive the edit untouched.
    let mut check: Value = serde_json::from_str(&new)
        .map_err(|e| HallError::internal(format!("editing {shown} broke it: {e}")))?;
    let got = check.as_object_mut().and_then(|o| o.remove("statusLine"));
    let mut rest = old.clone();
    if let Some(o) = rest.as_object_mut() {
        o.remove("statusLine");
    }
    if got.as_ref() != Some(&desired) || check != rest {
        return Err(HallError::internal(format!(
            "editing {shown} would change more than statusLine; it was left unchanged"
        )));
    }
    Ok(StatusPlan::Write {
        text: new,
        replaced,
    })
}

/// Sets the top-level `statusLine` in a JSON object's text, keeping every
/// other byte (key order, formatting) as it was. `None` if the text isn't a
/// JSON object.
fn merge_status_line(text: &str, command: &str) -> Option<String> {
    let cmd = serde_json::to_string(command).ok()?;
    let render = |indent: &str| {
        if indent.is_empty() {
            format!("{{\"type\":\"command\",\"command\":{cmd}}}")
        } else {
            format!(
                "{{\n{indent}{indent}\"type\": \"command\",\n{indent}{indent}\"command\": {cmd}\n{indent}}}"
            )
        }
    };
    let m = scan_object(text)?;
    if let Some(&(_, ks, vs, ve)) = m.members.iter().find(|(k, ..)| k == "statusLine") {
        let indent = line_indent(text, ks);
        return Some(format!("{}{}{}", &text[..vs], render(indent), &text[ve..]));
    }
    match m.members.last() {
        Some(&(_, ks, _, ve)) => {
            let indent = line_indent(text, ks);
            let member = if indent.is_empty() {
                format!(",\"statusLine\":{}", render(""))
            } else {
                format!(",\n{indent}\"statusLine\": {}", render(indent))
            };
            Some(format!("{}{member}{}", &text[..ve], &text[ve..]))
        }
        None => Some(format!(
            "{}{{\n  \"statusLine\": {}\n}}{}",
            &text[..m.open],
            render("  "),
            &text[m.close + 1..]
        )),
    }
}

/// The whitespace before a key on its line, or "" if the key isn't first on
/// its line.
fn line_indent(text: &str, at: usize) -> &str {
    let start = text[..at].rfind('\n').map(|p| p + 1).unwrap_or(0);
    let ind = &text[start..at];
    if ind.chars().all(|c| c == ' ' || c == '\t') {
        ind
    } else {
        ""
    }
}

struct Members {
    /// (key, key start, value start, value end), as byte offsets.
    members: Vec<(String, usize, usize, usize)>,
    open: usize,
    close: usize,
}

/// The members of the top-level JSON object in `s`, assumed valid JSON.
fn scan_object(s: &str) -> Option<Members> {
    let b = s.as_bytes();
    let open = skip_ws(b, 0);
    if b.get(open) != Some(&b'{') {
        return None;
    }
    let mut members = Vec::new();
    let mut i = skip_ws(b, open + 1);
    if b.get(i) == Some(&b'}') {
        return Some(Members {
            members,
            open,
            close: i,
        });
    }
    loop {
        let ks = i;
        let ke = skip_string(b, ks)?;
        let key: String = serde_json::from_str(&s[ks..ke]).ok()?;
        i = skip_ws(b, ke);
        if b.get(i) != Some(&b':') {
            return None;
        }
        let vs = skip_ws(b, i + 1);
        let ve = skip_value(b, vs)?;
        members.push((key, ks, vs, ve));
        i = skip_ws(b, ve);
        match b.get(i)? {
            b',' => i = skip_ws(b, i + 1),
            b'}' => {
                return Some(Members {
                    members,
                    open,
                    close: i,
                });
            }
            _ => return None,
        }
    }
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// `b[i]` is a quote; returns the index just past the closing quote.
fn skip_string(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i) != Some(&b'"') {
        return None;
    }
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'"' => return Some(j + 1),
            _ => j += 1,
        }
    }
    None
}

fn skip_value(b: &[u8], i: usize) -> Option<usize> {
    match b.get(i)? {
        b'"' => skip_string(b, i),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut j = i;
            while j < b.len() {
                match b[j] {
                    b'"' => {
                        j = skip_string(b, j)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(j + 1);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            None
        }
        _ => {
            let mut j = i;
            while j < b.len() && !matches!(b[j], b',' | b'}' | b']') && !b[j].is_ascii_whitespace()
            {
                j += 1;
            }
            (j > i).then_some(j)
        }
    }
}

/// Writes through a temporary file and a rename, so Claude Code never reads
/// a half-written settings file.
fn write_atomic(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".hall-tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// ── hall setup: everything ──────────────────────────────────────────────────

/// `hall setup`: the plugin and status line, this repo, the daemon at
/// login, a phone topic, then `hall doctor`. Every step is idempotent and
/// reported; a failed step doesn't stop the others.
fn all(cwd: &Path, a: &SetupArgs) -> Result<Response> {
    use crate::cli::{DaemonAction, DaemonArgs, InitArgs, NotifyAction, NotifyArgs};
    let exe = paths::normalize(&std::env::current_exe()?);
    let shown = paths::display(&exe);
    let planned = |command: &str| step(command.to_string(), "planned", None);
    let mut steps = Vec::new();
    let mut next: Vec<String> = Vec::new();

    // 1. Which hall the hooks and the daemon will run.
    if shown.contains("/target/debug/") || shown.contains("/target/release/") {
        steps.push(step(
            format!("use {shown}"),
            "warn",
            Some(
                "this is a build directory, and the plugin and the daemon will run this file. \
                 Install hall (`dev install`, or `cargo install --path . --locked`) and run \
                 `hall setup` from the installed copy."
                    .into(),
            ),
        ));
    } else {
        steps.push(step(format!("use {shown}"), "done", None));
        steps.push(path_step(&exe, a.dry_run));
    }

    // 2. The Claude Code plugin, and the status line unless told not to.
    let mut claude_args = SetupArgs {
        target: Some(SetupTarget::Claude),
        dry_run: a.dry_run,
        statusline: !a.no_statusline,
        force: a.force,
        no_statusline: a.no_statusline,
        no_daemon: a.no_daemon,
        no_notify: a.no_notify,
    };
    let mut plugin_files = Vec::new();
    let mut statusline = None;
    let mut result = claude(&claude_args);
    if let Err(e) = &result
        && e.code == ErrorCode::Usage
        && claude_args.statusline
    {
        // Someone else's status line: keep it, install the rest.
        steps.push(step(
            "set Claude Code's status line to hall statusline".into(),
            "skipped",
            Some(e.message.clone()),
        ));
        next.push("hall setup claude --statusline --force".into());
        claude_args.statusline = false;
        result = claude(&claude_args);
    }
    match result {
        Ok(r) => {
            let sub = r.value["steps"].as_array().cloned().unwrap_or_default();
            let failed = sub.iter().any(|s| s["status"] == "failed");
            plugin_files =
                serde_json::from_value(r.value["plugin_files"].clone()).unwrap_or_default();
            statusline = serde_json::from_value(r.value["statusline"].clone())
                .ok()
                .flatten();
            steps.push(step(
                format!(
                    "hall setup claude{}",
                    if claude_args.statusline {
                        " --statusline"
                    } else {
                        ""
                    }
                ),
                if a.dry_run {
                    "planned"
                } else if failed {
                    "failed"
                } else {
                    "done"
                },
                Some(format!(
                    "{} plugin files; restart Claude Code to load them",
                    plugin_files.len()
                )),
            ));
        }
        Err(e) => steps.push(step("hall setup claude".into(), "failed", Some(e.message))),
    }

    // 3. This repository.
    match crate::repo::Repo::discover(cwd) {
        Err(_) => steps.push(step(
            "hall init".into(),
            "skipped",
            Some("not in a git repository".into()),
        )),
        Ok(_) if a.dry_run => steps.push(planned("hall init")),
        Ok(_) => match super::init::init(
            cwd,
            InitArgs {
                capture: None,
                base: None,
                no_claude_md: false,
            },
        ) {
            Ok(r) => {
                let created: Vec<String> =
                    serde_json::from_value(r.value["created"].clone()).unwrap_or_default();
                let detail = if created.is_empty() {
                    "already set up".to_string()
                } else {
                    next.push(format!(
                        "git add {} && git commit -m \"Adopt Hallmark\"",
                        created.join(" ")
                    ));
                    format!("wrote {}", created.join(", "))
                };
                steps.push(step("hall init".into(), "done", Some(detail)));
            }
            Err(e) => steps.push(step("hall init".into(), "failed", Some(e.message))),
        },
    }

    // 4. The daemon, at login; restarted when this hall is newer than it.
    if a.no_daemon {
        steps.push(step("hall daemon start --at-login".into(), "skipped", None));
    } else if a.dry_run {
        steps.push(planned("hall daemon start --at-login"));
    } else {
        let mut detail = Vec::new();
        if daemon_is_stale(&exe) {
            let _ = super::daemon::daemon(
                cwd,
                DaemonArgs {
                    action: DaemonAction::Stop { at_login: false },
                },
            );
            detail.push("restarted it on the new hall".to_string());
        }
        match super::daemon::daemon(
            cwd,
            DaemonArgs {
                action: DaemonAction::Start { at_login: true },
            },
        ) {
            Ok(r) => {
                if let Some(n) = r.value["note"].as_str() {
                    detail.push(n.to_string());
                }
                steps.push(step(
                    "hall daemon start --at-login".into(),
                    "done",
                    (!detail.is_empty()).then(|| detail.join("; ")),
                ));
            }
            Err(e) => steps.push(step(
                "hall daemon start --at-login".into(),
                "failed",
                Some(e.message),
            )),
        }
    }

    // 5. A private phone topic, once.
    let has_phone = crate::repo::Repo::discover(cwd)
        .ok()
        .map(|r| r.config.notify())
        .or_else(|| {
            crate::config::UserConfig::load()
                .ok()
                .and_then(|u| u.notify)
        })
        .is_some_and(|n| n.phone.is_some());
    if a.no_notify || has_phone {
        steps.push(step(
            "hall notify test".into(),
            "skipped",
            has_phone.then(|| "a phone channel is already set".into()),
        ));
    } else if a.dry_run {
        steps.push(planned("hall notify test"));
    } else {
        match super::notify::notify(
            cwd,
            NotifyArgs {
                action: NotifyAction::Test { channel: None },
            },
        ) {
            Ok(r) => steps.push(step(
                "hall notify test".into(),
                "done",
                r.value["subscribe"].as_str().map(String::from),
            )),
            Err(e) => steps.push(step("hall notify test".into(), "failed", Some(e.message))),
        }
    }

    // 6. Check everything.
    if !a.dry_run {
        match super::doctor::doctor(cwd) {
            Ok(r) => {
                let failing: Vec<String> = r.value["checks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|c| c["ok"] == false)
                    .filter_map(|c| c["name"].as_str().map(String::from))
                    .collect();
                for f in r.value["next"].as_array().into_iter().flatten() {
                    if let Some(f) = f.as_str() {
                        next.push(f.to_string());
                    }
                }
                steps.push(step(
                    "hall doctor".into(),
                    if failing.is_empty() { "done" } else { "warn" },
                    (!failing.is_empty()).then(|| format!("not yet: {}", failing.join(", "))),
                ));
            }
            Err(e) => steps.push(step("hall doctor".into(), "failed", Some(e.message))),
        }
    }

    next.push("restart Claude Code to load the plugin".into());
    next.push("hall new \"<request>\"".into());
    next.dedup();
    let out = SetupOut {
        target: "all".into(),
        marketplace_dir: paths::display(&user_dir().join("claude-plugin")),
        hall: shown,
        dry_run: a.dry_run,
        plugin_files,
        statusline,
        steps,
    };
    Ok(Response::new(out, next))
}

/// Makes sure the shell finds this `hall`. Hooks and the daemon use its
/// absolute path, but you type `hall`: `cargo install` puts it in
/// `~/.cargo/bin`, which isn't on PATH on a machine whose Rust comes from
/// Nix rather than rustup. Links it into `~/.local/bin` when that is on
/// PATH, or says which line to add to the shell profile.
fn path_step(exe: &Path, dry_run: bool) -> Step {
    let command = "put hall on PATH".to_string();
    let found = crate::gh::which("hall").map(|p| paths::normalize(&p));
    if found.as_deref() == Some(exe) {
        return step(command, "done", Some("already on PATH".into()));
    }
    let dir = exe.parent().map(paths::display).unwrap_or_default();
    let profile_line = format!("export PATH=\"{dir}:$PATH\"");
    #[cfg(unix)]
    if let Some(bin) = local_bin_on_path() {
        let link = bin.join("hall");
        let ours = std::fs::read_link(&link).is_ok_and(|t| paths::normalize(&t) == exe);
        if link.exists() && !ours && std::fs::symlink_metadata(&link).is_ok_and(|m| !m.is_symlink())
        {
            return step(
                command,
                "warn",
                Some(format!(
                    "{} is another hall; remove it, or add `{profile_line}` to your shell profile",
                    paths::display(&link)
                )),
            );
        }
        let what = format!("link {} → {}", paths::display(&link), paths::display(exe));
        if dry_run {
            return step(command, "planned", Some(what));
        }
        let _ = std::fs::remove_file(&link);
        return match std::fs::create_dir_all(&bin)
            .and_then(|_| std::os::unix::fs::symlink(exe, &link))
        {
            Ok(()) => step(command, "done", Some(what)),
            Err(e) => step(
                command,
                "warn",
                Some(format!(
                    "could not {what}: {e}; add `{profile_line}` to your shell profile"
                )),
            ),
        };
    }
    let _ = dry_run;
    step(
        command,
        "warn",
        Some(match found {
            Some(other) => format!(
                "`hall` on PATH is {}, not this one; add `{profile_line}` to your shell profile",
                paths::display(&other)
            ),
            None => format!("{dir} isn't on PATH; add `{profile_line}` to your shell profile"),
        }),
    )
}

/// `~/.local/bin` when PATH lists it: the usual home for user programs.
#[cfg(unix)]
fn local_bin_on_path() -> Option<PathBuf> {
    let bin = dirs::home_dir()?.join(".local").join("bin");
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .any(|p| p == bin)
        .then_some(bin)
}

/// True when a daemon runs but started before this binary was built, so it
/// runs an older hall.
fn daemon_is_stale(exe: &Path) -> bool {
    if !crate::daemon::state::instance_running() {
        return false;
    }
    let started = crate::daemon::state::load().and_then(|s| s.started_at);
    let built = std::fs::metadata(exe)
        .and_then(|m| m.modified())
        .ok()
        .map(chrono::DateTime::<chrono::Utc>::from);
    matches!((started, built), (Some(s), Some(b)) if b > s)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: &str = "/opt/hall/bin/hall statusline";

    #[test]
    fn inserts_after_the_last_member_keeping_order_and_style() {
        let text =
            "{\n  \"model\": \"opus\",\n  \"env\": {\"A\": \"1\"},\n  \"alpha\": [1, 2]\n}\n";
        let got = merge_status_line(text, CMD).unwrap();
        assert_eq!(
            got,
            "{\n  \"model\": \"opus\",\n  \"env\": {\"A\": \"1\"},\n  \"alpha\": [1, 2],\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"/opt/hall/bin/hall statusline\"\n  }\n}\n"
        );
    }

    #[test]
    fn replaces_in_place() {
        let text =
            "{\n    \"statusLine\": {\"type\": \"command\", \"command\": \"x\"},\n    \"z\": 1\n}";
        let got = merge_status_line(text, CMD).unwrap();
        assert!(got.starts_with("{\n    \"statusLine\": {\n        \"type\": \"command\",\n"));
        assert!(got.ends_with("\n    },\n    \"z\": 1\n}"), "{got}");
        let v: Value = serde_json::from_str(&got).unwrap();
        assert_eq!(v["statusLine"]["command"], CMD);
    }

    #[test]
    fn minified_and_empty_objects() {
        let got = merge_status_line(r#"{"a":"}\"{","b":null}"#, CMD).unwrap();
        assert_eq!(
            got,
            r#"{"a":"}\"{","b":null,"statusLine":{"type":"command","command":"/opt/hall/bin/hall statusline"}}"#
        );
        let got = merge_status_line(" {} \n", CMD).unwrap();
        let v: Value = serde_json::from_str(&got).unwrap();
        assert_eq!(v["statusLine"]["type"], "command");
        assert!(got.ends_with("} \n"));
        assert_eq!(merge_status_line("[1]", CMD), None);
    }

    #[test]
    fn recognises_its_own_status_line() {
        assert!(is_hall_statusline("hall statusline"));
        assert!(is_hall_statusline("/old/place/hall statusline"));
        assert!(is_hall_statusline(
            r#""C:\Program Files\hall\hall.exe" statusline"#
        ));
        assert!(!is_hall_statusline("~/.claude/statusline.sh"));
        assert!(!is_hall_statusline("npx ccstatusline"));
        assert!(!is_hall_statusline("hall status"));
    }

    #[test]
    fn quotes_paths_with_spaces() {
        assert_eq!(
            statusline_command(Path::new("/opt/hall bin/hall")),
            "\"/opt/hall bin/hall\" statusline"
        );
        assert_eq!(
            statusline_command(Path::new("/usr/bin/hall")),
            "/usr/bin/hall statusline"
        );
    }
}
