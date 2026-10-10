---
id: L-0026
kind: rule
scope: ["src/now.rs", "src/hooks.rs"]
lens: correctness
source: ["H-0008/F3"]
status: proposed
used: 0
---
# Each action offered for an entry in the now view or the inbox must be valid for that entry's state and kind, checked against the command's own state guard.

Parallel parts that are awaiting the human must offer hall merge, not hall ready, because parts merge into their parent and ready fails with WRONG_STATE. Stuck items must not offer hall watch. Before adding an action to next or to explain(), confirm the target command accepts that state, for example by reading the guard in merge.rs.
