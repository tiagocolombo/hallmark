//! Answers reach the asking session (workstream D, ADR 0020): defaults
//! after a question's wait expires, and `claude -p --resume <session>` with
//! the answer.
//!
//! Each tick:
//!
//! 1. **Defaults.** Every unanswered question with a `default` whose
//!    `deadline` has passed gets `answer { defaulted: true }` from `hall`.
//!    Escalations aren't questions, so they never take a default.
//! 2. **One resume.** The oldest answered question that has a session, no
//!    `answer_delivered`, and wasn't given up on is sent to its session
//!    through [`runner::run_metered`]. Success records `answer_delivered`.
//!    A failure is retried on the next tick; the second failure in a row
//!    gives up: a `notification` (type `resume_failed`) files the session
//!    as waiting in the inbox, so you can resume it yourself, and an item
//!    question's item is held (`run_failed`) like any run. Questions
//!    without a session can't be resumed and simply stay answered.

use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::event::{Body, Event};
use crate::index::{Index, QuestionRow};
use crate::item::ItemId;
use crate::repo::Repo;
use crate::runner::{self, Failure, Metered, RunRequest};
use crate::worker::{Progress, WorkerCtx};

/// The time limit for one resumed turn.
pub const RESUME_TIMEOUT_SECS: u64 = 1200;

/// The `notification_type` that marks a resume Hallmark gave up on.
pub const GAVE_UP: &str = "resume_failed";

pub fn tick(ctx: &WorkerCtx) -> Result<Progress> {
    let repo = ctx.repo;
    let ix = repo.index()?;
    let defaulted = apply_defaults(repo, &ix, Utc::now())?;
    let ix = if defaulted.is_empty() {
        ix
    } else {
        repo.index()?
    };
    let resumed = resume_one(ctx, &ix)?;
    let mut what = Vec::new();
    if !defaulted.is_empty() {
        what.push(format!("default answer used for {}", defaulted.join(", ")));
    }
    match resumed {
        Some(Progress::Worked { what: w }) => what.push(w),
        Some(p) => return Ok(p),
        None => {}
    }
    Ok(if what.is_empty() {
        Progress::Idle
    } else {
        Progress::worked(what.join("; "))
    })
}

/// Records the default answer for each question whose wait is over.
/// Returns the question IDs answered.
pub fn apply_defaults(repo: &Repo, ix: &Index, now: DateTime<Utc>) -> Result<Vec<String>> {
    let mut stmt = ix.conn().prepare(
        "SELECT qid FROM questions
         WHERE answer IS NULL AND dflt IS NOT NULL AND deadline IS NOT NULL
         ORDER BY asked_at",
    )?;
    let qids: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<_, _>>()?;
    let mut done = Vec::new();
    for qid in qids {
        let Some(q) = ix.question(&qid)? else {
            continue;
        };
        let (Some(default), Some(deadline)) = (q.default.clone(), q.deadline) else {
            continue;
        };
        if deadline > now {
            continue;
        }
        let body = Body::Answer {
            qid: q.qid.clone(),
            answer: default,
            defaulted: true,
        };
        if append_once(repo, q.item, &q.qid, body)? {
            repo.hall.live(&format!(
                "{} {} unanswered after {}: default used",
                label(q.item),
                q.qid,
                q.wait.as_deref().unwrap_or("its wait")
            ));
            done.push(q.qid);
        }
    }
    Ok(done)
}

/// Appends an answer unless the question's journal already has one (a
/// human may have answered since the index was read).
fn append_once(repo: &Repo, item: Option<ItemId>, qid: &str, body: Body) -> Result<bool> {
    let answered = |events: &[Event]| {
        events
            .iter()
            .any(|e| matches!(&e.body, Body::Answer { qid: q, .. } if q == qid))
    };
    match item {
        Some(id) => {
            let (_, wrote) = repo.hall.transact(id, |_, events| {
                if answered(events) {
                    return Ok((vec![], false));
                }
                Ok((vec![Event::new("hall", Some(id), body)], true))
            })?;
            Ok(wrote)
        }
        None => {
            let path = repo.hall.repo_events();
            if answered(&repo.hall.read(&path)?) {
                return Ok(false);
            }
            repo.hall.append(&path, &Event::new("hall", None, body))?;
            Ok(true)
        }
    }
}

/// An answered question waiting to reach its session.
#[derive(Debug, Clone)]
pub struct Pending {
    pub q: QuestionRow,
    pub session: String,
    pub answer: String,
    pub defaulted: bool,
    /// Resume failures in a row for this question.
    pub failures: u32,
}

