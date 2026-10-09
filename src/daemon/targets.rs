//! Which repositories the daemon serves: the registry that `hall init`
//! writes (`<user data dir>/hallmark/repos.json`), or one chosen with
//! `--repo`.

use std::path::{Path, PathBuf};

use crate::commands::init::Registry;
use crate::error::{ErrorCode, Result};
use crate::paths;
use crate::repo::{Repo, user_dir};

#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub name: String,
    /// The main checkout.
    pub path: PathBuf,
}

/// Why a registered repository was skipped.
#[derive(Debug, Clone, PartialEq)]
pub enum Skip {
    /// The directory is gone.
    Missing,
    /// It is no longer a git repository, or Hallmark isn't set up there.
    NotInitialized,
    Error(String),
}

impl Skip {
    pub fn result(&self) -> &'static str {
        match self {
            Skip::Missing => "missing",
            Skip::NotInitialized => "not_initialized",
            Skip::Error(_) => "error",
        }
    }

    pub fn note(&self) -> String {
        match self {
            Skip::Missing => "the directory no longer exists".into(),
            Skip::NotInitialized => "Hallmark isn't set up here (hall init)".into(),
            Skip::Error(e) => e.clone(),
        }
    }
}

pub fn registry_path() -> PathBuf {
    user_dir().join("repos.json")
}

/// Every registered repository, in registration order.
pub fn registered() -> Vec<Target> {
    let reg: Registry = std::fs::read_to_string(registry_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    reg.repos
        .into_iter()
        .map(|r| Target {
            name: r.name,
            path: PathBuf::from(r.path),
        })
        .collect()
}

/// `--repo`: a registered name, or a path inside a repository.
pub fn resolve(arg: &str) -> Result<Target> {
    if let Some(t) = registered().into_iter().find(|t| t.name == arg) {
        return Ok(t);
    }
    let repo = Repo::discover(Path::new(arg))?;
    Ok(Target {
        name: repo.name.clone(),
        path: repo.main_root.clone(),
    })
}

/// Opens a target for a pass, or says why it is skipped.
pub fn open(t: &Target) -> std::result::Result<Repo, Skip> {
    if !t.path.is_dir() {
        return Err(Skip::Missing);
    }
    match Repo::discover(&t.path) {
        Ok(r) if r.initialized() => Ok(r),
        Ok(_) => Err(Skip::NotInitialized),
        Err(e) if e.code == ErrorCode::NotARepo => Err(Skip::NotInitialized),
        Err(e) => Err(Skip::Error(e.message)),
    }
}

/// Same path, however it was spelled.
pub fn same(a: &Path, b: &Path) -> bool {
    paths::display(&paths::normalize(a)) == paths::display(&paths::normalize(b))
}
