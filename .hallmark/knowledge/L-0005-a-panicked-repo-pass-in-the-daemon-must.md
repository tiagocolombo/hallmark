---
id: L-0005
kind: rule
scope: ["src/daemon/run.rs"]
lens: operability
source: ["H-0007/F3"]
status: proposed
used: 0
---
# A panicked repo pass in the daemon must become an error outcome that is saved to state, so hall daemon status shows the repo as failing.

When join() on a repo thread returns Err, the loop should build an Outcome with a Skip::Error for the stored target and send it through the same summary path as normal passes. Logging the panic to daemon.log alone hides repeated crashes from status. Reviews should check any change to the panic path for this.
