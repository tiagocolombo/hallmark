---
id: L-0009
kind: rule
scope: ["src/ci.rs"]
lens: operability
source: ["H-0004/F5"]
status: proposed
used: 0
---
# `gh pr checks` exits 1 with empty stdout and a 'no checks reported' message for PRs without checks, so the CI watch must treat that as Waiting, not a parse error.

Repos without CI, or stacked PRs whose workflows only run on main, otherwise produce a live-log error every tick for the full 24h window, enough to rotate the live log. Map the empty-stdout case to an empty check list, and log watch errors once per item. Also write a single live line when the watch gives up.
