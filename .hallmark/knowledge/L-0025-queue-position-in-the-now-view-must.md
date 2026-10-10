---
id: L-0025
kind: rule
scope: ["src/now.rs"]
lens: correctness
source: ["H-0008/F2"]
status: proposed
used: 0
---
# Queue position in the now view must apply the same filters as lease::candidates, including skipping held items, or it reports the wrong item as next.

A held item keeps its ticket in the queue, but the assayer never picks it. Counting it as ahead makes hall status and hall watch say a ready item is behind an item that will not run. When the queue order changes in lease, mirror the change in now.rs, or better, share one filter function between them.
