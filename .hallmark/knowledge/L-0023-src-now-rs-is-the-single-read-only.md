---
id: L-0023
kind: map
scope: ["src/now.rs", "src/commands/status.rs", "src/commands/watch.rs", "src/commands/hud.rs", "src/hooks.rs", "src/tui/data.rs"]
lens: first_principles
source: ["H-0008"]
status: proposed
used: 0
---
# src/now.rs is the single read-only derivation of an item's 'now' state, shared by hall status, hall inbox, hall watch, the HUD and the TUI.

The now view explains what an item is doing (queued position, checks gate, review model and timeout, fixing run, held or stuck reason, awaiting-you evidence and actions). Its call sites are status, inbox, watch, the HUD JSON, the TUI, and the SessionStart and UserPromptSubmit hook briefs. New surfaces should call it rather than recompute phase logic, and it must not write the journal. Use now_cheap for HUD and TUI paths because the full now runs git.
