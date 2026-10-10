---
id: L-0006
kind: rule
scope: ["docs/decisions/0017-daemon-process-and-pauses.md", "src/daemon/run.rs", "src/daemon/mod.rs"]
lens: repo_rules
source: ["H-0007/F4"]
status: proposed
used: 0
---
# When daemon scheduling changes, update ADR 0017 and the daemon module headers in the same change, including what the passes counter counts.

ADR 0017's Passes and Stop sections and the header comments in src/daemon/run.rs and src/daemon/mod.rs describe the scheduling model. Under per-repo schedules, the registry refresh runs on its own timer, and state.passes counts finished per-repo passes rather than global ones. A scheduling change that leaves these describing lockstep passes should be flagged.
