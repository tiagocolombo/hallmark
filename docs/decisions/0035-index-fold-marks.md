# 0035. Each index row carries a fold mark; a row nobody vouches for is re-folded

Status: Accepted, 2026-10-12; amends 0032

## Context

PR #39 (H-0038) merged at 01:24:37 on 2026-10-12. The Delivery office
recorded `delivered: merged` nine seconds later. The journal and `item.json`
had it, but H-0038 stayed in the terminal app's Delivery lane, and the watch
kept reading the PR, for the next twelve hours.

Two terminal apps had been running since 02:21 and 02:39 on 2026-10-11. They
were built before the `delivered` event (ADR 0030) and before ADR 0032, and
they never restarted. One of them synced the index first:

- `parse_lines` drops a line it can't parse, so it folded H-0038 without
  `delivered` and wrote that row.
- It still advanced H-0038's offset to the end of the journal.
- It predates ADR 0032, so it left no `item_fields` mark. The daemon's next
  sync saw an index it owned and a journal with no new bytes, and never
  folded H-0038 again.

ADR 0032 only catches an older writer that runs its check and lacks an `Item`
field. It misses builds from before it ("they must be restarted once": these
weren't). It also misses a build that has every field but not a newer event
kind. Such a build skips that kind's lines and marks the index as its own. The
cause underneath both: a file offset was taken as proof that the row reflects
every byte before it.

## Decision

The `folds` table holds a mark for each item row: the journal offset it was
folded at, the event count it wrote (`Item::events`), and whether every line
of the journal parsed (`Hall::fold_item`). On every sync, besides the items
whose journal has new bytes, an item is folded again when:

- it has no mark, or the mark's offset isn't the indexed offset. A build from
  before fold marks moved the offset without saying so.
- the row's `events` isn't the mark's. A build from before fold marks
  rewrote the row from fewer lines, for example in a reindex.
- the mark says a line didn't parse. The new fold is written only if it read
  every line, so a build that can't do better leaves the row alone, and one
  that can replaces it.

The table is created by `CREATE TABLE IF NOT EXISTS`, without a schema version
bump: a bump makes every older build delete the index. On the first sync after
the upgrade, every item has no mark and is folded once. That heals rows
already wrong, such as H-0038's.

ADR 0032's ownership rules stand. A fold mark makes a stale row heal on the
next sync of a current build. ADR 0032 decides whose fields win.

## Consequences

- A forgotten hall of any age can make a row stale only until the next sync of
  a current build: the daemon's next pass, or the next CLI command.
- A build that can't read some lines of an item folds that item again on each
  sync without writing it. That is one journal read per such item per sync,
  and it ends when a newer build folds it, or the old process exits.
- A journal with a corrupt line counts as partial for every build, so it is
  folded on every sync. Corrupt lines never happen in normal use (ADR 0008).
