---
id: L-0033
kind: rule
scope: ["src/commands/flow.rs", "src/budget.rs", "src/commands/budget.rs"]
lens: correctness
source: ["H-0015/F5"]
status: proposed
used: 0
---
# Token amounts that flow into SQLite json_extract and sum() must stay within i64, because an overflowing integer reads back as REAL and sum() returns 0 for the whole item.

Values such as --tokens, grant amounts and the sums of several grants must be bounded to i64::MAX, and a single grant should be capped at half of i64::MAX so that adding an overshoot cannot overflow. Check parse paths: inputs like 'inf' or '1e19' saturate or exceed i64 and silently break the budget sum. A test should feed these inputs and expect a usage error.
