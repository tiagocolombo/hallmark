---
id: L-0008
kind: rule
scope: ["src/ci.rs"]
lens: operability
source: ["H-0004/F3"]
status: proposed
used: 0
---
# A failed PR in the CI watch must stay polled for a bounded window (24h after the failure), so a re-run that turns green records a pass and clears the ci_failed entry.

Do not drop an item from `watched()` as soon as a failure is recorded, because that leaves the inbox stuck on 'CI failed' after the user fixes the job. The window must still end, so a permanently red PR does not poll forever. Check that a failed-then-green sequence clears the entry, as the `a_failed_pr_that_turns_green_clears_the_inbox` test does.
