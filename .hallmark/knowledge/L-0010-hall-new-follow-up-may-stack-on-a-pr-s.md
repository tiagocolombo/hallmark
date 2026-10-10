---
id: L-0010
kind: rule
scope: ["src/commands/new.rs"]
lens: security_tenancy
source: ["H-0004/F1", "H-0004/F2"]
status: proposed
used: 0
---
# `hall new --follow-up` may stack on a PR's branch only when the PR is open, same-repo, shipped by a Hallmark item, and both the PR head and the local branch tip equal the hallmarked commit; otherwise it must start from main.

Using the PR's headRefName as a local branch fails at review (`rev-parse` in the main checkout) when the branch is not local, and stacking on an unreviewed branch lets the branch's own .hallmark.toml, lenses, max_rejects and review rules govern the review. Settings and rules must come from a commit Hallmark reviewed under main's rules. Check the gate conditions on any change to how the base is chosen.
