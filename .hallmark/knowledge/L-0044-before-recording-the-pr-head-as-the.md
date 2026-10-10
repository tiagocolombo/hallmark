---
id: L-0044
kind: rule
scope: ["src/commands/new.rs"]
lens: correctness
source: ["H-0018/F2", "H-0018/F7"]
status: proposed
used: 0
---
# Before recording the PR head as the force-push lease, the worktree must contain that head, and reopen must skip that check only when the PR head equals the item's recorded pr_head.

If the PR head moved (for example a reviewer's suggestion commit) and the worktree lacks it, a lease based on that head will silently force-push away the commit. ensure_has_head fetches and fast-forwards a clean worktree, and refuses a diverged one. The skip exists so a locally rebased worktree at the shipped sha can still be adopted. Review any change to reopen or adopt that records pr_head without going through ensure_has_head.
