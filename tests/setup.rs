//! `hall setup claude`: the plugin install plan and `--statusline`
//! (docs/decisions/0009, 0023). No real `claude` runs here.

mod common;

use std::path::{Path, PathBuf};

use common::{Env, Reply, p};
use insta::assert_json_snapshot;
use serde_json::{Value, json};

/// Replaces the temp dir with `[root]` and the binary with `[hall]`, with
/// `/` separators, so the snapshot is the same on every OS.
fn scrub(env: &Env, v: &Value) -> Value {
    let hall = p(&dunce::canonicalize(env!("CARGO_BIN_EXE_hall")).unwrap());
    match v {
        Value::String(s) => Value::String(
            s.replace('\\', "/")
                .replace(&hall, "[hall]")
                .replace(&p(&env.root), "[root]"),
        ),
        Value::Array(a) => Value::Array(a.iter().map(|x| scrub(env, x)).collect()),
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, x)| (k.clone(), scrub(env, x))).collect())
        }
        other => other.clone(),
    }
}

fn settings(env: &Env) -> PathBuf {
    env.root.join("claude-home").join("settings.json")
}

/// A fake `claude` that appends its arguments to `<root>/claude.log` and
/// succeeds: a shell script, or a batch file on Windows.
fn fake_claude(env: &Env) -> PathBuf {
    let log = env.root.join("claude.log");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let fake = env.root.join("claude");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\necho \"$@\" >> '{}'\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        fake
    }
    #[cfg(windows)]
    {
        let fake = env.root.join("claude.cmd");
        std::fs::write(
            &fake,
            format!("@echo %* >> \"{}\"\r\n@exit /b 0\r\n", log.display()),
        )
        .unwrap();
        fake
    }
}

fn setup(env: &Env, args: &[&str]) -> Reply {
    let out = env
        .cmd(&env.root)
        .env("HALL_CLAUDE", fake_claude(env))
        .env("HALL_CLAUDE_SETTINGS", settings(env))
        .args(["setup", "claude"])
        .args(args)
        .output()
        .unwrap();
    reply(out)
}

fn reply(out: std::process::Output) -> Reply {
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    Reply {
        json: serde_json::from_str(stdout.trim()).unwrap_or(Value::Null),
        code: out.status.code().unwrap_or(-1),
        stdout,
    }
}

fn hall_statusline() -> String {
    let exe = dunce::canonicalize(env!("CARGO_BIN_EXE_hall")).unwrap();
    let exe = exe.to_string_lossy();
    if exe.contains(' ') {
        format!("\"{exe}\" statusline")
    } else {
        format!("{exe} statusline")
    }
}

/// Every file under `plugin/`, as `plugins/hallmark/<path>`.
fn plugin_files_on_disk() -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let path = e.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                let rel = path.strip_prefix(root).unwrap();
                out.push(format!("plugins/hallmark/{}", p(rel)));
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin");
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out
}

#[test]
fn dry_run_plan() {
    let env = Env::new();
    // The default HALL_CLAUDE from `Env`: a dry run never runs it.
    let out = env
        .cmd(&env.root)
        .env("HALL_CLAUDE_SETTINGS", settings(&env))
        .args(["setup", "claude", "--dry-run", "--statusline"])
        .output()
        .unwrap();
    let r = reply(out);
    assert_eq!(r.code, 0, "{}", r.stdout);
    // Every plugin file (agents, skills, schemas, hooks) is extracted.
    let listed: Vec<String> = r.json["plugin_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    for f in plugin_files_on_disk() {
        assert!(listed.contains(&f), "{f} missing from plugin_files");
    }
    assert!(listed.contains(&".claude-plugin/marketplace.json".to_string()));
    // Nothing was written.
    assert!(!env.home.join("claude-plugin").exists());
    assert!(!settings(&env).exists());

    let mut v = scrub(&env, &r.json);
    // Grows as the plugin grows; checked above.
    v["plugin_files"] = json!("[files]");
    assert_json_snapshot!("setup_claude_dry_run", v);
}

