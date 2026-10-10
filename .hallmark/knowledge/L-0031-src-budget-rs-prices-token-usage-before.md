---
id: L-0031
kind: map
scope: ["src/budget.rs"]
source: ["H-0014"]
status: proposed
used: 0
---
# src/budget.rs prices token usage before comparing it to a cap: cache reads count at 0.1 and cache writes at 1.25 of input, and a cap is either a token count or a dollar amount written as "$N".

Token caps (per_item, room and daily) are applied to weighted tokens, not raw totals, because a cache read costs a tenth of input and a cache write costs 1.25 times input. Cap::parse accepts either a plain token number or a "$" dollar figure, and Cap::reached decides whether spend has hit the limit. Enforcement in check_with and the readouts both use these, so a change to the weights or the cap grammar has to be checked in the commands and TUI as well.
