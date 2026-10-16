//! `hall config`: read and change every setting without editing TOML by
//! hand. Repo keys live in the main checkout's `.hallmark.toml` (ADR 0012);
//! `--user` keys, and keys only the user config has (`tools.*`), live in
//! `<user data dir>/hallmark/config.toml`. Keys, kinds and help come from
//! the settings' JSON Schemas ([`crate::settings`]).

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use crate::cli::{ConfigAction, ConfigArgs};
use crate::config::{self, Tool, UserConfig};
use crate::error::{ErrorCode, HallError, Result};
use crate::output::{Render, Response};
use crate::paths;
use crate::repo::Repo;
use crate::settings::{self, Key, Kind, Scope};

#[derive(Debug, Serialize, JsonSchema)]
pub struct ConfigOut {
    /// list, get, set or unset.
    pub action: String,
    /// The repository's `.hallmark.toml`, when in a repository.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_file: Option<String>,
    /// Your user config.
    pub user_file: String,
    /// set and unset: whether the file changed.
    pub changed: bool,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Entry {
    pub key: String,
    pub scope: Scope,
    /// The value Hallmark uses; null when unset with no default.
    pub value: Value,
    /// Where the value comes from: default, repo, user, env (a `HALL_*`
    /// variable) or path (a tool found on PATH).
    pub source: String,
    pub kind: Kind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
}

impl Render for ConfigOut {
    fn render(&self) -> String {
        let mut s = String::new();
        if matches!(self.action.as_str(), "set" | "unset") {
            let verb = if self.changed { "" } else { " (unchanged)" };
            for e in &self.entries {
                s.push_str(&format!(
                    "{} = {} ({}){verb}\n",
                    e.key,
                    show(&e.value),
                    e.source
                ));
            }
            return s;
        }
        let width = self.entries.iter().map(|e| e.key.len()).max().unwrap_or(0);
        for scope in [Scope::Repo, Scope::User] {
            let rows: Vec<&Entry> = self.entries.iter().filter(|e| e.scope == scope).collect();
            if rows.is_empty() {
                continue;
            }
            if self.action == "list" {
                let file = match scope {
                    Scope::Repo => self.repo_file.as_deref().unwrap_or(config::FILE_NAME),
                    Scope::User => &self.user_file,
                };
                if !s.is_empty() {
                    s.push('\n');
                }
                s.push_str(&format!("{file}\n"));
            }
            for e in rows {
                s.push_str(&format!(
                    "  {:width$}  {}  [{}]\n",
                    e.key,
                    show(&e.value),
                    e.source
                ));
            }
        }
        s
    }
}

fn show(v: &Value) -> String {
    match v {
        Value::Null => "-".into(),
        other => other.to_string(),
    }
}

pub fn config(cwd: &Path, a: ConfigArgs) -> Result<Response> {
    match a.action.unwrap_or(ConfigAction::List {
        prefix: None,
        user: false,
    }) {
        ConfigAction::List { prefix, user } => list(cwd, prefix.as_deref(), user),
        ConfigAction::Get { key, user } => get(cwd, &key, user),
        ConfigAction::Set { key, value, user } => set(cwd, &key, Some(&value), user),
        ConfigAction::Unset { key, user } => set(cwd, &key, None, user),
    }
}

/// The key named `name`: in the user config with `--user`, else in the
/// repo's file, else in the user config when only it has the key.
pub fn resolve_key(name: &str, user: bool) -> Result<(Scope, Key)> {
    let name = name.trim();
    let scopes: &[Scope] = if user {
        &[Scope::User]
    } else {
        &[Scope::Repo, Scope::User]
    };
    for s in scopes {
        if let Some(k) = settings::find(*s, name) {
            return Ok((*s, k));
        }
    }
    let scope = scopes[0];
    // Keys that share the first or last part of the name.
    let near: Vec<String> = settings::keys(scope)
        .into_iter()
        .map(|k| k.key)
        .filter(|k| {
            let head = name.split('.').next().unwrap_or(name);
            let last = name.rsplit('.').next().unwrap_or(name);
            k.starts_with(head) || k.ends_with(last)
        })
        .take(8)
        .collect();
    let hint = if near.is_empty() {
        String::new()
    } else {
        format!("; did you mean {}?", near.join(", "))
    };
    let flag = if user { " --user" } else { "" };
    let is_table = settings::keys(scope)
        .iter()
        .any(|k| k.key.starts_with(&format!("{name}.")));
    if is_table {
        return Err(
            HallError::usage(format!("{name} is a table; set one of its keys"))
                .next([format!("hall config list {name}{flag}")]),
        );
    }
    Err(HallError::usage(format!(
        "no {} setting named {name}{hint}",
        if user { "user" } else { "Hallmark" }
    ))
    .next([
        format!("hall config list{flag}"),
        "hall schema config".into(),
    ]))
}

/// [`resolve_key`], except that `notify.*` goes to the user config unless the
/// repo's file already has a [notify] table: a repo's [notify] replaces the
/// user's as a whole (`Config::notify`), and channels are private.
fn route(cwd: &Path, name: &str, user: bool) -> Result<(Scope, Key)> {
    let (scope, key) = resolve_key(name, user)?;
    if scope == Scope::Repo && key.key.starts_with("notify.") {
        let repo_has = Files::load(cwd)
            .repo_file
            .and_then(|p| std::fs::read_to_string(p).ok())
            .is_some_and(|t| settings::lookup(&t, "notify").is_some());
        if !repo_has && let Some(k) = settings::find(Scope::User, &key.key) {
            return Ok((Scope::User, k));
        }
    }
    Ok((scope, key))
}

/// Where a scope's file is, and whether it must exist to be written.
struct Files {
    repo: Option<Repo>,
    repo_file: Option<PathBuf>,
    user_file: PathBuf,
}

impl Files {
    fn load(cwd: &Path) -> Files {
        let repo = Repo::discover(cwd).ok();
        // The main checkout's file: what `hall` reads (ADR 0012).
        let repo_file = repo.as_ref().map(|r| r.main_root.join(config::FILE_NAME));
        Files {
            repo,
            repo_file,
            user_file: UserConfig::path(),
        }
    }

