---
id: L-0024
kind: rule
scope: ["src/hooks.rs"]
lens: correctness
source: ["H-0008/F1"]
status: proposed
used: 0
---
# A hook's dedupe of already-announced inbox entries must key on the entry's since timestamp, and the told list must be pruned on every call, including calls that show nothing new.

If the key is only kind plus item id, an entry that is resolved and later re-escalated in the same session is never announced again. Pruning only on the path that shows new entries leaves stale keys behind when no new entry arrives. Check both: the key includes a per-occurrence timestamp, and the prune runs before the early return for an empty shown set.
