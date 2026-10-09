//! `hall setup claude`: install the plugin for Claude Code at user level,
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

#[derive(Debug, Serialize, JsonSchema)]
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

pub fn setup(a: SetupArgs) -> Result<Response> {
    let SetupTarget::Claude = a.target;
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
