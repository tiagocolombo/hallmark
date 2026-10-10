//! Front Desk: `hall new` and `hall adopt`.
//!
//! `hall new "<request>"` first asks the Front Desk (`crate::intake`)
//! whether the request splits into independent parts (ADR 0024). If it does,
//! the item becomes a **parent** and each part a **child** item with its own
//! branch (cut from the parent's) and worktree. The session's Claude
//! orchestrates: one subagent per child, `hall merge <child>` as each one
//! finishes, then the parent goes to review once, as a whole.

use std::path::Path;
use std::process::Command;

use schemars::JsonSchema;
use serde::Serialize;

use super::{ItemView, next_for, slug};
use crate::cli::{AdoptArgs, NewArgs};
use crate::config::SplitMode;
use crate::error::{ErrorCode, HallError, Result};
use crate::event::Body;
use crate::git;
use crate::intake::{Part, Plan};
use crate::item::{Issue, Item, ItemId, State};
use crate::journal::ev;
use crate::output::{Mode, Render, Response, clip};
use crate::paths;
use crate::repo::{Repo, actor};

#[derive(Debug, Serialize, JsonSchema)]
pub struct NewOut {
    /// The item created: for a split request, the parent.
    #[serde(flatten)]
    pub item: ItemView,
    /// False when the command returned an item that already existed.
    pub created: bool,
    /// The command that opens Claude Code in the worktree.
    pub open: String,
    /// The Front Desk's plan, when the request was offered to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlanView>,
    /// The parallel parts, one child item each, when the request was split.
    /// Each merges into this item with `hall merge`; this item is reviewed
    /// once, as a whole.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<ChildView>,
    /// The parent this item merges into (`--parent`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<ItemId>,
    /// The PR whose feedback this item follows up (`--follow-up`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<String>,
}

/// What the Front Desk decided.
#[derive(Debug, Serialize, JsonSchema)]
pub struct PlanView {
    /// True when an intake run was made (even one that failed).
    pub ran: bool,
    /// True when the request was split into parallel parts.
    pub parallel: bool,
    /// Why it was split, or why it is one item.
    pub reason: String,
}

/// A parallel part: a child item with its own branch and worktree.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ChildView {
    #[serde(flatten)]
    pub item: ItemView,
    /// The part's self-contained request, for the subagent that builds it.
    pub request: String,
    /// The paths the Front Desk expects it to touch.
    pub paths: Vec<String>,
}

impl Render for NewOut {
    fn render(&self) -> String {
        let verb = if self.created {
            "Started"
        } else {
            "Already tracking"
        };
        let mut s = format!("{verb} {}.\n\n{}", self.item.id, self.item.render());
        if let Some(p) = &self.parent {
            s.push_str(&format!("  parent    {p} (merges into it: `hall merge`)\n"));
        }
        if let Some(f) = &self.follow_up {
            s.push_str(&format!("  follows   {f}\n"));
        }
        if let Some(plan) = &self.plan
            && plan.ran
        {
            s.push_str(&format!("\nFront Desk: {}\n", plan.reason));
        }
        if !self.children.is_empty() {
            s.push_str(&format!(
                "\nSplit into {} parallel parts:\n",
                self.children.len()
            ));
            for c in &self.children {
                s.push_str(&format!(
                    "  {} · {}\n    {}\n",
                    c.item.id, c.item.title, c.item.worktree
                ));
            }
        }
        s
    }
}

/// Everything needed to create one item.
struct Spec<'a> {
    title: String,
    branch: Option<String>,
    request: Option<String>,
    issue: Option<Issue>,
    parent: Option<ItemId>,
    follow_up: Option<String>,
    /// The branch the new branch starts from (a child starts at its parent).
    start: Option<&'a str>,
}

