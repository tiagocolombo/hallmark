//! `hall update`: the latest commit on main, read from a local source
//! repository, and a fake `cargo` that logs what it was asked to install.
//! Nothing is built or fetched from the network.

mod common;

use std::path::{Path, PathBuf};

use common::{Env, Reply, git, p, reply};

/// A bare repository whose `main` is the test repo's one commit, and that
/// commit. git takes a path where cargo would need a URL; the fake cargo
/// doesn't mind.
fn source(env: &Env) -> (String, String) {
    let bare = env.root.join("source.git");
    git(
        &env.root,
        &["clone", "-q", "--bare", &p(&env.repo), &p(&bare)],
    );
    let sha = git(&bare, &["rev-parse", "main"]).trim().to_string();
    (p(&bare), sha)
}

/// A copy of `hall` installed at `<root>/inst/bin`, as cargo would put it.
fn installed(env: &Env) -> PathBuf {
    let bin = env.root.join("inst").join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = bin.join(Path::new(env!("CARGO_BIN_EXE_hall")).file_name().unwrap());
    std::fs::copy(env!("CARGO_BIN_EXE_hall"), &exe).unwrap();
    exe
}

/// A fake `cargo` that appends its arguments and environment to
/// `<root>/cargo.log` and, when it succeeds, writes a new `hall` at `exe`.
fn fake_cargo(env: &Env, exe: &Path, exit: i32) -> PathBuf {
    let log = env.root.join("cargo.log");
    let real = env!("CARGO_BIN_EXE_hall");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let fake = env.root.join("cargo");
        let install = if exit == 0 {
            // A new file, renamed into place, as cargo does: a running
            // program can't be written to.
            format!(
                "cp '{real}' '{e}.new' && mv '{e}.new' '{e}'\n",
                e = exe.display()
            )
        } else {
            String::new()
        };
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\necho \"$@\" >> '{l}'\n\
                 echo \"HALL_BUILD_COMMIT=$HALL_BUILD_COMMIT \
                 CARGO_NET_GIT_FETCH_WITH_CLI=$CARGO_NET_GIT_FETCH_WITH_CLI\" >> '{l}'\n\
                 {install}exit {exit}\n",
                l = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        fake
    }
    #[cfg(windows)]
    {
        let fake = env.root.join("cargo.cmd");
        let install = if exit == 0 {
            format!("@copy /y \"{real}\" \"{}\" > nul\r\n", exe.display())
        } else {
            String::new()
        };
        std::fs::write(
            &fake,
            format!(
                "@echo %* >> \"{l}\"\r\n\
                 @echo HALL_BUILD_COMMIT=%HALL_BUILD_COMMIT% \
                 CARGO_NET_GIT_FETCH_WITH_CLI=%CARGO_NET_GIT_FETCH_WITH_CLI% >> \"{l}\"\r\n\
                 {install}@exit /b {exit}\r\n",
                l = log.display()
            ),
        )
        .unwrap();
        fake
    }
}

/// A `claude` that does nothing and succeeds.
fn noop(env: &Env) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let fake = env.root.join("claude");
        std::fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        fake
    }
    #[cfg(windows)]
    {
        let fake = env.root.join("claude.cmd");
        std::fs::write(&fake, "@exit /b 0\r\n").unwrap();
        fake
    }
}

fn cargo_log(env: &Env) -> Option<String> {
    std::fs::read_to_string(env.root.join("cargo.log"))
        .ok()
        .map(|s| s.replace('\\', "/"))
}

/// Runs `exe update <args>` with the fake cargo.
fn update(env: &Env, exe: &Path, cargo: &Path, args: &[&str]) -> Reply {
    let out = env
        .cmd_exe(exe, &env.root)
        .env("HALL_CARGO", cargo)
        .env_remove("HALL_UPDATE_SOURCE")
        .arg("update")
        .args(args)
        .output()
        .unwrap();
    reply(out)
}

#[test]
fn check_reports_an_update_without_installing_it() {
    let env = Env::new();
    let (src, sha) = source(&env);
    let exe = installed(&env);
    let cargo = fake_cargo(&env, &exe, 0);
    let r = update(&env, &exe, &cargo, &["--check", "--source", &src]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["status"], "available");
    assert_eq!(r.json["latest"], sha.as_str());
    assert_eq!(r.json["branch"], "main");
    let next = r.json["next"][0].as_str().unwrap();
    assert!(next.starts_with("hall update --source "), "{next}");
    assert!(next.contains(&src), "{next}");
    assert_eq!(cargo_log(&env), None, "--check must not install");
}

