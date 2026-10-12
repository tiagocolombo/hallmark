//! The principal engineer's run: one `claude -p` with the verdict schema,
//! in the assay worktree checked out at the frozen commit, isolated from
//! anything the branch under review could use to steer it.

use chrono::Utc;

use crate::config::Role;
use crate::error::Result;
use crate::event::{Body, Event, short};
use crate::findings::ModelVerdict;
use crate::item::{HoldKind, Item, State};
use crate::repo::Repo;
use crate::runner::{self, Failure, Metered, RunRequest};
use crate::worker::{Progress, WorkerCtx};

/// The plugin agent, namespaced: a bare name would let a branch's own
/// `.claude/agents/principal-engineer.md` replace the reviewer.
pub const AGENT: &str = "hallmark:principal-engineer";

/// The reviewer's tools: read-only. Kept equal to the agent's frontmatter
/// (a test checks). An agent with a `tools:` list must name
/// `StructuredOutput`, or `--json-schema` silently yields no
/// `structured_output` (found in the live check, ADR 0018).
pub const TOOLS: &[&str] = &[
    "Read",
    "Grep",
    "Glob",
    "Bash(git diff:*)",
    "Bash(git log:*)",
    "Bash(git show:*)",
    "Bash(hall log:*)",
    "Bash(hall library search:*)",
    "StructuredOutput",
];

/// Reviews the item at its frozen commit and records the verdict.
pub fn run(ctx: &WorkerCtx, item: &Item) -> Result<Progress> {
    let repo = ctx.repo;
    let Some(sha) = item.frozen_sha.clone() else {
        return Ok(Progress::Idle);
    };
    let events = repo.hall.read_item(item.id)?;
    let (req, review_base) = request(repo, item, &events, &sha)?;
    // The review checkout goes away on every exit, verdict or not.
    struct Cleanup<'a>(&'a crate::repo::Repo, crate::item::ItemId);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            super::remove_review_checkout(self.0, self.1);
        }
    }
    let _cleanup = Cleanup(repo, item.id);
    match runner::run_metered(repo, ctx.runner, Some(item.id), &req)? {
        Metered::Blocked(stop) => Ok(stop.progress()),
        Metered::Failed {
            failure: Failure::RateLimited { until },
            ..
        } => Ok(Progress::Pause {
            reason: format!("{} review: Claude's plan limit", item.id),
            until,
        }),
        Metered::Failed { failure, held, .. } => Ok(Progress::worked(format!(
            "{} review failed: {}{}",
            item.id,
            failure.describe(),
            if held {
                "; held for you"
            } else {
                "; will retry"
            }
        ))),
        Metered::Done(outcome) => {
            let reviewer = outcome
                .result
                .as_ref()
                .and_then(|r| {
                    r.model_usage
                        .iter()
                        .max_by(|a, b| a.1.cost_usd.total_cmp(&b.1.cost_usd))
                        .map(|(m, _)| m.clone())
                })
                .or(req.model.clone());
            let structured = outcome.structured().cloned().unwrap_or_default();
            let mv = match ModelVerdict::parse(&structured) {
                Ok(mv) => mv,
                Err(e) => return invalid(repo, item, &e.message),
            };
            // Pulled back or dropped while the reviewer ran: discard.
            let now = repo.hall.load_item(item.id)?;
            if now.state != State::Review || now.frozen_sha.as_deref() != Some(sha.as_str()) {
                let what = format!(
                    "{} review at {} discarded: the item moved on",
                    item.id,
                    short(&sha)
                );
                repo.hall.live(&what);
                return Ok(Progress::worked(what));
            }
            let rec = super::record_verdict(
                repo,
                item.id,
                &sha,
                &mv,
                super::ROLE,
                reviewer,
                Some(review_base),
            )?;
            Ok(Progress::worked(format!(
                "{} review round {}: {}{} ({} findings, {} blocker, {} major open)",
                item.id,
                rec.round,
                super::verdict::kind(rec.verdict),
                if rec.escalated { ", escalated" } else { "" },
                rec.findings.len(),
                rec.open.blocker,
                rec.open.major
            )))
        }
    }
}