pub fn new(cwd: &Path, a: NewArgs, mode: Mode) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    repo.require_init()?;
    if a.request.is_none() && a.title.is_none() && a.issue.is_none() && a.follow_up.is_none() {
        return Err(
            HallError::usage("say what the work is: a request, --title or --issue").next([
                "hall new \"<request>\"",
                "hall new --title \"<short title>\"",
                "hall new --issue 212",
                "hall new --follow-up <pr-url>",
            ]),
        );
    }
    if a.parent.is_some() && a.follow_up.is_some() {
        return Err(HallError::usage(
            "--parent and --follow-up don't mix: a follow-up is a new top-level item",
        )
        .next(["hall new --follow-up <pr-url>"]));
    }
    let parent = match &a.parent {
        Some(p) => Some(parent_for_child(&repo, p)?),
        None => None,
    };
    let issue = match a.issue {
        Some(n) => Some(crate::gh::issue(&repo.main_root, n)?),
        None => None,
    };
    let pr = match &a.follow_up {
        Some(pr) => Some(crate::gh::pr(&repo.main_root, pr)?),
        None => None,
    };
    let title = a
        .title
        .clone()
        .or_else(|| {
            pr.as_ref()
                .map(|p| clip(&format!("Follow-up: {}", p.title), 72))
        })
        .or_else(|| issue.as_ref().map(|i| i.title.clone()))
        .or_else(|| a.request.as_ref().map(|r| title_from(r)))
        .unwrap_or_default();
    let request = match &pr {
        Some(p) => {
            let shipped = shipped_by(&repo, &p.url);
            Some(follow_up_request(p, shipped, a.request.as_deref()))
        }
        None => a.request.clone().or_else(|| {
            issue.as_ref().map(|i| {
                format!("#{} {}\n\n{}", i.number, i.title, i.body)
                    .trim()
                    .to_string()
            })
        }),
    };

    // The Front Desk: can this request be built as parallel parts? It runs
    // before the repo lock (it can take a minute) and fails open.
    let intake = match &request {
        Some(r)
            if repo.config.intake.split == SplitMode::Auto
                && !a.no_split
                && parent.is_none()
                && pr.is_none() =>
        {
            Some(crate::intake::plan(&repo, r))
        }
        _ => None,
    };
    let raw = intake.as_ref().and_then(|o| o.raw.clone());
    let mut plan: Option<Plan> = intake.map(|o| o.plan);

    // Hold the repo lock from ID allocation until every item exists, so IDs
    // stay sequential.
    let (item, children) = repo.hall.with_repo_lock(|| {
        if let Some(p) = plan.as_mut()
            && p.parallel
            && let Some(why) = split_blocked(&repo, &title, a.branch.as_deref(), &p.parts)
        {
            *p = Plan {
                ran: p.ran,
                parallel: false,
                reason: format!("{why}; kept as one item"),
                parts: Vec::new(),
            };
        }
        let item = create(
            &repo,
            Spec {
                title: title.clone(),
                branch: a.branch.clone(),
                request: request.clone(),
                issue: issue.as_ref().map(|i| Issue {
                    number: i.number,
                    url: i.url.clone(),
                }),
                parent: parent.as_ref().map(|p| p.id),
                follow_up: pr.as_ref().map(|p| p.url.clone()),
                start: parent.as_ref().map(|p| p.branch.as_str()),
            },
        )?;
        if let Some(raw) = &raw {
            let dir = repo.hall.item_runs(item.id);
            if std::fs::create_dir_all(&dir).is_ok() {
                let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
                let _ = std::fs::write(dir.join(format!("{stamp}-intake.json")), raw);
            }
        }
        let mut children: Vec<(Item, Part)> = Vec::new();
        // The `planned` event is written only when intake actually ran.
        let Some(p) = plan.as_mut().filter(|p| p.ran) else {
            return Ok((item, children));
        };
        if p.parallel {
            let mut failed = Vec::new();
            for part in &p.parts {
                let spec = Spec {
                    title: part.title.clone(),
                    branch: None,
                    request: Some(part.request.clone()),
                    issue: None,
                    parent: Some(item.id),
                    follow_up: None,
                    start: Some(item.branch.as_str()),
                };
                match create(&repo, spec) {
                    Ok(c) => children.push((c, part.clone())),
                    Err(e) => failed.push(format!(
                        "part \"{}\" could not be created: {}",
                        part.title, e.message
                    )),
                }
            }
            if children.is_empty() {
                p.parallel = false;
            }
            if !failed.is_empty() {
                p.reason = format!("{}; {}", p.reason, failed.join("; "));
            }
        }
        let planned = ev(
            &actor(),
            Some(item.id),
            Body::Planned {
                parallel: p.parallel,
                children: children.iter().map(|(c, _)| c.id).collect(),
                reason: Some(p.reason.clone()),
            },
        );
        repo.hall.append_item(item.id, &planned)?;
        Ok((item, children))
    })?;

    let open = format!("cd {} && claude", shell_quote(&item.worktree));
    let mut next = vec![format!("cd {}", shell_quote(&item.worktree))];
    if children.is_empty() {
        next.extend(next_for(&item));
    } else {
        next.extend(orchestration_next(&item, &children));
    }
    let out = NewOut {
        item: ItemView::from(&item),
        created: true,
        open: open.clone(),
        plan: plan.map(|p| PlanView {
            ran: p.ran,
            parallel: p.parallel,
            reason: p.reason,
        }),
        children: children
            .iter()
            .map(|(c, part)| ChildView {
                item: ItemView::from(c),
                request: part.request.clone(),
                paths: part.paths.clone(),
            })
            .collect(),
        parent: item.parent,
        follow_up: item.follow_up.clone(),
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
            plan: None,
            children: Vec::new(),
            parent: item.parent,
            follow_up: item.follow_up.clone(),
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
                parent: None,
                follow_up: None,
            },
        );
        repo.hall.append_item(id, &created)?;
        repo.hall.refresh_item_json(id)
    })?;
    let out = NewOut {
        item: ItemView::from(&item),
        created: true,
        open: format!("cd {} && claude", shell_quote(&item.worktree)),
        plan: None,
        children: Vec::new(),
        parent: None,
        follow_up: None,
    };
    Ok(Response::new(out, next_for(&item)))
}

