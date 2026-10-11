//! Records the commit `hall` is built from, for `hall --version` and
//! `hall update`: `HALL_BUILD_COMMIT` when set (`hall update` sets it), else
//! the checkout's HEAD. Builds without git (a crate tarball, a Nix sandbox)
//! have no commit.

use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=HALL_BUILD_COMMIT");
    let commit = std::env::var("HALL_BUILD_COMMIT")
        .ok()
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .or_else(head);
    let version = env!("CARGO_PKG_VERSION");
    let long = match &commit {
        Some(c) => format!("{version} ({})", &c[..c.len().min(7)]),
        None => version.to_string(),
    };
    println!("cargo:rustc-env=HALL_COMMIT={}", commit.unwrap_or_default());
    println!("cargo:rustc-env=HALL_VERSION={long}");
}

/// HEAD of the checkout being built, rebuilding when it moves.
fn head() -> Option<String> {
    let dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let dir = Path::new(&dir);
    let git_dir = git(dir, &["rev-parse", "--absolute-git-dir"])?;
    let common = git(dir, &["rev-parse", "--git-common-dir"])?;
    let common = dir.join(common);
    let mut watch = vec![Path::new(&git_dir).join("HEAD"), common.join("packed-refs")];
    if let Some(branch) = git(dir, &["symbolic-ref", "-q", "HEAD"]) {
        watch.push(common.join(branch));
    }
    // A path that doesn't exist would rerun this script on every build.
    for p in watch.iter().filter(|p| p.exists()) {
        println!("cargo:rerun-if-changed={}", p.display());
    }
    git(dir, &["rev-parse", "HEAD"])
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(args);
    for v in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
        c.env_remove(v);
    }
    let out = c.output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}
