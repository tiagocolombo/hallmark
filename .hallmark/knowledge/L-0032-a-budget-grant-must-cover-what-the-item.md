---
id: L-0032
kind: rule
scope: ["src/commands/flow.rs", "src/budget.rs"]
lens: correctness
source: ["H-0015/F1"]
status: proposed
used: 0
---
# A budget grant must cover what the item has already overspent (spent minus allowance), not only add to the cap, or a released item is held again on its next fixer run.

Retry or release paths that grant tokens to a budget-held item must record overshoot plus the grant, where overshoot is item_tokens minus item_allowance, saturating at zero. Adding only the grant to the cap leaves the item below its spend, and one fixer run can re-hold it straight away. The check: after a grant, the item's spent tokens must be below its new cap by the granted amount. Read the synced index before the transaction so the overshoot is current.