/// The run request for reviewing `item` at `sha`, and the base commit its
/// rules came from. The prompt is also saved under `runs/`.
pub fn request(
    repo: &Repo,
    item: &Item,
    events: &[Event],
    sha: &str,
) -> Result<(RunRequest, String)> {
    let default_effort = repo.config.budget.effort(Role::Reviewer);
    let review = super::prompt::prepare(repo, item, events, sha, default_effort.as_deref())?;
    let wt = super::checkout_review(repo, item.id, sha)?.path;
    let mut req = runner::request(
        repo,
        Role::Reviewer,
        Some(AGENT),
        review.prompt.clone(),
        &wt,
    )?;
    req.json_schema = Some(crate::plugin::file("schemas/verdict.json").into());
    req.allowed_tools = TOOLS.iter().map(|t| t.to_string()).collect();
    req.effort = review.effort.clone();
    if let Some(path) = path_with_hall() {
        req.env.push(("PATH".into(), path));
    }
    let dir = repo.hall.item_runs(item.id);
    std::fs::create_dir_all(&dir)?;
    let _ = std::fs::write(
        dir.join(format!(
            "{}-reviewer-r{}.prompt.md",
            Utc::now().format("%Y%m%dT%H%M%S"),
            item.round
        )),
        &review.prompt,
    );
    repo.hall.live(&format!(
        "{} review round {} at {}: {} files, {} lines{}{}",
        item.id,
        item.round,
        short(sha),
        review.changed.len(),
        review.lines,
        review
            .since
            .as_deref()
            .map(|s| format!(", since {}", short(s)))
            .unwrap_or_default(),
        review
            .effort
            .as_deref()
            .map(|e| format!(", effort {e}"))
            .unwrap_or_default()
    ));
    Ok((req, review.base.sha))
}

/// PATH with this binary's directory first, so the reviewer's
/// `hall log` is this `hall`.
fn path_with_hall() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    let mut dirs = vec![dir];
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    std::env::join_paths(dirs)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// A structured output that passed the schema but not Hallmark's checks
/// (an empty summary, say): a failed run, held after the second.
fn invalid(repo: &Repo, item: &Item, why: &str) -> Result<Progress> {
    let attempt = runner::failures_in_a_row(&repo.hall.read_item(item.id)?, "reviewer") + 1;
    let append = |body: Body| {
        repo.hall
            .append_item(item.id, &Event::new(super::ROLE, Some(item.id), body))
    };
    append(Body::RunFailed {
        role: "reviewer".into(),
        reason: why.to_string(),
        attempt,
    })?;
    let held = attempt >= 2;
    if held {
        append(Body::Held {
            kind: HoldKind::RunFailed,
            reason: format!("the reviewer run failed twice: {why}"),
        })?;
    }
    Ok(Progress::worked(format!(
        "{} review failed: {why}{}",
        item.id,
        if held {
            "; held for you"
        } else {
            "; will retry"
        }
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_allowed_tools_are_the_agents_tools() {
        let agent = crate::plugin::file("agents/principal-engineer.md");
        let line = agent
            .lines()
            .find_map(|l| l.strip_prefix("tools:"))
            .expect("the agent lists its tools");
        let tools: Vec<&str> = line.split(',').map(str::trim).collect();
        assert_eq!(tools, TOOLS);
        // Read-only: nothing that writes or runs arbitrary commands.
        for t in TOOLS {
            assert!(
                !["Edit", "Write", "Bash", "NotebookEdit"].contains(t),
                "{t}"
            );
        }
        assert!(AGENT.ends_with(":principal-engineer"));
    }

    #[test]
    fn the_schema_is_one_claude_accepts() {
        // Claude Code rejects a `--json-schema` that names a `$schema`
        // dialect ("no schema with key or ref …/2020-12/schema").
        let v: serde_json::Value =
            serde_json::from_str(crate::plugin::file("schemas/verdict.json")).unwrap();
        assert!(v.get("$schema").is_none());
        assert_eq!(v["type"], "object");
    }

    #[test]
    fn the_method_is_the_skill_without_frontmatter() {
        let m = super::super::prompt::method();
        assert!(m.starts_with("# Principal review"), "{m}");
        assert!(!m.contains("description:"));
        for lens in crate::findings::Lens::ALL {
            assert!(m.contains(&format!("`{}`", lens.as_str())), "{lens:?}");
        }
    }

    #[test]
    fn the_method_audits_tests_before_the_lenses_and_refutes_last() {
        // ADR 0036: the reviewer audits the tests first and tries to
        // disprove its findings before returning them.
        let m = super::super::prompt::method();
        let at = |h: &str| m.find(h).unwrap_or_else(|| panic!("{h} missing"));
        assert!(at("## The test audit") < at("## The seven lenses"));
        assert!(at("## The seven lenses") < at("## The refutation pass"));
        assert!(at("## The refutation pass") < at("## The verdict"));
    }
}
