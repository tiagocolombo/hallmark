//! Drives the real `hall` binary against temporary git repositories.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command as Std;

use assert_cmd::Command;
use serde_json::Value;

pub struct Env {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
    /// The main checkout, on branch `main`, with one commit.
    pub repo: PathBuf,
    pub home: PathBuf,
}

pub struct Reply {
    pub json: Value,
    pub code: i32,
    pub stdout: String,
}

impl Env {
    pub fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let repo = root.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README.md"), "# app\n").unwrap();
        std::fs::write(repo.join(".gitignore"), "*.log\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        let home = root.join("home");
        Env {
            _dir: dir,
            root,
            repo,
            home,
        }
    }

    /// A repo that has run `hall init`, with its init files committed.
    pub fn initialized() -> Env {
        let env = Env::new();
        let r = env.hall(&env.repo, &["init"]);
        assert_eq!(r.code, 0, "{}", r.stdout);
        git(&env.repo, &["add", "."]);
        git(&env.repo, &["commit", "-q", "-m", "Adopt Hallmark"]);
        env
    }

    pub fn cmd(&self, dir: &Path) -> Command {
        self.cmd_exe(Path::new(env!("CARGO_BIN_EXE_hall")), dir)
    }

    /// [`Env::cmd`] for another copy of `hall`, such as an installed one.
    pub fn cmd_exe(&self, exe: &Path, dir: &Path) -> Command {
        let mut c = Command::new(exe);
        c.current_dir(dir)
            .env("HALL_HOME", &self.home)
            .env("HALL_ACTOR", "you")
            .env("HALL_CLAUDE", self.root.join("no-claude-here"))
            // No real `gh` either: tests that need one pass a fake in HALL_GH.
            .env("HALL_GH", self.root.join("no-gh-here"))
            // Desktop notifications go to a file, never to the screen.
            .env("HALL_NOTIFY_LOG", self.root.join("notify.log"))
            .env_remove("HALL_RUNNER")
            .env_remove("HALL_NO_HOOKS")
            .env_remove("HALL_HEADLESS")
            .env_remove("HALL_ACTOR")
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_SESSION_ID")
            .env_remove("CLAUDE_CODE_SESSION_ID");
        for (k, v) in git_identity() {
            c.env(k, v);
        }
        c
    }

    pub fn hall(&self, dir: &Path, args: &[&str]) -> Reply {
        let out = self.cmd(dir).args(args).output().unwrap();
        reply(out)
    }

    /// Writes a fake-runner script (ADR 0013) and returns its path. Pass it
    /// to [`Env::hall_fake`].
    pub fn script(&self, steps: Value) -> PathBuf {
        let p = self.root.join("script.json");
        std::fs::write(&p, serde_json::json!({ "steps": steps }).to_string()).unwrap();
        let _ = std::fs::remove_file(self.root.join("script.state.json"));
        p
    }

    /// Runs `hall` with the fake runner replaying `script`.
    pub fn hall_fake(&self, dir: &Path, script: &Path, args: &[&str]) -> Reply {
        let out = self
            .cmd(dir)
            .env("HALL_RUNNER", format!("fake:{}", script.display()))
            .env("HALL_PLUGIN_DIR", self.root.join("plugin"))
            .args(args)
            .output()
            .unwrap();
        reply(out)
    }

    /// The requests the fake runner received, oldest first.
    pub fn fake_calls(&self) -> Vec<Value> {
        std::fs::read_to_string(self.root.join("script.calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// Runs `hall hook <event>` with `input` on stdin.
    pub fn hook(&self, event: &str, input: &Value) -> Reply {
        let out = self
            .cmd(&self.repo)
            .args(["hook", event])
            .write_stdin(input.to_string())
            .output()
            .unwrap();
        reply(out)
    }

    /// Creates an item and returns (id, worktree).
    pub fn new_item(&self, title: &str) -> (String, PathBuf) {
        let r = self.hall(&self.repo, &["new", "--title", title]);
        assert_eq!(r.code, 0, "{}", r.stdout);
        let id = r.json["id"].as_str().unwrap().to_string();
        let wt = PathBuf::from(r.json["worktree"].as_str().unwrap());
        (id, wt)
    }

    pub fn journal(&self, id: &str) -> Vec<Value> {
        let p = self
            .repo
            .join(".git/hallmark/items")
            .join(id)
            .join("events.jsonl");
        std::fs::read_to_string(p)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    pub fn repo_journal(&self) -> Vec<Value> {
        let p = self.repo.join(".git/hallmark/events.jsonl");
        std::fs::read_to_string(p)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

pub fn reply(out: std::process::Output) -> Reply {
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let json = serde_json::from_str(stdout.trim()).unwrap_or(Value::Null);
    Reply {
        json,
        code: out.status.code().unwrap_or(-1),
        stdout,
    }
}

fn git_identity() -> [(&'static str, &'static str); 4] {
    [
        ("GIT_AUTHOR_NAME", "Hallmark Test"),
        ("GIT_AUTHOR_EMAIL", "test@hallmark.invalid"),
        ("GIT_COMMITTER_NAME", "Hallmark Test"),
        ("GIT_COMMITTER_EMAIL", "test@hallmark.invalid"),
    ]
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Std::new("git")
        .current_dir(dir)
        .args(args)
        .envs(git_identity())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

pub fn commit_file(dir: &Path, name: &str, body: &str) -> String {
    std::fs::write(dir.join(name), body).unwrap();
    git(dir, &["add", name]);
    git(dir, &["commit", "-q", "-m", &format!("change {name}")]);
    git(dir, &["rev-parse", "HEAD"])
}

pub fn fixture(rel: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{rel}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

pub fn p(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
