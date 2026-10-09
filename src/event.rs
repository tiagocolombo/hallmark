//! Journal events. One JSON object per line; nothing is ever edited.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{HallError, Result};
use crate::item::{Issue, Item, ItemId, State};
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
    Verdict {
        sha: String,
        round: u32,
        verdict: VerdictKind,
        /// True when this rejection reached `max_rejects`.
        #[serde(default)]
        escalated: bool,
        #[serde(default)]
        findings: Vec<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        report: Option<String>,
    },
    FixSubmitted {
        sha: String,
        #[serde(default)]
        fixes: Vec<String>,
    },
    Hallmarked {
        sha: String,
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
            Body::FixSubmitted { sha, fixes } => {
                format!("fix submitted {} · {}", short(sha), fixes.join(", "))
            }
            Body::Hallmarked { sha } => format!("hallmark · {}", short(sha)),
            Body::Shipped { pr_url, .. } => {
                format!(
                    "shipped · {}",
                    pr_url.clone().unwrap_or_else(|| "PR opened".into())
                )
            }
            Body::Question { qid, text, .. } => format!("question {qid} · \"{}\"", clip(text, 60)),
            Body::Answer { qid, answer } => format!("answer {qid} · \"{}\"", clip(answer, 60)),
            Body::QuestionSession { qid } => format!("question {qid} linked to its session"),
            Body::Notification { message, .. } => {
                format!("session waiting · {}", clip(message, 60))
            }
            Body::Usage(u) => format!(
                "usage · {} · {} tok · ${:.2}",
                u.model,
                crate::usage::fmt_tokens(u.tokens()),
                u.cost_usd
            ),
            Body::SessionStarted { source } => {
                format!(
                    "session started ({})",
                    source.as_deref().unwrap_or("startup")
                )
            }
            Body::SessionBound {} => "session joined".into(),
            Body::GateDenied { tool, code, .. } => format!("gate denied {tool} · {code}"),
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
    };
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
            } else {
                continue;
            }
        }
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
            Body::Verdict {
                verdict: VerdictKind::Changes,
                ..
            } => item.rejects += 1,
            Body::Hallmarked { sha } => item.hallmark_sha = Some(sha.clone()),
            Body::Shipped { pr_url, .. } => item.pr_url = pr_url.clone(),
            Body::Dropped { reason, .. } => item.drop_reason = Some(reason.clone()),
            _ => {}
        }
    }
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
                report: None,
            }),
            ev(Body::FixSubmitted {
                sha: "bbb".into(),
                fixes: vec!["F1".into()],
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
            ev(Body::Hallmarked { sha: "x".into() }),
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
    fn fold_requires_created_first() {
        let events = vec![ev(Body::Prompt { text: "hi".into() })];
        assert!(fold(ItemId(1), &events).is_err());
        assert!(fold(ItemId(1), &[]).is_err());
    }
}
