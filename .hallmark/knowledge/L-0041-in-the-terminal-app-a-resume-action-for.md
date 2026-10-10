---
id: L-0041
kind: rule
scope: ["src/tui/app.rs"]
lens: correctness
source: ["H-0022/F1"]
status: proposed
used: 0
---
# In the terminal app, a resume action for a session with no item must run in the session's own folder, not in the worktree of whichever board item was selected earlier.

When a session_waiting entry has no item (notifications.item is NULL, session started in the main checkout), `hand_over` must not reuse the currently selected board item's worktree, because the Claude Code session is stored per project folder and `claude --resume` will fail or open the wrong conversation. Check that `follow_entry` or the selection path sets the working directory from the entry itself. This finding was raised in round 1 and was still open when the item closed, so verify it is fixed before accepting resume actions.
