---
id: L-0037
kind: rule
scope: ["src/commands/library.rs", "src/library/portable.rs"]
lens: repo_rules
source: ["H-0012/F3"]
status: proposed
used: 0
---
# Hints printed by hall library commands must not tell an agent to commit to the checked-out branch; route through hall new and the item's worktree.

In the main checkout the base branch is checked out, and the PreToolUse commit gate denies or asks for commits there outside an item's worktree. A next-step hint like 'git rm -r ... && git commit' is a dead end for an agent. The hint should start an item with hall new --title and perform the removal in that worktree, and appear only when nothing was skipped.
