//! `gh` as a subprocess. It may not be on PATH (on Nix machines it can live
//! only in the store), so it is searched for.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::error::{ErrorCode, HallError, Result};

/// `HALL_GH`, then PATH, then the newest `/nix/store/*-gh-*/bin/gh`.
pub fn find() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("HALL_GH") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    if let Some(p) = which("gh") {
        return Some(p);
    }
    nix_store_gh()
}

/// Searches PATH (with PATHEXT on Windows).
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(|e| e.to_ascii_lowercase())
            .chain(std::iter::once(String::new()))
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let p = dir.join(format!("{name}{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn nix_store_gh() -> Option<PathBuf> {
    let store = Path::new("/nix/store");
    let mut found: Vec<(Vec<u64>, PathBuf)> = std::fs::read_dir(store)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let version = name.split_once("-gh-")?.1;
            if !version.chars().next()?.is_ascii_digit() || name.ends_with(".drv") {
                return None;
            }
            let bin = e.path().join("bin/gh");
            bin.is_file().then(|| (parse_version(version), bin))
        })
        .collect();
    found.sort();
    found.pop().map(|(_, p)| p)
}

fn parse_version(v: &str) -> Vec<u64> {
    v.split(['.', '-']).map_while(|p| p.parse().ok()).collect()
}

fn gh() -> Result<PathBuf> {
    find().ok_or_else(|| {
        HallError::new(
            ErrorCode::GhUnavailable,
            "gh (the GitHub CLI) was not found",
        )
        .next([
            "install gh: https://cli.github.com",
            "set HALL_GH=/path/to/gh",
            "hall doctor",
        ])
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct GhIssue {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub url: Option<String>,
}

pub fn issue(dir: &Path, number: u64) -> Result<GhIssue> {
    let out = Command::new(gh()?)
        .current_dir(dir)
        .args([
            "issue",
            "view",
            &number.to_string(),
            "--json",
            "number,title,body,url",
        ])
        .output()?;
    if !out.status.success() {
        return Err(HallError::new(
            ErrorCode::GhUnavailable,
            format!(
                "gh issue view {number} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        )
        .next(["gh auth status", "hall doctor"]));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| {
        HallError::new(
            ErrorCode::GhUnavailable,
            format!("unreadable gh output: {e}"),
        )
    })
}

/// "logged in", "not logged in", or None when gh is missing.
pub fn auth_status() -> Option<(PathBuf, bool)> {
    let gh = find()?;
    let ok = Command::new(&gh)
        .args(["auth", "status"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    Some((gh, ok))
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions_sort_numerically() {
        assert!(super::parse_version("2.101.0") > super::parse_version("2.98.0"));
    }
}
