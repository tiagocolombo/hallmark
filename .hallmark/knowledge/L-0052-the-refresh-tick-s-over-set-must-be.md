---
id: L-0052
kind: rule
scope: ["src/refresh.rs"]
lens: correctness
source: ["H-0023/F5", "H-0023/F6"]
status: proposed
used: 0
---
# The refresh tick's over-set must be keyed on PR head, not PR URL, and must record merged or closed PRs so they stop being polled.

Keying the over-set on the URL alone makes a PR once seen closed ineligible forever, even after it is reopened or receives a new push. Keying it on the pr_head lets any later ship or refresh push make the item eligible again. A PR seen as MERGED or CLOSED should be recorded and filtered out of the due list, so merged PRs are not polled with gh every tick for as long as their worktree exists. Check that both the insert and the filter use the same key.
