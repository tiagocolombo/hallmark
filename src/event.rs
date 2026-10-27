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
    /// For a cached result, how long the original run took.
    pub duration_ms: u64,
    /// Relative to the item's journal directory (`checks/r2.log`), or, for a
    /// cached result, the original run's log relative to the journal root
    /// (`items/H-0003/checks/r1.log`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    /// Served from the checks cache: this gate passed before on the same
    /// tree, so it didn't run again.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cached: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
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
        /// The open PR this item took over (`hall adopt --pr`): the item
        /// updates it instead of opening a new one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pr_url: Option<String>,
        /// The commit that PR's head was at when the item took it over.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pr_head: Option<String>,
        /// False when the item lives in a checkout the user made (`hall
        /// adopt` in the current checkout). The sweeper only removes
        /// worktrees Hallmark made. Absent in older journals: an adopted
        /// item is then taken to live in the user's own checkout.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        own_worktree: Option<bool>,
    },
    /// A shipped item came back to development through its open PR
    /// (`hall adopt`), to rebase it, resolve conflicts or change it. When it
    /// is hallmarked again, the shipper updates that PR.
    Reopened {
        pr_url: String,
        /// The commit the PR's head was at on reopening; the update push
        /// leases on this commit, or on the branch's head on origin when
        /// this item hallmarked that commit, as after an update that pushed
        /// and then failed (ADR 0042).
        pr_head: String,
        /// The branch the item is reviewed against and the PR targets now.
        base: String,
        /// Where the work happens now.
        worktree: String,
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
        /// Why the change is needed: the PR description's Why.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        why: Option<String>,
        /// How it was done: the PR description's How.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        how: Option<String>,
        /// How it was tested, beyond the gates: the PR description's
        /// Testing (ADR 0057).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tested: Option<String>,
        /// How to undo it once merged, when a `git revert` isn't enough:
        /// the PR description's How to revert (ADR 0057).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revert: Option<String>,
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
        /// The reviewer asked for a person's approval before the merge, and
        /// why (ADR 0063).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        needs_person: Option<String>,
    },
    /// The security pass (ADR 0064) that ran, or couldn't, before this
    /// round's verdict was recorded: the paths that asked for it, the
    /// titles of the findings it added, or why it didn't count.
    SecurityPass {
        sha: String,
        round: u32,
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default)]
        added: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skipped: Option<String>,
    },
    /// The verifier's pass over the reviewer's blocking findings before
    /// the verdict was recorded (ADR 0062): how many it checked and the
    /// ones it showed are not real, which the verdict leaves out.
    Verified {
        sha: String,
        round: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        checked: u32,
        #[serde(default)]
        dropped: Vec<crate::findings::DroppedFinding>,
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
        /// Lifting a budget hold: tokens this item may spend on top of
        /// `per_item`, which itself is unchanged (ADR 0016).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        granted_tokens: Option<u64>,
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
    /// You let the loop go past the daily budget for one UTC day
    /// (`hall budget --grant`), without changing the cap: that day's cap is
    /// `daily` plus `tokens`. The latest grant for a day is the one that
    /// counts (ADR 0028).
    BudgetGranted {
        /// The UTC date, such as 2026-10-10.
        day: String,
        /// Tokens on top of `daily` for that day.
        tokens: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// A notification went out (key: what it was about).
    Notified {
        key: String,
        kind: String,
        channels: Vec<String>,
    },
    /// The librarian distilled this item into learnings in the local store.
    Distilled {
        /// The knowledge item that carried them (ADR 0021); absent since the
        /// Library stays local (ADR 0027).
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
    /// The Dispatch Dock rebased a hallmarked branch onto the latest base
    /// before shipping (ADR 0030). `carried`: the patch was identical and
    /// the gates passed at `to`, so the approval moved to `to` (a
    /// `hallmarked` event follows). Otherwise the item went back through
    /// the loop at `to`.
    Rebased {
        from: String,
        to: String,
        /// The base ref it was rebased onto (`origin/main`).
        onto: String,
        carried: bool,
        /// The gates run at `to`, when the patch was identical.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        checks: Vec<CheckRun>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The branch conflicts with its base: a headless dev run (the Delivery
    /// office) rebases it in development and sends it to review.
    RebaseRequested {
        /// The base branch to rebase onto.
        onto: String,
        reason: String,
        /// Files that conflicted, when known.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<String>,
    },
    /// New review comments on the shipped PR: a headless dev run (the
    /// Delivery office) addresses them in development and sends the item to
    /// review; the shipper then updates the same PR.
    FeedbackRequested {
        pr_url: String,
        comments: Vec<PrComment>,
        /// The newest comment's time: later comments are new next time.
        until: DateTime<Utc>,
    },
    /// The shipped PR's CI failed: a headless dev run (the Delivery office)
    /// fixes it in development and sends the item to review; the shipper
    /// then updates the same PR.
    CiFixRequested {
        pr_url: String,
        failing: Vec<FailingCheck>,
    },
    /// An item reopened to fix its PR's CI went back to the PR unchanged:
    /// the checks passed on a re-run, so there was nothing to fix (ADR 0047).
    /// The item is shipped again and its PR watched again.
    Restored {
        pr_url: String,
        /// The PR's head, which is also the branch's.
        sha: String,
        reason: String,
    },
    /// The Delivery office won't fix the red PR itself (someone else pushed
    /// to it, or its worktree is missing or dirty): it waits for you.
    CiFixDeclined {
        pr_url: String,
        reason: String,
    },
    /// The merger merged the shipped PR at its hallmarked head (ADR 0048).
    /// The Delivery office's watch then records `delivered`.
    PrMerged {
        pr_url: String,
        /// The PR's head the merge was matched against.
        sha: String,
    },
    /// The shipped PR left the open state: `merged` or `closed`. The
    /// Delivery office stops watching it.
    Delivered {
        pr_url: String,
        outcome: String,
    },
    /// The merged PR was reverted on its base (ADR 0061): `sha` is the
    /// commit it landed as, `revert_sha` the commit that undid it.
    Reverted {
        pr_url: String,
        base: String,
        sha: String,
        revert_sha: String,
        subject: String,
    },
    /// The sweeper removed the worktree of an item that is done with it
    /// (ADR 0040). Its branch and journal are kept.
    WorktreeRemoved {
        path: String,
    },
    /// You dismissed inbox entries (`hall inbox --dismiss`, `--clear`):
    /// each stays hidden until it changes. Kept in its own journal file.
    InboxDismissed {
        /// `InboxEntry::key` of each entry.
        keys: Vec<String>,
    },
    /// A run was given the person's reasons up to `until` (`words`, ADR
    /// 0044), so later runs aren't.
    ReasonsDelivered {
        /// reviewer, fixer, delivery, resume or session.
        to: String,
        /// When the newest reason it was given was recorded.
        until: DateTime<Utc>,
    },
    /// The shipped PR's CI finished (the daemon watches it after shipping).
    PrChecks {
        pr_url: String,
        passed: bool,
        /// The checks that failed: name and link.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        failing: Vec<FailingCheck>,
        /// Set when the PR left the open state while its checks were red:
        /// `merged` or `closed`. The checks did not pass.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ended: Option<String>,
        /// Set when none of the failing jobs ever started (billing, no
        /// runners): GitHub's reason. There is no code to fix.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        not_started: Option<String>,
    },
    /// A file or link attached to the item for its Gazette (`hall attach`):
    /// a mockup, a screenshot, a diagram.
    Attached {
        /// The copy under the item's `attachments/` (`attachments/1-mockup.png`),
        /// or a URL.
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },
}

/// A comment or review on a shipped PR.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PrComment {
    pub author: String,
    pub body: String,
    /// The file and line, for a comment on the diff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    pub at: DateTime<Utc>,
}

/// A CI check that failed on a shipped PR.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FailingCheck {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
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
            Body::SecurityPass { .. } => "security_pass",
            Body::Verified { .. } => "verified",
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
            Body::ReasonsDelivered { .. } => "reasons_delivered",
            Body::BudgetGranted { .. } => "budget_granted",
            Body::Notified { .. } => "notified",
            Body::Distilled { .. } => "distilled",
            Body::Planned { .. } => "planned",
            Body::Merged { .. } => "merged",
            Body::Reopened { .. } => "reopened",
            Body::ChildMerged { .. } => "child_merged",
            Body::PrChecks { .. } => "pr_checks",
            Body::InboxDismissed { .. } => "inbox_dismissed",
            Body::Rebased { .. } => "rebased",
            Body::RebaseRequested { .. } => "rebase_requested",
            Body::CiFixRequested { .. } => "ci_fix_requested",
            Body::Restored { .. } => "restored",
            Body::CiFixDeclined { .. } => "ci_fix_declined",
            Body::PrMerged { .. } => "pr_merged",
            Body::Delivered { .. } => "delivered",
            Body::Reverted { .. } => "reverted",
            Body::WorktreeRemoved { .. } => "worktree_removed",
            Body::Attached { .. } => "attached",
            Body::FeedbackRequested { .. } => "feedback_requested",
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
            Body::Reopened { .. } => Action::Reopen,
            Body::Restored { .. } => Action::Restore,
            _ => return None,
        })
    }

    /// One line for humans, with long text clipped.
    pub fn summary(&self) -> String {
        self.summary_at(false)
    }

    /// The same line with nothing clipped, for views that wrap it.
    pub fn summary_full(&self) -> String {
        self.summary_at(true)
    }

    fn summary_at(&self, full: bool) -> String {
        let c = |s: &str, n: usize| {
            if full {
                s.split_whitespace().collect::<Vec<_>>().join(" ")
            } else {
                crate::output::clip(s, n)
            }
        };
        match self {
            Body::Created {
                title,
                adopted,
                issue,
                ..
            } => {
                let how = if *adopted { "adopted" } else { "created" };
                match issue {
                    Some(i) => format!("{how} from #{} · \"{}\"", i.number, c(title, 60)),
                    None => format!("{how} · \"{}\"", c(title, 60)),
                }
            }
            Body::Prompt { text } => format!("prompt · \"{}\"", c(text, 70)),
            Body::Turn { summary } => format!("turn · {}", c(summary, 70)),
            Body::Commit {
                sha,
                subject,
                fixes,
            } if fixes.is_empty() => {
                format!("commit {} · {}", short(sha), c(subject, 60))
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
                    c(subject, 50)
                )
            }
            Body::ProposedReady { summary, .. } => {
                format!("proposes ready · \"{}\"", c(summary, 60))
            }
            Body::Ready { sha, round, note } => match note {
                Some(n) => format!("ready · round {round} · {} · \"{}\"", short(sha), c(n, 40)),
                None => format!("ready · round {round} · {}", short(sha)),
            },
            Body::PulledBack { reason } => match reason {
                Some(r) => format!("pulled back · \"{}\"", c(r, 60)),
                None => "pulled back".into(),
            },
            Body::Dropped { reason, .. } => format!("dropped · \"{}\"", c(reason, 60)),
            Body::Retried { reason } => format!("another round allowed · \"{}\"", c(reason, 50)),
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
            Body::SecurityPass { added, skipped, .. } => match skipped {
                Some(why) => format!("security pass · did not count: {}", c(why, 60)),
                None => format!(
                    "security pass · {} finding{} added",
                    added.len(),
                    if added.len() == 1 { "" } else { "s" }
                ),
            },
            Body::Verified {
                checked, dropped, ..
            } => format!(
                "verifier · {} of {} blocking finding{} dropped",
                dropped.len(),
                checked,
                if *checked == 1 { "" } else { "s" }
            ),
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
            Body::Question { qid, text, .. } => format!("question {qid} · \"{}\"", c(text, 60)),
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
                format!("answer {qid}{how} · \"{}\"", c(answer, 60))
            }
            Body::QuestionSession { qid } => format!("question {qid} linked to its session"),
            Body::Notification { message, .. } => {
                format!("session waiting · {}", c(message, 60))
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
                format!("held ({}) · {}", kind.as_str(), c(reason, 60))
            }
            Body::Released {
                reason,
                granted_tokens,
            } => {
                let mut s = "released".to_string();
                if let Some(n) = granted_tokens {
                    s.push_str(&format!(" · +{} tokens", crate::usage::fmt_tokens(*n)));
                }
                if let Some(r) = reason {
                    s.push_str(&format!(" · \"{}\"", c(r, 60)));
                }
                s
            }
            Body::Stuck { state, .. } => format!("stuck in {state}"),
            Body::RunFailed {
                role,
                reason,
                attempt,
            } => format!("{role} run failed (attempt {attempt}) · {}", c(reason, 50)),
            Body::Paused { reason, until } => match until {
                Some(u) => format!(
                    "daemon paused until {} · {}",
                    u.format("%H:%M"),
                    c(reason, 40)
                ),
                None => format!("daemon paused · {}", c(reason, 50)),
            },
            Body::Unpaused {} => "daemon resumed".into(),
            Body::ReasonsDelivered { to, .. } => {
                let to = match to.as_str() {
                    "delivery" => "the Delivery office",
                    "resume" => "the resumed session",
                    "session" => "the development session",
                    "reviewer" => "the reviewer",
                    "fixer" => "the fixer",
                    other => other,
                };
                format!("the words given went to {to}")
            }
            Body::BudgetGranted {
                day,
                tokens,
                reason,
                ..
            } => {
                let mut s = format!(
                    "daily budget +{} for {day}",
                    crate::usage::fmt_tokens(*tokens)
                );
                if let Some(r) = reason {
                    s.push_str(&format!(" · \"{}\"", c(r, 50)));
                }
                s
            }
            Body::Notified { kind, channels, .. } => {
                format!("notified ({kind}) via {}", channels.join(", "))
            }
            Body::Distilled { into, learnings } => match into {
                Some(i) => format!("{} learnings proposed in {i}", learnings.len()),
                None => format!("{} learnings added or updated", learnings.len()),
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
            Body::Reopened { pr_url, base, .. } => {
                format!("reopened from {pr_url} · base {base}")
            }
            Body::ChildMerged { child, sha } => format!("{child} merged · {}", short(sha)),
            Body::Rebased {
                from,
                to,
                onto,
                carried,
                reason,
                ..
            } => format!(
                "rebased {} → {} onto {onto}{}",
                short(from),
                short(to),
                match (carried, reason) {
                    (true, _) => " · approval carried".to_string(),
                    (false, Some(r))
                        if r.starts_with("rebase review") || r.starts_with("full review") =>
                    {
                        format!(" · {r}")
                    }
                    _ => " · back to review".to_string(),
                }
            ),
            Body::RebaseRequested { onto, files, .. } => {
                if files.is_empty() {
                    format!("conflicts with {onto} · rebase requested")
                } else {
                    format!(
                        "conflicts with {onto} in {} · rebase requested",
                        c(&files.join(", "), 50)
                    )
                }
            }
            Body::CiFixRequested { failing, .. } => format!(
                "CI failed on the PR: {} · fix requested",
                c(
                    &failing
                        .iter()
                        .map(|f| f.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    50
                )
            ),
            Body::Restored {
                pr_url,
                sha,
                reason,
            } => {
                format!("back on {pr_url} at {} · {}", short(sha), c(reason, 80))
            }
            Body::CiFixDeclined { pr_url, reason } => {
                format!("CI fix declined · {pr_url} · {reason}")
            }
            Body::PrMerged { pr_url, sha } => format!("merged {} · {pr_url}", short(sha)),
            Body::Delivered { pr_url, outcome } => format!("PR {outcome} · {pr_url}"),
            Body::Reverted {
                base,
                sha,
                revert_sha,
                subject,
                ..
            } => format!(
                "reverted on {base} · {} undone by {} \"{}\"",
                short(sha),
                short(revert_sha),
                c(subject, 50)
            ),
            Body::WorktreeRemoved { path } => format!("worktree removed · {path}"),
            Body::Attached { path, caption } => match caption {
                Some(c) => format!("attached {path}: {c}"),
                None => format!("attached {path}"),
            },
            Body::FeedbackRequested { comments, .. } => format!(
                "{} new PR comment{} to address",
                comments.len(),
                if comments.len() == 1 { "" } else { "s" }
            ),
            Body::InboxDismissed { keys } => format!(
                "{} inbox entr{} dismissed",
                keys.len(),
                if keys.len() == 1 { "y" } else { "ies" }
            ),
            Body::PrChecks {
                passed, failing, ..
            } => {
                if *passed {
                    "CI passed on the PR".into()
                } else {
                    let names: Vec<&str> = failing.iter().map(|f| f.name.as_str()).collect();
                    format!("CI failed on the PR: {}", names.join(", "))
                }
            }
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
        pr_url,
        pr_head,
        own_worktree,
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
        check_failures: 0,
        frozen_sha: None,
        hallmark_sha: None,
        pr_url: pr_url.clone(),
        pr_head: pr_head.clone(),
        summary: None,
        drop_reason: None,
        adopted: *adopted,
        events: 1,
        parent: *parent,
        follow_up: follow_up.clone(),
        hold: None,
        stuck_since: None,
        reviewed_sha: None,
        escalation: None,
        findings: Counts::default(),
        ci: None,
        rebase: None,
        feedback: None,
        ci_fix: None,
        ci_fixes: 0,
        ci_declined: None,
        delivered: None,
        delivered_at: None,
        merged: None,
        merge_approved: None,
        reverted: None,
        needs_person: None,
        own_worktree: own_worktree.unwrap_or(!*adopted),
        swept: false,
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
                // And a rebase or feedback request: asked for again if needed.
                item.rebase = None;
                item.feedback = None;
                item.ci_fix = None;
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
            Body::ProposedReady { summary, .. } => item.summary = Some(summary.clone()),
            Body::Ready { sha, round, .. } => {
                item.frozen_sha = Some(sha.clone());
                item.round = *round;
                item.check_failures = 0;
            }
            Body::Checks { passed, .. } => {
                item.check_failures = if *passed { 0 } else { item.check_failures + 1 };
            }
            Body::FixSubmitted { sha, .. } => {
                item.frozen_sha = Some(sha.clone());
                item.round += 1;
            }
            Body::Verdict {
                verdict,
                sha,
                escalated,
                summary,
                needs_person,
                ..
            } => {
                if *verdict == VerdictKind::Changes {
                    item.rejects += 1;
                }
                item.reviewed_sha = Some(sha.clone());
                // The escalating verdict's summary is the diagnosis the
                // person reads; an approval or a non-escalating one clears
                // what an earlier escalation left.
                item.escalation = if *escalated { summary.clone() } else { None };
                // The latest review's word on whether a person must approve
                // the merge (ADR 0063).
                item.needs_person = needs_person.clone();
            }
            Body::Hallmarked { sha, .. } => item.hallmark_sha = Some(sha.clone()),
            Body::Held { kind, reason } => {
                item.hold = Some(Hold {
                    kind: *kind,
                    reason: reason.clone(),
                    since: e.t,
                })
            }
            Body::Released { .. } => {
                // Releasing a lit hold is your approval to merge that head.
                if item.hold.as_ref().is_some_and(|h| h.kind == HoldKind::Lit) {
                    item.merge_approved = item.pr_head.clone();
                }
                // A retried merge that never landed tries again.
                if item
                    .hold
                    .as_ref()
                    .is_some_and(|h| h.kind == HoldKind::MergeFailed)
                {
                    item.merged = None;
                }
                // Lifting a `checks` hold allows another set of rounds.
                if item.hold.as_ref().map(|h| h.kind) == Some(HoldKind::Checks) {
                    item.check_failures = 0;
                }
                item.hold = None;
            }
            Body::PrChecks {
                passed,
                failing,
                ended,
                not_started,
                ..
            } => {
                item.ci = Some(crate::item::Ci {
                    passed: *passed,
                    ended: ended.clone(),
                    failing: failing.iter().map(|f| f.name.clone()).collect(),
                    not_started: not_started.clone(),
                    t: e.t,
                });
                // Green again: a later failure gets fixes of its own.
                if *passed {
                    item.ci_fixes = 0;
                    item.ci_declined = None;
                }
            }
            Body::Shipped { sha, pr_url } => {
                // An update ship keeps the PR it updated.
                if pr_url.is_some() {
                    item.pr_url = pr_url.clone();
                }
                item.pr_head = Some(sha.clone());
            }
            Body::Reopened {
                pr_url,
                pr_head,
                base,
                worktree,
            } => {
                // Another checkout is the user's; and the PR is watched
                // again, with its worktree, until it ends again.
                item.own_worktree &= item.worktree == *worktree;
                item.swept = false;
                item.delivered = None;
                item.delivered_at = None;
                item.merged = None;
                item.merge_approved = None;
                item.worktree = worktree.clone();
                item.pr_url = Some(pr_url.clone());
                item.pr_head = Some(pr_head.clone());
                item.base = base.clone();
                // The PR's CI is watched again once the update ships.
                item.ci = None;
                item.ci_declined = None;
                // Reviewed again like any other change.
                item.rejects = 0;
            }
            Body::Dropped { reason, .. } => item.drop_reason = Some(reason.clone()),
            Body::RebaseRequested {
                onto,
                reason,
                files,
            } => {
                item.rebase = Some(crate::item::RebaseRequest {
                    onto: onto.clone(),
                    reason: reason.clone(),
                    files: files.clone(),
                    since: e.t,
                })
            }
            Body::CiFixDeclined { reason, .. } => item.ci_declined = Some(reason.clone()),
            Body::PrMerged { sha, .. } => item.merged = Some(sha.clone()),
            Body::Delivered { outcome, .. } => {
                item.delivered = Some(outcome.clone());
                item.delivered_at = Some(e.t);
            }
            Body::Reverted {
                base,
                sha,
                revert_sha,
                subject,
                ..
            } => {
                item.reverted = Some(crate::item::Reverted {
                    base: base.clone(),
                    sha: sha.clone(),
                    revert_sha: revert_sha.clone(),
                    subject: subject.clone(),
                    at: e.t,
                })
            }
            Body::WorktreeRemoved { .. } => item.swept = true,
            Body::FeedbackRequested { comments, .. } => {
                item.feedback = Some(crate::item::FeedbackRequest {
                    comments: comments.clone(),
                    since: e.t,
                })
            }
            Body::CiFixRequested { pr_url, failing } => {
                item.ci_fixes += 1;
                item.ci_fix = Some(crate::item::CiFixRequest {
                    pr_url: pr_url.clone(),
                    failing: failing.clone(),
                    since: e.t,
                })
            }
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
            pr_url: None,
            pr_head: None,
            own_worktree: Some(true),
        })
    }

    #[test]
    fn a_rebase_says_how_it_goes_on_in_log_and_status() {
        let rebased = |carried: bool, reason: Option<&str>| {
            ev(Body::Rebased {
                from: "a".repeat(40),
                to: "b".repeat(40),
                onto: "main".into(),
                carried,
                checks: vec![],
                reason: reason.map(String::from),
            })
            .body
            .summary()
        };
        let s = rebased(
            false,
            Some("rebase review: only the resolution (3 lines in 1 files) since aaaaaaa"),
        );
        assert!(
            s.contains("rebase review: only the resolution (3 lines"),
            "{s}"
        );
        let s = rebased(
            false,
            Some("full review: the resolution changed 101 lines of the change"),
        );
        assert!(s.contains("full review: the resolution changed 101"), "{s}");
        assert!(rebased(false, Some("other")).ends_with(" · back to review"));
        assert!(rebased(false, None).ends_with(" · back to review"));
        assert!(rebased(true, None).ends_with(" · approval carried"));
    }

    #[test]
    fn a_legacy_adopted_created_event_is_not_the_sweepers() {
        let line = r#"{"v":1,"t":"2026-01-01T00:00:00Z","actor":"you","item":"H-0001","event":"created","title":"T","branch":"b","worktree":"/home/me/app","base":"main","adopted":true}"#;
        let e: Event = serde_json::from_str(line).unwrap();
        assert!(matches!(
            &e.body,
            Body::Created {
                own_worktree: None,
                adopted: true,
                ..
            }
        ));
        assert!(!fold(ItemId(1), &[e]).unwrap().own_worktree);
        // A legacy item made by `hall new` is Hallmark's.
        let line = line.replace("\"adopted\":true", "\"adopted\":false");
        let e: Event = serde_json::from_str(&line).unwrap();
        assert!(fold(ItemId(1), &[e]).unwrap().own_worktree);
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
                why: None,
                how: None,
                tested: None,
                revert: None,
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
                needs_person: None,
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
    fn check_failures_count_in_a_row_and_reset() {
        let ready = || {
            ev(Body::Ready {
                sha: "a".into(),
                round: 1,
                note: None,
            })
        };
        let checks = |passed| {
            ev(Body::Checks {
                sha: "a".into(),
                round: 1,
                passed,
                runs: vec![],
            })
        };
        let resubmit = || {
            ev(Body::FixSubmitted {
                sha: "b".into(),
                fixes: vec![],
                disputes: vec![],
            })
        };
        let count = |events: &[Event]| fold(ItemId(1), events).unwrap().check_failures;
        let mut e = vec![created(), ready(), checks(false), resubmit(), checks(false)];
        assert_eq!(count(&e), 2);
        // A pass resets the run.
        e.extend([resubmit(), checks(true)]);
        assert_eq!(count(&e), 0);
        // Pulled back and readied again: a fresh count.
        let mut e = vec![created(), ready(), checks(false), resubmit(), checks(false)];
        e.extend([ev(Body::PulledBack { reason: None }), ready()]);
        assert_eq!(count(&e), 0);
        // Releasing a checks hold resets; releasing a budget hold keeps it.
        let held = |kind| {
            ev(Body::Held {
                kind,
                reason: "r".into(),
            })
        };
        let released = || {
            ev(Body::Released {
                reason: None,
                granted_tokens: None,
            })
        };
        let base = vec![created(), ready(), checks(false), resubmit(), checks(false)];
        let mut e = base.clone();
        e.extend([held(HoldKind::Checks), released()]);
        assert_eq!(count(&e), 0);
        let mut e = base;
        e.extend([held(HoldKind::Budget), released()]);
        assert_eq!(count(&e), 2);
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

    /// ready → checks → approve → hallmark → shipped, at `sha`.
    fn through_review(sha: &str, round: u32, pr_url: Option<&str>) -> Vec<Event> {
        vec![
            ev(Body::Ready {
                sha: sha.into(),
                round,
                note: None,
            }),
            ev(Body::Checks {
                sha: sha.into(),
                round,
                passed: true,
                runs: vec![],
            }),
            ev(Body::Verdict {
                sha: sha.into(),
                round,
                verdict: VerdictKind::Approve,
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
                needs_person: None,
            }),
            ev(Body::Hallmarked {
                sha: sha.into(),
                round,
            }),
            ev(Body::Shipped {
                sha: sha.into(),
                pr_url: pr_url.map(String::from),
            }),
        ]
    }

    #[test]
    fn a_shipped_item_reopens_through_its_pr_and_ships_again() {
        let url = "https://github.com/acme/app/pull/7";
        let reopen = || {
            ev(Body::Reopened {
                pr_url: url.into(),
                pr_head: "aaa".into(),
                base: "hm/H-0002-parent".into(),
                worktree: "/tmp/elsewhere".into(),
            })
        };
        // Reopening an item that never shipped does nothing.
        let item = fold(ItemId(1), &[created(), reopen()]).unwrap();
        assert_eq!(item.state, State::Development);
        assert_eq!(item.pr_url, None);
        assert_eq!(item.base, "main");

        let mut events = vec![created()];
        events.extend(through_review("aaa", 1, Some(url)));
        events.push(ev(Body::PrChecks {
            pr_url: url.into(),
            passed: false,
            failing: vec![],
            ended: None,
            not_started: None,
        }));
        let item = fold(ItemId(1), &events).unwrap();
        assert_eq!(item.state, State::Closed);
        assert_eq!(item.pr_head.as_deref(), Some("aaa"));
        assert!(item.ci.is_some());

        events.push(reopen());
        let item = fold(ItemId(1), &events).unwrap();
        assert_eq!(item.state, State::Development);
        assert_eq!(item.pr_url.as_deref(), Some(url));
        assert_eq!(item.pr_head.as_deref(), Some("aaa"));
        assert_eq!(item.base, "hm/H-0002-parent");
        assert_eq!(item.worktree, "/tmp/elsewhere");
        assert_eq!(item.ci, None, "CI is watched again after the update");

        // The update ship keeps the PR even when it reports no URL.
        events.extend(through_review("bbb", 2, None));
        let item = fold(ItemId(1), &events).unwrap();
        assert_eq!(item.state, State::Closed);
        assert_eq!(item.pr_url.as_deref(), Some(url));
        assert_eq!(item.pr_head.as_deref(), Some("bbb"));
        assert_eq!(item.hallmark_sha.as_deref(), Some("bbb"));
    }

    #[test]
    fn a_reopened_item_is_watched_again_and_keeps_only_worktrees_hallmark_made() {
        let url = "https://github.com/acme/app/pull/7";
        let mut events = vec![created()];
        events.extend(through_review("aaa", 1, Some(url)));
        events.push(ev(Body::Delivered {
            pr_url: url.into(),
            outcome: "closed".into(),
        }));
        events.push(ev(Body::WorktreeRemoved {
            path: "/tmp/repo-H-0001".into(),
        }));
        let item = fold(ItemId(1), &events).unwrap();
        assert!(item.swept && item.own_worktree);
        assert_eq!(item.delivered.as_deref(), Some("closed"));

        // Back in the same worktree path: still Hallmark's, watched again.
        events.push(ev(Body::Reopened {
            pr_url: url.into(),
            pr_head: "aaa".into(),
            base: "main".into(),
            worktree: "/tmp/repo-H-0001".into(),
        }));
        let item = fold(ItemId(1), &events).unwrap();
        assert!(!item.swept && item.own_worktree);
        assert_eq!(item.delivered, None);

        // Reopened in a checkout the user made: not the sweeper's.
        events.push(ev(Body::Reopened {
            pr_url: url.into(),
            pr_head: "aaa".into(),
            base: "main".into(),
            worktree: "/home/me/app".into(),
        }));
        let mut events2 = events[..events.len() - 1].to_vec();
        events2.extend(through_review("bbb", 2, None));
        events2.push(events[events.len() - 1].clone());
        assert!(!fold(ItemId(1), &events2).unwrap().own_worktree);
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
        events.push(ev(Body::Released {
            reason: None,
            granted_tokens: None,
        }));
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
