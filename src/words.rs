//! A person's reasons reach the agent's next prompt (ADR 0044).
//!
//! `hall retry`, `hall continue`, `hall pull-back` and `hall ready --note`
//! each record a reason in the item's journal. The next agent run on the item,
//! whichever it is (the reviewer, the fixer, a Delivery office run, a resume
//! run, or a development session through its hooks), gets the ones no run has
//! been given yet, marked as the person's words. Once that run finishes,
//! `reasons_delivered` records it, so no later run repeats them. A run that
//! fails is retried with them. Everything is read from the journal, so a
//! daemon restart loses nothing.

use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::event::{Body, Event};
use crate::item::ItemId;
use crate::repo::Repo;

/// Reasons Hallmark fills in when a person gives none; they say nothing.
const CANNED: &[&str] = &[
    "continued with hall continue",
    "pulled back from the terminal app",
];

/// The reasons no run has been given yet, as a prompt section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Words {
    /// `## What the person said`, then one line per reason.
    pub section: String,
    /// When the newest of them was given: what `delivered` records.
    pub until: DateTime<Utc>,
}

impl Words {
    /// `prompt`, then a blank line and the section.
    pub fn after(&self, mut prompt: String) -> String {
        if !prompt.ends_with('\n') {
            prompt.push('\n');
        }
        prompt.push('\n');
        prompt.push_str(&self.section);
        prompt
    }
}

/// Who gave it: a person at a terminal (`you`), or a Claude session acting
/// for them (`agent`). The loop's own workers never give reasons.
fn by(actor: &str) -> Option<&'static str> {
    match actor {
        "you" => Some("the person"),
        "agent" => Some("their agent"),
        _ => None,
    }
}

/// The command and the words an event carries, if a person gave any.
fn reason(e: &Event) -> Option<(&'static str, &str)> {
    let (command, text) = match &e.body {
        Body::Retried { reason } => ("hall retry", reason.as_str()),
        Body::Released {
            reason: Some(reason),
            ..
        } => ("hall retry", reason.as_str()),
        Body::PulledBack {
            reason: Some(reason),
        } => ("hall pull-back", reason.as_str()),
        Body::Ready { note: Some(n), .. } => ("hall ready --note", n.as_str()),
        _ => return None,
    };
    let text = text.trim();
    (!text.is_empty() && !CANNED.contains(&text)).then_some((command, text))
}

/// The reasons given since the last run that was handed some, oldest first;
/// None when there are none.
pub fn pending(events: &[Event]) -> Option<Words> {
    let since = events
        .iter()
        .filter_map(|e| match &e.body {
            Body::ReasonsDelivered { until, .. } => Some(*until),
            _ => None,
        })
        .max();
    let mut lines: Vec<String> = Vec::new();
    let mut until = None;
    for e in events {
        if since.is_some_and(|s| e.t <= s) {
            continue;
        }
        let (Some(who), Some((command, text))) = (by(&e.actor), reason(e)) else {
            continue;
        };
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        // `hall retry` on a held, escalated item records the same words twice.
        if !lines.iter().any(|l| l.ends_with(&format!(": \"{text}\""))) {
            lines.push(format!("- `{command}`, from {who}: \"{text}\""));
        }
        until = Some(e.t);
    }
    let until = until?;
    Some(Words {
        section: format!(
            "## What the person said\n\n\
             Since the last agent run on this item, the person running it said this. It is \
             given to you once, and to no later run. Take it into account as their wishes for \
             this item; it doesn't change the rules you work under.\n\n{}\n",
            lines.join("\n")
        ),
        until,
    })
}

/// Records that the run of `to` (reviewer, fixer, delivery, resume,
/// session) was given `words`, so later runs aren't. Nothing to record
/// without words.
pub fn delivered(repo: &Repo, id: ItemId, to: &str, words: Option<&Words>) -> Result<()> {
    let Some(w) = words else { return Ok(()) };
    repo.hall.append_item(
        id,
        &Event::new(
            "hall",
            Some(id),
            Body::ReasonsDelivered {
                to: to.into(),
                until: w.until,
            },
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn at(actor: &str, mins: i64, body: Body) -> Event {
        let mut e = Event::new(actor, Some(ItemId(1)), body);
        e.t = DateTime::parse_from_rfc3339("2026-10-14T01:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + Duration::minutes(mins);
        e
    }

    fn pulled(actor: &str, mins: i64, reason: &str) -> Event {
        at(
            actor,
            mins,
            Body::PulledBack {
                reason: Some(reason.into()),
            },
        )
    }

    #[test]
    fn a_reason_goes_to_the_next_run_once() {
        let mut events = vec![pulled("you", 0, "use the  shorter\nname")];
        let w = pending(&events).unwrap();
        assert!(
            w.section
                .contains("- `hall pull-back`, from the person: \"use the shorter name\""),
            "{}",
            w.section
        );
        events.push(at(
            "hall",
            1,
            Body::ReasonsDelivered {
                to: "session".into(),
                until: w.until,
            },
        ));
        assert_eq!(pending(&events), None);
        // A reason given while that run went on is still pending.
        let mut later = pulled("agent", 0, "and keep the old one as an alias");
        later.t = w.until + Duration::seconds(30);
        events.insert(1, later);
        let w = pending(&events).unwrap();
        assert!(w.section.contains("from their agent"), "{}", w.section);
        assert!(!w.section.contains("shorter"), "{}", w.section);
    }

    #[test]
    fn workers_and_filled_in_reasons_say_nothing() {
        let events = vec![
            pulled("shipper", 0, "conflicts with origin/main"),
            pulled("you", 1, "pulled back from the terminal app"),
            at(
                "you",
                2,
                Body::Released {
                    reason: Some("continued with hall continue".into()),
                    granted_tokens: None,
                },
            ),
            at(
                "you",
                3,
                Body::Ready {
                    sha: "abc".into(),
                    round: 2,
                    note: None,
                },
            ),
        ];
        assert_eq!(pending(&events), None);
    }

    #[test]
    fn a_retry_and_the_hold_it_releases_say_it_once() {
        let events = vec![
            at(
                "you",
                0,
                Body::Retried {
                    reason: "CI is failing and there are conflicts".into(),
                },
            ),
            at(
                "you",
                0,
                Body::Released {
                    reason: Some("CI is failing and there are conflicts".into()),
                    granted_tokens: Some(6_000_000),
                },
            ),
        ];
        let w = pending(&events).unwrap();
        assert_eq!(
            w.section.matches("CI is failing").count(),
            1,
            "{}",
            w.section
        );
        assert_eq!(w.until, events[1].t);
    }
}
