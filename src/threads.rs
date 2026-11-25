//! Threads (ADR 0070): the agents' conversations, folded from an item's
//! journal and shown like questions on a developer Q&A site. Nothing new is
//! recorded; every post is an event that is already in the journal.
//!
//! - A **finding** is a question from the reviewer. The verifier comments on
//!   it, the fixer answers with a fix or a dispute, and the reviewer's next
//!   round rules on it: an accepted answer (fixed, or the dispute upheld) or
//!   still open.
//! - A **refuted** finding is one the verifier showed was not real before it
//!   was recorded (ADR 0062): asked and closed in the same round.
//! - **Planning** is the request and the Front Desk's plan for it.
//! - A **question** is an agent asking a person (`hall ask`) and the answer.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;

use crate::event::{Body, Event, short};
use crate::findings::{Ledger, PriorStatus, Severity, Status};
use crate::item::{ItemId, State};

/// What a thread is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Finding,
    Refuted,
    Planning,
    Question,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Finding, Kind::Refuted, Kind::Planning, Kind::Question];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Finding => "finding",
            Kind::Refuted => "refuted",
            Kind::Planning => "planning",
            Kind::Question => "question",
        }
    }
}

/// Where a thread stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStatus {
    /// Asked, with no answer yet.
    Open,
    /// Answered (a fix or a dispute), waiting for the next review to rule.
    Answered,
    /// Settled: fixed, or answered.
    Resolved,
    /// The reviewer took the finding back: it was wrong, or a dispute held.
    Withdrawn,
    /// The verifier showed the finding was not real; it was never recorded.
    Refuted,
    /// Still open or answered when its item ended (closed or dropped):
    /// nobody will answer it now, such as a nit left at approval.
    Closed,
}

impl ThreadStatus {
    pub const ALL: [ThreadStatus; 6] = [
        ThreadStatus::Open,
        ThreadStatus::Answered,
        ThreadStatus::Resolved,
        ThreadStatus::Withdrawn,
        ThreadStatus::Refuted,
        ThreadStatus::Closed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ThreadStatus::Open => "open",
            ThreadStatus::Answered => "answered",
            ThreadStatus::Resolved => "resolved",
            ThreadStatus::Withdrawn => "withdrawn",
            ThreadStatus::Refuted => "refuted",
            ThreadStatus::Closed => "closed",
        }
    }

    /// Open and answered threads still wait on someone.
    pub fn live(self) -> bool {
        matches!(self, ThreadStatus::Open | ThreadStatus::Answered)
    }
}

/// A post's place in its thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PostKind {
    Question,
    Answer,
    /// A note on the question: the verifier's check, a delivery.
    Comment,
    /// The reviewer's ruling in a later round.
    Ruling,
}

/// One message in a thread.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Post {
    pub t: DateTime<Utc>,
    /// reviewer, verifier, fixer, front desk, agent, you…
    pub author: String,
    pub kind: PostKind,
    pub body: String,
    /// The commit it is about, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    /// The review round, for review posts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round: Option<u32>,
    /// The answer the thread settled on.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub accepted: bool,
}

/// One conversation, from the question to where it ended.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Thread {
    /// `H-0042/F3`, `H-0042/R2.1`, `H-0042/plan`, `H-0042/q-<qid>`.
    pub id: String,
    pub item: ItemId,
    pub item_title: String,
    pub item_state: State,
    pub kind: Kind,
    pub status: ThreadStatus,
    pub title: String,
    /// Who asked.
    pub asked_by: String,
    pub asked_at: DateTime<Utc>,
    /// The last post's time.
    pub last_at: DateTime<Utc>,
    /// Lens, severity, `file:line`, pre-existing…
    pub tags: Vec<String>,
    /// Answers and rulings after the question.
    pub replies: usize,
    pub posts: Vec<Post>,
}

/// Who an event's actor is, as a thread names them.
fn author(actor: &str) -> String {
    match actor {
        "assayer" => "reviewer".into(),
        "hall" => "Hallmark".into(),
        a => a.to_string(),
    }
}

