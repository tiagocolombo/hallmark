# 0042. An update that pushed and then failed doesn't stick its item

Status: Accepted, 2026-10-13. Amends 0029.

## Context

- **The update command is several steps.** The default `update` pushes under
  a lease on `{pr_head}`, then runs `gh pr edit`. When the push goes through
  and `gh pr edit` fails (H-0054: GitHub's API dropped the connection), the
  command exits 1, the item is held (`ship_failed`), and `pr_head` stays at
  the commit the PR had before.
- **Every later try failed the same way.** The PR's head is now the commit
  Hallmark just pushed, so a lease on the recorded head is rejected as
  `stale info`. Releasing the hold, or reviewing more work, ended in the same
  hold, and nothing in the loop could move the item on.

## Decision

Before an update, the shipper asks `origin` where the branch is
(`git ls-remote`, one small request). The `{pr_head}` it fills in is:

- **the recorded head** when the PR is still there, when `origin` can't be
  asked, or when the branch is at a commit Hallmark didn't produce for this
  item: someone else's push still stops the update (`ship_failed`), as ADR
  0029 decided;
- **the remote head** when it differs from the recorded one but is a commit
  this item hallmarked. Hallmark pushed it, so replacing it loses nobody's
  work. The live log says so.

No event changes: the lease is worked out from the journal and the remote
each time, so an item held this way before the fix ships on its next pass
once released (`hall retry`).

## Consequences

- A network failure after a successful push costs one hold, which
  `hall retry` releases, not an item stuck for good.
- The guarantee of ADR 0029 is unchanged for anyone else's commits: only a
  head this item hallmarked may be replaced without being the recorded one.
- An update needs `origin` to read the remote head; without it the recorded
  head is used, as before.
