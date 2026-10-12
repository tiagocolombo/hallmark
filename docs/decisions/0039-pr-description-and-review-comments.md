# 0039. The PR description is What, Why and How; the review goes in comments

Status: Accepted, 2026-10-12. Amends 0019 and 0029.

## Context

The shipper wrote one PR body (ADR 0019): the author's summary, then the
approving review's summary, rounds and rejections, the settled findings,
the open minors and nits, the checks and the usage. A teammate opening the
PR had to read past the review to learn what the change is and why it
exists. An update of a shipped PR (ADR 0029) posted that same body again as
a comment, so later reviews repeated the first one instead of saying what
changed and why the PR came back.

## Decision

- **The description is What, Why and How.** `items/<id>/pr-body.md` has
  `## What`, `## Why` and `## How`, then `Closes #<n>` for an issue item and
  one line saying the review is in the comments. Nothing of the review.
  - What: the author's `hall propose-ready --summary`, else the title.
  - Why: `--why`, else the request typed with `hall new`, else the issue.
  - How: `--how`, else the subjects of the commits the PR ships.
  The `hallmark` skill and the session context ask the author for `--why`
  and `--how`: they know the reason and the approach best.
- **Every review is a PR comment.** The shipper writes
  `items/<id>/review.md`: the verdict's summary, the round and the reviewer,
  a rebase that carried the approval, every finding with how it was settled
  (fixed in which commit, withdrawn, disputed and why, or still open with the
  suggested fix), the checks, the usage and where the full record is.
  After `ship` or `update` succeeds and the PR's URL is known, the new
  `review_comment` command posts it. Default:
  `gh pr comment {pr} --body-file {review_file}`; empty turns it off.
- **Updates comment, they don't rewrite.** An update's comment is titled
  "Hallmark review of this update". It says why the PR came back (the PR
  comments it addressed, with their authors; the failing CI checks; the
  conflict it was rebased for; or a reopen), and lists the findings raised
  or settled since the last ship, plus any still open. The description stays
  as it was: on an adopted PR it is a teammate's. The default `update` no
  longer comments; `review_comment` does.
- **Hallmark's comments are never feedback.** The comment starts with
  `<!-- hallmark:review -->`, and the Delivery office skips comments that
  carry it, as it skipped the record line before.
- **A comment is not a ship.** If `review_comment` fails, the item is still
  shipped: the step and the live log say so and name `review.md`. A command
  that starts with `gh` runs only when Hallmark can find the `gh` it is
  configured with (`HALL_GH`, `[tools] gh`), never another one on PATH.
- **Old configs.** `hall init` wrote the old `update` (which posted the body
  as a comment) into `.hallmark.toml`. That exact string is read as today's
  default, so those repos don't post the description as a comment. A
  changed `update` is kept as written.

## Consequences

- A PR reads as a change first; the review sits under it, one comment per
  review, in order.
- A repo whose own `ship` or `update` posted `{body_file}` as a comment now
  posts the description there. It should drop that step: `review_comment`
  posts the review.
- `review_comment` runs only when the PR's URL is known, so a `ship` that
  prints no URL gets no comment; the review stays in `review.md`.
- Hosts other than GitHub set `review_comment` to their own command (`glab
  mr note …`), or empty.
