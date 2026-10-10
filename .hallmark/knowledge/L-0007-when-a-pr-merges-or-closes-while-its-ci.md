---
id: L-0007
kind: rule
scope: ["src/ci.rs"]
lens: correctness
source: ["H-0004/F8"]
status: proposed
used: 0
---
# When a PR merges or closes while its CI checks are still red, the CI watch must record passed:false with an ended marker, never a pass.

The watch in src/ci.rs checks `pr_state` on each tick, and a MERGED or CLOSED PR must not fall through to a 'passed' record. Recording a pass there permanently contradicts the earlier failed entry in the append-only journal and clears the ci_failed inbox item. A test should cover a red PR that is then merged, asserting the journal ends with passed:false and the inbox entry is kept.
