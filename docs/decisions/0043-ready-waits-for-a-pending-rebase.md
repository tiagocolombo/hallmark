# 0043. `hall ready` waits for a pending rebase

Status: Accepted, 2026-10-14. Amends 0030.

## Context

- **A rebase request lived only until the next move.** When the Dispatch
  Dock finds a conflict, the item goes back to development with
  `rebase_requested`, and the Delivery office rebases it (ADR 0030). Any
  transition clears the request, so it isn't carried into the next round.
- **`hall ready` was such a move.** In H-0054 the Delivery office was held
  for budget a few seconds after the request, and a `hall ready` sent the
  un-rebased commit, which conflicted with `main`, to review. The request
  was gone, a full review ran on a commit that could never ship, and after a
  pull-back nothing was left to rebase the item.

## Decision

While an item in development has a pending rebase request, `hall ready` and
`hall propose-ready` (and the terminal app's ready, which is `hall ready`)
look at the base first (`origin/<base>`, fetched, or the local branch). If it
isn't an ancestor of HEAD, they refuse with `WRONG_STATE` and say why. The
request stays, and `next` offers:

- `hall retry <id>` when the item is held, so the Delivery office rebases it,
  or `hall status <id>` when it isn't held and the office takes it on its
  next pass;
- `git -C <worktree> rebase origin/<base>`, then `hall ready <id>`, to do it
  yourself.

A branch that is already on its base goes to review as before, which is how
a rebase you did yourself is sent (ADR 0030).

## Consequences

- A review never runs on a branch Hallmark already knows conflicts with its
  base, and a rebase request isn't lost to a ready.
- `hall ready` fetches the base when a rebase is pending: one small network
  call, only then.
