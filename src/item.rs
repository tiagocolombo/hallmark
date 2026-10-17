//! Items: one piece of work from first prompt to PR.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{ErrorCode, HallError};

/// `H-0042`. Numbered per repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemId(pub u32);

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "H-{:04}", self.0)
    }
}

impl FromStr for ItemId {
    type Err = HallError;

    /// Accepts `H-0042`, `h-42`, `H42` and `42`.
    fn from_str(s: &str) -> Result<Self, HallError> {
        let t = s.trim();
        let digits = t
            .strip_prefix("H-")
            .or_else(|| t.strip_prefix("h-"))
            .or_else(|| t.strip_prefix('H'))
            .or_else(|| t.strip_prefix('h'))
            .unwrap_or(t);
        match digits.parse::<u32>() {
            Ok(n) if n > 0 => Ok(ItemId(n)),
            _ => Err(HallError::new(
                ErrorCode::Usage,
                format!("\"{s}\" is not an item ID; IDs look like H-0042"),
            )
            .next(["hall status"])),
        }
    }
}

impl Serialize for ItemId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ItemId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse()
            .map_err(|e: HallError| serde::de::Error::custom(e.message))
    }
}

impl JsonSchema for ItemId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ItemId".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^H-[0-9]{4,}$",
            "description": "An item ID, such as H-0042"
        })
    }
}

/// Where an item is in its life. See `state.rs` for the transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// You and the dev agent are building it (Workshop).
    Development,
    /// The agent thinks it's done and waits for your call (Your Office).
    AwaitingYou,
    /// The frozen commit is queued for, or running, the repo's gates (Assay Office).
    Checks,
    /// The principal engineer is reviewing it (Assay Office).
    Review,
    /// A fixer is addressing failing checks or findings (Repair Shop).
    Fixing,
    /// Rejected too many times; waits for you (Your Office).
    Escalated,
    /// Approved; the commit carries a hallmark (Stamping Room).
    Hallmarked,
    /// The PR is open. The item's life is over, unless it is reopened
    /// through that PR (`hall adopt`) (Dispatch Dock).
    Closed,
    /// Abandoned, with a reason (Records).
    Dropped,
}

impl State {
    pub const ALL: [State; 9] = [
        State::Development,
        State::AwaitingYou,
        State::Checks,
        State::Review,
        State::Fixing,
        State::Escalated,
        State::Hallmarked,
        State::Closed,
        State::Dropped,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            State::Development => "development",
            State::AwaitingYou => "awaiting_you",
            State::Checks => "checks",
            State::Review => "review",
            State::Fixing => "fixing",
            State::Escalated => "escalated",
            State::Hallmarked => "hallmarked",
            State::Closed => "closed",
            State::Dropped => "dropped",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            State::Development => "Development",
            State::AwaitingYou => "Awaiting you",
            State::Checks => "Checks",
            State::Review => "Review",
            State::Fixing => "Fixing",
            State::Escalated => "Escalated",
            State::Hallmarked => "Hallmarked",
            State::Closed => "Closed",
            State::Dropped => "Dropped",
        }
    }

    /// The room in the building where this state happens.
    pub fn room(self) -> &'static str {
        match self {
            State::Development => "Workshop",
            State::AwaitingYou | State::Escalated => "Your Office",
            State::Checks | State::Review => "Assay Office",
            State::Fixing => "Repair Shop",
            State::Hallmarked => "Stamping Room",
            State::Closed => "Dispatch Dock",
            State::Dropped => "Records",
        }
    }

    /// Closed and Dropped end the item's life. A closed item whose PR is
    /// still open can be reopened (`state::Action::Reopen`).
    pub fn is_terminal(self) -> bool {
        matches!(self, State::Closed | State::Dropped)
    }

    /// The review loop runs on its own; the session should not edit the item.
    pub fn in_loop(self) -> bool {
        matches!(self, State::Checks | State::Review | State::Fixing)
    }

    pub fn parse(s: &str) -> Option<State> {
        State::ALL.into_iter().find(|st| st.as_str() == s)
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A GitHub issue an item started from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Issue {
    pub number: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// An item as folded from its journal. `item.json` holds a copy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Item {
    pub id: ItemId,
    pub title: String,
    pub state: State,
    pub branch: String,
    pub worktree: String,
    pub base: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<Issue>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// When the item entered its current state.
    pub state_since: DateTime<Utc>,
    /// Review rounds started (each `ready` or fix starts one).
    pub round: u32,
    /// Rejected reviews so far.
    pub rejects: u32,
    /// Checks that failed in a row: reset when they pass, at `ready`, and
    /// when `hall retry` lifts a `checks` hold.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub check_failures: u32,
    /// The commit frozen by the last `ready`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frozen_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hallmark_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    /// The commit the PR's head is at, as far as Hallmark knows: what it
    /// shipped, or what the PR had when the item took it over. An update
    /// push leases on this commit, or on the branch's head on origin when
    /// this item hallmarked that commit, as after an update that pushed and
    /// then failed (ADR 0042).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_head: Option<String>,
    /// The dev agent's summary from `propose-ready`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drop_reason: Option<String>,
    pub adopted: bool,
    pub events: u32,
    /// The parent item when this is a parallel part of a split request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ItemId>,
    /// The PR whose feedback started this item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<String>,
    /// Set while the loop waits for you (budget, repeated run failures).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<Hold>,
    /// Set by the watchman; cleared by the next event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stuck_since: Option<DateTime<Utc>>,
    /// The commit the last verdict reviewed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_sha: Option<String>,
    /// Open findings by severity.
    #[serde(default)]
    pub findings: crate::findings::Counts,
    /// The shipped PR's CI, once it finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci: Option<Ci>,
    /// Set while the branch waits for the Delivery office to rebase it
    /// onto its base (conflicts); cleared when the item moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebase: Option<RebaseRequest>,
    /// Set while new PR comments wait for the Delivery office's dev run;
    /// cleared when the item moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<FeedbackRequest>,
    /// Set while the shipped PR's failing CI waits for the Delivery
    /// office's dev run; cleared when the item moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_fix: Option<CiFixRequest>,
    /// How many times the Delivery office reopened the item to fix its PR's
    /// CI. After `delivery::CI_FIXES`, a red PR waits for you.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ci_fixes: u32,
    /// Why the Delivery office won't fix the red PR itself; it waits for you.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_declined: Option<String>,
    /// How the shipped PR ended: `merged` or `closed`. Until then the
    /// Delivery office watches it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered: Option<String>,
    /// The PR head the merger merged (ADR 0048); the watch then records
    /// `delivered`. Cleared when the item reopens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged: Option<String>,
    /// The PR head you let the merger merge although it touches a `lit`
    /// path (`hall retry` on its `lit` hold). Cleared when the item reopens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_approved: Option<String>,
    /// False when the worktree is a checkout the user made and adopted; the
    /// sweeper leaves it alone.
    #[serde(default = "own_default")]
    pub own_worktree: bool,
    /// The sweeper removed the worktree; cleared when the item reopens.
    #[serde(default, skip_serializing_if = "is_false")]
    pub swept: bool,
}

fn own_default() -> bool {
    true
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// PR comments the Delivery office's dev run should address.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FeedbackRequest {
    pub comments: Vec<crate::event::PrComment>,
    pub since: DateTime<Utc>,
}

/// The shipped PR's failing checks the Delivery office's dev run should fix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CiFixRequest {
    pub pr_url: String,
    pub failing: Vec<crate::event::FailingCheck>,
    pub since: DateTime<Utc>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Why and onto what the Delivery office should rebase an item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RebaseRequest {
    /// The base branch.
    pub onto: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    pub since: DateTime<Utc>,
}

/// How the shipped PR's CI ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Ci {
    pub passed: bool,
    /// The failing checks' names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failing: Vec<String>,
    /// `merged` or `closed` when the PR ended with red checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<String>,
    /// Set when no failing job ever started (billing, no runners): CI
    /// didn't run, so there is no code to fix. GitHub's reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_started: Option<String>,
    pub t: DateTime<Utc>,
}

