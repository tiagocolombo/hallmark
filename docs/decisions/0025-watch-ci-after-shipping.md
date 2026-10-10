# 0025. Watch CI after shipping; follow-ups carry it and stack on open PRs

Status: Accepted, 2026-10-10

## Context

H-0001, the first item Hallmark reviewed for itself, passed the local gates
and an approving review, shipped as PR #1, and then failed CI on Linux: a
test ran a binary it had just copied and hit `Text file busy`, a race that
macOS doesn't have. The gates run on one machine; CI runs the matrix. Once
the PR opened the item was closed, so nothing in Hallmark noticed, and a
follow-up item started from `main` and read only review comments.

## Decision

- **The `ci` worker** (after the shipper) follows each shipped PR's checks
  with `gh pr checks --json name,bucket,link`, at most once a minute per PR,
  for up to 24 hours. A failed or cancelled check concludes it at once;
  otherwise it concludes when every check has finished. It records
  `pr_checks {pr_url, passed, failing}` on the item (`Item.ci`).
- **The inbox** shows `ci_failed` for a closed item whose PR failed, until a
  follow-up item for that PR exists (one not dropped). Its action is
  `hall new --follow-up <pr>`; notifications route it like an escalation.
- **Follow-ups carry CI:** `hall new --follow-up <pr>` adds the failing checks
  and an excerpt of each one's log (`gh run view --job --log-failed`, focused
  on panics, errors and failures) to the request, beside the review feedback.
- **Follow-ups stack on open PRs:** when the PR is still open, the follow-up
  starts from the PR's branch and its `base` is that branch, so it is reviewed
  against it and its PR targets it. A merged or closed PR's follow-up starts
  from the configured base as before.

## Consequences

- A red CI on a shipped PR reaches you like any other inbox entry, with the
  command that starts fixing it and the evidence in the request.
- A stacked follow-up is reviewed against the open PR's branch, whose rules
  were themselves reviewed; once both merge, `main` has both.
- Requests keep their line breaks (they were flattened before), so lists and
  log excerpts stay readable.