/// Answered questions with a session, not delivered and not given up on,
/// oldest answer first. Questions of closed or dropped items are skipped.
pub fn pending(repo: &Repo, ix: &Index) -> Result<Vec<Pending>> {
    let mut stmt = ix.conn().prepare(
        "SELECT qid FROM questions
         WHERE answer IS NOT NULL AND session IS NOT NULL AND session != ''
         ORDER BY answered_at, asked_at",
    )?;
    let qids: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<_, _>>()?;
    if qids.is_empty() {
        return Ok(Vec::new());
    }
    let items = ix.items(true)?;
    let mut out = Vec::new();
    for qid in qids {
        let Some(q) = ix.question(&qid)? else {
            continue;
        };
        if let Some(id) = q.item
            && items.iter().any(|i| i.id == id && i.state.is_terminal())
        {
            continue;
        }
        let events = match q.item {
            Some(id) => repo.hall.read_item(id)?,
            None => repo.hall.read(&repo.hall.repo_events())?,
        };
        if let Some(p) = pending_from(&q, &events) {
            out.push(p);
        }
    }
    Ok(out)
}

/// Reads one question's state from its journal: the answer, whether it was
/// delivered or given up on, and resume failures since it was answered.
pub fn pending_from(q: &QuestionRow, events: &[Event]) -> Option<Pending> {
    let session = q.session.clone().filter(|s| !s.is_empty())?;
    let mut answer: Option<(String, bool)> = None;
    let mut failures = 0;
    for e in events {
        match &e.body {
            Body::Answer {
                qid,
                answer: a,
                defaulted,
            } if *qid == q.qid && answer.is_none() => {
                answer = Some((a.clone(), *defaulted));
                failures = 0;
            }
            Body::AnswerDelivered { qid } if *qid == q.qid => return None,
            // Given up: the session is filed as waiting for you.
            Body::Notification {
                notification_type: Some(t),
                ..
            } if t == GAVE_UP
                && answer.is_some()
                && e.session.as_deref() == Some(session.as_str()) =>
            {
                return None;
            }
            // Another delivery in this journal ends that question's run of
            // failures; they were not this one's.
            Body::AnswerDelivered { .. } => failures = 0,
            Body::Notification {
                notification_type: Some(t),
                ..
            } if t == GAVE_UP => failures = 0,
            Body::RunFailed { role, .. } if role == "resume" && answer.is_some() => failures += 1,
            _ => {}
        }
    }
    let (answer, defaulted) = answer?;
    Some(Pending {
        q: q.clone(),
        session,
        answer,
        defaulted,
        failures,
    })
}

/// What the resumed session reads.
pub fn prompt(p: &Pending) -> String {
    if p.defaulted {
        format!(
            "Nobody answered {} (\"{}\") within {}, so go ahead with the default: {}",
            p.q.qid,
            p.q.text,
            p.q.wait.as_deref().unwrap_or("the wait"),
            p.answer
        )
    } else {
        format!(
            "The human answered {} (\"{}\"): {}",
            p.q.qid, p.q.text, p.answer
        )
    }
}

/// The resume run: in the item's worktree when it still exists, else the
/// main checkout. Claude Code finds a session by its ID from any directory
/// (checked 2026-10-09), and the worktree is where the item's work is.
pub fn request(repo: &Repo, p: &Pending) -> Result<RunRequest> {
    let cwd =
        p.q.item
            .and_then(|id| repo.hall.load_item(id).ok())
            .map(|i| PathBuf::from(i.worktree))
            .filter(|w| w.is_dir())
            .unwrap_or_else(|| repo.main_root.clone());
    Ok(RunRequest {
        role: "resume".into(),
        room: "workshop".into(),
        prompt: prompt(p),
        timeout_secs: RESUME_TIMEOUT_SECS,
        cwd,
        resume: Some(p.session.clone()),
        plugin_dir: Some(crate::plugin::runtime_dir()?),
        extra_args: repo.config.claude_args.clone(),
        env: vec![("HALL_ACTOR".into(), "agent".into())],
        ..Default::default()
    })
}

