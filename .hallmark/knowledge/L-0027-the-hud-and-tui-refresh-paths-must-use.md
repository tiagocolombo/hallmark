---
id: L-0027
kind: rule
scope: ["src/commands/hud.rs", "src/tui/**"]
lens: operability
source: ["H-0008/F6"]
status: proposed
used: 0
---
# The HUD and TUI refresh paths must use the cheap now variant and must not run git in the worktree for fields they discard.

hud.rs promises one cheap call that reads only the journal, the index and daemon state. Running git log or git status there and keeping only some fields makes every refresh slow in large worktrees. The fix is now_cheap, which reads bounded tails of the live and checks logs. Any new field in these paths should be checked for the same cost.
