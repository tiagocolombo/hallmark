# 0072. The reviewer's request for a person can be a note

Status: Accepted, 2026-11-26. Supersedes the part of 0063 that makes the
reviewer's `needs_person` a hold in every repo.

## Context

ADR 0063 lets the reviewer ask for a person on any change (`needs_person`
in its verdict). With the merger on, that request always counts as one
approval needed, and no setting changes it. In a repo that runs the merger
to keep people out of routine merges, the reviewer asks often: on changes to
what a command costs, or to how a worker behaves. Each request held a PR
that was otherwise green and hallmarked until someone ran `hall retry`.
Hallmark's own repo had three in a row (H-0106, H-0112, H-0109), and each
one was released without changes.

The paths a repo marks as risky (`lit`, `one_approval`, `two_approvals`) are
the repo owner's decision, made ahead of time. The reviewer's request is a
judgement made per change. Some owners want it to stop the merge; others
want it written down where they can read it later, without the merge
waiting.

## Decision

A new setting, `[merge] needs_person`, says what the reviewer's request does
under `mode = "auto"`:

- `hold` (the default, ADR 0063 as it was): the PR waits for one person's
  approval, an approving review on GitHub or your `hall retry`.
- `note`: the PR merges like any other. The request still goes in the PR
  description's `## Risk` ("Noted, not held"), in `hall status` and on the
  item, and in the merger's reason for merging it in `hall queue`.

`lit`, `one_approval` and `two_approvals` paths hold as before under both
values: they are the owner's own rules. A PR already waiting only on the
reviewer's request is released on the merger's next pass once the setting
is `note`, as any tier wait is when its requirement drops.

Hallmark's own repo sets `note`.

## Consequences

- With `note`, a change the reviewer thought needed a person can merge
  unseen. The request stays on the PR and in the journal, so it can be
  read afterwards, and a revert is a signal (ADR 0061).
- The reviewer is still told to set `needs_person` when it applies. Its
  prompt doesn't change, so the setting can go back to `hold` and the
  requests are already there.
