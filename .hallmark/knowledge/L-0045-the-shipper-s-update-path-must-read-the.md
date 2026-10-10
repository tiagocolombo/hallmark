---
id: L-0045
kind: rule
scope: ["src/shipper.rs"]
lens: correctness
source: ["H-0018/F6"]
status: proposed
used: 0
---
# The shipper's update path must read the PR state and refuse to push when the PR is not OPEN, before running the leased force-push.

A merged or closed PR whose branch still exists would otherwise receive the rebased commits, and the item would then close as shipped into a PR that no longer merges. The message should point to `hall new --follow-up <url>`. Flag any update path that pushes before checking gh pr state.
