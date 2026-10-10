---
id: L-0035
kind: rule
scope: ["src/library/portable.rs", "src/library/store.rs"]
lens: data_migrations
source: ["H-0012/F1"]
status: proposed
used: 0
---
# Importing a learning whose ID is already taken must renumber it, never skip it, because a skipped learning is lost for good.

A store that was empty when a new learning was distilled can hand out an ID (for example L-0001) that a committed learning already uses. Import must give the incoming learning a fresh number above the store's IDs and every incoming ID, and report the rename. Check that an ID collision does not fall through to a 'already in the Library' skip unless the content is genuinely the same learning. Test by importing a committed set into a store that already issued the same IDs.
