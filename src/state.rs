//! The item state machine. Every state change in the journal goes through
//! [`next`]; the table is tested exhaustively below.

use schemars::JsonSchema;
use serde::Serialize;

use crate::item::State;

/// Something that moves an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// The dev agent thinks it's done (`hall propose-ready`).
    ProposeReady,
    /// You send it to review (`hall ready`): freezes the commit.
    Ready,
    /// You take it back to development (`hall pull-back`, or "not yet").
    PullBack,
    /// You abandon it (`hall drop --reason`).
    Drop,
    /// The gates passed at the frozen commit.
    ChecksPass,
    /// A gate failed.
    ChecksFail,
    /// The principal engineer approved: 0 blockers, 0 majors.
    Approve,
    /// The principal engineer asked for changes.
    RequestChanges,
    /// Changes requested for the last allowed time (`max_rejects`).
    Escalate,
    /// The fixer committed and resubmitted.
    Resubmit,
    /// You allow another round after an escalation (`hall retry`).
    Retry,
    /// The shipper opened the PR.
    Ship,
    /// A child item (a parallel part) was merged into its parent's branch.
    Merge,
    /// A shipped item whose PR is still open goes back to development, to
    /// rebase it, resolve conflicts or change it (`hall adopt`).
    Reopen,
    /// An item reopened to fix its PR's CI goes back to the PR unchanged:
    /// the checks passed on a re-run (ADR 0047).
    Restore,
}

impl Action {
    pub const ALL: [Action; 15] = [
        Action::ProposeReady,
        Action::Ready,
        Action::PullBack,
        Action::Drop,
        Action::ChecksPass,
        Action::ChecksFail,
        Action::Approve,
        Action::RequestChanges,
        Action::Escalate,
        Action::Resubmit,
        Action::Retry,
        Action::Ship,
        Action::Merge,
        Action::Reopen,
        Action::Restore,
    ];
}

/// The state an action leads to, or `None` if it isn't allowed.
pub fn next(from: State, action: Action) -> Option<State> {
    use Action as A;
    use State as S;

    // A closed item comes back only through its open PR; nothing else
    // moves a closed or dropped item.
    if (from, action) == (S::Closed, A::Reopen) {
        return Some(S::Development);
    }
    if from.is_terminal() {
        return None;
    }
    match (from, action) {
        // Dropped at any time, with a reason.
        (_, A::Drop) => Some(S::Dropped),

        (S::Development, A::ProposeReady) => Some(S::AwaitingYou),
        (S::Development | S::AwaitingYou, A::Ready) => Some(S::Checks),

        // "Not yet", a pull-back from the loop, or taking an escalation back.
        (
            S::AwaitingYou | S::Checks | S::Review | S::Fixing | S::Escalated | S::Hallmarked,
            A::PullBack,
        ) => Some(S::Development),

        (S::Checks, A::ChecksPass) => Some(S::Review),
        (S::Checks, A::ChecksFail) => Some(S::Fixing),

        (S::Review, A::Approve) => Some(S::Hallmarked),
        (S::Review, A::RequestChanges) => Some(S::Fixing),
        (S::Review, A::Escalate) => Some(S::Escalated),

        (S::Fixing, A::Resubmit) => Some(S::Checks),
        (S::Escalated, A::Retry) => Some(S::Fixing),

        (S::Hallmarked, A::Ship) => Some(S::Closed),

        // A parallel part merges into its parent; the parent is reviewed.
        (S::Development | S::AwaitingYou, A::Merge) => Some(S::Closed),
        // Back to its PR, unchanged, once its CI passes again.
        (S::Development, A::Restore) => Some(S::Closed),

        _ => None,
    }
}

