# 0049. A rebase keeps the approval it earned

Status: Accepted, 2026-10-17. Amends 0030, 0048.

## Context

ADR 0030 carries a hallmark across one kind of rebase: the Dispatch Dock's,
right before shipping, when the patch is the same (`git patch-id --stable`)
and the gates pass at the new commit. Every other rewrite of an approved
commit paid for a full review again:

- the Delivery office's rebase of a shipped PR, clean or with conflicts
  resolved, went straight back to review (`send_to_review`);
- that review wasn't incremental, because the last reviewed commit is no
  longer an ancestor of the rebased head, so the reviewer read the whole
  change from its fork point again;
- effort followed the size of that whole diff: the configured default
  (`high` in this repo), or `max` when any changed file is sensitive, even
  when the conflict was three lines in a docs table.

With the merger on (ADR 0048), each PR in the queue is rebased at least once
before it merges, and on 2026-10-17 re-reviews of already approved changes
cost $10 to $16 per item. The reviewer had already approved each of those
changes. After a rebase, the only new question is whether the rebase and any
conflict resolution kept both sides' intent.

## Decision

When the commit under review rewrites the item's hallmarked commit (the
hallmarked commit isn't an ancestor of it, and its note belongs to this
item), the assayer compares the approved change with the change under
review. Each side is the diff from the commit's fork point on its base, with
its context lines and no renames, compared file by file with hunk line
numbers taken out, as `git patch-id` reads it. The blob ids of a binary file
stay, they are its content. So a base that moved the change's lines doesn't
make it a different change, but a hunk that landed elsewhere in its file, or
a base edit next to it, does.

Every comparison that isn't a carry is written on the journal as `rebased
{carried: false, reason}`: "rebase review: only the resolution (N lines …)"
or "full review: the resolution changed N lines …". `hall log` shows it. An
interdiff too large for the prompt gets a full review.

1. **The same change:** the approval is carried, and no review runs. The
   gates already passed at the new commit (checks come before review). The
   assayer records `rebased {carried: true}`, then an approving verdict whose
   reviewer is `carried`, and strikes the hallmark. The note names the
   approved commit (`rebased_from`). If a blocker or major is open in the
   ledger, a reviewer rules instead.
2. **A small difference** (at most `MEDIUM_LINES`, 100 changed lines): a
   **rebase review**. The prompt shows only a diff between the approved
   change and the rewritten one, a diff of two diffs, kept in the item's
   `runs/rebase/`. The reviewer is asked one question: is each difference
   right? A resolved conflict must keep what both sides meant, with nothing
   dropped, duplicated or half-merged, and any other difference must hold up
   on its own. Effort follows the size of that difference, and is `max` only
   when a file in it is sensitive. Findings, the fixer and approval work as
   in any round.
3. **A larger difference,** or nothing to compare with (never hallmarked, or
   the note is gone): a full review, as before. The prompt says how much of
   the approved change the rewrite changed.

The decision lives in the review step, so it holds for every rewrite: the
Delivery office's rebase, the Dispatch Dock's rebase whose patch changed only
in its context, and a branch you rebased or amended yourself and sent with
`hall ready`. A branch that only gained commits on top keeps the incremental
review it already had.

## Consequences

- A rebase that resolves to the same change costs the gates, not a review.
  This is the common case for the merge queue: conflicts in docs tables and
  lists that the Delivery office resolves by keeping both sides.
- A rebase that changed the approved change costs a short review of only that
  difference, at low or medium effort.
- The hallmark still never moves to code that neither a reviewer nor the
  gates have seen: the same approved change with passing gates, or an
  approved rebase review. CI still runs on the rebased head before the merger
  merges it.
- The comparison keeps context lines, as ADR 0030 did. A base edit next to
  the change, or a hunk that moved within its file, gets a rebase review of
  just that difference rather than a carry.
