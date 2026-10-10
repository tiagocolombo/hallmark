---
id: L-0040
kind: rule
scope: ["src/library/knowledge.rs", "src/library/librarian.rs", "src/index.rs", "src/tui/app.rs"]
lens: correctness
source: ["H-0022"]
status: proposed
used: 0
---
# Library knowledge items must bypass the human inbox and go straight to review; only development blockers may create inbox entries.

Knowledge items produced by the Library are routed to `hall ready` for review and must not be written as inbox entries. Only blockers on development (things that stop a work item) belong in the inbox. When changing how entries are created or filtered, check that a knowledge item never surfaces in the inbox view or its counts. This was the requested behaviour and was approved in review.
