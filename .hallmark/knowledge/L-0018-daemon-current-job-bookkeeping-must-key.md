---
id: L-0018
kind: rule
scope: ["src/daemon/mod.rs", "src/daemon/run.rs"]
lens: operability
source: ["H-0009/F3"]
status: proposed
used: 0
---
# Daemon current-job bookkeeping must key on repo, worker and slot together, not on repo and slot alone.

When keys are only repo plus slot, different workers in the same repo overwrite and then remove each other's entries, so hall daemon status and the HUD show nothing running or the wrong worker. Use keys like '<repo> <worker>' and '<repo> <worker> #<slot>'.
