//! Pauses (ADR 0016, ADR 0017). A worker that hits Claude's rate limit or
//! the daily budget returns `Progress::Pause`; the daemon then records
//! `paused` in the journal of every repository it serves, keeps the pause in
//! its state file (so a restart honors it), and skips the model workers
//! until `until`. Then it records `unpaused` in the same journals.

use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};

use crate::error::Result;
use crate::event::{Body, Event};
use crate::journal::Hall;
use crate::worker::Progress;

use super::Step;
use super::state::{DaemonState, Pause};

/// How long a pause lasts when the worker didn't say.
pub const DEFAULT_PAUSE_MINS: i64 = 30;

/// The workers that run models. A pause skips them; the watchman, the
/// shipper and notifications keep going.
pub const MODEL_WORKERS: &[&str] = &["resume", "assayer", "fixer", "librarian"];

/// The actor on `paused` and `unpaused`.
pub const ACTOR: &str = "daemon";

pub fn is_model_worker(name: &str) -> bool {
    MODEL_WORKERS.contains(&name)
}

/// The first pause a pass asked for: (repo, reason, until).
pub fn requested(steps: &[Step]) -> Option<(String, String, Option<DateTime<Utc>>)> {
    steps.iter().find_map(|s| match &s.progress {
        Progress::Pause { reason, until } => Some((s.repo.clone(), reason.clone(), *until)),
        _ => None,
    })
}

/// Starts a pause unless one is already in force: records `paused` in each
/// of `journals` and sets `state.paused`. Returns the new pause.
pub fn begin(
    state: &mut DaemonState,
    repo: &str,
    reason: &str,
    until: Option<DateTime<Utc>>,
    journals: &[Hall],
    now: DateTime<Utc>,
) -> Result<Option<Pause>> {
    if state.pause_at(now).is_some() {
        return Ok(None);
    }
    let until = until
        .filter(|u| *u > now)
        .unwrap_or(now + Duration::minutes(DEFAULT_PAUSE_MINS));
    let mut recorded = Vec::new();
    for hall in journals {
        if !hall.exists() {
            continue;
        }
        let e = Event::new(
            ACTOR,
            None,
            Body::Paused {
                reason: reason.to_string(),
                until: Some(until),
            },
        );
        hall.append(&hall.repo_events(), &e)?;
        hall.live(&format!(
            "daemon paused until {}: {reason} (from {repo})",
            until.format("%Y-%m-%d %H:%M UTC")
        ));
        recorded.push(hall.root.to_string_lossy().to_string());
    }
    let p = Pause {
        reason: reason.to_string(),
        until: Some(until),
        since: now,
        repo: Some(repo.to_string()),
        journals: recorded,
    };
    state.paused = Some(p.clone());
    Ok(Some(p))
}

/// Ends a pause whose time has come: records `unpaused` in the journals
/// that recorded it and clears `state.paused`. Returns the pause that
/// ended, if one did.
pub fn settle(state: &mut DaemonState, now: DateTime<Utc>) -> Result<Option<Pause>> {
    let Some(p) = state.paused.clone() else {
        return Ok(None);
    };
    if state.pause_at(now).is_some() {
        return Ok(None);
    }
    for j in &p.journals {
        let hall = Hall {
            root: PathBuf::from(j),
        };
        if !hall.exists() {
            continue;
        }
        hall.append(
            &hall.repo_events(),
            &Event::new(ACTOR, None, Body::Unpaused {}),
        )?;
        hall.live("daemon resumed after the pause");
    }
    state.paused = None;
    Ok(Some(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hall() -> (tempfile::TempDir, Hall) {
        let dir = tempfile::tempdir().unwrap();
        let hall = Hall::new(dir.path());
        hall.create().unwrap();
        (dir, hall)
    }

    fn step(worker: &str, progress: Progress) -> Step {
        Step {
            repo: "app".into(),
            worker: worker.into(),
            progress,
            error: None,
            skipped: None,
        }
    }

    fn names(hall: &Hall) -> Vec<&'static str> {
        hall.read(&hall.repo_events())
            .unwrap()
            .iter()
            .map(|e| e.body.name())
            .collect()
    }

    #[test]
    fn a_rate_limit_pauses_every_journal_once_and_resumes() {
        let (_a, app) = hall();
        let (_b, api) = hall();
        let now = Utc::now();
        let steps = vec![
            step("watchman", Progress::Idle),
            step(
                "assayer",
                Progress::Pause {
                    reason: "rate limited".into(),
                    until: Some(now + Duration::minutes(5)),
                },
            ),
        ];
        let (repo, reason, until) = requested(&steps).unwrap();
        assert_eq!(repo, "app");
        let mut st = DaemonState::default();
        let p = begin(
            &mut st,
            &repo,
            &reason,
            until,
            &[app.clone(), api.clone()],
            now,
        )
        .unwrap()
        .unwrap();
        assert_eq!(p.until, until);
        assert_eq!(p.journals.len(), 2);
        assert_eq!(names(&app), ["paused"]);
        assert_eq!(names(&api), ["paused"]);
        // A second request while paused changes nothing.
        assert!(
            begin(
                &mut st,
                "api",
                "again",
                None,
                std::slice::from_ref(&app),
                now
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(names(&app), ["paused"]);
        // Not yet time: still paused.
        assert!(settle(&mut st, now).unwrap().is_none());
        assert!(st.paused.is_some());
        // Time's up: unpaused in both journals, state cleared.
        let later = now + Duration::minutes(6);
        assert!(settle(&mut st, later).unwrap().is_some());
        assert!(st.paused.is_none());
        assert_eq!(names(&app), ["paused", "unpaused"]);
        assert_eq!(names(&api), ["paused", "unpaused"]);
        assert!(settle(&mut st, later).unwrap().is_none());
    }

    #[test]
    fn a_pause_without_a_time_lasts_thirty_minutes() {
        let (_d, app) = hall();
        let now = Utc::now();
        let mut st = DaemonState::default();
        let p = begin(&mut st, "app", "daily budget", None, &[app], now)
            .unwrap()
            .unwrap();
        assert_eq!(p.until, Some(now + Duration::minutes(DEFAULT_PAUSE_MINS)));
        assert!(requested(&[step("fixer", Progress::worked("fixed H-0001"))]).is_none());
        assert!(is_model_worker("assayer") && !is_model_worker("watchman"));
    }
}
