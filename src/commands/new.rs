//! Front Desk: `hall new` and `hall adopt`.

use std::path::Path;
use std::process::Command;

use schemars::JsonSchema;
use serde::Serialize;

use super::{ItemView, next_for, slug};
use crate::cli::{AdoptArgs, NewArgs};
use crate::error::{ErrorCode, HallError, Result};
use crate::event::Body;
use crate::git;
use crate::item::{Issue, ItemId};
use crate::journal::ev;
use crate::output::{Mode, Render, Response};
use crate::paths;
use crate::repo::{Repo, actor};

#[derive(Debug, Serialize, JsonSchema)]
pub struct NewOut {
    #[serde(flatten)]
    pub item: ItemView,
    /// False when the command returned an item that already existed.
    pub created: bool,
    /// The command that opens Claude Code in the worktree.
    pub open: String,
}

impl Render for NewOut {
    fn render(&self) -> String {
        let verb = if self.created {
            "Started"
        } else {
            "Already tracking"
        };
        format!("{verb} {}.\n\n{}", self.item.id, self.item.render())
    }
}

pub fn new(cwd: &Path, a: NewArgs, mode: Mode) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    repo.require_init()?;
    if a.request.is_none() && a.title.is_none() && a.issue.is_none() {
        return Err(
            HallError::usage("say what the work is: a request, --title or --issue").next([
                "hall new \"<request>\"",
                "hall new --title \"<short title>\"",
                "hall new --issue 212",
            ]),
        );
    }
    let issue = match a.issue {
        Some(n) => Some(crate::gh::issue(&repo.main_root, n)?),
        None => None,
    };
    let title = a
        .title
        .clone()
        .or_else(|| issue.as_ref().map(|i| i.title.clone()))
        .or_else(|| a.request.as_ref().map(|r| title_from(r)))
        .unwrap_or_default();
    let request = a.request.clone().or_else(|| {
        issue.as_ref().map(|i| {
            format!("#{} {}\n\n{}", i.number, i.title, i.body)
                .trim()
                .to_string()
        })
    });

    // Hold the repo lock from ID allocation until the item exists.
    let item = repo.hall.with_repo_lock(|| {
        let id = repo.hall.next_item_id()?;
        let branch = a.branch.clone().unwrap_or_else(|| {
            fill(
                &repo.config.branch,
                &[("id", &id.to_string()), ("slug", &slug(&title))],
            )
        });
        if git::branch_exists(&repo.main_root, &branch) {
            return Err(HallError::new(
                ErrorCode::WrongState,
                format!("branch {branch} already exists"),
            )
            .next([
                "hall adopt (run it in that branch's worktree)",
                "hall new ... --branch <other>",
            ]));
        }
        let vars = [
            ("repo", repo.name.as_str()),
            ("id", &id.to_string()),
            ("branch", branch.as_str()),
            ("slug", &slug(&title)),
            ("base", repo.config.base.as_str()),
        ];
        let cmd = fill_shell(&repo.config.worktree, &vars);
        run_shell(&cmd, &repo.main_root)?;
        let worktree = git::worktrees(&repo.main_root)?
            .into_iter()
            .find(|w| w.branch.as_deref() == Some(branch.as_str()))
            .map(|w| w.path)
            .ok_or_else(|| {
                HallError::new(
                    ErrorCode::CommandFailed,
                    format!("`{cmd}` ran, but no worktree has branch {branch}"),
                )
                .next(["git worktree list", "hall doctor"])
            })?;
        let created = ev(
            &actor(),
            Some(id),
            Body::Created {
                title: title.clone(),
                branch: branch.clone(),
                worktree: paths::display(&worktree),
                base: repo.config.base.clone(),
                issue: issue.as_ref().map(|i| Issue {
                    number: i.number,
                    url: i.url.clone(),
                }),
                request: request.clone(),
                adopted: false,
            },
        );
        repo.hall.append_item(id, &created)?;
        repo.hall.refresh_item_json(id)
    })?;

    let open = format!("cd {} && claude", shell_quote(&item.worktree));
    let mut next = vec![format!("cd {}", shell_quote(&item.worktree))];
    next.extend(next_for(&item));
    let out = NewOut {
        item: ItemView::from(&item),
        created: true,
        open: open.clone(),
    };
    let resp = Response::new(out, next);
    if a.open && mode == Mode::Text {
        resp.print(mode);
        let prompt = request.unwrap_or(title);
        let status = Command::new("claude")
            .arg(prompt)
            .current_dir(&item.worktree)
            .status();
        if let Err(e) = status {
            eprintln!("could not start claude: {e}. Run: {open}");
        }
        return Ok(Response::raw(None));
    }
    Ok(resp)
}

