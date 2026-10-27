//! The security pass (ADR 0064): a second, security-only review of a
//! change that touches a `security_review` path, whose findings join the
//! principal engineer's verdict before it is recorded.
//!
//! The main review applies seven lenses at once; a change to auth,
//! billing, an installer or a workflow deserves one run that asks only
//! "who can reach this, and what can they do?", at the deepest effort,
//! the way Anthropic runs a separate security review on every PR. The
//! pass reuses the reviewer's agent, method and schema with its lenses
//! narrowed to `security_tenancy`. Only its new findings count: its
//! rulings on earlier findings and its learnings are the main review's
//! job. When it can't run, the verdict stands as the reviewer gave it.

use chrono::Utc;

use crate::config::Role;
use crate::error::Result;
use crate::event::{Event, short};
use crate::findings::{Lens, ModelFinding, ModelVerdict};
use crate::item::Item;
use crate::repo::Repo;
use crate::runner::{self, Metered, Runner};

/// The run's role, for the fake runner, the live log and usage. It shares
/// the review budget (`budget::role_of`).
pub const ROLE: &str = "security";

/// What the pass did, for the journal and the report.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SecurityPass {
    /// The paths that asked for it.
    pub paths: Vec<String>,
    pub model: Option<String>,
    /// Titles of the findings it added to the verdict.
    pub added: Vec<String>,
    /// Why it didn't run or didn't count: the verdict stands as the
    /// reviewer gave it.
    pub skipped: Option<String>,
}

/// The changed files under a `security_review` glob, read from the base
/// like the other review settings.
pub fn paths(repo: &Repo, base: &super::base::Base, merge_base: &str, sha: &str) -> Vec<String> {
    let globs = &base.config.security_review;
    if globs.is_empty() {
        return Vec::new();
    }
    let set = super::base::globset(globs);
    crate::git::run(
        &repo.main_root,
        &["diff", "--name-only", "--no-renames", merge_base, sha],
    )
    .unwrap_or_default()
    .lines()
    .map(str::trim)
    .filter(|f| !f.is_empty() && set.is_match(f))
    .map(String::from)
    .collect()
}

