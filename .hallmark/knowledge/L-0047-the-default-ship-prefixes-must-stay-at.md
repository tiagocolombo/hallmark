---
id: L-0047
kind: rule
scope: ["src/config.rs"]
lens: repo_rules
source: ["H-0018/F3"]
status: proposed
used: 0
---
# The default ship_prefixes must stay at git push and gh pr create; gh pr comment and gh pr edit push no commits and must not be push-gated.

Gating them denies an agent in an item worktree from commenting on or labelling its own PR with GATE_DENIED. The unit test that pins the default should be kept in line with this. Flag any addition of non-pushing gh subcommands to the default gate list.
