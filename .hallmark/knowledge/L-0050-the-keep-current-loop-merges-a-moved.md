---
id: L-0050
kind: map
scope: ["src/refresh.rs", "src/ci.rs", "src/shipper.rs", "src/fixer.rs"]
source: ["H-0023"]
status: proposed
used: 0
---
# The keep-current loop merges a moved base into a shipped PR branch from refresh.rs, and conflicts are handed back to the fixer.

When the base branch moves under an open shipped PR, refresh.rs merges the latest base into the item's branch in its worktree. A clean merge is pushed under a base-merge hallmark without re-review, and the CI watch in ci.rs then tracks the new head. A conflicting merge reopens the item into fixing with a failed merge check; the review checks the resolution and the shipper updates the PR. CI state never blocks this path.
