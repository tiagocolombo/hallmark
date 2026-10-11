---
id: L-0051
kind: rule
scope: ["src/refresh.rs"]
lens: correctness
source: ["H-0023/F3"]
status: proposed
used: 0
---
# Refresh must merge into the base only when the worktree has the item's own branch checked out and is clean at the PR head.

A worktree whose HEAD equals the PR head can still be on a different branch after `git switch -c`, so checking only the commit and cleanliness would merge the base into the wrong branch. Refresh must also require that the current branch equals the item's branch before merging; otherwise it should take the not-at-PR-head path. The reviewer can check this by reading the branch guard in refresh() before the merge call.
