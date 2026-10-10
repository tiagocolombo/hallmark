---
id: L-0003
kind: rule
scope: ["src/daemon/run.rs"]
lens: correctness
source: ["H-0007/F1"]
status: proposed
used: 0
---
# A rate-limit pause in the daemon must be recorded in the journal of every repo it affects, not only the repo that requested it.

Model workers are shared across repos, so a pause from one repo's assayer also stops the others' model work. Check that record_pause (or its successor) receives the Hall of every current target that has finished a pass, with duplicate repo event paths removed, and that state.paused.journals lists them all. A review should flag any pause path that writes to only the single requesting repo's journal.
