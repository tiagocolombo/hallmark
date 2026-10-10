---
id: L-0049
kind: map
scope: ["src/commands/new.rs", "src/shipper.rs"]
lens: first_principles
source: ["H-0018"]
status: proposed
used: 0
---
# Adopting an open PR runs through adopt_pr and reopen in src/commands/new.rs, which call check_base and ensure_has_head before creating or reopening the item, and the shipper's update step then does a leased force-push and gh pr edit --base.

A closed item's pr_head is the hallmarked sha the shipper last pushed, so reopen compares it with the live PR head before deciding whether a fetch is needed. The shipper's update path, not ship, is what changes an existing PR, and it runs only after the review approves. Read these two paths together before changing how a PR is taken over or updated.
