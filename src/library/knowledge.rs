//! Knowledge items: proposed learnings travel through Hallmark like any
//! change. A knowledge item is an ordinary item titled `Library: …`; the
//! Library creates it by running `hall new` as a subprocess (the CLI is the
//! API), writes and commits learning files in its worktree, and runs
//! `hall propose-ready` so it shows in the inbox as "ready?". From there you
//! send it to review with `hall ready`, and it ships like code.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::Command;

use fs4::FileExt;

use super::learning::Learning;
use super::store::{self, KNOWLEDGE_PREFIX};
use crate::error::{ErrorCode, HallError, Result};
use crate::event::{Body, Event};
use crate::item::{Item, ItemId, State};
use crate::repo::Repo;

/// An open knowledge item Hallmark may commit to.
#[derive(Debug, Clone)]
pub struct Knowledge {
    pub item: Item,
    pub worktree: PathBuf,
    /// True when this call created it.
    pub created: bool,
}

/// Runs `f` under the Library's own lock, so two librarians (or a librarian
/// and `hall library review`) never write the same knowledge item at once.
/// It is not the repo lock: `hall new` takes that one.
pub fn with_lock<R>(repo: &Repo, f: impl FnOnce() -> Result<R>) -> Result<R> {
    let dir = repo.hall.root.join("library");
    std::fs::create_dir_all(&dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(".lock"))?;
    FileExt::lock(&lock)?;
    let r = f();
    let _ = FileExt::unlock(&lock);
    r
}

/// Runs this `hall` binary with JSON output and returns its reply.
pub fn hall(repo: &Repo, actor: &str, args: &[&str]) -> Result<serde_json::Value> {
    let exe = std::env::current_exe()?;
    let out = Command::new(&exe)
        .arg("--json")
        .args(args)
        .current_dir(&repo.main_root)
        .env("HALL_ACTOR", actor)
        .env_remove("CLAUDECODE")
        .output()
        .map_err(|e| {
            HallError::new(
                ErrorCode::CommandFailed,
                format!("could not run {}: {e}", exe.display()),
            )
        })?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).map_err(|_| {
        HallError::new(
            ErrorCode::CommandFailed,
            format!(
                "`hall {}` printed no JSON: {}",
                args.join(" "),
                crate::output::clip(&String::from_utf8_lossy(&out.stderr), 200)
            ),
        )
    })?;
    if let Some(e) = v.get("error") {
        let code = e
            .get("code")
            .and_then(|c| c.as_str())
            .and_then(|c| ErrorCode::ALL.into_iter().find(|k| k.as_str() == c))
            .unwrap_or(ErrorCode::CommandFailed);
        let next: Vec<String> = e
            .get("next")
            .and_then(|n| serde_json::from_value(n.clone()).ok())
            .unwrap_or_default();
        return Err(HallError::new(
            code,
            format!(
                "`hall {}`: {}",
                args.first().copied().unwrap_or(""),
                e.get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("failed")
            ),
        )
        .next(next));
    }
    Ok(v)
}

/// The oldest open knowledge item that is still editable, or a new one
/// titled `Library: <what>`.
pub fn get_or_create(repo: &Repo, actor: &str, what: &str) -> Result<Knowledge> {
    if let Some(item) = store::open_knowledge(repo)?
        .into_iter()
        .find(store::editable)
    {
        let item = repo.hall.load_item(item.id)?;
        return Ok(Knowledge {
            worktree: PathBuf::from(&item.worktree),
            item,
            created: false,
        });
    }
    let title = format!("{KNOWLEDGE_PREFIX} {what}");
    let v = hall(repo, actor, &["new", "--title", &title, "--no-split"])?;
    let id: ItemId = v
        .get("id")
        .and_then(|i| i.as_str())
        .ok_or_else(|| HallError::internal("`hall new` returned no id"))?
        .parse()?;
    let item = repo.hall.load_item(id)?;
    Ok(Knowledge {
        worktree: PathBuf::from(&item.worktree),
        item,
        created: true,
    })
}

/// The knowledge item `id`, if it can take commits.
pub fn editable(repo: &Repo, id: ItemId) -> Result<Knowledge> {
    let item = repo.hall.load_item(id)?;
    if !store::editable(&item) {
        return Err(HallError::new(
            ErrorCode::WrongState,
            format!(
                "{id} is {}; learnings can only change while it is in development or awaiting you",
                item.state
            ),
        )
        .next([format!("hall pull-back {id}"), format!("hall status {id}")]));
    }
    Ok(Knowledge {
        worktree: PathBuf::from(&item.worktree),
        item,
        created: false,
    })
}

/// Writes `l` into the knowledge worktree: over the file that already holds
/// its ID, else at `base_path` (where the base keeps it), else under a new
/// name. Returns the repo-relative path.
pub fn write(repo: &Repo, k: &Knowledge, l: &Learning, base_path: Option<&str>) -> Result<String> {
    let dir = store::dir(repo);
    let abs = store::find_in_worktree(repo, &k.worktree, &l.id)
        .or_else(|| base_path.map(|p| k.worktree.join(p)))
        .unwrap_or_else(|| k.worktree.join(&dir).join(l.file_name()));
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&abs, l.render())?;
    Ok(rel(&k.worktree, &abs))
}

/// Restores `id` to its base version (`base_path`, `text`), or deletes it
/// when the base has none.
pub fn restore(
    repo: &Repo,
    k: &Knowledge,
    id: &str,
    base: Option<(&str, &str)>,
) -> Result<Option<String>> {
    let current = store::find_in_worktree(repo, &k.worktree, id);
    match base {
        Some((path, text)) => {
            let target = k.worktree.join(path);
            if let Some(c) = &current
                && *c != target
            {
                std::fs::remove_file(c)?;
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, text)?;
            Ok(Some(path.to_string()))
        }
        None => {
            if let Some(c) = current {
                let r = rel(&k.worktree, &c);
                std::fs::remove_file(c)?;
                return Ok(Some(r));
            }
            Ok(None)
        }
    }
}

fn rel(root: &Path, p: &Path) -> String {
    crate::paths::display(p.strip_prefix(root).unwrap_or(p))
}

/// Commits the learnings directory in the knowledge worktree, if it
/// changed, and journals the commit. Returns the new commit.
pub fn commit(repo: &Repo, k: &Knowledge, actor: &str, message: &str) -> Result<Option<String>> {
    let wt = &k.worktree;
    let dir = store::dir(repo);
    crate::git::run(wt, &["add", "-A", "--", &dir])?;
    if crate::git::ok(wt, &["diff", "--cached", "--quiet", "--", &dir]) {
        return Ok(None);
    }
    crate::git::run(wt, &["commit", "-q", "-m", message, "--", &dir])?;
    let sha = crate::git::head(wt)?;
    let subject = message.lines().next().unwrap_or(message).to_string();
    let e = Event::new(
        actor,
        Some(k.item.id),
        Body::Commit {
            sha: sha.clone(),
            subject,
            fixes: Vec::new(),
        },
    );
    repo.hall.append_item(k.item.id, &e)?;
    Ok(Some(sha))
}

/// `hall propose-ready <id> --summary …` for an item still in development,
/// so it shows in the inbox as "ready?".
pub fn propose_ready(repo: &Repo, k: &Knowledge, actor: &str, summary: &str) -> Result<()> {
    let item = repo.hall.load_item(k.item.id)?;
    if item.state != State::Development {
        return Ok(());
    }
    let id = item.id.to_string();
    hall(repo, actor, &["propose-ready", &id, "--summary", summary])?;
    Ok(())
}
