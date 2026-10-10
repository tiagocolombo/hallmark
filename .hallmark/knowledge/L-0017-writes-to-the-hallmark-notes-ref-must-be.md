---
id: L-0017
kind: rule
scope: ["src/stamp.rs"]
lens: correctness
source: ["H-0009/F2"]
status: proposed
used: 0
---
# Writes to the hallmark notes ref must be serialized with the cross-process repo lock, because git notes add does not check the old ref value.

Two approvals landing close together both read the same notes ref and write a commit on top of it, so the second write silently drops the first note. Wrap git notes add in hall.with_repo_lock. Do not call strike while that lock is held, because it runs after transact in record_verdict and would deadlock.
