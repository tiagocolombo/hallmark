---
id: L-0022
kind: rule
scope: ["README.md", "docs/DESIGN.md", "docs/decisions/**"]
lens: repo_rules
source: ["H-0009/F7"]
status: proposed
used: 0
---
# When an ADR supersedes an earlier one, update the superseded ADRs' status, the Supersedes line, the README config defaults, and the DESIGN.md text that described the old behaviour in the same change.

A change that removes a serial rule (such as one item at a time per repo) leaves stale text if only the new ADR is written. Check README's default config block, DESIGN.md's description of leases and worker order, and the module docs for the old behaviour. Also check that the new ADR names every ADR it supersedes, including ones whose rule is only partly replaced.