pub fn adopt(cwd: &Path, a: AdoptArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    repo.require_init()?;
    let branch = git::current_branch(&repo.toplevel).ok_or_else(|| {
        HallError::new(
            ErrorCode::WrongState,
            "HEAD is detached; check out a branch first",
        )
        .next(["git switch -c <branch>"])
    })?;
    if branch == repo.config.base {
        return Err(HallError::new(
            ErrorCode::WrongState,
            format!("{branch} is the base branch; items live on their own branches"),
        )
        .next([
            "hall new \"<request>\"",
            "git switch -c <branch> && hall adopt",
        ]));
    }
    let ix = repo.index()?;
    let items = ix.items(false)?;
    let existing = items
        .iter()
        .find(|i| i.branch == branch)
        .cloned()
        .or_else(|| repo.item_at(&items, &repo.toplevel));
    if let Some(existing) = existing {
        let item = repo.hall.load_item(existing.id)?;
        let out = NewOut {
            item: ItemView::from(&item),
            created: false,
            open: format!("cd {} && claude", shell_quote(&item.worktree)),
        };
        return Ok(Response::new(out, next_for(&item)));
    }
    let title = a.title.unwrap_or_else(|| title_from_branch(&branch));
    let item = repo.hall.with_repo_lock(|| {
        let id: ItemId = repo.hall.next_item_id()?;
        let created = ev(
            &actor(),
            Some(id),
            Body::Created {
                title: title.clone(),
                branch: branch.clone(),
                worktree: paths::display(&repo.toplevel),
                base: repo.config.base.clone(),
                issue: None,
                request: None,
                adopted: true,
            },
        );
        repo.hall.append_item(id, &created)?;
        repo.hall.refresh_item_json(id)
    })?;
    let out = NewOut {
        item: ItemView::from(&item),
        created: true,
        open: format!("cd {} && claude", shell_quote(&item.worktree)),
    };
    Ok(Response::new(out, next_for(&item)))
}

/// The first line of a request, trimmed to a title.
fn title_from(request: &str) -> String {
    let line = request
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(request)
        .trim();
    crate::output::clip(line, 72)
}

/// `hm/H-0003-fix-login` → "Fix login".
fn title_from_branch(branch: &str) -> String {
    let last = branch.rsplit('/').next().unwrap_or(branch);
    // Drop an item prefix such as `H-0003-`.
    let rest = last
        .strip_prefix("H-")
        .map(|r| {
            r.trim_start_matches(|c: char| c.is_ascii_digit())
                .trim_start_matches('-')
        })
        .unwrap_or(last);
    let words: Vec<&str> = rest.split(['-', '_']).filter(|w| !w.is_empty()).collect();
    let mut t = words.join(" ");
    if let Some(f) = t.get(..1) {
        t = f.to_uppercase() + &t[1..];
    }
    if t.is_empty() { branch.to_string() } else { t }
}

/// Replaces `{name}` placeholders.
pub fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut s = template.to_string();
    for (k, v) in vars {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

/// Like [`fill`], quoting values that need it for the shell.
pub fn fill_shell(template: &str, vars: &[(&str, &str)]) -> String {
    let quoted: Vec<(&str, String)> = vars.iter().map(|(k, v)| (*k, shell_quote(v))).collect();
    let refs: Vec<(&str, &str)> = quoted.iter().map(|(k, v)| (*k, v.as_str())).collect();
    fill(template, &refs)
}

pub fn shell_quote(s: &str) -> String {
    let safe = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./:@+=,".contains(c));
    if safe && !s.is_empty() {
        s.to_string()
    } else if cfg!(windows) {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Runs a configured command through the platform shell.
pub fn run_shell(cmd: &str, dir: &Path) -> Result<String> {
    let mut c = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(cmd);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };
    let out = c.current_dir(dir).output().map_err(|e| {
        HallError::new(
            ErrorCode::CommandFailed,
            format!("could not run `{cmd}`: {e}"),
        )
    })?;
    if !out.status.success() {
        return Err(HallError::new(
            ErrorCode::CommandFailed,
            format!(
                "`{cmd}` failed ({}): {}",
                out.status.code().map_or("signal".into(), |c| c.to_string()),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        )
        .next(["hall doctor", "check `worktree` in .hallmark.toml"]));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles() {
        assert_eq!(
            title_from("\n  Retry failed webhooks\nwith backoff"),
            "Retry failed webhooks"
        );
        assert_eq!(title_from_branch("hm/H-0003-fix-login"), "Fix login");
        assert_eq!(title_from_branch("feature/new_billing"), "New billing");
    }

    #[test]
    fn placeholders() {
        let v = [("repo", "app"), ("id", "H-0001"), ("branch", "hm/H-0001-x")];
        assert_eq!(
            fill_shell("git worktree add ../{repo}-{id} -b {branch}", &v),
            "git worktree add ../app-H-0001 -b hm/H-0001-x"
        );
        if !cfg!(windows) {
            assert_eq!(shell_quote("it's"), "'it'\\''s'");
        }
    }
}