/// Why the loop stopped working on an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HoldKind {
    /// A per-item budget cap was reached.
    Budget,
    /// A model run failed twice in a row.
    RunFailed,
    /// The ship command failed.
    ShipFailed,
    /// The branch conflicts with its base and the headless rebase couldn't
    /// resolve it.
    Conflict,
    /// New PR comments the Delivery office's dev run didn't act on: they
    /// need you.
    Feedback,
    /// The shipped PR's CI failed and the Delivery office's dev run
    /// couldn't fix it.
    CiFailed,
    /// The merge command failed (ADR 0048).
    MergeFailed,
    /// The PR is ready to merge but touches a `[merge] lit` path, so a
    /// person merges it, or lets the merger with `hall retry`.
    Lit,
    /// The checks failed `max_check_failures` times in a row: the fixes
    /// aren't turning the gates green.
    Checks,
    /// Anything else that needs a person.
    Other,
}

impl HoldKind {
    pub fn as_str(self) -> &'static str {
        match self {
            HoldKind::Budget => "budget",
            HoldKind::RunFailed => "run_failed",
            HoldKind::ShipFailed => "ship_failed",
            HoldKind::Conflict => "conflict",
            HoldKind::Feedback => "feedback",
            HoldKind::CiFailed => "ci_failed",
            HoldKind::MergeFailed => "merge_failed",
            HoldKind::Lit => "lit",
            HoldKind::Checks => "checks",
            HoldKind::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Hold {
    pub kind: HoldKind,
    pub reason: String,
    pub since: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_format_and_parse() {
        assert_eq!(ItemId(42).to_string(), "H-0042");
        assert_eq!(ItemId(12345).to_string(), "H-12345");
        for s in ["H-0042", "h-42", "H42", "42", " 42 "] {
            assert_eq!(s.parse::<ItemId>().unwrap(), ItemId(42), "{s}");
        }
        for s in ["", "H-", "H-x", "0", "Q-1"] {
            assert!(s.parse::<ItemId>().is_err(), "{s}");
        }
    }

    #[test]
    fn states_round_trip() {
        for s in State::ALL {
            assert_eq!(State::parse(s.as_str()), Some(s));
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(json, format!("\"{}\"", s.as_str()));
        }
    }
}
