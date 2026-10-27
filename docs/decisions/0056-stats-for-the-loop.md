# 0056. Stats for the loop: reviews to approval, finding outcomes, noise

Status: Accepted, 2026-10-27. Amended by 0065: `hall eval` replays the reviewer on reverted items.

## Context

Every change to the reviewer (its skill, its lenses, its model, what the
prompt carries) shipped on judgement. The journal records what would tell
whether it helped: how many reviews each item took to pass, what became of
each finding (fixed, withdrawn, still open), which findings the fixer
disputed and how the reviewer ruled. Nothing read those numbers back.

Anthropic's own account of agent-authored code review
([Solving code review at Anthropic](https://claude.dev/blog/solving-code-review-at-anthropic/))
tracks exactly these: reviews to pass (mean, the share passing after one
round, the share needing five or more), and the author's response to each
comment as the noise measure. With an agent author, noise costs more than a
dismissed nit: every finding becomes a commit, a gate run and another
review.

## Decision

- **`hall stats`** folds the journals of the items that ended (closed,
  dropped, parts merged) and reports, as JSON off a terminal and as a few
  lines on one:
  - items counted, by outcome, and how many escalated;
  - reviews: items reviewed and approved, reviews, fixes, failed checks;
  - reviews to approval over the approved items: mean, median, the share
    approved by the first review, the share that needed three or more;
  - findings raised by severity, and how many were fixed, withdrawn or
    left open;
  - **noise**: withdrawn findings as a share of those raised, overall and
    by lens. A withdrawn finding was wrong, or a dispute was right;
  - disputes, and how many the reviewer upheld (withdrew the finding) or
    rejected (kept it open, or it was fixed anyway).
- `--since <duration>` keeps items that ended in the window; `--all` counts
  live items too, as they stand now.
- It is a reader: it writes nothing and uses the index only to list items.
  The numbers come from each item's journal, through the same ledger the
  reviews use (`findings::Ledger`), so they can't disagree with
  `hall findings`.
- The web app's Records page shows the same numbers as tiles above the
  history table, for the same window.

## Consequences

- A change to the reviewer can be judged: run `hall stats --since 30d`
  before and after. The share approved by the first review should rise and
  the noise fall.
- Reviews to approval counts reviews, not rounds: a round whose gates fail
  never reaches the reviewer and is counted under failed checks.
- Reverts are the next number to add (an item whose merged PR was reverted
  is the reviewer's miss); the command is where they will show.
