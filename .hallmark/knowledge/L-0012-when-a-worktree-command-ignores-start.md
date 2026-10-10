---
id: L-0012
kind: rule
scope: ["src/commands/new.rs"]
lens: correctness
source: ["H-0004/F7"]
status: proposed
used: 0
---
# When a worktree command ignores {start}, the stacked follow-up's create step must reset the fresh branch to the start commit and fail `hall new` on error, never discard a failed fast-forward.

A silent failure of `git merge --ff-only` leaves the follow-up on main, while its request says it starts from the PR's branch, and the review diff is then wrong. The create path should verify the branch's tip after the worktree command, and surface any error from the reset.
