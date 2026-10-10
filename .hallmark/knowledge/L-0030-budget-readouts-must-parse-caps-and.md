---
id: L-0030
kind: rule
scope: ["src/commands/budget.rs", "src/commands/hud.rs", "src/tui/**", "src/budget.rs"]
lens: correctness
source: ["H-0014/F1", "H-0015/F4"]
status: proposed
used: 0
---
# Budget readouts must add per-item grants to the cap, using the same function enforcement uses, so an item granted more tokens is not shown as over its limit.

Any readout of an item's budget (hall budget, the HUD, the terminal app's usage bars and 'Most expensive items' pane) must compute the limit as per_item plus the item's granted tokens, via the same budget helper that enforcement calls (budget::item_granted or its successor). Bars or percentages computed from per_item alone show a granted item as 150% spent while the loop runs it again. To check a change, confirm each readout gets the granted value from the same source as the enforcement check.
