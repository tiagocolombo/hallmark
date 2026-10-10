---
id: L-0013
kind: rule
scope: ["src/shipper.rs"]
lens: correctness
source: ["H-0004/F6"]
status: proposed
used: 0
---
# Shipping a stacked follow-up must check its parent PR's state first: a MERGED parent switches `base` to the repo's configured base before `{base}` is filled, and a CLOSED parent holds the item with a next step.

Otherwise `gh pr create --base` targets a branch already merged or deleted, so the new PR's merge never reaches main, and nothing else can change the item's base after creation. Any change to the ship path for stacked items should keep these two branches; the parent-state tests were still missing when this item closed (H-0004/F9).
