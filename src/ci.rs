//! The CI watch (ADR 0025): after the shipper opens a PR, the daemon follows
//! its checks until they finish, and records the result. A red check is
//! feedback on a closed item: it lands in the inbox (`ci_failed`) until a
//! follow-up item (`hall new --follow-up <pr>`) takes it on.
//!
//! The local gates run on one machine; CI runs the full matrix. H-0001 was
//! the reason: a Linux-only test race passed the gates and the review, failed
//! CI, and nothing noticed.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::event::{Body, Event, FailingCheck};
use crate::gh::GhCheck;
use crate::item::{Item, ItemId, State};
use crate::worker::{Progress, WorkerCtx};

/// How often one PR's checks are read.
const EVERY: i64 = 60;
/// After this long without a result, the watch gives up.
const GIVE_UP_HOURS: i64 = 24;

/// What a PR's checks say.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Some still run, or none have started yet.
    Waiting,
    Passed,
    Failed(Vec<FailingCheck>),
}

/// Reads a PR's checks: failed as soon as one failed or was cancelled,
/// passed once every check finished without failing.
pub fn conclude(checks: &[GhCheck]) -> Outcome {
    let failing: Vec<FailingCheck> = checks
        .iter()
        .filter(|c| matches!(c.bucket.as_str(), "fail" | "cancel"))
        .map(|c| FailingCheck {
            name: c.name.clone(),
            link: c.link.clone(),
        })
        .collect();
    if !failing.is_empty() {
        return Outcome::Failed(failing);
    }
    if checks.is_empty() || checks.iter().any(|c| c.bucket == "pending") {
        return Outcome::Waiting;
    }
    Outcome::Passed
}

/// When each item's PR was last read, so a pass reads at most one PR and
/// each PR at most once a minute.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Watch {
    #[serde(default)]
    last: BTreeMap<String, DateTime<Utc>>,
}

/// The shipped items whose PR checks haven't finished, oldest check first.
pub fn watched(items: &[Item], now: DateTime<Utc>) -> Vec<Item> {
    items
        .iter()
        .filter(|i| {
            i.state == State::Closed
                && i.ci.is_none()
                && i.pr_url.as_deref().is_some_and(|u| u.contains("/pull/"))
                && now - i.state_since < Duration::hours(GIVE_UP_HOURS)
        })
        .cloned()
        .collect()
}

pub fn tick(ctx: &WorkerCtx) -> Result<Progress> {
    let repo = ctx.repo;
    let now = Utc::now();
    let items = repo.index()?.items(true)?;
    let candidates = watched(&items, now);
    if candidates.is_empty() {
        return Ok(Progress::Idle);
    }
    let path = repo.hall.root.join("ci-watch.json");
    let mut watch: Watch = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let due = candidates
        .into_iter()
        .filter(|i| {
            watch
                .last
                .get(&i.id.to_string())
                .is_none_or(|t| now - *t >= Duration::seconds(EVERY))
        })
        .min_by_key(|i| watch.last.get(&i.id.to_string()).copied());
    let Some(item) = due else {
        return Ok(Progress::Idle);
    };
    let url = item.pr_url.clone().unwrap_or_default();
    watch.last.insert(item.id.to_string(), now);
    let _ = std::fs::write(&path, serde_json::to_string(&watch)?);
    let checks = match crate::gh::pr_checks(&repo.main_root, &url) {
        Ok(c) => c,
        Err(e) => {
            repo.hall
                .live(&format!("{} CI watch: {}", item.id, e.message));
            return Ok(Progress::Idle);
        }
    };
    match conclude(&checks) {
        Outcome::Waiting => Ok(Progress::Idle),
        Outcome::Passed => record(ctx, item.id, &url, true, vec![]),
        Outcome::Failed(failing) => record(ctx, item.id, &url, false, failing),
    }
}

fn record(
    ctx: &WorkerCtx,
    id: ItemId,
    url: &str,
    passed: bool,
    failing: Vec<FailingCheck>,
) -> Result<Progress> {
    let names: Vec<String> = failing.iter().map(|f| f.name.clone()).collect();
    ctx.repo.hall.append_item(
        id,
        &Event::new(
            "ci",
            Some(id),
            Body::PrChecks {
                pr_url: url.to_string(),
                passed,
                failing,
            },
        ),
    )?;
    let what = if passed {
        format!("{id}: CI passed on {url}")
    } else {
        format!("{id}: CI failed on {url}: {}", names.join(", "))
    };
    ctx.repo.hall.live(&what);
    Ok(Progress::worked(what))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, bucket: &str) -> GhCheck {
        GhCheck {
            name: name.into(),
            bucket: bucket.into(),
            link: Some(format!("https://github.com/o/r/actions/runs/1/job/{name}")),
        }
    }

    #[test]
    fn checks_conclude_once_they_finish() {
        assert_eq!(conclude(&[]), Outcome::Waiting);
        assert_eq!(
            conclude(&[check("fmt", "pass"), check("test", "pending")]),
            Outcome::Waiting
        );
        assert_eq!(
            conclude(&[check("fmt", "pass"), check("lint", "skipping")]),
            Outcome::Passed
        );
        // A failure is final even while other checks still run.
        match conclude(&[
            check("test (ubuntu-latest)", "fail"),
            check("win", "pending"),
        ]) {
            Outcome::Failed(f) => {
                assert_eq!(f.len(), 1);
                assert_eq!(f[0].name, "test (ubuntu-latest)");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            conclude(&[check("test", "cancel")]),
            Outcome::Failed(_)
        ));
    }
}