fn resume_one(ctx: &WorkerCtx, ix: &Index) -> Result<Option<Progress>> {
    let repo = ctx.repo;
    let Some(p) = pending(repo, ix)?.into_iter().next() else {
        return Ok(None);
    };
    let req = request(repo, &p)?;
    let what = format!(
        "{} {} → session {}",
        label(p.q.item),
        p.q.qid,
        short(&p.session)
    );
    match runner::run_metered(repo, ctx.runner, p.q.item, &req)? {
        Metered::Blocked(stop) => Ok(Some(stop.progress())),
        Metered::Done(_) => {
            record(
                repo,
                p.q.item,
                Some(&p.session),
                Body::AnswerDelivered {
                    qid: p.q.qid.clone(),
                },
            )?;
            Ok(Some(Progress::worked(format!("answer delivered: {what}"))))
        }
        Metered::Failed {
            failure: Failure::RateLimited { until },
            ..
        } => Ok(Some(Progress::Pause {
            reason: format!("rate limited while resuming {what}"),
            until,
        })),
        Metered::Failed { failure, .. } => {
            let failures = p.failures + 1;
            if failures < 2 {
                return Ok(Some(Progress::worked(format!(
                    "resume failed ({}), retrying next tick: {what}",
                    failure.describe()
                ))));
            }
            record(
                repo,
                p.q.item,
                Some(&p.session),
                Body::Notification {
                    message: format!(
                        "Hallmark couldn't resume this session with the answer to {} (\"{}\": {}): {}. \
                         Resume it yourself.",
                        p.q.qid,
                        crate::output::clip(&p.q.text, 60),
                        p.answer,
                        failure.describe()
                    ),
                    notification_type: Some(GAVE_UP.into()),
                },
            )?;
            Ok(Some(Progress::worked(format!(
                "resume failed twice, filed in the inbox: {what}"
            ))))
        }
    }
}

fn record(repo: &Repo, item: Option<ItemId>, session: Option<&str>, body: Body) -> Result<()> {
    let e = Event::new("hall", item, body).with_session(session.map(String::from));
    match item {
        Some(id) => repo.hall.append_item(id, &e),
        None => repo.hall.append(&repo.hall.repo_events(), &e),
    }
}

fn label(item: Option<ItemId>) -> String {
    item.map(|i| i.to_string()).unwrap_or_else(|| "repo".into())
}

fn short(s: &str) -> &str {
    &s[..s.len().min(8)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q() -> QuestionRow {
        QuestionRow {
            qid: "Q-3".into(),
            item: Some(ItemId(1)),
            text: "billed or raw?".into(),
            options: vec!["billed".into(), "raw".into()],
            default: Some("billed".into()),
            wait: Some("4h".into()),
            deadline: None,
            asked_at: Utc::now(),
            session: Some("s-1".into()),
            answer: Some("raw".into()),
            answered_at: Some(Utc::now()),
        }
    }

    fn ev(body: Body) -> Event {
        Event::new("hall", Some(ItemId(1)), body)
    }

    fn failed() -> Event {
        ev(Body::RunFailed {
            role: "resume".into(),
            reason: "timed out".into(),
            attempt: 1,
        })
    }

    #[test]
    fn prompts() {
        let mut p = pending_from(
            &q(),
            &[ev(Body::Answer {
                qid: "Q-3".into(),
                answer: "raw".into(),
                defaulted: false,
            })],
        )
        .unwrap();
        assert_eq!(
            prompt(&p),
            "The human answered Q-3 (\"billed or raw?\"): raw"
        );
        p.defaulted = true;
        p.answer = "billed".into();
        assert_eq!(
            prompt(&p),
            "Nobody answered Q-3 (\"billed or raw?\") within 4h, so go ahead with the default: billed"
        );
    }

    #[test]
    fn delivery_failures_and_giving_up() {
        let answer = ev(Body::Answer {
            qid: "Q-3".into(),
            answer: "raw".into(),
            defaulted: false,
        });
        // Not answered yet: nothing to deliver.
        assert!(pending_from(&q(), &[]).is_none());
        // Failures before the answer don't count.
        let p = pending_from(&q(), &[failed(), answer.clone(), failed()]).unwrap();
        assert_eq!(p.failures, 1);
        assert!(!p.defaulted);
        // Delivered.
        let delivered = ev(Body::AnswerDelivered { qid: "Q-3".into() });
        assert!(pending_from(&q(), &[answer.clone(), delivered]).is_none());
        // Given up for this session.
        let gave_up = ev(Body::Notification {
            message: "x".into(),
            notification_type: Some(GAVE_UP.into()),
        })
        .with_session(Some("s-1".into()));
        assert!(pending_from(&q(), &[answer.clone(), failed(), failed(), gave_up]).is_none());
        // Another question's delivery resets the run of failures.
        let other = ev(Body::AnswerDelivered { qid: "Q-2".into() });
        let p = pending_from(&q(), &[answer.clone(), failed(), other]).unwrap();
        assert_eq!(p.failures, 0);
        // No session: can't be resumed.
        let mut nosess = q();
        nosess.session = None;
        assert!(pending_from(&nosess, &[answer]).is_none());
    }
}