/// Runs the pass when a changed path asks for it and folds its findings
/// into `mv`. `wt` is the review checkout; `base_sha` the base the rules
/// came from.
#[allow(clippy::too_many_arguments)]
pub fn run(
    repo: &Repo,
    runner_: &dyn Runner,
    item: &Item,
    events: &[Event],
    sha: &str,
    wt: &std::path::Path,
    base_sha: &str,
    mv: ModelVerdict,
) -> Result<(ModelVerdict, SecurityPass)> {
    let base = super::base::Base::at(repo, &item.base, base_sha);
    let merge_base = super::prompt::fork_point(repo, item, &base.sha, sha);
    let paths = paths(repo, &base, &merge_base, sha);
    if paths.is_empty() {
        return Ok((
            mv,
            SecurityPass {
                skipped: Some("no `security_review` path in the change".into()),
                ..Default::default()
            },
        ));
    }
    let pass = super::prompt::Pass {
        title: "Security pass".into(),
        lenses: vec![Lens::SecurityTenancy],
        note: format!(
            "This is a second, security-only review of a change the principal engineer has \
             already reviewed with every lens. It runs because the change touches {}: paths \
             this repository marks `security_review`. Apply only the security and tenancy \
             lens, at full depth: who can reach each changed path, whether authorization is \
             checked on every entry, whether one tenant can see or change another's data, \
             whether user input is executed, interpolated into a query, a path or a shell, or \
             logged with secrets, and whether a new dependency, permission or workflow step \
             widens what can run. Report only what you can show: a concrete path to the \
             break. Your rulings on earlier findings and your learnings are not used; your \
             summary and your new findings are added to the review.",
            paths
                .iter()
                .map(|p| format!("`{p}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    let review = super::prompt::prepare_with(repo, item, events, sha, None, Some(&pass))?;
    let mut req = runner::request(
        repo,
        Role::Reviewer,
        Some(super::review::AGENT),
        review.prompt.clone(),
        wt,
    )?;
    req.role = ROLE.into();
    req.json_schema = Some(crate::plugin::file("schemas/verdict.json").into());
    req.allowed_tools = super::review::TOOLS.iter().map(|t| t.to_string()).collect();
    req.effort = review.effort.clone();
    let dir = repo.hall.item_runs(item.id);
    std::fs::create_dir_all(&dir)?;
    let _ = std::fs::write(
        dir.join(format!(
            "{}-security-r{}.prompt.md",
            Utc::now().format("%Y%m%dT%H%M%S"),
            item.round
        )),
        &review.prompt,
    );
    repo.hall.live(&format!(
        "{} security pass round {} at {}: touches {}",
        item.id,
        item.round,
        short(sha),
        paths.join(", ")
    ));
    let outcome = match runner::run_metered(repo, runner_, Some(item.id), &req)? {
        Metered::Done(o) => o,
        Metered::Blocked(stop) => {
            return Ok((
                mv,
                SecurityPass {
                    paths,
                    skipped: Some(format!("didn't run: {}", stop.reason)),
                    ..Default::default()
                },
            ));
        }
        Metered::Failed { failure, .. } => {
            return Ok((
                mv,
                SecurityPass {
                    paths,
                    skipped: Some(format!("the run failed: {}", failure.describe())),
                    ..Default::default()
                },
            ));
        }
    };
    let model = outcome
        .result
        .as_ref()
        .and_then(|r| {
            r.model_usage
                .iter()
                .max_by(|a, b| a.1.cost_usd.total_cmp(&b.1.cost_usd))
                .map(|(m, _)| m.clone())
        })
        .or(req.model.clone());
    let theirs = match ModelVerdict::parse(&outcome.structured().cloned().unwrap_or_default()) {
        Ok(v) => v,
        Err(e) => {
            return Ok((
                mv,
                SecurityPass {
                    paths,
                    model,
                    skipped: Some(format!("its verdict didn't parse: {}", e.message)),
                    ..Default::default()
                },
            ));
        }
    };
    let (merged, added) = merge(mv, theirs);
    Ok((
        merged,
        SecurityPass {
            paths,
            model,
            added,
            skipped: None,
        },
    ))
}

/// Adds the pass's findings to the main verdict (its rulings and learnings
/// are dropped), skipping one the reviewer already raised at the same file
/// and line with the same title, and appends its summary. Returns the
/// titles added.
pub fn merge(mut mv: ModelVerdict, theirs: ModelVerdict) -> (ModelVerdict, Vec<String>) {
    let same = |a: &ModelFinding, b: &ModelFinding| {
        a.file == b.file && a.line == b.line && a.title.trim().eq_ignore_ascii_case(b.title.trim())
    };
    let mut added = Vec::new();
    for f in theirs.findings {
        if mv.findings.iter().any(|m| same(m, &f)) {
            continue;
        }
        added.push(f.title.trim().to_string());
        mv.findings.push(f);
    }
    let s = theirs.summary.trim();
    if !s.is_empty() {
        mv.summary = format!("{}\n\nSecurity pass: {s}", mv.summary.trim_end());
    }
    (mv, added)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::findings::Severity;

    fn mf(sev: Severity, lens: Lens, line: u32, title: &str) -> ModelFinding {
        ModelFinding {
            lens,
            severity: sev,
            file: "auth.rs".into(),
            line,
            title: title.into(),
            scenario: "s".into(),
            fix: "f".into(),
        }
    }

    fn mv(summary: &str, findings: Vec<ModelFinding>) -> ModelVerdict {
        ModelVerdict {
            summary: summary.into(),
            findings,
            prior: vec![],
            learnings: vec![],
        }
    }

    #[test]
    fn the_pass_adds_new_findings_and_its_summary_and_nothing_else() {
        let main = mv(
            "Adds a login path.",
            vec![mf(
                Severity::Major,
                Lens::Correctness,
                10,
                "Retries forever",
            )],
        );
        let mut theirs = mv(
            "The token check can be skipped.",
            vec![
                mf(
                    Severity::Blocker,
                    Lens::SecurityTenancy,
                    42,
                    "Token check skipped",
                ),
                // The same thing the reviewer found: not twice.
                mf(
                    Severity::Major,
                    Lens::SecurityTenancy,
                    10,
                    " retries FOREVER ",
                ),
            ],
        );
        theirs.prior.push(crate::findings::PriorRuling {
            id: "F1".into(),
            status: crate::findings::PriorStatus::Fixed,
            note: None,
        });
        let (merged, added) = merge(main, theirs);
        assert_eq!(added, ["Token check skipped"]);
        assert_eq!(merged.findings.len(), 2);
        assert_eq!(merged.findings[1].title, "Token check skipped");
        assert!(merged.prior.is_empty(), "the pass rules on nothing");
        assert_eq!(
            merged.summary,
            "Adds a login path.\n\nSecurity pass: The token check can be skipped."
        );
        // An empty summary adds no paragraph.
        let (m, _) = merge(mv("s", vec![]), mv("  ", vec![]));
        assert_eq!(m.summary, "s");
    }
}
