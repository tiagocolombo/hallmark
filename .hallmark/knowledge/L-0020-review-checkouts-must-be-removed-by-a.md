---
id: L-0020
kind: rule
scope: ["src/assayer/review.rs"]
lens: operability
source: ["H-0009/F5"]
status: proposed
used: 0
---
# Review checkouts must be removed by a guard that runs on every exit after the checkout is created, not only after a recorded verdict.

A review can end without a verdict (held after timeouts, pulled back mid-review, dropped, or invalid output). If removal happens only on success, the worktree stays on disk and registered, and nothing else cleans it up. Use a Drop guard set up right after request() creates the checkout.
