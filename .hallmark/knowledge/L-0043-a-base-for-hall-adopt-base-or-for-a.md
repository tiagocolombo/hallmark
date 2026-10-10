---
id: L-0043
kind: rule
scope: ["src/commands/new.rs"]
lens: security_tenancy
source: ["H-0018/F1"]
status: proposed
used: 0
---
# A base for `hall adopt --base`, or for a reopened or adopted PR, must be the configured base or the branch of an open hallmarked PR still at its hallmark commit.

Any other local branch can carry unreviewed commits, and a reopened item stacked on it would have its update force-push them into the PR under a hallmark. The check_base rule is the same one --follow-up uses, and it runs on --base and on the PR's own base. Flag any new path that accepts a base without going through check_base, and any path that accepts a base by name alone.
