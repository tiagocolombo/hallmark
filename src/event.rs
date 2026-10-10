//! Journal events. One JSON object per line; nothing is ever edited.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{HallError, Result};
use crate::findings::{Counts, Dispute, Finding, LearningUse, Ledger, PriorRuling};
use crate::item::{Hold, HoldKind, Issue, Item, ItemId, State};
use crate::state::{self, Action};

pub const EVENT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Event {
    /// Schema version of the line.
    pub v: u32,
    pub t: DateTime<Utc>,
    /// Who did it: you, agent, assayer, fixer, shipper, librarian, hook, hall.
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
    /// The Claude Code session, when one was involved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(flatten)]
    pub body: Body,
}

impl Event {
    pub fn new(actor: impl Into<String>, item: Option<ItemId>, body: Body) -> Event {
        Event {
            v: EVENT_VERSION,
            t: Utc::now(),
            actor: actor.into(),
            item,
            session: None,
            body,
        }
    }

    pub fn with_session(mut self, session: Option<String>) -> Event {
        self.session = session;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerdictKind {
    Approve,
    Changes,
}

/// One gate command's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CheckRun {
    pub command: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
}

/// One model run, or one Workshop turn's worth of usage.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Usage {
    /// `headless` (claude -p) or `transcript` (interactive session).
    pub source: String,
    /// workshop, assay, repair, front, library.
    pub room: String,
    /// dev, reviewer, fixer, intake, librarian.
    pub role: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// API list-price equivalent, in USD.
    pub cost_usd: f64,
    /// Model requests counted.
    pub requests: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

impl Usage {
    pub fn tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_write_tokens
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Body {
    /// The item was created (`hall new`) or adopted (`hall adopt`).
    Created {
        title: String,
        branch: String,
        worktree: String,
        base: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        issue: Option<Issue>,
        /// The request as typed, when there was one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request: Option<String>,
        #[serde(default)]
        adopted: bool,
        /// The parent item, when the Front Desk split the request into
        /// parallel parts. A child merges into its parent instead of being
        /// reviewed on its own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<ItemId>,
        /// The PR whose feedback started this follow-up item.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        follow_up: Option<String>,
    },
    /// A prompt typed in a Claude Code session.
    Prompt {
        text: String,
    },
    /// The end of an agent turn, with the agent's last message.
    Turn {
        summary: String,
    },
    Commit {
        sha: String,
        subject: String,
        /// Findings this commit claims to fix.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        fixes: Vec<String>,
    },
    ProposedReady {
        summary: String,
    },
    Ready {
        sha: String,
        round: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    PulledBack {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Dropped {
        reason: String,
        worktree_removed: bool,
    },
    Retried {
        reason: String,
    },
    Checks {
        sha: String,
        round: u32,
        passed: bool,
        #[serde(default)]
        runs: Vec<CheckRun>,
    },
    /// The principal engineer's review, judged by Hallmark (ADR 0015).
    Verdict {
        sha: String,
        round: u32,
        /// Computed: approve when no blocker or major stays open.
        verdict: VerdictKind,
        /// True when this rejection reached `max_rejects`.
        #[serde(default)]
        escalated: bool,
        /// New findings raised in this round, numbered F1, F2, … per item.
        #[serde(default)]
        findings: Vec<Finding>,
        /// Rulings on findings from earlier rounds.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        prior: Vec<PriorRuling>,
        /// Open findings after this verdict.
        #[serde(default)]
        counts: Counts,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        /// The full report, relative to the item's journal directory
        /// (`reviews/r2.md`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        report: Option<String>,
        /// The reviewer's model.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reviewer: Option<String>,
        /// Library learnings the review used, and whether they held.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        learnings: Vec<LearningUse>,
        /// The base commit the rules were read from.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
        /// Incremental review: the last reviewed commit.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<String>,
    },
    /// The fixer resubmits (`hall fixed`): the commit, the findings it
    /// claims to fix, and the ones it disputes.
    FixSubmitted {
        sha: String,
        #[serde(default)]
        fixes: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        disputes: Vec<Dispute>,
    },
    /// The git note under refs/notes/hallmark was written on `sha`.
    Hallmarked {
        sha: String,
        #[serde(default)]
        round: u32,
    },
    Shipped {
        sha: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pr_url: Option<String>,
    },
    Question {
        qid: String,
        text: String,
        #[serde(default)]
        options: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deadline: Option<DateTime<Utc>>,
    },
    Answer {
        qid: String,
        answer: String,
        /// True when nobody answered within the wait and the default was used.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        defaulted: bool,
    },
    /// The answer was sent to the asking session (`claude -p --resume`).
    AnswerDelivered {
        qid: String,
    },
    /// Links a question to the session that asked it, so the answer can
    /// resume that session.
    QuestionSession {
        qid: String,
    },
    /// Claude Code's Notification hook: a session needs you.
    Notification {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notification_type: Option<String>,
    },
    Usage(Usage),
    SessionStarted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
    },
    /// A session started working on this item.
    SessionBound {},
    GateDenied {
        tool: String,
        code: String,
        target: String,
    },
    /// The loop stops working on the item until you act (`hall retry`):
    /// a budget cap, or a run that failed twice.
    Held {
        kind: HoldKind,
        reason: String,
    },
    /// The hold is lifted.
    Released {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The watchman saw no event for `stuck_after`.
    Stuck {
        state: State,
        since: DateTime<Utc>,
    },
    /// A model run failed: timeout, budget, error, or no structured output.
    RunFailed {
        /// reviewer, fixer, intake, librarian, resume.
        role: String,
        reason: String,
        /// 1 for the first failure in a row, 2 for the retry.
        attempt: u32,
    },
    /// The daemon paused (rate limit, daily budget). Repo-level.
    Paused {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<DateTime<Utc>>,
    },
    /// The daemon resumed after a pause. Repo-level.
    Unpaused {},
    /// A notification went out (key: what it was about).
    Notified {
        key: String,
        kind: String,
        channels: Vec<String>,
    },
    /// The librarian distilled this item into proposed learnings.
    Distilled {
        /// The knowledge item that carries them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        into: Option<ItemId>,
        learnings: Vec<String>,
    },
    /// The Front Desk's plan for a request: parallel parts, or one item.
    Planned {
        parallel: bool,
        #[serde(default)]
        children: Vec<ItemId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// This child item was merged into its parent's branch (it closes).
    Merged {
        into: ItemId,
        sha: String,
    },
    /// A child item was merged into this one.
    ChildMerged {
        child: ItemId,
        sha: String,
    },
}

impl Body {
    /// The event's name as written in the journal.
    pub fn name(&self) -> &'static str {
        match self {
            Body::Created { .. } => "created",
            Body::Prompt { .. } => "prompt",
            Body::Turn { .. } => "turn",
            Body::Commit { .. } => "commit",
            Body::ProposedReady { .. } => "proposed_ready",
            Body::Ready { .. } => "ready",
            Body::PulledBack { .. } => "pulled_back",
            Body::Dropped { .. } => "dropped",
            Body::Retried { .. } => "retried",
            Body::Checks { .. } => "checks",
            Body::Verdict { .. } => "verdict",
            Body::FixSubmitted { .. } => "fix_submitted",
            Body::Hallmarked { .. } => "hallmarked",
            Body::Shipped { .. } => "shipped",
            Body::Question { .. } => "question",
            Body::Answer { .. } => "answer",
            Body::QuestionSession { .. } => "question_session",
            Body::Notification { .. } => "notification",
            Body::Usage(_) => "usage",
            Body::SessionStarted { .. } => "session_started",
            Body::SessionBound {} => "session_bound",
            Body::GateDenied { .. } => "gate_denied",
            Body::AnswerDelivered { .. } => "answer_delivered",
            Body::Held { .. } => "held",
            Body::Released { .. } => "released",
            Body::Stuck { .. } => "stuck",
            Body::RunFailed { .. } => "run_failed",
            Body::Paused { .. } => "paused",
            Body::Unpaused {} => "unpaused",
            Body::Notified { .. } => "notified",
            Body::Distilled { .. } => "distilled",
            Body::Planned { .. } => "planned",
            Body::Merged { .. } => "merged",
            Body::ChildMerged { .. } => "child_merged",
        }
    }

    /// The state-machine action this event records, if any.
    pub fn action(&self) -> Option<Action> {
        Some(match self {
            Body::ProposedReady { .. } => Action::ProposeReady,
            Body::Ready { .. } => Action::Ready,
            Body::PulledBack { .. } => Action::PullBack,
            Body::Dropped { .. } => Action::Drop,
            Body::Retried { .. } => Action::Retry,
            Body::Checks { passed: true, .. } => Action::ChecksPass,
            Body::Checks { passed: false, .. } => Action::ChecksFail,
            Body::Verdict {
                verdict: VerdictKind::Approve,
                ..
            } => Action::Approve,
            Body::Verdict {
                escalated: true, ..
            } => Action::Escalate,
            Body::Verdict { .. } => Action::RequestChanges,
            Body::FixSubmitted { .. } => Action::Resubmit,
            Body::Shipped { .. } => Action::Ship,
            Body::Merged { .. } => Action::Merge,
            _ => return None,
        })
    }

    /// One line for humans.
    pub fn summary(&self) -> String {
        use crate::output::clip;
        match self {
            Body::Created {
                title,
                adopted,
                issue,
                ..
            } => {
                let how = if *adopted { "adopted" } else { "created" };
                match issue {
                    Some(i) => format!("{how} from #{} · \"{}\"", i.number, clip(title, 60)),
                    None => format!("{how} · \"{}\"", clip(title, 60)),
                }
            }
            Body::Prompt { text } => format!("prompt · \"{}\"", clip(text, 70)),
            Body::Turn { summary } => format!("turn · {}", clip(summary, 70)),
            Body::Commit {
                sha,
                subject,
                fixes,
            } if fixes.is_empty() => {
                format!("commit {} · {}", short(sha), clip(subject, 60))
            }
            Body::Commit {
                sha,
                subject,
                fixes,
            } => {
                format!(
                    "commit {} · fixes {} · {}",
                    short(sha),
                    fixes.join(", "),
                    clip(subject, 50)
                )
            }
            Body::ProposedReady { summary } => {
                format!("proposes ready · \"{}\"", clip(summary, 60))
            }
            Body::Ready { sha, round, note } => match note {
                Some(n) => format!(
                    "ready · round {round} · {} · \"{}\"",
                    short(sha),
                    clip(n, 40)
                ),
                None => format!("ready · round {round} · {}", short(sha)),
            },
            Body::PulledBack { reason } => match reason {
                Some(r) => format!("pulled back · \"{}\"", clip(r, 60)),
                None => "pulled back".into(),
            },
            Body::Dropped { reason, .. } => format!("dropped · \"{}\"", clip(reason, 60)),
            Body::Retried { reason } => format!("another round allowed · \"{}\"", clip(reason, 50)),
            Body::Checks { passed, round, .. } => {
                format!(
                    "checks {} · round {round}",
                    if *passed { "passed" } else { "failed" }
                )
            }
            Body::Verdict {
                verdict,
                findings,
                round,
                ..
            } => {
                let v = match verdict {
                    VerdictKind::Approve => "approve",
                    VerdictKind::Changes => "changes",
                };
                format!("review · {v} · round {round} · {} findings", findings.len())
            }
            Body::FixSubmitted { sha, fixes, .. } => {
                format!("fix submitted {} · {}", short(sha), fixes.join(", "))
            }
            Body::Hallmarked { sha, .. } => format!("hallmark · {}", short(sha)),
            Body::Shipped { pr_url, .. } => {
                format!(
                    "shipped · {}",
                    pr_url.clone().unwrap_or_else(|| "pushed".into())
                )
            }
            Body::Question { qid, text, .. } => format!("question {qid} · \"{}\"", clip(text, 60)),
            Body::Answer {
                qid,
                answer,
                defaulted,
            } => {
                let how = if *defaulted {
                    " (default, no reply)"
                } else {
                    ""
                };
                format!("answer {qid}{how} · \"{}\"", clip(answer, 60))
            }
            Body::QuestionSession { qid } => format!("question {qid} linked to its session"),
            Body::Notification { message, .. } => {
                format!("session waiting · {}", clip(message, 60))
            }
            Body::Usage(u) => format!(
                "usage · {} · {} tok · {}",
                u.model,
                crate::usage::fmt_tokens(u.tokens()),
                crate::usage::fmt_usd(u.cost_usd)
            ),
            Body::SessionStarted { source } => {
                format!(
                    "session started ({})",
                    source.as_deref().unwrap_or("startup")
                )
            }
            Body::SessionBound {} => "session joined".into(),
            Body::GateDenied { tool, code, .. } => format!("gate denied {tool} · {code}"),
            Body::AnswerDelivered { qid } => format!("answer {qid} sent to the asking session"),
            Body::Held { kind, reason } => {
                format!("held ({}) · {}", kind.as_str(), clip(reason, 60))
            }
            Body::Released { reason } => match reason {
                Some(r) => format!("released · \"{}\"", clip(r, 60)),
                None => "released".into(),
            },
            Body::Stuck { state, .. } => format!("stuck in {state}"),
            Body::RunFailed {
                role,
                reason,
                attempt,
            } => format!(
                "{role} run failed (attempt {attempt}) · {}",
                clip(reason, 50)
            ),
            Body::Paused { reason, until } => match until {
                Some(u) => format!(
                    "daemon paused until {} · {}",
                    u.format("%H:%M"),
                    clip(reason, 40)
                ),
                None => format!("daemon paused · {}", clip(reason, 50)),
            },
            Body::Unpaused {} => "daemon resumed".into(),
            Body::Notified { kind, channels, .. } => {
                format!("notified ({kind}) via {}", channels.join(", "))
            }
            Body::Distilled { into, learnings } => match into {
                Some(i) => format!("{} learnings proposed in {i}", learnings.len()),
                None => format!("{} learnings proposed", learnings.len()),
            },
            Body::Planned {
                parallel, children, ..
            } => {
                if *parallel {
                    let ids: Vec<String> = children.iter().map(|c| c.to_string()).collect();
                    format!(
                        "split into {} parallel parts: {}",
                        children.len(),
                        ids.join(", ")
                    )
                } else {
                    "planned as one item".into()
                }
            }
            Body::Merged { into, sha } => format!("merged into {into} at {}", short(sha)),
            Body::ChildMerged { child, sha } => format!("{child} merged · {}", short(sha)),
        }
    }
}

pub fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

/// Fold an item's journal into its current state.
pub fn fold(id: ItemId, events: &[Event]) -> Result<Item> {
    let mut it = events.iter();
    let first = it
        .next()
        .ok_or_else(|| HallError::internal(format!("{id} has an empty journal")))?;
    let Body::Created {
        title,
        branch,
        worktree,
        base,
        issue,
        adopted,
        parent,
        follow_up,
        ..
    } = &first.body
    else {
        return Err(HallError::internal(format!(
            "{id}'s journal does not start with `created`"
        )));
    };
    let mut item = Item {
        id,
        title: title.clone(),
        state: State::Development,
        branch: branch.clone(),
        worktree: worktree.clone(),
        base: base.clone(),
        issue: issue.clone(),
        created_at: first.t,
        updated_at: first.t,
        state_since: first.t,
        round: 0,
        rejects: 0,
        frozen_sha: None,
        hallmark_sha: None,
        pr_url: None,
        summary: None,
        drop_reason: None,
        adopted: *adopted,
        events: 1,
        parent: *parent,
        follow_up: follow_up.clone(),
        hold: None,
        stuck_since: None,
        reviewed_sha: None,
        findings: Counts::default(),
    };
    let mut ledger = Ledger::default();
    for e in it {
        item.events += 1;
        item.updated_at = e.t;
        if let Some(action) = e.body.action() {
            // A transition the machine rejects is recorded but ignored, so a
            // bad line can never corrupt the state.
            if let Some(to) = state::next(item.state, action) {
                if to != item.state {
                    item.state_since = e.t;
                }
                item.state = to;
                // Any move settles a hold: the item is somewhere new.
                item.hold = None;
            } else {
                continue;
            }
        }
        // The watchman's flag lasts until something else happens.
        match &e.body {
            Body::Stuck { since, .. } => item.stuck_since = Some(*since),
            Body::Notified { .. } | Body::Usage(_) => {}
            _ => item.stuck_since = None,
        }
        ledger.apply(&e.body);
        match &e.body {
            Body::ProposedReady { summary } => item.summary = Some(summary.clone()),
            Body::Ready { sha, round, .. } => {
                item.frozen_sha = Some(sha.clone());
                item.round = *round;
            }
            Body::FixSubmitted { sha, .. } => {
                item.frozen_sha = Some(sha.clone());
                item.round += 1;
            }
            Body::Verdict { verdict, sha, .. } => {
                if *verdict == VerdictKind::Changes {
                    item.rejects += 1;
                }
                item.reviewed_sha = Some(sha.clone());
            }
            Body::Hallmarked { sha, .. } => item.hallmark_sha = Some(sha.clone()),
            Body::Held { kind, reason } => {
                item.hold = Some(Hold {
                    kind: *kind,
                    reason: reason.clone(),
                    since: e.t,
                })
            }
            Body::Released { .. } => item.hold = None,
            Body::Shipped { pr_url, .. } => item.pr_url = pr_url.clone(),
            Body::Dropped { reason, .. } => item.drop_reason = Some(reason.clone()),
            _ => {}
        }
    }
    item.findings = ledger.open_counts();
    Ok(item)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(body: Body) -> Event {
        Event::new("you", Some(ItemId(1)), body)
    }

    fn created() -> Event {
        ev(Body::Created {
            title: "Retry webhooks".into(),
            branch: "hm/H-0001-retry-webhooks".into(),
            worktree: "/tmp/repo-H-0001".into(),
            base: "main".into(),
            issue: None,
            request: None,
            adopted: false,
            parent: None,
            follow_up: None,
        })
    }

    #[test]
    fn events_serialize_flat() {
        let e = ev(Body::Ready {
            sha: "7c2d018".into(),
            round: 1,
            note: None,
        });
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["event"], "ready");
        assert_eq!(v["sha"], "7c2d018");
        assert_eq!(v["item"], "H-0001");
        let back: Event = serde_json::from_value(v).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn usage_is_flat_too() {
        let e = ev(Body::Usage(Usage {
            model: "claude-haiku-5-5".into(),
            ..Default::default()
        }));
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["event"], "usage");
        assert_eq!(v["model"], "claude-haiku-5-5");
    }

    #[test]
    fn fold_follows_the_machine() {
        let events = vec![
            created(),
            ev(Body::ProposedReady {
                summary: "done".into(),
            }),
            ev(Body::Ready {
                sha: "aaa".into(),
                round: 1,
                note: None,
            }),
            ev(Body::Checks {
                sha: "aaa".into(),
                round: 1,
                passed: true,
                runs: vec![],
            }),
            ev(Body::Verdict {
                sha: "aaa".into(),
                round: 1,
                verdict: VerdictKind::Changes,
                escalated: false,
                findings: vec![],
                prior: vec![],
                counts: Counts::default(),
                summary: None,
                report: None,
                reviewer: None,
                learnings: vec![],
                base: None,
                since: None,
            }),
            ev(Body::FixSubmitted {
                sha: "bbb".into(),
                fixes: vec!["F1".into()],
                disputes: vec![],
            }),
        ];
        let item = fold(ItemId(1), &events).unwrap();
        assert_eq!(item.state, State::Checks);
        assert_eq!(item.rejects, 1);
        assert_eq!(item.round, 2);
        assert_eq!(item.frozen_sha.as_deref(), Some("bbb"));
        assert_eq!(item.summary.as_deref(), Some("done"));
        assert_eq!(item.events, 6);
    }

    #[test]
    fn fold_ignores_impossible_transitions() {
        let events = vec![
            created(),
            ev(Body::Hallmarked {
                sha: "x".into(),
                round: 1,
            }),
            ev(Body::Shipped {
                sha: "x".into(),
                pr_url: None,
            }),
        ];
        let item = fold(ItemId(1), &events).unwrap();
        assert_eq!(item.state, State::Development);
        assert_eq!(item.pr_url, None);
    }

    #[test]
    fn holds_and_stuck_flags_clear() {
        let mut events = vec![
            created(),
            ev(Body::Ready {
                sha: "aaa".into(),
                round: 1,
                note: None,
            }),
            ev(Body::Held {
                kind: HoldKind::Budget,
                reason: "800k cap".into(),
            }),
        ];
        let item = fold(ItemId(1), &events).unwrap();
        assert_eq!(item.hold.as_ref().unwrap().kind, HoldKind::Budget);
        events.push(ev(Body::Released { reason: None }));
        assert!(fold(ItemId(1), &events).unwrap().hold.is_none());
        events.push(ev(Body::Held {
            kind: HoldKind::RunFailed,
            reason: "timeout twice".into(),
        }));
        events.push(ev(Body::PulledBack { reason: None }));
        assert!(
            fold(ItemId(1), &events).unwrap().hold.is_none(),
            "moving clears it"
        );

        events.push(ev(Body::Stuck {
            state: State::Development,
            since: Utc::now(),
        }));
        assert!(fold(ItemId(1), &events).unwrap().stuck_since.is_some());
        events.push(ev(Body::Prompt { text: "hi".into() }));
        assert!(fold(ItemId(1), &events).unwrap().stuck_since.is_none());
    }

    #[test]
    fn children_close_by_merging() {
        let events = vec![
            created(),
            ev(Body::Merged {
                into: ItemId(7),
                sha: "abc".into(),
            }),
        ];
        assert_eq!(fold(ItemId(1), &events).unwrap().state, State::Closed);
    }

    #[test]
    fn fold_requires_created_first() {
        let events = vec![ev(Body::Prompt { text: "hi".into() })];
        assert!(fold(ItemId(1), &events).is_err());
        assert!(fold(ItemId(1), &[]).is_err());
    }
}