    fn text(&self, scope: Scope) -> String {
        let p = match scope {
            Scope::Repo => self.repo_file.as_deref(),
            Scope::User => Some(self.user_file.as_path()),
        };
        p.and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default()
    }

    fn out(&self, action: &str, changed: bool, entries: Vec<Entry>) -> ConfigOut {
        ConfigOut {
            action: action.into(),
            repo_file: self.repo_file.as_deref().map(paths::display),
            user_file: paths::display(&self.user_file),
            changed,
            entries,
        }
    }
}

/// The effective value of every key of a scope, and where each comes from.
fn entries(f: &Files, scope: Scope, keys: &[Key]) -> Vec<Entry> {
    let text = f.text(scope);
    let user_text = f.text(Scope::User);
    let effective = match scope {
        Scope::Repo => {
            let c = f
                .repo
                .as_ref()
                .map(|r| r.config.clone())
                .unwrap_or_default();
            let mut v = serde_json::to_value(&c).unwrap_or_default();
            v["notify"] = serde_json::to_value(c.notify()).unwrap_or_default();
            // An unset per-run cap is the role's default, not "no cap".
            for r in crate::budget::ROLES {
                v["budget"][crate::budget::table_key(r)]["max_usd"] = match c.budget.max_usd(r) {
                    Some(u) => Value::from(u),
                    None => Value::from("none"),
                };
            }
            v
        }
        Scope::User => {
            let u: UserConfig = toml::from_str(&user_text).unwrap_or_default();
            let mut v = serde_json::to_value(&u).unwrap_or_default();
            v["notify"] = serde_json::to_value(u.notify.unwrap_or_default()).unwrap_or_default();
            v
        }
    };
    // The repo's [notify] replaces the user's as a whole (`Config::notify`).
    let repo_has_notify = scope == Scope::Repo && settings::lookup(&text, "notify").is_some();
    keys.iter()
        .map(|k| {
            let mut value = settings::pick(&effective, &k.key);
            let mut source = if settings::lookup(&text, &k.key).is_some() {
                scope.as_str().to_string()
            } else if scope == Scope::Repo
                && k.key.starts_with("notify.")
                && !repo_has_notify
                && settings::lookup(&user_text, &k.key).is_some()
            {
                "user".into()
            } else {
                "default".into()
            };
            if let Some(tool) = k
                .key
                .strip_prefix("tools.")
                .and_then(|t| Tool::ALL.into_iter().find(|x| x.name() == t))
            {
                if std::env::var(tool.env()).is_ok_and(|v| !v.trim().is_empty()) {
                    source = "env".into();
                    value = Value::from(std::env::var(tool.env()).unwrap_or_default());
                } else if value.is_null() {
                    let found = match tool {
                        Tool::Gh => crate::gh::find(),
                        _ => crate::gh::which(tool.name()),
                    };
                    if let Some(p) = found {
                        value = Value::from(paths::display(&p));
                        source = "path".into();
                    }
                }
            }
            Entry {
                key: k.key.clone(),
                scope,
                value,
                source,
                kind: k.kind,
                help: k.help.clone(),
                values: k.values.clone(),
            }
        })
        .collect()
}

/// Every key's current value: the repo's (in a repository) and the user's.
pub(crate) fn snapshot(cwd: &Path) -> Vec<Entry> {
    let f = Files::load(cwd);
    let mut out = Vec::new();
    if f.repo.is_some() {
        out.extend(entries(&f, Scope::Repo, &settings::keys(Scope::Repo)));
    }
    out.extend(entries(&f, Scope::User, &settings::keys(Scope::User)));
    out
}

/// The file a scope's keys are written to.
pub(crate) fn file_for(cwd: &Path, scope: Scope) -> Result<PathBuf> {
    Ok(match scope {
        Scope::Repo => Repo::discover(cwd)?.main_root.join(config::FILE_NAME),
        Scope::User => UserConfig::path(),
    })
}

fn list(cwd: &Path, prefix: Option<&str>, user: bool) -> Result<Response> {
    let f = Files::load(cwd);
    let matches = |k: &Key| {
        prefix.is_none_or(|p| {
            let p = p.trim_end_matches('.');
            k.key == p || k.key.starts_with(&format!("{p}."))
        })
    };
    // In a repo: its keys, then the keys only the user config has (tools).
    // With --user, or outside a repo: every user key.
    let repo_scope = !user && f.repo.is_some();
    let mut out = Vec::new();
    if repo_scope {
        let keys: Vec<Key> = settings::keys(Scope::Repo)
            .into_iter()
            .filter(matches)
            .collect();
        out.extend(entries(&f, Scope::Repo, &keys));
    }
    let keys: Vec<Key> = settings::keys(Scope::User)
        .into_iter()
        .filter(matches)
        .filter(|k| !repo_scope || settings::find(Scope::Repo, &k.key).is_none())
        .collect();
    out.extend(entries(&f, Scope::User, &keys));
    if out.is_empty()
        && let Some(p) = prefix
    {
        return Err(HallError::usage(format!("no settings under {p}"))
            .next(["hall config list", "hall schema config"]));
    }
    let mut next = vec![
        "hall config set <key> <value>".to_string(),
        "hall config unset <key>".into(),
    ];
    if f.repo.is_none() && !user {
        next.insert(0, "hall init".into());
    }
    Ok(Response::new(f.out("list", false, out), next))
}

fn get(cwd: &Path, name: &str, user: bool) -> Result<Response> {
    let (scope, key) = route(cwd, name, user)?;
    let f = Files::load(cwd);
    if scope == Scope::Repo && f.repo.is_none() {
        Repo::discover(cwd)?;
    }
    let e = entries(&f, scope, std::slice::from_ref(&key));
    Ok(Response::new(
        f.out("get", false, e),
        vec![format!(
            "hall config set {}{} <value>",
            if scope == Scope::User { "--user " } else { "" },
            key.key
        )],
    ))
}

/// Sets a key (`Some`) or removes it (`None`), so its default applies.
fn set(cwd: &Path, name: &str, value: Option<&[String]>, user: bool) -> Result<Response> {
    let action = if value.is_some() { "set" } else { "unset" };
    // The fixer, reviewer and other headless runs work under these rules;
    // they don't get to change them.
    if std::env::var_os("HALL_HEADLESS").is_some() {
        return Err(HallError::new(
            ErrorCode::GateDenied,
            format!("`hall config {action}` doesn't run inside a Hallmark agent run"),
        )
        .next(["hall config list"]));
    }
    let (scope, key) = route(cwd, name, user)?;
    let parsed = value
        .map(|v| settings::parse(&key, v))
        .transpose()
        .map_err(|e| {
            HallError::usage(e).next([
                format!("hall config get {}", key.key),
                "hall schema config".into(),
            ])
        })?;
    let f = Files::load(cwd);
    let path = match scope {
        Scope::Repo => {
            let repo = Repo::discover(cwd)?;
            repo.require_init()?;
            repo.main_root.join(config::FILE_NAME)
        }
        Scope::User => f.user_file.clone(),
    };
    let changed = write(scope, &path, &key.key, parsed)?;
    // Re-read, so the reply shows what Hallmark now uses.
    let f = Files::load(cwd);
    let e = entries(&f, scope, std::slice::from_ref(&key));
    Ok(Response::new(
        f.out(action, changed, e),
        vec![
            format!("hall config get {}", key.key),
            "hall config list".into(),
        ],
    ))
}

/// Applies one change to a scope's file; true when the file changed.
pub fn write(
    scope: Scope,
    path: &Path,
    key: &str,
    value: Option<toml_edit::Value>,
) -> Result<bool> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let shown = paths::display(path);
    let new = settings::edit(scope, &text, key, value)
        .map_err(|e| HallError::usage(format!("{shown}: {e}")).next(["hall doctor"]))?;
    if new == text {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, new)?;
    Ok(true)
}
