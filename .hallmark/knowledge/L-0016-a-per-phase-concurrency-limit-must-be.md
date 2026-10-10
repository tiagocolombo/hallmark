---
id: L-0016
kind: rule
scope: ["src/lease.rs", "src/daemon/**"]
lens: correctness
source: ["H-0009/F1", "H-0009/F8"]
status: proposed
used: 0
---
# A per-phase concurrency limit must be counted and enforced under the same repo lock as the claim, never in a snapshot taken before it.

Concurrent slots that each build a candidate list before any of them claims will all see the phase as below its limit and overshoot it. The count of live same-role leases and the claim must happen together inside with_repo_lock (claim_next and claim_locked). Tests must cover the concurrent-slot path: a sequential test passes even if the atomic check is deleted, because candidates already filters full phases.
