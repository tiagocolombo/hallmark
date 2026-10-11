# 0032. An older hall never rewrites a newer one's index; the terminal app restarts on update

Status: Accepted, 2026-10-11

## Context

On 2026-10-11, after `hall update`, three terminal apps were still running.
One had started a minute before the update and two the night before. Each
kept running its old binary:

- It showed the old behaviour. "Closed today" listed the oldest first, which
  ADR 0030 had already fixed.
- Every refresh synced the index. `Index::sync` re-folds each touched item
  from its journal and rewrites its row. An old build folds an `Item` without
  the fields it doesn't know, so the rows lost `rebase`. Twice, the Delivery
  office's rebase request for an open PR vanished from the index (H-0029,
  H-0030). `lease::candidates` reads the index, so the office went idle on a
  PR that still conflicted.

The journal was never wrong, and `item.json` kept the field, because both are
written from a full fold by the writer that knows the event. Only the index,
the shared cache, was rewritten by whichever process synced last.

## Decision

### The index records which hall owns it

The index's `meta` table holds `item_fields` and `item_fields_at`. They are
the field names in `Item`'s JSON Schema of the hall that last synced it, and
when. A newer build adds fields, so it is the one whose list the others don't
contain.

- `sync` skips everything when a build whose fields this one doesn't know
  synced in the last hour. It writes no rows, indexes no events and advances
  no offsets, so the newer build still re-folds the items those events touch.
  Reads work as before.
- `hall reindex` never empties an index a newer build owns.
- After an hour without the newer build (a downgrade), the older one takes
  over and marks the index as its own.
- The writer refreshes the mark at most every five minutes, or when its fields
  change.

The field list comes from the schema, so adding a field to `Item` needs no
version bump. Builds from before this one don't check the mark, so they must
be restarted once.

### The terminal app restarts itself on update

Every two seconds the app compares its executable on disk (size, modification
time, and inode on Unix) with what it was at start. When `hall update` replaces
it, the app restores the terminal and re-executes the new `hall` with the same
arguments. On Windows, where a running binary isn't replaced, it exits and says
to run `hall` again.

## Consequences

- A forgotten terminal app can no longer hide work from the workers, and it
  shows the current behaviour within two seconds of an update.
- While a newer build runs, an older CLI reads an index that may lag by one
  daemon pass.
- The daemon already restarts after its current pass on `hall update`
  (ADR 0023). Hooks run the installed binary each time, so they are always
  current.
