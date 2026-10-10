---
id: L-0004
kind: rule
scope: ["src/daemon/run.rs"]
lens: correctness
source: ["H-0007/F2"]
status: proposed
used: 0
---
# In the daemon's collect loop, check for finished repo threads before draining the outcome channel, so an outcome sent just before a thread exits is not lost on stop.

A repo thread sends its Outcome before it finishes. If the loop removes finished threads from the running set before draining the channel, and running becomes empty, the loop can break with an Outcome still queued, dropping its summary and any pause. Reviews of stop or shutdown logic in this file should confirm the drain happens after the is_finished scan.
