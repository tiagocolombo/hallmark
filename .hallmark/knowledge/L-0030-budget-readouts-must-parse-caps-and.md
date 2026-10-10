---
id: L-0030
kind: rule
scope: ["src/commands/budget.rs", "src/commands/hud.rs", "src/tui/**", "src/budget.rs"]
lens: correctness
source: ["H-0014/F1"]
status: proposed
used: 0
---
# Budget readouts must parse caps and decide reached/over_cap with the same functions and data as enforcement, so dollar caps are not shown as null or unreached.

Any command that displays a budget (hall budget, hall hud, the TUI usage pages) should read the cap with budget::Cap::parse and compute reached or over_cap with Cap::reached, on the same weighted usage queries that check_with uses. A parser that only understands token strings returns None for a dollar cap like "$15", which hides a hold the enforcer already applied. To check a change, confirm that a held item under a $ cap shows a numeric limit and reached = true in every readout.
