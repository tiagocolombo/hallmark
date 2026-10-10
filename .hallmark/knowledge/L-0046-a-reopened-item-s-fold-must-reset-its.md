---
id: L-0046
kind: rule
scope: ["src/event.rs"]
lens: correctness
source: ["H-0018/F5"]
status: proposed
used: 0
---
# A reopened item's fold must reset its reject count to zero, so a rebase review gets the full max_rejects attempts before escalating.

Otherwise the first rejected review of the rebase has cur.rejects + 1 at or above max_rejects and escalates to the human at once, skipping the fixer. Check that the reopened event resets rejects in fold and that record_verdict's escalation test uses the reset count. No test covered this in H-0018, so a reviewer should look for one.
