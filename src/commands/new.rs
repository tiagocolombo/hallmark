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
    /// True when `hall adopt` brought a shipped item back to development
    /// through its open PR.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub reopened: bool,
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
        } else if self.reopened {
            "Reopened"
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
    /// The branch it is reviewed against and ships into, when not the
    /// configured base (a follow-up to an open PR targets that PR's branch).
    base: Option<String>,
    /// Made by `hall adopt` rather than `hall new`.
    adopted: bool,
    /// The open PR the item takes over: its URL and head commit.
    pr: Option<(String, String)>,
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
    // A follow-up carries the PR's failing CI checks, with log excerpts.
    let failing_ci: Vec<(String, Option<String>)> = match &pr {
        Some(p) => crate::gh::pr_checks(&repo.main_root, &p.url)
            .unwrap_or_default()
            .into_iter()
            .filter(|c| matches!(c.bucket.as_str(), "fail" | "cancel"))
            .take(4)
            .map(|c| {
                let log = c
                    .link
                    .as_deref()
                    .and_then(|l| crate::gh::failed_log(&repo.main_root, l, 60));
                (c.name, log)
            })
            .collect(),
        None => Vec::new(),
    };
    // A follow-up to a PR that is still open stacks on it: it starts from
    // the PR's branch and is reviewed against and shipped into it.
    // Only a PR Hallmark shipped, still at the commit it reviewed, is stacked
    // on: its branch is local, and the rules the follow-up is hallmarked under
    // were themselves reviewed. Anything else starts from the configured base.
    let shipped = pr.as_ref().and_then(|p| shipped_by(&repo, &p.url));
    let stack: Option<String> = match (&pr, &shipped) {
        (Some(p), Some(it))
            if p.state == "OPEN"
                && !p.cross_repo
                && !p.head_oid.is_empty()
                && it.hallmark_sha.as_deref() == Some(p.head_oid.as_str())
                && git::branch_exists(&repo.main_root, &it.branch)
                && git::run(
                    &repo.main_root,
                    &["rev-parse", &format!("{}^{{commit}}", it.branch)],
                )
                .is_ok_and(|tip| tip.trim() == p.head_oid) =>
        {
            Some(it.branch.clone())
        }
        _ => None,
    };
    let request = match &pr {
        Some(p) => Some(follow_up_request(
            p,
            shipped.as_ref().map(|i| i.id),
            stack.as_deref(),
            a.request.as_deref(),
            &failing_ci,
        )),
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
                start: parent
                    .as_ref()
                    .map(|p| p.branch.as_str())
                    .or(stack.as_deref()),
                base: stack.clone(),
                adopted: false,
                pr: None,
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
                    base: None,
                    adopted: false,
                    pr: None,
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
        reopened: false,
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

/// `hall adopt`: turn the current branch, or an open PR (`--pr`), into an
/// item (ADR 0027).
///
/// - A branch with an open item returns that item.
/// - A branch Hallmark shipped, whose PR is still open, reopens that item:
///   it goes back to development to be rebased, have conflicts resolved or
///   change, and when it is hallmarked again the shipper updates the PR.
/// - Any other branch becomes a new item. If it has an open PR (Hallmark's
///   or not), the item takes that PR over and updates it instead of opening
///   a new one.
pub fn adopt(cwd: &Path, a: AdoptArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    repo.require_init()?;
    if let Some(base) = &a.base {
        check_base(&repo, base, None)?;
    }
    if let Some(pr) = &a.pr {
        return adopt_pr(&repo, pr, &a);
    }
    let branch = git::current_branch(&repo.toplevel).ok_or_else(|| {
        HallError::new(
            ErrorCode::WrongState,
            "HEAD is detached; check out a branch first",
        )
        .next(["git switch -c <branch>", "hall adopt --pr <number>"])
    })?;
    if branch == repo.config.base {
        return Err(HallError::new(
            ErrorCode::WrongState,
            format!("{branch} is the base branch; items live on their own branches"),
        )
        .next([
            "hall new \"<request>\"",
            "git switch -c <branch> && hall adopt",
            "hall adopt --pr <number>",
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
        return existing_item(&repo, existing.id, a.base.as_deref());
    }
    // A shipped item on this branch whose PR is still open comes back.
    let closed = ix
        .items(true)?
        .into_iter()
        .rev()
        .find(|i| i.branch == branch && reopenable(i));
    if let Some(closed) = closed {
        let url = closed.pr_url.clone().unwrap_or_default();
        let pr = crate::gh::pr(&repo.main_root, &url)?;
        if pr.state == "OPEN" {
            return reopen(
                &repo,
                &closed,
                &pr,
                &repo.toplevel,
                a.base.as_deref(),
                false,
            );
        }
        // A merged or closed PR is history: the branch starts a new item.
    }
    // Any other open PR on this branch is taken over. Best effort: without
    // `gh` or a GitHub remote, the item opens a PR of its own when it ships.
    let pr = crate::gh::pr(&repo.main_root, &branch)
        .ok()
        .filter(|p| p.state == "OPEN" && !p.cross_repo && p.head == branch);
    let title = a
        .title
        .clone()
        .or_else(|| pr.as_ref().map(|p| p.title.clone()))
        .unwrap_or_else(|| title_from_branch(&branch));
    let base = a
        .base
        .clone()
        .or_else(|| pr.as_ref().map(|p| p.base.clone()))
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| repo.config.base.clone());
    if let Some(p) = &pr {
        check_base(&repo, &base, None)?;
        ensure_has_head(&repo.toplevel, &branch, &p.head_oid)?;
    }
    let item = repo.hall.with_repo_lock(|| {
        let id: ItemId = repo.hall.next_item_id()?;
        let created = ev(
            &actor(),
            Some(id),
            Body::Created {
                title: title.clone(),
                branch: branch.clone(),
                worktree: paths::display(&repo.toplevel),
                base: base.clone(),
                issue: None,
                request: pr.as_ref().map(took_over),
                adopted: true,
                parent: None,
                follow_up: None,
                pr_url: pr.as_ref().map(|p| p.url.clone()),
                pr_head: pr.as_ref().map(|p| p.head_oid.clone()),
            },
        );
        repo.hall.append_item(id, &created)?;
        repo.hall.refresh_item_json(id)
    })?;
    adopted(&item, true, false)
}

/// `hall adopt --pr <pr>`: take over an open PR, checking its branch out in
/// a worktree when none has it.
fn adopt_pr(repo: &Repo, pr_ref: &str, a: &AdoptArgs) -> Result<Response> {
    let pr = crate::gh::pr(&repo.main_root, pr_ref)?;
    if pr.state != "OPEN" {
        return Err(HallError::new(
            ErrorCode::WrongState,
            format!(
                "PR #{} is {}; only an open PR can be adopted",
                pr.number,
                pr.state.to_lowercase()
            ),
        )
        .next([
            format!("hall new --follow-up {}", pr.url),
            "hall status --all".into(),
        ]));
    }
    if pr.cross_repo {
        return Err(HallError::new(
            ErrorCode::WrongState,
            format!(
                "PR #{} comes from a fork; Hallmark can only update a branch in this repository",
                pr.number
            ),
        )
        .next([format!("hall new --follow-up {}", pr.url)]));
    }
    let ix = repo.index()?;
    let all = ix.items(true)?;
    // An item that already holds this PR, or that shipped it.
    let holder = all
        .iter()
        .rev()
        .find(|i| i.pr_url.as_deref() == Some(pr.url.as_str()) && i.state != State::Dropped)
        .cloned()
        .or_else(|| {
            all.iter()
                .rev()
                .find(|i| i.branch == pr.head && !i.state.is_terminal())
                .cloned()
        });
    if let Some(it) = holder {
        if it.state == State::Closed {
            if !reopenable(&it) {
                return Err(HallError::new(
                    ErrorCode::WrongState,
                    format!("{} is closed and can't be reopened", it.id),
                )
                .next([format!("hall new --follow-up {}", pr.url)]));
            }
            let wt = worktree_of(repo, &it.branch).ok_or_else(|| no_worktree(&it))?;
            return reopen(repo, &it, &pr, &wt, a.base.as_deref(), true);
        }
        return existing_item(repo, it.id, a.base.as_deref());
    }
    // Nobody holds it: a new item on the PR's own branch.
    if git::branch_exists(&repo.main_root, &pr.head) && worktree_of(repo, &pr.head).is_none() {
        return Err(HallError::new(
            ErrorCode::WrongState,
            format!(
                "branch {} exists here but no worktree has it checked out",
                pr.head
            ),
        )
        .next([
            format!(
                "git worktree add ../{}-pr-{} {}",
                repo.name, pr.number, pr.head
            ),
            format!("cd ../{}-pr-{} && hall adopt", repo.name, pr.number),
        ]));
    }
    if let Some(wt) = worktree_of(repo, &pr.head) {
        // Adopt from the worktree that has the branch, as `hall adopt` there would.
        return adopt(
            &wt,
            AdoptArgs {
                title: a.title.clone(),
                pr: None,
                base: a.base.clone(),
            },
        );
    }
    git::run(&repo.main_root, &["fetch", "-q", "origin", &pr.head]).map_err(|e| {
        HallError::new(
            ErrorCode::CommandFailed,
            format!("could not fetch {} from origin: {}", pr.head, e.message),
        )
        .next(["git remote -v", "hall doctor"])
    })?;
    let start = format!("origin/{}", pr.head);
    let base = a.base.clone().unwrap_or_else(|| pr.base.clone());
    check_base(repo, &base, Some(pr.number))?;
    let item = repo.hall.with_repo_lock(|| {
        create(
            repo,
            Spec {
                title: a.title.clone().unwrap_or_else(|| pr.title.clone()),
                branch: Some(pr.head.clone()),
                request: Some(took_over(&pr)),
                issue: None,
                parent: None,
                follow_up: None,
                start: Some(&start),
                base: Some(base.clone()),
                adopted: true,
                pr: Some((pr.url.clone(), pr.head_oid.clone())),
            },
        )
    })?;
    adopted(&item, true, false)
}

/// A closed item that shipped a PR of its own (not a part merged into its
/// parent) can come back through that PR.
fn reopenable(i: &Item) -> bool {
    i.state == State::Closed
        && i.parent.is_none()
        && i.pr_url.as_deref().is_some_and(|u| u.contains("/pull/"))
}

/// Moves a shipped item back to development through its open PR.
fn reopen(
    repo: &Repo,
    item: &Item,
    pr: &crate::gh::GhPr,
    worktree: &Path,
    base: Option<&str>,
    via_pr: bool,
) -> Result<Response> {
    if pr.cross_repo {
        return Err(HallError::new(
            ErrorCode::WrongState,
            format!(
                "PR #{} comes from a fork; Hallmark can't update it",
                pr.number
            ),
        )
        .next([format!("hall new --follow-up {}", pr.url)]));
    }
    let base = base
        .map(str::to_string)
        .or_else(|| (!pr.base.is_empty()).then(|| pr.base.clone()))
        .unwrap_or_else(|| item.base.clone());
    check_base(repo, &base, via_pr.then_some(pr.number))?;
    // The PR still at the commit this item shipped hasn't moved: a rebase or
    // amend of it is what the update replaces.
    if item.pr_head.as_deref() != Some(pr.head_oid.as_str()) {
        ensure_has_head(worktree, &item.branch, &pr.head_oid)?;
    }
    let id = item.id;
    let url = pr.url.clone();
    let (item, reopened) = repo.hall.transact(id, |it, _| {
        if it.state != State::Closed {
            return Ok((vec![], false));
        }
        Ok((
            vec![ev(
                &actor(),
                Some(id),
                Body::Reopened {
                    pr_url: url.clone(),
                    pr_head: pr.head_oid.clone(),
                    base: base.clone(),
                    worktree: paths::display(worktree),
                },
            )],
            true,
        ))
    })?;
    if reopened {
        repo.hall.live(&format!(
            "{id} reopened from {url}: back in development, based on {base}"
        ));
    }
    adopted(&item, false, reopened)
}

/// An open item asked for again. `--base` can't move it: its base is what
/// it is reviewed against.
fn existing_item(repo: &Repo, id: ItemId, base: Option<&str>) -> Result<Response> {
    let item = repo.hall.load_item(id)?;
    if let Some(b) = base
        && b != item.base
    {
        return Err(HallError::new(
            ErrorCode::WrongState,
            format!(
                "{id} is already open, based on {}; --base only applies when an item is created or reopened",
                item.base
            ),
        )
        .next([format!("hall status {id}")]));
    }
    adopted(&item, false, false)
}

fn adopted(item: &Item, created: bool, reopened: bool) -> Result<Response> {
    let mut next = next_for(item);
    // A rebased branch no longer contains the PR's head: say what the
    // update push will replace.
    if let (Some(pr), Some(head)) = (&item.pr_url, &item.pr_head)
        && item.state == State::Development
        && !head.is_empty()
        && !git::ok(
            Path::new(&item.worktree),
            &["merge-base", "--is-ancestor", head, "HEAD"],
        )
    {
        next.insert(
            0,
            format!(
                "Note: HEAD doesn't contain {pr}'s head {}. When this item ships, its update \
                 replaces that commit (a rebase does this); to keep it, merge or rebase onto it first",
                crate::event::short(head)
            ),
        );
    }
    let out = NewOut {
        item: ItemView::from(item),
        created,
        reopened,
        open: format!("cd {} && claude", shell_quote(&item.worktree)),
        plan: None,
        children: Vec::new(),
        parent: item.parent,
        follow_up: item.follow_up.clone(),
    };
    Ok(Response::new(out, next))
}

/// The base must be a branch here: the review compares against it and the
/// PR targets it. Beyond the configured base, only the branch of an open PR
/// Hallmark shipped, still at the commit it reviewed, can be stacked on.
fn check_base(repo: &Repo, base: &str, pr: Option<u64>) -> Result<()> {
    if !git::branch_exists(&repo.main_root, base) {
        return Err(HallError::new(
            ErrorCode::WrongState,
            format!("the base {base} is not a branch in this repository"),
        )
        .next([
            format!("git fetch origin {base}:{base}"),
            "git branch --list".into(),
        ]));
    }
    if stackable_base(repo, base) {
        return Ok(());
    }
    Err(HallError::new(
        ErrorCode::WrongState,
        format!(
            "the base {base} is neither {} nor the branch of an open PR still at its hallmarked \
             commit; its commits would ship without review",
            repo.config.base
        ),
    )
    .next([
        match pr {
            Some(n) => format!("hall adopt --pr {n} --base {}", repo.config.base),
            None => format!("hall adopt --base {}", repo.config.base),
        },
        "hall status --all".into(),
    ]))
}

/// Whether `base` may be reviewed against and shipped into: the configured
/// base, or the local branch of an open PR whose item is hallmarked and whose
/// tip is still that hallmarked commit (what `hall new --follow-up` requires).
pub fn stackable_base(repo: &Repo, base: &str) -> bool {
    if base == repo.config.base {
        return true;
    }
    let Ok(ix) = repo.index() else {
        return false;
    };
    let Ok(items) = ix.items(true) else {
        return false;
    };
    let tip = git::run(
        &repo.main_root,
        &["rev-parse", &format!("{base}^{{commit}}")],
    )
    .map(|t| t.trim().to_string())
    .unwrap_or_default();
    items.iter().any(|i| {
        i.branch == base
            && !tip.is_empty()
            && i.hallmark_sha.as_deref() == Some(tip.as_str())
            && i.pr_url
                .as_deref()
                .is_some_and(|u| crate::gh::pr_state(&repo.main_root, u).is_ok_and(|s| s == "OPEN"))
    })
}

/// The worktree must hold the PR's head, so an update push only replaces
/// commits the item had. Fetches the branch; a clean worktree behind the PR
/// is fast-forwarded.
fn ensure_has_head(worktree: &Path, branch: &str, head: &str) -> Result<()> {
    let has = |w: &Path| git::ok(w, &["merge-base", "--is-ancestor", head, "HEAD"]);
    if head.is_empty() || has(worktree) {
        return Ok(());
    }
    let _ = git::run(worktree, &["fetch", "-q", "origin", branch]);
    if has(worktree) {
        return Ok(());
    }
    if git::ok(worktree, &["merge-base", "--is-ancestor", "HEAD", head])
        && git::dirty_files(worktree).is_ok_and(|d| d.is_empty())
        && git::run(worktree, &["merge", "-q", "--ff-only", head]).is_ok()
    {
        return Ok(());
    }
    Err(HallError::new(
        ErrorCode::WrongState,
        format!(
            "the PR's head {} is not in this worktree; an update would overwrite it",
            crate::event::short(head)
        ),
    )
    .next([format!("git rebase origin/{branch}"), "hall adopt".into()]))
}

/// The worktree that has `branch` checked out.
fn worktree_of(repo: &Repo, branch: &str) -> Option<std::path::PathBuf> {
    git::worktrees(&repo.main_root)
        .ok()?
        .into_iter()
        .find(|w| w.branch.as_deref() == Some(branch))
        .map(|w| w.path)
}

fn no_worktree(it: &Item) -> HallError {
    HallError::new(
        ErrorCode::WrongState,
        format!(
            "{} shipped from branch {}, which no worktree has checked out",
            it.id, it.branch
        ),
    )
    .next([
        format!(
            "git worktree add {} {}",
            shell_quote(&it.worktree),
            it.branch
        ),
        format!("cd {} && hall adopt", shell_quote(&it.worktree)),
    ])
}

/// The request of an item that took a PR over.
fn took_over(pr: &crate::gh::GhPr) -> String {
    format!(
        "Took over PR #{} \"{}\" ({}).\n\n{}",
        pr.number, pr.title, pr.url, pr.body
    )
    .trim()
    .to_string()
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
        ("base", spec.base.as_deref().unwrap_or(&repo.config.base)),
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
    // The new branch has no commits of its own yet, so it is reset to the
    // start point; if that fails, say so rather than build on the wrong code.
    if let Some(start) = spec.start
        && let (Ok(head), Ok(tip)) = (
            git::head(&worktree),
            git::run(&worktree, &["rev-parse", &format!("{start}^{{commit}}")]),
        )
        && head != tip.trim()
        && let Err(e) = git::run(&worktree, &["reset", "--hard", "-q", tip.trim()])
    {
        return Err(HallError::new(
            ErrorCode::CommandFailed,
            format!(
                "the worktree command ignored the start point {start}, and moving {branch} there failed: {}",
                e.message
            ),
        )
        .next(["git worktree list", "hall doctor"]));
    }
    let created = ev(
        &actor(),
        Some(id),
        Body::Created {
            title: spec.title,
            branch,
            worktree: paths::display(&worktree),
            base: spec.base.unwrap_or_else(|| repo.config.base.clone()),
            issue: spec.issue,
            request: spec.request,
            adopted: spec.adopted,
            parent: spec.parent,
            follow_up: spec.follow_up,
            pr_url: spec.pr.as_ref().map(|p| p.0.clone()),
            pr_head: spec.pr.map(|p| p.1),
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
fn shipped_by(repo: &Repo, url: &str) -> Option<Item> {
    repo.index()
        .ok()?
        .items(true)
        .ok()?
        .into_iter()
        .find(|i| i.pr_url.as_deref() == Some(url))
}

/// The request for a follow-up item: the PR and its review feedback.
pub fn follow_up_request(
    pr: &crate::gh::GhPr,
    shipped: Option<ItemId>,
    stacked_on: Option<&str>,
    extra: Option<&str>,
    failing_ci: &[(String, Option<String>)],
) -> String {
    let mut s = format!(
        "Follow-up to PR #{} \"{}\" ({})",
        pr.number, pr.title, pr.url
    );
    if let Some(id) = shipped {
        s.push_str(&format!(", which shipped {id}"));
    }
    if let Some(branch) = stacked_on {
        s.push_str(&format!(
            ". The PR is still open: this item starts from its branch, {branch}, and ships into it"
        ));
    }
    s.push_str(".\nAddress the CI failures and the review feedback below.\n");
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
    if !failing_ci.is_empty() {
        // After everything a person wrote, and capped as a whole, so the
        // final cut can only ever reach the logs.
        let each = 6_000 / failing_ci.len();
        s.push_str("\nFailing CI checks:\n");
        for (name, log) in failing_ci {
            s.push_str(&format!("- {name}\n"));
            if let Some(log) = log {
                s.push_str(&format!("\n```text\n{}\n```\n", clip_lines(log, each)));
            }
        }
    }
    // Keep the line breaks: the request carries lists and log excerpts.
    match s.char_indices().nth(12_000) {
        Some((cut, _)) => format!("{}\n…", &s[..cut]),
        None => s,
    }
}

/// Keeps the end of `s` within `max` bytes, on a line boundary.
fn clip_lines(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    let tail = &s[start..];
    let tail = tail.split_once('\n').map_or(tail, |(_, rest)| rest);
    format!("…\n{tail}")
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
            worktree_command("rig wt new {branch} --from {start}", &v, Some("hm/p")),
            "rig wt new hm/H-0002-x --from hm/p"
        );
        assert_eq!(
            worktree_command("rig wt new {branch} {start}", &v, None),
            "rig wt new hm/H-0002-x"
        );
        assert_eq!(
            worktree_command("rig wt new {branch}", &v, Some("hm/p")),
            "rig wt new hm/H-0002-x"
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
            state: "OPEN".into(),
            head: "hm/H-0007-retry".into(),
            base: "main".into(),
            head_oid: String::new(),
            cross_repo: false,
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
        let ci = vec![(
            "test (ubuntu-latest)".to_string(),
            Some("thread 'x' panicked at tests/setup.rs:405:14:\nText file busy".to_string()),
        )];
        let r = follow_up_request(
            &pr,
            Some(ItemId(7)),
            Some("hm/H-0007-retry"),
            Some("Keep it small."),
            &ci,
        );
        assert!(r.starts_with(
            "Follow-up to PR #12 \"Retry webhooks\" (https://github.com/acme/app/pull/12), which shipped H-0007. \
             The PR is still open: this item starts from its branch, hm/H-0007-retry, and ships into it."
        ), "{r}");
        assert!(r.contains(
            "Failing CI checks:\n- test (ubuntu-latest)\n\n```text\nthread 'x' panicked"
        ));
        assert!(r.contains("Text file busy"));
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
        assert!(follow_up_request(&quiet, None, None, None, &[]).contains("No review comments"));
    }
}
