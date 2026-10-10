---
id: L-0048
kind: rule
scope: ["docs/decisions/0027-adopt-open-prs.md", "plugin/skills/hallmark/SKILL.md"]
lens: repo_rules
source: ["H-0018/F9"]
status: proposed
used: 0
---
# ADR 0027 and SKILL.md must describe the base and push-gate rules the code enforces, so a change to check_base or the gate must update both in the same commit.

Round 2 left the ADR saying any local branch is an acceptable base and that update's commands follow ship's gate, both no longer true. A PR based on another branch is adopted by retargeting it to the configured base with --base. Compare the ADR's --pr and --base bullets with check_base whenever either changes.
