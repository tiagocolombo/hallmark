---
id: L-0053
kind: rule
scope: ["src/ci.rs"]
lens: operability
source: ["H-0023/F1", "H-0023/F7"]
status: proposed
used: 0
---
# The CI watch window must start at the last push (set by both shipped and refreshed), and the give-up note must be keyed by the PR head.

If a refresh does not reset the window start, a PR refreshed more than 24h after it shipped is never watched again, and the refresh can erase a red CI result from the inbox. The window should use the pushed-at timestamp, which both the shipped and refreshed events set. The give-up note key should include the PR head, so a refresh push that moves the head can produce a fresh give-up note instead of being silenced by the old one.
