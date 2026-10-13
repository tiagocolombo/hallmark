# 0040. The sweeper removes the worktrees of finished items

Status: Accepted, 2026-10-13

## Context

Every item gets its own worktree (`git worktree add ../{repo}-{id}`), and
nothing removed it once the item was over, except `hall drop` and
`hall merge`. On 2026-10-13 this repo had 53 item worktrees, 115 GB in all,
most of them for closed items; one with its build output takes up to 4.2 GB.
Some were the worktrees of the `Library: learnings from …` items that ADR 0021
made. ADR 0027 already moved the Library out of the repository, to
`<git-common-dir>/hallmark/library/`, but those old worktrees stayed.

A closed item can't simply lose its worktree when it closes. It closes when
its PR opens, and while that PR is open the Delivery office reopens it in
that worktree to rebase, address review comments or fix red CI (ADR 0030,
0031), and `hall adopt` reopens it there (ADR 0029).

## Decision

- **A new daemon worker, `sweeper`,** runs after `ci`. It removes the
  worktree of each item that is done with it: closed, not a part, and not
  watched by the Delivery office. That is either a PR that merged or closed
  (`delivered`), or a ship that opened no PR to watch.
- **It runs `git worktree remove` without `--force`.** A worktree with
  uncommitted or untracked changes is kept, and the live log says why once.
  It is tried again every half an hour (git scans the whole checkout before
  it refuses), and goes once you clean it up.
- **It removes only checkouts Hallmark made.** An item adopted in a checkout
  you made (`hall adopt` in the current checkout, or a reopening that points
  the item at one) records `own_worktree: false`, and the sweeper leaves that
  directory alone. A swept item is not swept again, and a reopened item is
  watched, and keeps its worktree, until its PR ends again.
- **It removes only the item's own checkout.** It never removes the main
  checkout or a worktree that now has another branch. It also never removes a
  worktree that a live item, or a shipped one whose PR is still open, still
  uses (an adopted branch).
- **One removal per tick,** recorded as `worktree_removed { path }` in the
  item's journal. The branch and the journal stay, so `hall adopt` or
  `git worktree add` can bring the worktree back.
- **Parts and dropped items are not the sweeper's.** `hall merge` and
  `hall drop` already remove their worktrees and honour `--keep-worktree`,
  which their events don't record.
- **Nothing else lives in a worktree.** The journal, the index and the Library
  are in `<git-common-dir>/hallmark/`, shared by every worktree (ADR 0027), so
  removing a worktree loses nothing Hallmark knows.

## Consequences

- Worktrees no longer pile up. The ones left over from before go one per
  daemon pass.
- A shipped PR keeps its worktree until it merges or closes, so the Delivery
  office can still work in it.
- With no daemon running, nothing is swept. `hall daemon run --once --only
  sweeper` sweeps by hand, one worktree per run: repeat it.
- Removal uses `git worktree remove`, like `hall drop` and `hall merge`,
  whatever the `worktree` command that made it.
