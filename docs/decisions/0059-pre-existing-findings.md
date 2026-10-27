# 0059. A finding can be pre-existing: recorded, never blocking, not the author's to fix

Status: Accepted, 2026-10-27. Amends 0015.

## Context

The reviewer's scope rule (ADR 0036) is to report what the change
introduces, makes reachable or makes worse, and leave the rest alone. Code
the change touches often carries defects it didn't cause. The reviewer had
two choices, both wrong: report the defect as a finding, which sends the
item back and makes the author fix something that isn't theirs, or stay
silent and lose it. Anthropic measured the cost of the first choice: about
59% of what their reviewer kept flagging across rounds predated the
change, and each round surfaced a few more old issues rather than new
bugs. Their fix was a Pre-existing tag that keeps old bugs from looking
like the author's fault, while still posting them.

## Decision

- **The verdict schema gains `pre_existing`** (boolean, optional, default
  false) on a finding: the base already has the same defect
  (`git show <base>:<path>`) and the change doesn't make it worse. The
  reviewer still rates severity by consequences; the skill says the flag is
  not for softening a problem the change caused.
- **It never counts toward approval.** `Ledger::open_counts` leaves
  pre-existing findings out, so the rule from ADR 0015, 0 open blockers and
  0 open majors, ignores them. They are tracked like any finding: numbered,
  open until a review rules on them, listed in `hall findings`, the review
  report, the PR comment, the terminal app and the web app, each with a
  pre-existing mark.
- **The fixer may leave them.** Its prompt lists them last, says they never
  block, and asks it to fix one only when that is cheap and safe, else to
  leave it and say so. A dispute "because it is old" is never needed.
- **The journal stays readable.** The flag is written only when true, and
  a verdict without it parses as before. The index gains a `pre_existing`
  column (schema version 3, rebuilt on first use).

## Consequences

- An old defect the reviewer sees is on the record and in front of the
  team, under the PR, without costing the author a round.
- A pre-existing finding stays open across rounds and is listed under the
  open findings each time; the reviewer rules on it like any other. One
  that a later change does fix is ruled `fixed` then.
- `hall stats` counts should treat them as their own class, not as noise
  or as the author's defects; that lands with the stats command.