#[test]
fn installs_the_latest_main_over_the_running_copy() {
    let env = Env::new();
    let (src, sha) = source(&env);
    let exe = installed(&env);
    let cargo = fake_cargo(&env, &exe, 0);
    let r = update(&env, &exe, &cargo, &["--source", &src]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["status"], "updated");
    assert_eq!(r.json["latest"], sha.as_str());

    let log = cargo_log(&env).expect("cargo ran");
    let root = p(&env.root.join("inst"));
    assert!(
        log.contains(&format!(
            "install --locked --force --git {src} --rev {sha} --root {root}"
        )),
        "{log}"
    );
    // The new binary is told its commit, and cargo fetches with git.
    assert!(log.contains(&format!("HALL_BUILD_COMMIT={sha}")), "{log}");
    assert!(log.contains("CARGO_NET_GIT_FETCH_WITH_CLI=true"), "{log}");

    let steps: Vec<(&str, &str)> = r.json["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["command"].as_str().unwrap(),
                s["status"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(steps[0].1, "done", "{steps:?}");
    assert!(steps[0].0.starts_with("cargo install --git"), "{steps:?}");
    // No plugin was installed and no daemon runs here: nothing to refresh.
    assert_eq!(steps[1], ("hall setup claude", "skipped"));
    assert_eq!(steps[2], ("restart the daemon", "skipped"));
    assert!(exe.is_file());
}

#[test]
fn refreshes_an_installed_plugin_with_the_new_hall() {
    let env = Env::new();
    let (src, _) = source(&env);
    let exe = installed(&env);
    let cargo = fake_cargo(&env, &exe, 0);
    std::fs::create_dir_all(env.home.join("claude-plugin")).unwrap();
    let out = env
        .cmd_exe(&exe, &env.root)
        .env("HALL_CARGO", &cargo)
        .env("HALL_CLAUDE", noop(&env))
        .args(["update", "--source", &src])
        .output()
        .unwrap();
    let r = reply(out);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["steps"][1]["command"], "hall setup claude");
    assert_eq!(r.json["steps"][1]["status"], "done", "{}", r.stdout);
    // The hooks it wrote call the installed hall.
    let hooks = std::fs::read_to_string(
        env.home
            .join("claude-plugin/plugins/hallmark/hooks/hooks.json"),
    )
    .unwrap()
    .replace("\\\\", "/");
    assert!(hooks.contains(&p(&exe)), "{hooks}");
}

#[test]
fn skips_and_says_so_when_already_the_latest() {
    let env = Env::new();
    let exe = installed(&env);
    let cargo = fake_cargo(&env, &exe, 0);
    // The commit this hall was built from, as main of a source repo.
    let (src, _) = source(&env);
    let r = update(&env, &exe, &cargo, &["--check", "--source", &src]);
    let Some(built) = r.json["installed"].as_str() else {
        eprintln!("this hall was built without git; skipping");
        return;
    };
    let bare = env.root.join("built.git");
    git(&env.root, &["init", "-q", "--bare", &p(&bare)]);
    git(
        &bare,
        &[
            "fetch",
            "-q",
            env!("CARGO_MANIFEST_DIR"),
            &format!("{built}:refs/heads/main"),
        ],
    );
    let src = p(&bare);

    let r = update(&env, &exe, &cargo, &["--source", &src]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json["status"], "up_to_date");
    assert_eq!(r.json["installed"], r.json["latest"]);
    assert_eq!(r.json["steps"], serde_json::json!([]));
    assert_eq!(r.json["next"], serde_json::json!(["hall doctor"]));
    assert_eq!(cargo_log(&env), None, "nothing to install");
}

#[test]
fn a_failed_build_leaves_the_installed_hall_alone() {
    let env = Env::new();
    let (src, _) = source(&env);
    let exe = installed(&env);
    let before = std::fs::read(&exe).unwrap();
    let cargo = fake_cargo(&env, &exe, 101);
    let r = update(&env, &exe, &cargo, &["--source", &src]);
    assert_eq!(r.code, 1, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "COMMAND_FAILED");
    let msg = r.json["error"]["message"].as_str().unwrap();
    assert!(msg.contains("cargo install failed"), "{msg}");
    assert_eq!(std::fs::read(&exe).unwrap(), before);
}

#[test]
fn an_unknown_branch_is_an_error() {
    let env = Env::new();
    let (src, _) = source(&env);
    let exe = installed(&env);
    let cargo = fake_cargo(&env, &exe, 0);
    let r = update(&env, &exe, &cargo, &["--source", &src, "--branch", "nope"]);
    assert_eq!(r.code, 1, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "COMMAND_FAILED");
    let msg = r.json["error"]["message"].as_str().unwrap();
    assert!(msg.contains("has no branch nope"), "{msg}");
    assert_eq!(cargo_log(&env), None);
}

#[test]
fn an_unreachable_source_is_an_error() {
    let env = Env::new();
    let exe = installed(&env);
    let cargo = fake_cargo(&env, &exe, 0);
    let missing = p(&env.root.join("missing.git"));
    let r = update(&env, &exe, &cargo, &["--source", &missing]);
    assert_eq!(r.code, 1, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "COMMAND_FAILED");
    assert!(
        r.json["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("could not read main from"),
        "{}",
        r.stdout
    );
}

#[test]
fn a_build_directory_is_not_updated_in_place() {
    let env = Env::new();
    let (src, _) = source(&env);
    let cargo = fake_cargo(&env, Path::new(env!("CARGO_BIN_EXE_hall")), 0);
    let r = update(
        &env,
        Path::new(env!("CARGO_BIN_EXE_hall")),
        &cargo,
        &["--source", &src],
    );
    assert_eq!(r.code, 1, "{}", r.stdout);
    assert_eq!(r.json["error"]["code"], "COMMAND_FAILED");
    assert_eq!(cargo_log(&env), None);
}
