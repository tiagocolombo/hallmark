---
id: L-0036
kind: rule
scope: ["src/library/librarian.rs", "src/library/retire*.rs", "src/commands/library.rs", "src/library/store.rs"]
lens: operability
source: ["H-0012/F2"]
status: proposed
used: 0
---
# Automatic retirements must leave a dated record that hall library review lists, so the human can see and undo them.

The librarian retires learnings on its own (contradicted, or unused past retire_after), and a briefing can lose a learning between rounds with no other trace. Each retirement needs a dated 'Retired YYYY-MM-DD' line in the learning file, and the review command must surface recent retirements with a way to accept them back. A change that retires without writing that line, or that drops the review listing, reopens this gap.
