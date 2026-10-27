# 0061. A revert is a signal

Status: Accepted, 2026-10-27. Amends 0021, 0031 and 0056.

## Context

Hallmark forgot a change once its PR merged: the watch recorded
`delivered`, the sweeper removed the worktree, and nothing looked again.
The one fact that says the loop was wrong, a merged change that a person
had to undo, was never recorded. Anthropic builds its reviewer's eval set
from exactly those changes, asks in every incident review which layer
should have caught it, and wants circuit breakers that tighten approval
when the revert rate rises. None of that is possible without the signal.

Git already carries it. `git revert` and GitHub's Revert button both write
"This reverts commit <sha>" into the reverting commit, and a revert PR's
subject starts with `Revert "`. The commit a PR landed as is findable from
its number (`(#N)` on a squash, `Merge pull request #N` on a merge) or its
head (a fast-forward only). GitHub's "Rebase and merge" writes new SHAs and
no `(#N)`, so a PR merged that way is not matched and its revert is not
recorded: a known limitation.

## Decision

- **A `reverts` worker**, a script after the sweeper. For each base that
  has a merged, unreverted item merged within the last 30 days, at most
  every ten minutes, it fetches `origin/<base>` (the Delivery office's
  `base_ref`), reads the last 400 commits, finds the commit each PR landed
  as (the oldest match, so a revert PR naming the number is never taken for
  the landing), and looks for a newer commit that reverts it: body
  "This reverts commit <sha>", or subject `Revert "<subject>"`.
- **It is recorded once**: `reverted {pr_url, base, sha, revert_sha,
  subject}` on the item, under the journal's lock. The item folds it into
  `reverted`; `hall status` shows it.
- **It reaches you.** An inbox entry of kind `reverted` (an escalation, so
  it is notified) says what undid the PR and asks which layer should have
  caught it, with `hall new --follow-up <pr>` and the item's record as its
  actions. It stays until dismissed or a follow-up is made.
- **The librarian distils the item again.** A `reverted` event counts as
  a move for `already_distilled`, so the next librarian pass takes the
  item. Its digest gains a Reverted section that asks the incident-review
  question, which layer should have caught it (the tests, the gates, the
  review and its lens, the person who merged), and asks for a `rule`
  learning scoped to the changed paths, and the retirement of any
  `false-positive` that a withdrawn finding near the cause produced.
- **`hall stats` counts reverted items** next to escalations: the loop's
  misses.

## Consequences

- The reviewer's misses are on the record, with the commit that fixed
  them. The eval set (replaying the reviewer on reverted items) can be
  built on it, and so can a circuit breaker.
- One fetch and one `git log` per base every ten minutes, only while a
  merge is less than 30 days old. A repository with no remote reads its
  local base.
- A revert done any other way (a manual fix-forward, a new commit that
  undoes the change by hand) isn't seen; neither is a revert of a revert,
  which leaves the item reverted. Both are rare and both are visible in
  the record a person reads from the entry.
- A revert is recorded on the item once; the entry is dismissable, so a
  revert that was itself a mistake costs one dismissal.
