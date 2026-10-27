//! The principal engineer's run: one `claude -p` with the verdict schema,
//! in the assay worktree checked out at the frozen commit, isolated from
//! anything the branch under review could use to steer it.

use chrono::Utc;

use crate::config::Role;
use crate::error::Result;
use crate::event::{Body, Event, short};
use crate::findings::{Ledger, ModelVerdict};
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
    if let Some(p) = carry(repo, item, &events, &sha)? {
        return Ok(p);
    }
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
            let moved_on = || -> Result<Option<Progress>> {
                let now = repo.hall.load_item(item.id)?;
                if now.state != State::Review || now.frozen_sha.as_deref() != Some(sha.as_str()) {
                    let what = format!(
                        "{} review at {} discarded: the item moved on",
                        item.id,
                        short(&sha)
                    );
                    repo.hall.live(&what);
                    return Ok(Some(Progress::worked(what)));
                }
                Ok(None)
            };
            if let Some(p) = moved_on()? {
                return Ok(p);
            }
            // The pass is a second run: the lease covers it too.
            let _ = crate::lease::claim(
                &repo.hall,
                item.id,
                super::ROLE,
                &ctx.holder,
                super::lease_ttl(repo),
            );
            // A change on a `security_review` path gets a second,
            // security-only pass whose findings join this verdict (ADR
            // 0064). The checkout is still here.
            let (mv, pass) = match super::security::run(
                repo,
                ctx.runner,
                item,
                &events,
                &sha,
                &req.cwd,
                &review_base,
                mv,
            )? {
                super::security::Ran::Done(mv, pass) => (mv, pass),
                super::security::Ran::Stop(p) => return Ok(p),
            };
            // The pass can take as long as the review did.
            if let Some(p) = moved_on()? {
                return Ok(p);
            }
            // The reviewer read the person's words; the next run doesn't.
            // Only now: a round stopped or discarded before its verdict is
            // recorded is reviewed again with them (ADR 0044).
            crate::words::delivered(
                repo,
                item.id,
                "reviewer",
                crate::words::pending(&events, "reviewer").as_ref(),
                &events,
            )?;
            // The verifier tries to refute the blocking findings before
            // anyone acts on them (ADR 0062). The checkout is still here.
            let (mv, verification) =
                super::verify::verify(repo, ctx.runner, item, &sha, &req.cwd, &review_base, mv)?;
            // It can take minutes: look again before recording.
            if let Some(p) = moved_on()? {
                return Ok(p);
            }
            let rec = super::record_verdict(
                repo,
                item.id,
                &sha,
                &mv,
                super::ROLE,
                reviewer,
                Some(review_base),
                Some(&verification),
            )?;
            if !pass.paths.is_empty() {
                repo.hall.append_item(
                    item.id,
                    &Event::new(
                        super::ROLE,
                        Some(item.id),
                        Body::SecurityPass {
                            sha: sha.clone(),
                            round: rec.round,
                            paths: pass.paths.clone(),
                            model: pass.model.clone(),
                            added: pass.added.clone(),
                            skipped: pass.skipped.clone(),
                        },
                    ),
                )?;
            }
            Ok(Progress::worked(format!(
                "{} review round {}: {}{} ({} findings, {} blocker, {} major open{}{})",
                item.id,
                rec.round,
                super::verdict::kind(rec.verdict),
                if rec.escalated { ", escalated" } else { "" },
                rec.findings.len(),
                rec.open.blocker,
                rec.open.major,
                match (&pass.skipped, pass.paths.is_empty()) {
                    (_, true) => String::new(),
                    (Some(why), false) => format!("; no security pass: {why}"),
                    (None, false) => format!("; the security pass added {}", pass.added.len()),
                },
                if verification.dropped.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; the verifier dropped {} of {}",
                        verification.dropped.len(),
                        verification.checked
                    )
                }
            )))
        }
    }
}

/// A rebase that left the approved change as it was keeps its approval
/// (ADR 0049): the gates just passed at `sha`, and the reviewer already
/// approved this exact change, so no review runs. Records `rebased
/// {carried: true}`, an approving verdict by `carried`, and the hallmark,
/// whose note names the approved commit. None when it doesn't apply.
fn carry(repo: &Repo, item: &Item, events: &[Event], sha: &str) -> Result<Option<Progress>> {
    let base = super::base::Base::resolve(repo, &item.base)?;
    let merge_base = super::prompt::fork_point(repo, item, &base.sha, sha);
    let Some(cmp) = super::rebased::compare(repo, item, sha, &merge_base) else {
        return Ok(None);
    };
    // Something serious was reopened since the approval: a reviewer rules.
    let serious = Ledger::from_events(events)
        .open()
        .iter()
        .any(|t| t.finding.severity.blocks() && !t.finding.pre_existing);
    if !cmp.same() || serious {
        return Ok(None);
    }
    let id = item.id;
    let mv = ModelVerdict {
        summary: format!(
            "Approval carried from {}: the rebase onto {} left the approved change as it was, \
             and the gates passed at {}. No review ran.",
            short(&cmp.approved),
            base.name,
            short(sha)
        ),
        findings: vec![],
        prior: vec![],
        learnings: vec![],
    };
    let rec = super::record_verdict(
        repo,
        id,
        sha,
        &mv,
        super::ROLE,
        Some(CARRIED.into()),
        Some(base.sha.clone()),
        None,
    )?;
    // Only now is the approval known to be recorded.
    repo.hall.append_item(
        id,
        &Event::new(
            super::ROLE,
            Some(id),
            Body::Rebased {
                from: cmp.approved.clone(),
                to: sha.to_string(),
                onto: base.name.clone(),
                carried: true,
                checks: vec![],
                reason: Some(
                    "the rebase left the approved change as it was, and the gates passed".into(),
                ),
            },
        ),
    )?;
    crate::stamp::name_rebased_from(repo, sha, &cmp.approved)?;
    Ok(Some(Progress::worked(format!(
        "{id} review round {}: {}, approval carried from {} (the same change after the rebase)",
        rec.round,
        super::verdict::kind(rec.verdict),
        short(&cmp.approved)
    ))))
}

/// The reviewer named on a verdict that carried an approval across a
/// rebase, with no review run.
pub const CARRIED: &str = "carried";

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
    if let Some(c) = &review.compared {
        let reason = if review.rebase_of.is_some() {
            format!(
                "rebase review: only the resolution ({} lines in {} files) since {}",
                c.lines,
                c.files.len(),
                short(&c.approved)
            )
        } else {
            format!(
                "full review: the resolution changed {} lines of the change approved at {}",
                c.lines,
                short(&c.approved)
            )
        };
        let seen = events.iter().any(|e| {
            matches!(&e.body, Body::Rebased { to, reason: Some(r), .. } if to == sha && *r == reason)
        });
        if !seen {
            repo.hall.append_item(
                item.id,
                &Event::new(
                    super::ROLE,
                    Some(item.id),
                    Body::Rebased {
                        from: c.approved.clone(),
                        to: sha.to_string(),
                        onto: review.base.name.clone(),
                        carried: false,
                        checks: vec![],
                        reason: Some(reason),
                    },
                ),
            )?;
        }
    }
    if let Some(a) = &review.rebase_of {
        repo.hall.live(&format!(
            "{} review round {}: a rebase review, only what the rebase changed since {}",
            item.id,
            item.round,
            short(a)
        ));
    }
    Ok((req, review.base.sha))
}

/// PATH with this binary's directory first, so the reviewer's
/// `hall log` is this `hall`.
pub(super) fn path_with_hall() -> Option<String> {
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
