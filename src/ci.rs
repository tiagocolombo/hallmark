//! The CI watch (ADR 0025): after the shipper opens a PR, the daemon follows
//! its checks until they finish, and records the result. A red check on an
//! open PR is the Delivery office's to fix (ADR 0031); one on a merged PR is
//! a notice until a follow-up item (`hall new --follow-up <pr>`) takes it on.
//! Jobs that never started (billing, no runners) aren't code failures: they
//! are recorded as `not_started` and reach you as one `ci_down` entry.
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
    /// What was already logged, so a stuck PR doesn't fill the live log.
    #[serde(default)]
    noted: std::collections::BTreeSet<String>,
}

/// The shipped items whose PR checks haven't finished, oldest check first.
pub fn watched(items: &[Item], now: DateTime<Utc>) -> Vec<Item> {
    items
        .iter()
        .filter(|i| {
            i.state == State::Closed
                && i.pr_url.as_deref().is_some_and(|u| u.contains("/pull/"))
                && match &i.ci {
                    None => now - i.state_since < Duration::hours(GIVE_UP_HOURS),
                    // A failed PR is watched until it turns green or closes.
                    // An outage or a PR the office gave up on stays watched
                    // while it is open, so a green re-run clears its entry.
                    Some(c) => {
                        !c.passed
                            && c.ended.is_none()
                            && i.delivered.is_none()
                            && (c.not_started.is_some()
                                || i.ci_fixes >= crate::delivery::CI_FIXES
                                || i.ci_declined.is_some()
                                || now - c.t < Duration::hours(GIVE_UP_HOURS))
                    }
                }
        })
        .cloned()
        .collect()
}

pub fn tick(ctx: &WorkerCtx) -> Result<Progress> {
    let repo = ctx.repo;
    let now = Utc::now();
    let items = repo.index()?.items(true)?;
    let candidates = watched(&items, now);
    let path = repo.hall.root.join("ci-watch.json");
    let mut watch: Watch = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    // Say once when the watch gave up on a PR whose checks never finished.
    let mut changed = false;
    for i in items.iter().filter(|i| {
        i.state == State::Closed
            && i.ci.is_none()
            && i.pr_url.as_deref().is_some_and(|u| u.contains("/pull/"))
            && now - i.state_since >= Duration::hours(GIVE_UP_HOURS)
    }) {
        if watch.noted.insert(format!("gave-up-{}", i.id)) {
            changed = true;
            repo.hall.live(&format!(
                "{} CI watch gave up after {GIVE_UP_HOURS}h: {} never reported finished checks",
                i.id,
                i.pr_url.as_deref().unwrap_or("its PR")
            ));
        }
    }
    if changed {
        let _ = std::fs::write(&path, serde_json::to_string(&watch)?);
    }
    if candidates.is_empty() {
        return Ok(Progress::Idle);
    }
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
            // Once per item, not once a minute.
            if watch.noted.insert(format!("error-{}", item.id)) {
                repo.hall
                    .live(&format!("{} CI watch: {}", item.id, e.message));
                let _ = std::fs::write(&path, serde_json::to_string(&watch)?);
            }
            return Ok(Progress::Idle);
        }
    };
    // After a failure the watch goes on while the PR is open: a re-run that
    // goes green, or a fix pushed on GitHub, clears the inbox entry.
    if item.ci.as_ref().is_some_and(|c| !c.passed) {
        // A failure recorded before `not_started` existed (or before the
        // outage was told apart): record it again so it joins `ci_down`.
        if item
            .ci
            .as_ref()
            .is_some_and(|c| c.not_started.is_none() && c.ended.is_none())
            && let Outcome::Failed(f) = conclude(&checks)
            && not_started(&repo.main_root, &f).is_some()
        {
            return record(ctx, item.id, &url, false, f, None);
        }
        return match conclude(&checks) {
            Outcome::Passed => record(ctx, item.id, &url, true, vec![], None),
            _ => {
                let ended = crate::gh::pr_state(&repo.main_root, &url)
                    .ok()
                    .and_then(|s| ended_by(&s));
                if let Some(ended) = ended {
                    // Closed or merged with the checks still red: stop
                    // watching, but never record that they passed.
                    let failing = match conclude(&checks) {
                        Outcome::Failed(f) => f,
                        _ => item
                            .ci
                            .as_ref()
                            .map(|c| {
                                c.failing
                                    .iter()
                                    .map(|n| FailingCheck {
                                        name: n.clone(),
                                        link: None,
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    };
                    record(ctx, item.id, &url, false, failing, Some(ended))
                } else {
                    Ok(Progress::Idle)
                }
            }
        };
    }
    match conclude(&checks) {
        Outcome::Waiting => Ok(Progress::Idle),
        Outcome::Passed => record(ctx, item.id, &url, true, vec![], None),
        Outcome::Failed(failing) => record(ctx, item.id, &url, false, failing, None),
    }
}

/// GitHub's reason when none of the failing checks ever started, else None.
/// Only checks with a job link are asked; one that ran settles it.
pub fn not_started(dir: &std::path::Path, failing: &[FailingCheck]) -> Option<String> {
    let links: Vec<&str> = failing.iter().filter_map(|f| f.link.as_deref()).collect();
    if links.is_empty() || links.len() < failing.len() {
        return None;
    }
    let mut why = None;
    for l in links {
        why = Some(crate::gh::never_started(dir, l)?);
    }
    why
}

/// `merged` or `closed` for a PR that left the open state.
pub fn ended_by(state: &str) -> Option<String> {
    match state {
        "MERGED" => Some("merged".into()),
        "CLOSED" => Some("closed".into()),
        _ => None,
    }
}

fn record(
    ctx: &WorkerCtx,
    id: ItemId,
    url: &str,
    passed: bool,
    failing: Vec<FailingCheck>,
    ended: Option<String>,
) -> Result<Progress> {
    let names: Vec<String> = failing.iter().map(|f| f.name.clone()).collect();
    let not_started = if passed {
        None
    } else {
        not_started(&ctx.repo.main_root, &failing)
    };
    ctx.repo.hall.append_item(
        id,
        &Event::new(
            "ci",
            Some(id),
            Body::PrChecks {
                pr_url: url.to_string(),
                passed,
                failing,
                ended: ended.clone(),
                not_started: not_started.clone(),
            },
        ),
    )?;
    let what = if passed {
        format!("{id}: CI passed on {url}")
    } else if let Some(why) = &not_started {
        format!("{id}: CI never ran on {url}: {why}")
    } else if let Some(e) = &ended {
        format!("{id}: {url} {e} with CI failing: {}", names.join(", "))
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
    fn a_pr_that_ended_red_never_counts_as_passed() {
        assert_eq!(ended_by("OPEN"), None);
        assert_eq!(ended_by("MERGED").as_deref(), Some("merged"));
        assert_eq!(ended_by("CLOSED").as_deref(), Some("closed"));
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
