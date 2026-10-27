# 0063. Risk tiers: the change decides how many people approve the merge

Status: Accepted, 2026-10-27. Amends 0048 and 0057.

## Context

With the merger on, a hallmark was the whole approval: every PR not on a
`[merge] lit` path merged with no person in the loop, and a lit path meant
"wait for me", nothing in between. Anthropic's rule for agent-authored
code is graded: low-risk changes merge on green checks, ordinary ones need
one person, the riskiest two; the change sets the tier, not the author; a
classifier can require a person on any change regardless of paths; and an
agent's approval never fills a human slot. Hallmark's hallmark is an
agent's approval.

## Decision

- **Two more path lists in `[merge]`**: `one_approval` and
  `two_approvals`, globs like `lit`. A PR that touches one needs that many
  people's approvals before the merger merges it. `lit` stays what it was:
  a person merges, whoever approved.
- **An approval is a person's.** The merger counts the logins whose latest
  review on the PR is `APPROVED` at the PR's current head, bots excluded,
  and only people with standing on the repo (author association OWNER,
  MEMBER or COLLABORATOR, as for comments the Delivery office acts on):
  GitHub lets anyone review a public repo (`merger::approvals`). It counts your `hall retry` on the hold (`merge_approved` at that head) as
  one. The hallmark never counts. GitHub refuses an author's approval of
  their own PR, so on a one-person repository `hall retry` is the
  approval, as for `lit`.
- **The reviewer can ask for a person.** The verdict schema gains
  `needs_person`, a one-sentence reason, for a change the paths don't name
  but a person should approve (billing, auth, a release workflow, data that
  can't be got back, a pattern others will copy). It is not a finding and
  never blocks the review; the item folds the latest review's word, and it
  counts as one approval needed. A carried approval after a rebase keeps
  it, and later rounds' prompts show the standing request, so the reviewer
  repeats it or drops it on purpose. This is the second classifier: the first
  is the paths.
- **It waits like a lit PR.** Short of approvals, the PR gets the `lit`
  status and hold with a reason that says how many it needs, how many it
  has and from whom, which paths set the tier, and the reviewer's reason.
  It holds up nothing behind it, and a person is asked only once nothing
  ahead claims its files (ADR 0048). The merger judges the wait again on
  every pass and lifts it itself (a `released` event by the merger, which
  is not your approval) when the approvals are in or the requirement drops.
  Judging, and so `hall queue`, writes nothing.
- **The PR says its tier.** `## Risk` in the description (after How to
  revert, ADR 0057): a person merges it (lit), needs N approvals and the
  paths, the reviewer's reason, or "Low: … merges on its hallmark once CI
  is green." With `merge.mode = "off"` it says only that a person merges
  it, since nothing gates the merge. The description isn't touched on an update, so the review
  comment posted on every ship and update ends with the current `**Risk**`
  line.

## Consequences

- A team can let ordinary changes merge dark and keep billing, auth, CI
  and installers behind one or two people, by path, without turning the
  merger off; a reviewer's judgement reaches the same gate for changes the
  paths miss.
- An approval is per head: the merger counts only reviews left on the
  PR's current head (GitHub keeps older approvals unless branch protection
  dismisses them), and `merge_approved` is bound to the head.
- A reviewer that over-uses `needs_person` makes every PR wait; the skill
  says when to use it, and the PR comment shows the reason, so a person
  can see it and tune the skill or the paths.