fn post(e: &Event, who: &str, kind: PostKind, body: String) -> Post {
    Post {
        t: e.t,
        author: who.to_string(),
        kind,
        body,
        sha: None,
        round: None,
        accepted: false,
    }
}

fn severity(s: Severity) -> &'static str {
    s.as_str()
}

/// Every thread of one item, in the order they started.
pub fn of_item(id: ItemId, item_title: &str, state: State, events: &[Event]) -> Vec<Thread> {
    let mut threads: Vec<Thread> = Vec::new();
    let new =
        |key: String, kind: Kind, title: String, who: String, first: Post, tags: Vec<String>| {
            Thread {
                id: format!("{id}/{key}"),
                item: id,
                item_title: item_title.to_string(),
                item_state: state,
                kind,
                status: ThreadStatus::Open,
                title,
                asked_by: who,
                asked_at: first.t,
                last_at: first.t,
                tags,
                replies: 0,
                posts: vec![first],
            }
        };
    let find = |threads: &mut Vec<Thread>, key: &str| -> Option<usize> {
        let full = format!("{id}/{key}");
        threads.iter().position(|t| t.id == full)
    };
    // The request, until the plan answers it.
    let mut request: Option<(Event, String)> = None;
    // The verifier's last pass, by round: how many it checked.
    let mut verified: Option<(u32, u32, String)> = None;

    for e in events {
        match &e.body {
            Body::Created {
                request: Some(r), ..
            } => request = Some((e.clone(), r.clone())),
            Body::Planned {
                parallel,
                children,
                reason,
            } => {
                let Some((asked, text)) = request.take() else {
                    continue;
                };
                let who = author(&asked.actor);
                let mut q = post(&asked, &who, PostKind::Question, text.clone());
                q.t = asked.t;
                let mut t = new(
                    "plan".into(),
                    Kind::Planning,
                    crate::output::clip(&text, 100),
                    who,
                    q,
                    vec!["planning".into()],
                );
                let mut body = if *parallel {
                    let ids: Vec<String> = children.iter().map(|c| c.to_string()).collect();
                    format!(
                        "Split into {} parallel parts: {}.",
                        children.len(),
                        ids.join(", ")
                    )
                } else {
                    "One item.".to_string()
                };
                if let Some(r) = reason.as_deref().filter(|r| !r.trim().is_empty()) {
                    body.push(' ');
                    body.push_str(r);
                }
                let mut a = post(e, "front desk", PostKind::Answer, body);
                a.accepted = true;
                t.posts.push(a);
                t.status = ThreadStatus::Resolved;
                threads.push(t);
            }
            Body::Verified {
                round,
                checked,
                dropped,
                model,
                ..
            } => {
                let who = match model {
                    Some(m) => format!("verifier ({m})"),
                    None => "verifier".into(),
                };
                verified = Some((*round, *checked, who.clone()));
                for (n, d) in dropped.iter().enumerate() {
                    let mut q = post(
                        e,
                        "reviewer",
                        PostKind::Question,
                        format!("{} ({}:{})", d.title, d.file, d.line),
                    );
                    q.round = Some(*round);
                    let mut t = new(
                        format!("R{round}.{}", n + 1),
                        Kind::Refuted,
                        d.title.clone(),
                        "reviewer".into(),
                        q,
                        vec![
                            d.lens.as_str().into(),
                            severity(d.severity).into(),
                            format!("{}:{}", d.file, d.line),
                        ],
                    );
                    let mut c = post(
                        e,
                        &who,
                        PostKind::Comment,
                        format!("Not real: {}", d.reason),
                    );
                    c.round = Some(*round);
                    c.accepted = true;
                    t.posts.push(c);
                    t.status = ThreadStatus::Refuted;
                    threads.push(t);
                }
            }
            Body::Verdict {
                sha,
                round,
                findings,
                prior,
                reviewer,
                ..
            } => {
                let who = match reviewer {
                    Some(m) => format!("reviewer ({m})"),
                    None => "reviewer".into(),
                };
                for p in prior {
                    let Some(i) = find(&mut threads, &p.id) else {
                        continue;
                    };
                    let t = &mut threads[i];
                    let disputed = t
                        .posts
                        .iter()
                        .rev()
                        .find(|x| x.kind == PostKind::Answer)
                        .is_some_and(|x| x.body.starts_with("Disputed"));
                    let mut body = match p.status {
                        PriorStatus::Fixed => format!("Fixed at {}.", short(sha)),
                        PriorStatus::Open => {
                            if disputed {
                                "Dispute turned down: still open.".into()
                            } else {
                                "Still open.".into()
                            }
                        }
                        PriorStatus::Withdrawn => {
                            if disputed {
                                "Dispute upheld: withdrawn.".into()
                            } else {
                                "Withdrawn.".into()
                            }
                        }
                    };
                    if let Some(n) = p.note.as_deref().filter(|n| !n.trim().is_empty()) {
                        body.push(' ');
                        body.push_str(n);
                    }
                    let settles = p.status != PriorStatus::Open;
                    if settles
                        && let Some(a) = t
                            .posts
                            .iter_mut()
                            .rev()
                            .find(|x| x.kind == PostKind::Answer)
                    {
                        // A fix accepted by a fix, a dispute by a withdrawal.
                        let fits = match p.status {
                            PriorStatus::Fixed => !a.body.starts_with("Disputed"),
                            _ => a.body.starts_with("Disputed"),
                        };
                        a.accepted = fits;
                    }
                    let mut r = post(e, &who, PostKind::Ruling, body);
                    r.round = Some(*round);
                    r.sha = Some(sha.clone());
                    t.posts.push(r);
                }
                let checked = verified
                    .as_ref()
                    .filter(|(r, n, _)| r == round && *n > 0)
                    .map(|(_, _, w)| w.clone());
                for f in findings {
                    if find(&mut threads, &f.id).is_some() {
                        continue;
                    }
                    // The title heads the thread; the question is the scenario.
                    let mut body = f.scenario.clone();
                    if !f.fix.trim().is_empty() {
                        body.push_str(&format!("\n\nSuggested fix: {}", f.fix));
                    }
                    let mut q = post(e, &who, PostKind::Question, body);
                    q.round = Some(*round);
                    q.sha = Some(sha.clone());
                    let mut tags = vec![
                        f.lens.as_str().into(),
                        severity(f.severity).into(),
                        format!("{}:{}", f.file, f.line),
                    ];
                    if f.pre_existing {
                        tags.push("pre-existing".into());
                    }
                    let mut t = new(
                        f.id.clone(),
                        Kind::Finding,
                        f.title.clone(),
                        who.clone(),
                        q,
                        tags,
                    );
                    let blocking = matches!(f.severity, Severity::Blocker | Severity::Major);
                    if let (true, false, Some(v)) = (blocking, f.pre_existing, checked.as_ref()) {
                        let mut c = post(
                            e,
                            v,
                            PostKind::Comment,
                            "Checked it and couldn't show it was wrong: it stands.".into(),
                        );
                        c.round = Some(*round);
                        t.posts.push(c);
                    }
                    threads.push(t);
                }
            }
            Body::FixSubmitted {
                sha,
                fixes,
                disputes,
            } => {
                let who = author(&e.actor);
                for f in fixes {
                    if let Some(i) = find(&mut threads, f) {
                        let mut a = post(
                            e,
                            &who,
                            PostKind::Answer,
                            format!("Fixed in {}.", short(sha)),
                        );
                        a.sha = Some(sha.clone());
                        threads[i].posts.push(a);
                    }
                }
                for d in disputes {
                    if let Some(i) = find(&mut threads, &d.id) {
                        let mut a =
                            post(e, &who, PostKind::Answer, format!("Disputed: {}", d.reason));
                        a.sha = Some(sha.clone());
                        threads[i].posts.push(a);
                    }
                }
            }
            Body::Question {
                qid,
                text,
                options,
                default,
                ..
            } => {
                let who = author(&e.actor);
                let mut body = text.clone();
                if !options.is_empty() {
                    body.push_str(&format!("\n\nOptions: {}", options.join(" · ")));
                }
                if let Some(d) = default {
                    body.push_str(&format!("\nDefault: {d}"));
                }
                let q = post(e, &who, PostKind::Question, body);
                threads.push(new(
                    format!("q-{qid}"),
                    Kind::Question,
                    crate::output::clip(text, 100),
                    who,
                    q,
                    vec!["question".into()],
                ));
            }
            Body::Answer {
                qid,
                answer,
                defaulted,
            } => {
                if let Some(i) = find(&mut threads, &format!("q-{qid}")) {
                    let (who, body) = if *defaulted {
                        (
                            "Hallmark".to_string(),
                            format!("Nobody answered in time; the default was used: {answer}"),
                        )
                    } else {
                        (author(&e.actor), answer.clone())
                    };
                    let mut a = post(e, &who, PostKind::Answer, body);
                    a.accepted = true;
                    threads[i].posts.push(a);
                    threads[i].status = ThreadStatus::Resolved;
                }
            }
            Body::AnswerDelivered { qid } => {
                if let Some(i) = find(&mut threads, &format!("q-{qid}")) {
                    threads[i].posts.push(post(
                        e,
                        "Hallmark",
                        PostKind::Comment,
                        "Sent to the session that asked.".into(),
                    ));
                }
            }
            _ => {}
        }
    }

    // A finding's status is the ledger's, so it agrees with `hall findings`;
    // an open finding with an answer since the last ruling is answered.
    let ledger = Ledger::from_events(events);
    for t in threads.iter_mut().filter(|t| t.kind == Kind::Finding) {
        let fid = t.id.rsplit('/').next().unwrap_or_default();
        let Some(tr) = ledger.get(fid) else { continue };
        t.status = match tr.status {
            Status::Fixed => ThreadStatus::Resolved,
            Status::Withdrawn => ThreadStatus::Withdrawn,
            Status::Open => {
                let last = t.posts.iter().rev().find(|p| p.kind != PostKind::Comment);
                if last.is_some_and(|p| p.kind == PostKind::Answer) {
                    ThreadStatus::Answered
                } else {
                    ThreadStatus::Open
                }
            }
        };
    }
    for t in &mut threads {
        if state.is_terminal() && t.status.live() {
            t.status = ThreadStatus::Closed;
        }
        t.replies = t
            .posts
            .iter()
            .filter(|p| p.kind != PostKind::Question)
            .count();
        t.last_at = t.posts.iter().map(|p| p.t).max().unwrap_or(t.asked_at);
    }
    threads
}

