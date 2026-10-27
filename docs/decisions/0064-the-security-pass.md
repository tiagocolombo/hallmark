# 0064. The security pass: a second, security-only review on sensitive paths

Status: Accepted, 2026-10-27. Amends 0018.

## Context

The principal engineer applies seven lenses in one run; security and
tenancy is one of them, and on a large change it competes with six others
for the reviewer's attention. Anthropic runs a separate automated security
review on every PR, whose open findings block the merge like the bug
review's. Hallmark has the pieces: a reviewer whose lenses can be
narrowed, review notes that name sensitive paths, and a ledger that takes
any number of findings per round.

## Decision

- **`security_review`** in `.hallmark.toml` lists globs. When a change
  touches one (`git diff --name-only` from the merge base, no renames),
  the review run is followed by a **security pass**: the same agent,
  method, schema and read-only tools over the same change, with the
  prompt's heading "Security pass", a note saying why it runs and what to
  look for, only the `security_tenancy` lens, and `max` effort.
  `prompt::prepare_with` builds it; the setting is read from the base
  like `lenses`.
- **Its new findings join the verdict** before it is recorded: one the
  reviewer already raised at the same file and line, at least as severe, is
  not added twice (the pass is told the reviewer's findings; a more severe
  one is added), and its summary is appended as "Security pass: …". Its rulings
  on earlier findings and its learnings are dropped: those are the main
  review's. The ledger then numbers and judges one verdict for the round,
  so the fixer, the report, the PR comment and `hall stats` see the
  security findings like any other.
- **It is on the record.** A `security_pass` event carries the paths, the
  model, the titles it added, or why it didn't count. It runs as role
  `security` under the review budget.
- **When it fails, the verdict stands** as the reviewer gave it, and the
  event and the live log say why. A budget stop or the plan limit is not a
  failure: no verdict is recorded and the round is reviewed again after
  the grant or the reset.

## Consequences

- A repository names its sensitive paths once and every change to them
  gets a focused security review on top of the ordinary one, at the cost
  of one more review run for those changes only.
- Findings from the pass block approval by their severity, the same as
  the reviewer's; a wrong one is disputed and withdrawn the same way, and
  counts in `hall stats` noise the same way.
- The pass runs on every round of such a change, incremental from round 2
  like the main review, so it sees what the fixer changed.
