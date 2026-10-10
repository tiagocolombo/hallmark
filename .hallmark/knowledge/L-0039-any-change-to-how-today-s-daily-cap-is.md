---
id: L-0039
kind: rule
scope: ["src/commands/hud.rs", "src/tui/ui.rs", "src/budget.rs"]
lens: correctness
source: ["H-0019/F1"]
status: proposed
used: 0
---
# Any change to how today's daily cap is computed must reach every reader of that cap, including the HUD and the TUI usage view, not only the pre-run checks.

H-0019 added a grant that raises today's daily cap, and wired it into the pre-run budget checks, `hall budget`, the statusline and the session hooks. It missed `hall hud` (src/commands/hud.rs), which reports the cap and draws '50k/40k today', and the TUI usage view (src/tui/ui.rs), which scales the loop bar against the unadjusted cap. Review cap-related changes by grepping for every reader of the daily cap. The finding was still open when the item closed, so check whether it was fixed before relying on this.