/// Live threads first, then the most recently active.
pub fn sort(threads: &mut [Thread]) {
    threads.sort_by(|a, b| {
        b.status
            .live()
            .cmp(&a.status.live())
            .then(b.last_at.cmp(&a.last_at))
            .then(a.id.cmp(&b.id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::VerdictKind;
    use crate::findings::{
        Counts, Dispute, DroppedFinding, Finding, Lens, PriorRuling, PriorStatus,
    };

    fn ev(actor: &str, body: Body) -> Event {
        Event::new(actor, Some("H-0001".parse().unwrap()), body)
    }

    fn finding(id: &str, round: u32, sev: Severity) -> Finding {
        Finding {
            id: id.into(),
            round,
            lens: Lens::Correctness,
            severity: sev,
            file: "src/a.rs".into(),
            line: 12,
            title: format!("{id} breaks"),
            scenario: "When x, then y.".into(),
            fix: "Check x.".into(),
            pre_existing: false,
        }
    }

    fn verdict(round: u32, sha: &str, findings: Vec<Finding>, prior: Vec<PriorRuling>) -> Body {
        Body::Verdict {
            sha: sha.into(),
            round,
            verdict: if findings.is_empty() {
                VerdictKind::Approve
            } else {
                VerdictKind::Changes
            },
            escalated: false,
            findings,
            prior,
            counts: Counts::default(),
            summary: None,
            report: None,
            reviewer: Some("opus".into()),
            learnings: vec![],
            base: None,
            since: None,
            needs_person: None,
        }
    }

    fn ruling(id: &str, status: PriorStatus) -> PriorRuling {
        PriorRuling {
            id: id.into(),
            status,
            note: None,
        }
    }

    fn threads(events: &[Event]) -> Vec<Thread> {
        of_item(
            "H-0001".parse().unwrap(),
            "Retry webhooks",
            State::Review,
            events,
        )
    }

    #[test]
    fn a_finding_reads_as_a_question_answered_and_ruled() {
        let events = vec![
            ev(
                "assayer",
                Body::Verified {
                    sha: "aaa1111".into(),
                    round: 1,
                    model: Some("sonnet".into()),
                    checked: 1,
                    dropped: vec![],
                },
            ),
            ev(
                "assayer",
                verdict(
                    1,
                    "aaa1111",
                    vec![finding("F1", 1, Severity::Major)],
                    vec![],
                ),
            ),
            ev(
                "fixer",
                Body::FixSubmitted {
                    sha: "bbb2222".into(),
                    fixes: vec!["F1".into()],
                    disputes: vec![],
                },
            ),
            ev(
                "assayer",
                verdict(2, "bbb2222", vec![], vec![ruling("F1", PriorStatus::Fixed)]),
            ),
        ];
        let t = &threads(&events)[0];
        assert_eq!(t.id, "H-0001/F1");
        assert_eq!(t.kind, Kind::Finding);
        assert_eq!(t.status, ThreadStatus::Resolved);
        assert_eq!(t.asked_by, "reviewer (opus)");
        assert_eq!(t.tags, vec!["correctness", "major", "src/a.rs:12"]);
        let kinds: Vec<(PostKind, &str)> = t
            .posts
            .iter()
            .map(|p| (p.kind, p.author.as_str()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (PostKind::Question, "reviewer (opus)"),
                (PostKind::Comment, "verifier (sonnet)"),
                (PostKind::Answer, "fixer"),
                (PostKind::Ruling, "reviewer (opus)"),
            ]
        );
        assert!(t.posts[2].accepted, "the fix is the accepted answer");
        assert_eq!(t.posts[3].body, "Fixed at bbb2222.");
        assert_eq!(t.replies, 3);
    }

    #[test]
    fn a_dispute_waits_then_is_upheld_or_turned_down() {
        let mut events = vec![
            ev(
                "assayer",
                verdict(
                    1,
                    "aaa",
                    vec![
                        finding("F1", 1, Severity::Minor),
                        finding("F2", 1, Severity::Minor),
                    ],
                    vec![],
                ),
            ),
            ev(
                "fixer",
                Body::FixSubmitted {
                    sha: "bbb".into(),
                    fixes: vec![],
                    disputes: vec![
                        Dispute {
                            id: "F1".into(),
                            reason: "x is never empty here".into(),
                        },
                        Dispute {
                            id: "F2".into(),
                            reason: "out of scope".into(),
                        },
                    ],
                },
            ),
        ];
        let t = threads(&events);
        assert_eq!(t[0].status, ThreadStatus::Answered);
        assert_eq!(t[0].posts[1].body, "Disputed: x is never empty here");

        events.push(ev(
            "assayer",
            verdict(
                2,
                "bbb",
                vec![],
                vec![
                    ruling("F1", PriorStatus::Withdrawn),
                    ruling("F2", PriorStatus::Open),
                ],
            ),
        ));
        let t = threads(&events);
        assert_eq!(t[0].status, ThreadStatus::Withdrawn);
        assert!(t[0].posts[1].accepted);
        assert_eq!(t[0].posts[2].body, "Dispute upheld: withdrawn.");
        assert_eq!(t[1].status, ThreadStatus::Open);
        assert!(!t[1].posts[1].accepted);
        assert_eq!(t[1].posts[2].body, "Dispute turned down: still open.");
    }

    #[test]
    fn the_verifiers_refutations_are_closed_threads() {
        let events = vec![ev(
            "assayer",
            Body::Verified {
                sha: "aaa".into(),
                round: 2,
                model: None,
                checked: 2,
                dropped: vec![DroppedFinding {
                    lens: Lens::Correctness,
                    severity: Severity::Blocker,
                    file: "src/b.rs".into(),
                    line: 4,
                    title: "Panics on empty input".into(),
                    reason: "src/b.rs:2 returns early when empty".into(),
                }],
            },
        )];
        let t = &threads(&events)[0];
        assert_eq!(t.id, "H-0001/R2.1");
        assert_eq!(t.status, ThreadStatus::Refuted);
        assert_eq!(
            t.posts[1].body,
            "Not real: src/b.rs:2 returns early when empty"
        );
    }

    #[test]
    fn planning_and_questions_are_threads() {
        let created = ev(
            "you",
            Body::Created {
                title: "Retry webhooks".into(),
                branch: "b".into(),
                worktree: "w".into(),
                base: "main".into(),
                issue: None,
                request: Some("Retry failed webhook deliveries".into()),
                adopted: false,
                parent: None,
                follow_up: None,
                pr_url: None,
                pr_head: None,
                own_worktree: None,
            },
        );
        let events = vec![
            created,
            ev(
                "you",
                Body::Planned {
                    parallel: false,
                    children: vec![],
                    reason: Some("One change in one module.".into()),
                },
            ),
            ev(
                "agent",
                Body::Question {
                    qid: "q1".into(),
                    text: "Six attempts or ten?".into(),
                    options: vec!["6".into(), "10".into()],
                    default: Some("6".into()),
                    wait: None,
                    deadline: None,
                },
            ),
        ];
        let t = threads(&events);
        assert_eq!(t[0].id, "H-0001/plan");
        assert_eq!(t[0].status, ThreadStatus::Resolved);
        assert_eq!(t[0].posts[1].author, "front desk");
        assert_eq!(t[0].posts[1].body, "One item. One change in one module.");
        assert_eq!(t[1].id, "H-0001/q-q1");
        assert_eq!(t[1].status, ThreadStatus::Open);
        assert!(t[1].posts[0].body.contains("Options: 6 · 10"));

        let mut events = events;
        events.push(ev(
            "you",
            Body::Answer {
                qid: "q1".into(),
                answer: "6".into(),
                defaulted: false,
            },
        ));
        events.push(ev("hall", Body::AnswerDelivered { qid: "q1".into() }));
        let t = threads(&events);
        assert_eq!(t[1].status, ThreadStatus::Resolved);
        assert_eq!(t[1].replies, 2);
        assert_eq!(t[1].posts[2].body, "Sent to the session that asked.");
    }

    #[test]
    fn threads_left_open_close_with_their_item() {
        let events = vec![ev(
            "assayer",
            verdict(1, "aaa", vec![finding("F1", 1, Severity::Nit)], vec![]),
        )];
        let t = of_item("H-0001".parse().unwrap(), "x", State::Closed, &events);
        assert_eq!(t[0].status, ThreadStatus::Closed);
        let t = of_item("H-0001".parse().unwrap(), "x", State::Review, &events);
        assert_eq!(t[0].status, ThreadStatus::Open);
    }

    #[test]
    fn live_threads_sort_first() {
        let events = vec![
            ev(
                "assayer",
                verdict(1, "aaa", vec![finding("F1", 1, Severity::Minor)], vec![]),
            ),
            ev(
                "agent",
                Body::Question {
                    qid: "q1".into(),
                    text: "?".into(),
                    options: vec![],
                    default: None,
                    wait: None,
                    deadline: None,
                },
            ),
            ev(
                "you",
                Body::Answer {
                    qid: "q1".into(),
                    answer: "yes".into(),
                    defaulted: false,
                },
            ),
        ];
        let mut t = threads(&events);
        sort(&mut t);
        assert_eq!(t[0].id, "H-0001/F1");
    }
}