/// `--parent <id>`: the parent must be a top-level item in development.
fn parent_for_child(repo: &Repo, id: &str) -> Result<Item> {
    let p = repo.hall.load_item(id.parse()?)?;
    if let Some(grand) = p.parent {
        return Err(HallError::new(
            ErrorCode::WrongState,
            format!("{} is itself a part of {grand}; parts don't nest", p.id),
        )
        .next([format!("hall new \"<request>\" --parent {grand}")]));
    }
    if p.state != State::Development {
        return Err(HallError::new(
            ErrorCode::WrongState,
            format!(
                "{} is {}; parts can only be added to an item in development",
                p.id,
                p.state.label().to_lowercase()
            ),
        )
        .next([
            format!("hall pull-back {}", p.id),
            format!("hall status {}", p.id),
        ]));
    }
    Ok(p)
}

/// A reason not to split after all: a branch the items would use exists.
/// Call under the repo lock.
fn split_blocked(repo: &Repo, title: &str, branch: Option<&str>, parts: &[Part]) -> Option<String> {
    let first = repo.hall.next_item_id().ok()?.0;
    let parent_branch = branch.map(String::from).unwrap_or_else(|| {
        fill(
            &repo.config.branch,
            &[("id", &ItemId(first).to_string()), ("slug", &slug(title))],
        )
    });
    let mut names = vec![parent_branch];
    for (i, p) in parts.iter().enumerate() {
        names.push(fill(
            &repo.config.branch,
            &[
                ("id", &ItemId(first + 1 + i as u32).to_string()),
                ("slug", &slug(&p.title)),
            ],
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for n in &names {
        if !seen.insert(n.as_str()) {
            return Some(format!("two items would share branch {n}"));
        }
        if git::branch_exists(&repo.main_root, n) {
            return Some(format!("branch {n} already exists"));
        }
    }
    None
}

/// Creates one item: its branch and worktree, then its `created` event.
/// Call under the repo lock.
fn create(repo: &Repo, spec: Spec) -> Result<Item> {
    let id = repo.hall.next_item_id()?;
    let branch = spec.branch.clone().unwrap_or_else(|| {
        fill(
            &repo.config.branch,
            &[("id", &id.to_string()), ("slug", &slug(&spec.title))],
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
        ("slug", &slug(&spec.title)),
        ("base", repo.config.base.as_str()),
    ];
    let cmd = worktree_command(&repo.config.worktree, &vars, spec.start);
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
    // A custom worktree command may ignore `{start}`: bring a fresh child up
    // to its parent's tip when that is a fast-forward.
    if let Some(start) = spec.start
        && let (Ok(head), Ok(tip)) = (
            git::head(&worktree),
            git::run(&worktree, &["rev-parse", start]),
        )
        && head != tip
    {
        let _ = git::run(&worktree, &["merge", "--ff-only", "-q", start]);
    }
    let created = ev(
        &actor(),
        Some(id),
        Body::Created {
            title: spec.title,
            branch,
            worktree: paths::display(&worktree),
            base: repo.config.base.clone(),
            issue: spec.issue,
            request: spec.request,
            adopted: false,
            parent: spec.parent,
            follow_up: spec.follow_up,
        },
    );
    repo.hall.append_item(id, &created)?;
    repo.hall.refresh_item_json(id)
}

/// The worktree command for a new item. `{start}` is the branch it starts
/// from: a child's parent branch, else nothing (the main checkout's HEAD).
/// The default command gets the start point appended; a custom command
/// without `{start}` is left as is, and `create` fast-forwards instead.
pub fn worktree_command(template: &str, vars: &[(&str, &str)], start: Option<&str>) -> String {
    let mut t = template.trim().to_string();
    if start.is_some() && !t.contains("{start}") && t == crate::config::Config::default().worktree {
        t.push_str(" {start}");
    }
    let cmd = fill_shell(&t, vars);
    match start {
        Some(s) => cmd.replace("{start}", &shell_quote(s)),
        None => cmd.replace(" {start}", "").replace("{start}", ""),
    }
}

/// What the orchestrating session does with a split request.
fn orchestration_next(parent: &Item, children: &[(Item, Part)]) -> Vec<String> {
    let mut n = vec![
        "Launch one subagent per child, all at once (Agent tool); each works only in its child's \
         worktree, commits there and never pushes:"
            .to_string(),
    ];
    for (c, _) in children {
        n.push(format!("{} \"{}\": {}", c.id, c.title, c.worktree));
    }
    for (c, _) in children {
        n.push(format!("hall merge {} (when its subagent is done)", c.id));
    }
    n.push(format!(
        "hall propose-ready {} --summary \"<what was done>\" (after every child is merged)",
        parent.id
    ));
    n.push(format!("hall ready {}", parent.id));
    n.push(format!("hall status {}", parent.id));
    n
}

/// The item that shipped this PR, if Hallmark has it.
fn shipped_by(repo: &Repo, url: &str) -> Option<ItemId> {
    repo.index()
        .ok()?
        .items(true)
        .ok()?
        .into_iter()
        .find(|i| i.pr_url.as_deref() == Some(url))
        .map(|i| i.id)
}

/// The request for a follow-up item: the PR and its review feedback.
pub fn follow_up_request(
    pr: &crate::gh::GhPr,
    shipped: Option<ItemId>,
    extra: Option<&str>,
) -> String {
    let mut s = format!(
        "Follow-up to PR #{} \"{}\" ({})",
        pr.number, pr.title, pr.url
    );
    if let Some(id) = shipped {
        s.push_str(&format!(", which shipped {id}"));
    }
    s.push_str(".\nAddress the review feedback below.\n");
    if let Some(e) = extra.map(str::trim).filter(|e| !e.is_empty()) {
        s.push_str(&format!("\n{e}\n"));
    }
    let who = |c: &crate::gh::GhComment| {
        c.author
            .as_ref()
            .filter(|a| !a.login.is_empty())
            .map(|a| format!("@{}", a.login))
            .unwrap_or_else(|| "someone".into())
    };
    let mut lines = Vec::new();
    for r in &pr.reviews {
        if r.body.trim().is_empty() {
            continue;
        }
        let state = r
            .state
            .as_deref()
            .map(|st| format!(" ({})", st.to_lowercase().replace('_', " ")))
            .unwrap_or_default();
        lines.push(format!(
            "- {}{state}: {}",
            who(r),
            clip(r.body.trim(), 1500)
        ));
    }
    for c in &pr.line_comments {
        if c.body.trim().is_empty() {
            continue;
        }
        let place = match (&c.path, c.line) {
            (Some(p), Some(l)) => format!(" on {p}:{l}"),
            (Some(p), None) => format!(" on {p}"),
            _ => String::new(),
        };
        lines.push(format!(
            "- {}{place}: {}",
            who(c),
            clip(c.body.trim(), 1500)
        ));
    }
    for c in &pr.comments {
        if c.body.trim().is_empty() {
            continue;
        }
        lines.push(format!("- {}: {}", who(c), clip(c.body.trim(), 1500)));
    }
    if lines.is_empty() {
        s.push_str("\nNo review comments were found; read the PR for context.\n");
    } else {
        s.push_str("\nReview feedback:\n");
        s.push_str(&lines.join("\n"));
        s.push('\n');
    }
    clip(&s, 12_000)
}

/// The first line of a request, trimmed to a title.
fn title_from(request: &str) -> String {
    let line = request
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(request)
        .trim();
    clip(line, 72)
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

/// A command line run through the platform shell: `sh -c`, or `cmd /C` on
/// Windows.
///
/// On Windows the line goes to cmd.exe verbatim (`/S /C "<line>"` through
/// `raw_arg`): `Command::arg` would escape its quotes as `\"`, which cmd.exe
/// doesn't understand, and break any command with a quoted argument, such
/// as `git commit -m "fix F1"` or a ship command's `--title {title}`.
pub fn shell(cmd: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new("cmd");
        c.raw_arg(format!("/S /C \"{cmd}\""));
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    }
}

/// Runs a configured command through the platform shell.
pub fn run_shell(cmd: &str, dir: &Path) -> Result<String> {
    let out = shell(cmd).current_dir(dir).output().map_err(|e| {
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
    use crate::gh::{GhAuthor, GhComment, GhPr};

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
    fn shell_keeps_quoted_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let out = run_shell("git --version && echo \"two words\"", dir.path()).unwrap();
        assert!(out.contains("two words"), "{out}");
        assert!(!out.contains('\\'), "{out}");
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

    #[test]
    fn children_start_from_their_parent() {
        let v = [("repo", "app"), ("id", "H-0002"), ("branch", "hm/H-0002-x")];
        let default = crate::config::Config::default().worktree;
        assert_eq!(
            worktree_command(&default, &v, None),
            "git worktree add ../app-H-0002 -b hm/H-0002-x"
        );
        assert_eq!(
            worktree_command(&default, &v, Some("hm/H-0001-p")),
            "git worktree add ../app-H-0002 -b hm/H-0002-x hm/H-0001-p"
        );
        assert_eq!(
            worktree_command("dev wt new {branch} --from {start}", &v, Some("hm/p")),
            "dev wt new hm/H-0002-x --from hm/p"
        );
        assert_eq!(
            worktree_command("dev wt new {branch} {start}", &v, None),
            "dev wt new hm/H-0002-x"
        );
        assert_eq!(
            worktree_command("dev wt new {branch}", &v, Some("hm/p")),
            "dev wt new hm/H-0002-x"
        );
    }

    fn comment(login: &str, body: &str) -> GhComment {
        GhComment {
            author: Some(GhAuthor {
                login: login.into(),
            }),
            body: body.into(),
            ..Default::default()
        }
    }

    #[test]
    fn follow_up_requests_carry_the_feedback() {
        let pr = GhPr {
            number: 12,
            title: "Retry webhooks".into(),
            body: "Adds retries.".into(),
            url: "https://github.com/acme/app/pull/12".into(),
            reviews: vec![
                GhComment {
                    state: Some("CHANGES_REQUESTED".into()),
                    ..comment("alice", "Backoff needs a cap.")
                },
                GhComment {
                    state: Some("APPROVED".into()),
                    ..comment("bob", "")
                },
            ],
            comments: vec![comment("carol", "Also log the attempt number.")],
            line_comments: vec![GhComment {
                path: Some("src/retry.rs".into()),
                line: Some(14),
                ..comment("alice", "Use saturating_mul here.")
            }],
        };
        let r = follow_up_request(&pr, Some(ItemId(7)), Some("Keep it small."));
        assert!(r.starts_with(
            "Follow-up to PR #12 \"Retry webhooks\" (https://github.com/acme/app/pull/12), which shipped H-0007."
        ));
        assert!(r.contains("Keep it small."));
        assert!(r.contains("- @alice (changes requested): Backoff needs a cap."));
        assert!(r.contains("- @alice on src/retry.rs:14: Use saturating_mul here."));
        assert!(r.contains("- @carol: Also log the attempt number."));
        assert!(!r.contains("@bob"), "empty approvals are skipped");
        let quiet = GhPr {
            reviews: vec![],
            comments: vec![],
            line_comments: vec![],
            ..pr
        };
        assert!(follow_up_request(&quiet, None, None).contains("No review comments"));
    }
}