/// The actions allowed from a state, for `next` hints and docs.
pub fn allowed(from: State) -> Vec<Action> {
    Action::ALL
        .into_iter()
        .filter(|a| next(from, *a).is_some())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use Action as A;
    use State as S;
    use std::collections::HashMap;

    /// The whole transition table. Anything not listed is rejected.
    fn table() -> HashMap<(State, Action), State> {
        let mut t = HashMap::new();
        let rows: &[(State, Action, State)] = &[
            (S::Development, A::ProposeReady, S::AwaitingYou),
            (S::Development, A::Ready, S::Checks),
            (S::Development, A::Drop, S::Dropped),
            (S::Development, A::Merge, S::Closed),
            (S::Development, A::Restore, S::Closed),
            (S::AwaitingYou, A::Merge, S::Closed),
            (S::AwaitingYou, A::Ready, S::Checks),
            (S::AwaitingYou, A::PullBack, S::Development),
            (S::AwaitingYou, A::Drop, S::Dropped),
            (S::Checks, A::ChecksPass, S::Review),
            (S::Checks, A::ChecksFail, S::Fixing),
            (S::Checks, A::PullBack, S::Development),
            (S::Checks, A::Drop, S::Dropped),
            (S::Review, A::Approve, S::Hallmarked),
            (S::Review, A::RequestChanges, S::Fixing),
            (S::Review, A::Escalate, S::Escalated),
            (S::Review, A::PullBack, S::Development),
            (S::Review, A::Drop, S::Dropped),
            (S::Fixing, A::Resubmit, S::Checks),
            (S::Fixing, A::PullBack, S::Development),
            (S::Fixing, A::Drop, S::Dropped),
            (S::Escalated, A::Retry, S::Fixing),
            (S::Escalated, A::PullBack, S::Development),
            (S::Escalated, A::Drop, S::Dropped),
            (S::Hallmarked, A::Ship, S::Closed),
            (S::Hallmarked, A::PullBack, S::Development),
            (S::Hallmarked, A::Drop, S::Dropped),
            (S::Closed, A::Reopen, S::Development),
        ];
        for (from, a, to) in rows {
            t.insert((*from, *a), *to);
        }
        t
    }

    #[test]
    fn transition_table_is_exact() {
        let t = table();
        for from in State::ALL {
            for a in Action::ALL {
                assert_eq!(
                    next(from, a),
                    t.get(&(from, a)).copied(),
                    "{from:?} + {a:?}"
                );
            }
        }
    }

    #[test]
    fn terminal_states_never_move() {
        for a in Action::ALL {
            assert_eq!(next(S::Dropped, a), None);
            if a != A::Reopen {
                assert_eq!(next(S::Closed, a), None);
            }
        }
        // Only a live item is reopened; Reopen moves nothing else.
        for from in State::ALL.into_iter().filter(|s| *s != S::Closed) {
            assert_eq!(next(from, A::Reopen), None, "{from:?}");
        }
    }

    #[test]
    fn every_live_state_can_be_dropped_and_reached() {
        for from in State::ALL.into_iter().filter(|s| !s.is_terminal()) {
            assert_eq!(next(from, A::Drop), Some(S::Dropped));
        }
        // Every state except Development is the target of some transition;
        // Development is where items are created.
        let targets: Vec<State> = table().values().copied().collect();
        for s in State::ALL {
            assert!(
                s == S::Development || targets.contains(&s),
                "{s:?} unreachable"
            );
        }
    }

    #[test]
    fn the_happy_path_and_the_fix_loop() {
        let path = [
            (A::ProposeReady, S::AwaitingYou),
            (A::Ready, S::Checks),
            (A::ChecksFail, S::Fixing),
            (A::Resubmit, S::Checks),
            (A::ChecksPass, S::Review),
            (A::RequestChanges, S::Fixing),
            (A::Resubmit, S::Checks),
            (A::ChecksPass, S::Review),
            (A::Approve, S::Hallmarked),
            (A::Ship, S::Closed),
        ];
        let mut s = S::Development;
        for (a, want) in path {
            s = next(s, a).unwrap_or_else(|| panic!("{s:?} + {a:?}"));
            assert_eq!(s, want);
        }
    }
}