#[test]
fn statusline_merges_and_is_idempotent() {
    let env = Env::new();
    let path = settings(&env);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let before = "{\n  \"model\": \"opus\",\n  \"permissions\": {\n    \"allow\": [\"Bash(ls:*)\"]\n  },\n  \"alwaysThinkingEnabled\": true\n}\n";
    std::fs::write(&path, before).unwrap();

    let r = setup(&env, &["--statusline"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let after = std::fs::read_to_string(&path).unwrap();
    let v: Value = serde_json::from_str(&after).unwrap();
    assert_eq!(
        v["statusLine"],
        json!({ "type": "command", "command": hall_statusline() })
    );
    // The rest is preserved byte for byte, key order included.
    assert!(
        after.starts_with(before.trim_end().trim_end_matches('}').trim_end()),
        "{after}"
    );
    assert_eq!(v["model"], "opus");
    assert_eq!(v["permissions"]["allow"][0], "Bash(ls:*)");
    assert_eq!(r.json["statusline"]["command"], hall_statusline());
    let st = step(&r.json, "set statusLine");
    assert_eq!(st["status"], "done");

    // Again: nothing to do, file untouched.
    let r = setup(&env, &["--statusline"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(step(&r.json, "set statusLine")["status"], "skipped");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), after);
}

#[test]
fn statusline_creates_the_settings_file() {
    let env = Env::new();
    let r = setup(&env, &["--statusline"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let v: Value = serde_json::from_str(&std::fs::read_to_string(settings(&env)).unwrap()).unwrap();
    assert_eq!(
        v,
        json!({ "statusLine": { "type": "command", "command": hall_statusline() } })
    );
}

#[test]
fn statusline_refuses_someone_elses_without_force() {
    let env = Env::new();
    let path = settings(&env);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mine =
        r#"{"statusLine":{"type":"command","command":"~/.claude/my-line.sh"},"theme":"dark"}"#;
    std::fs::write(&path, mine).unwrap();

    let r = setup(&env, &["--statusline"]);
    assert_eq!(r.code, 2, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "USAGE");
    assert_eq!(
        r.json["error"]["next"][0],
        "hall setup claude --statusline --force"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), mine);
    // A refusal changes nothing else either.
    assert!(!env.home.join("claude-plugin").exists());

    // --dry-run refuses the same way.
    let r = setup(&env, &["--statusline", "--dry-run"]);
    assert_eq!(r.code, 2, "{}", r.stdout);

    let r = setup(&env, &["--statusline", "--force", "--dry-run"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let st = step(&r.json, "set statusLine");
    assert_eq!(st["status"], "planned");
    assert_eq!(st["detail"], "replaces `~/.claude/my-line.sh`");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), mine);

    let r = setup(&env, &["--statusline", "--force"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["statusLine"]["command"], hall_statusline());
    assert_eq!(v["theme"], "dark");
}

#[test]
fn statusline_replaces_a_stale_hall_path() {
    let env = Env::new();
    let path = settings(&env);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"{"statusLine":{"type":"command","command":"/old/place/hall statusline"}}"#,
    )
    .unwrap();
    let r = setup(&env, &["--statusline"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["statusLine"]["command"], hall_statusline());
}

#[test]
fn statusline_leaves_invalid_settings_alone() {
    let env = Env::new();
    let path = settings(&env);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{ not json").unwrap();
    let r = setup(&env, &["--statusline"]);
    assert_eq!(r.code, 1, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "INTERNAL");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
}

#[test]
fn force_needs_statusline() {
    let env = Env::new();
    let r = setup(&env, &["--force"]);
    assert_eq!(r.code, 2, "{}", r.stdout);
}

/// With a fake `claude`, setup runs the ADR 0009 sequence, extracts every
/// plugin file, drops stale ones and sets the status line.
#[test]
fn installs_with_a_fake_claude() {
    let env = Env::new();
    // A file an older plugin had and this one doesn't.
    let stale = env
        .home
        .join("claude-plugin/plugins/hallmark/skills/gone/SKILL.md");
    std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
    std::fs::write(&stale, "old").unwrap();

    let r = setup(&env, &["--statusline"]);
    assert_eq!(r.code, 0, "{}", r.stdout);

    let dir = env.home.join("claude-plugin");
    let calls: Vec<String> = std::fs::read_to_string(env.root.join("claude.log"))
        .unwrap()
        .lines()
        .map(|l| l.trim().to_string())
        .collect();
    assert_eq!(calls.len(), 4, "{calls:?}");
    #[cfg(unix)]
    assert_eq!(
        calls,
        vec![
            format!("plugin marketplace add {} --scope user", dir.display()),
            "plugin marketplace update hallmark".into(),
            "plugin install hallmark@hallmark --scope user".into(),
            "plugin update hallmark@hallmark --scope user".into(),
        ]
    );
    for f in plugin_files_on_disk() {
        assert!(dir.join(&f).is_file(), "{f} was not extracted");
    }
    assert!(!stale.exists(), "a stale plugin file survived");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(settings(&env)).unwrap()).unwrap();
    assert_eq!(v["statusLine"]["command"], hall_statusline());
    let next: Vec<&str> = r.json["next"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap())
        .collect();
    assert_eq!(
        &next[..3],
        [
            "hall init",
            "hall daemon start --at-login",
            "hall notify test"
        ]
    );
}

fn step<'a>(reply: &'a Value, prefix: &str) -> &'a Value {
    reply["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["command"].as_str().unwrap().starts_with(prefix))
        .unwrap_or_else(|| panic!("no step {prefix}: {reply}"))
}

#[test]
fn setup_without_a_target_plans_every_step() {
    let env = Env::initialized();
    let r = env.hall(&env.repo, &["setup", "--dry-run"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["target"], "all");
    let steps: Vec<(String, String)> = r.json["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["command"].as_str().unwrap().to_string(),
                s["status"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let has =
        |cmd: &str, status: &str| steps.iter().any(|(c, s)| c.starts_with(cmd) && s == status);
    assert!(
        has("hall setup claude --statusline", "planned"),
        "{steps:?}"
    );
    assert!(has("hall init", "planned"), "{steps:?}");
    assert!(has("hall daemon start --at-login", "planned"), "{steps:?}");
    assert!(has("hall notify test", "planned"), "{steps:?}");
    // Off a terminal the wizard doesn't ask, and the user config is planned.
    assert!(has("ask about every setting", "skipped"), "{steps:?}");
    assert!(has("write ", "planned"), "{steps:?}");
    // A dry run changes nothing: no plugin written, no daemon state, no
    // user config.
    assert!(!env.home.join("claude-plugin").exists());
    assert!(!env.home.join("daemon").join("state.json").exists());
    assert!(!env.home.join("config.toml").exists());

    let r = env.hall(
        &env.repo,
        &[
            "setup",
            "--dry-run",
            "--no-daemon",
            "--no-notify",
            "--no-statusline",
        ],
    );
    let steps = r.json["steps"].as_array().unwrap();
    let status = |cmd: &str| {
        steps
            .iter()
            .find(|s| s["command"].as_str().unwrap().starts_with(cmd))
            .map(|s| s["status"].as_str().unwrap().to_string())
    };
    assert_eq!(status("hall daemon").as_deref(), Some("skipped"));
    assert_eq!(status("hall notify").as_deref(), Some("skipped"));
    assert_eq!(status("hall setup claude").as_deref(), Some("planned"));
    assert!(
        !steps
            .iter()
            .any(|s| s["command"] == "hall setup claude --statusline")
    );
}

/// `hall setup` writes the user config with the defaults once, and
/// `--defaults` is how a script says it doesn't want the wizard (ADR 0055).
#[test]
fn setup_writes_the_user_config_once() {
    let env = Env::initialized();
    let user_file = env.home.join("config.toml");
    let run = || {
        let out = env
            .cmd(&env.repo)
            // No real home: nothing is linked into ~/.local/bin.
            .env("HOME", &env.root)
            .args([
                "setup",
                "--defaults",
                "--no-daemon",
                "--no-notify",
                "--no-statusline",
            ])
            .output()
            .unwrap();
        reply(out)
    };
    let first = run();
    assert_eq!(first.code, 0, "{}", first.stdout);
    let status = |r: &Reply, cmd: &str| {
        r.json["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["command"].as_str().unwrap().starts_with(cmd))
            .map(|s| s["status"].as_str().unwrap().to_string())
    };
    assert_eq!(
        status(&first, "ask about every setting").as_deref(),
        Some("skipped")
    );
    assert_eq!(status(&first, "write ").as_deref(), Some("done"));
    let text = std::fs::read_to_string(&user_file).unwrap();
    assert!(text.contains("desktop = true"), "{text}");
    // The file is the defaults: Hallmark reads it as if it weren't there.
    let r = env.hall(&env.repo, &["config", "list", "notify"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    for e in r.json["entries"].as_array().unwrap() {
        let source = e["source"].as_str().unwrap();
        assert!(
            source == "default" || (e["key"] == "notify.desktop" && source == "user"),
            "{e}"
        );
    }

    let second = run();
    assert_eq!(status(&second, "write ").as_deref(), Some("skipped"));
    assert_eq!(std::fs::read_to_string(&user_file).unwrap(), text);

    let r = env.hall(&env.repo, &["setup", "--interactive", "--defaults"]);
    assert_eq!(r.code, 2, "{}", r.stdout);
}

#[cfg(unix)]
#[test]
fn setup_puts_hall_on_path_through_local_bin() {
    let env = Env::initialized();
    let home = env.root.join("user");
    let bin = home.join(".local").join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // An installed-looking copy, outside any target/ directory.
    let installed = env.root.join("cargo-bin").join("hall");
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    // A hard link, not a copy: a copy is open for writing until it's done,
    // and on Linux a process another test thread forks meanwhile inherits
    // that handle, so running the copy fails with "Text file busy". A link
    // writes nothing. Across filesystems it has to be a copy, and `spawn`
    // below retries while the file is busy.
    if std::fs::hard_link(env!("CARGO_BIN_EXE_hall"), &installed).is_err() {
        std::fs::copy(env!("CARGO_BIN_EXE_hall"), &installed).unwrap();
    }
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let run = |args: &[&str]| {
        let mut tries = 0;
        let out = loop {
            let r = std::process::Command::new(&installed)
                .current_dir(&env.repo)
                .env("HOME", &home)
                .env("PATH", &path)
                .env("HALL_HOME", &env.home)
                .env("HALL_CLAUDE", env.root.join("no-claude-here"))
                .args(args)
                .output();
            match r {
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 50 => {
                    tries += 1;
                    hallmark::daemon::nap(std::time::Duration::from_millis(20));
                }
                r => break r.unwrap(),
            }
        };
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()
    };
    let step = |v: &serde_json::Value| {
        v["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["command"] == "put hall on PATH")
            .cloned()
            .unwrap()
    };
    let planned = step(&run(&["--json", "setup", "--dry-run"]));
    assert_eq!(planned["status"], "planned", "{planned}");
    assert!(!bin.join("hall").exists(), "a dry run links nothing");

    let done = step(&run(&[
        "--json",
        "setup",
        "--no-daemon",
        "--no-notify",
        "--no-statusline",
    ]));
    assert_eq!(done["status"], "done", "{done}");
    let target = std::fs::read_link(bin.join("hall")).unwrap();
    assert_eq!(
        dunce::canonicalize(target).unwrap(),
        dunce::canonicalize(&installed).unwrap()
    );
    // Once linked, setup and doctor see it on PATH.
    let again = step(&run(&["--json", "setup", "--dry-run"]));
    assert_eq!(again["detail"], "already on PATH", "{again}");
    let doctor = run(&["--json", "doctor"]);
    let check = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "path")
        .cloned()
        .unwrap();
    assert_eq!(check["ok"], true, "{check}");
}
