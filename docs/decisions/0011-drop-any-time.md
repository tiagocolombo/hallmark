# 0011. An item can be dropped at any time

Status: Accepted, 2026-10-09

## Context

The design page allowed dropping only from Development or Escalated. The
project brief, which is authoritative, says an item can be dropped at any time,
with a reason.

## Decision

- **When.** `hall drop --reason <text>` is valid from every non-terminal state:
  development, awaiting you, checks, review, fixing, escalated and hallmarked.
  The reason is required.
- **What happens.**
  - The drop removes the item's queue ticket.
  - It removes the worktree with a non-forced `git worktree remove`, unless
    `--keep-worktree` is given.
  - The branch and the journal always stay.
- **Exceptions.**
  - If the removal fails (uncommitted changes, or a locked directory on
    Windows), the item is still dropped, and the reply reports
    `worktree_removed: false` with the reason.
  - The repo's main worktree is never removed.
- **Repeating.** Dropping an already-dropped item returns the current result.

## Consequences

- Nobody loses uncommitted work by dropping an item.
- A dropped item is terminal. To resume the work, run `hall adopt` on its
  branch, which creates a new item.
