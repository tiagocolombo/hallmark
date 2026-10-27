# 0062. The verifier: a second run refutes blocking findings before the fixer sees them

Status: Accepted, 2026-10-27. Amends 0018.

## Context

A finding that isn't real costs more with an agent author than with a
person: the fixer commits, the gates run, the reviewer reads again. The
reviewer already has a refutation pass (ADR 0036), but it refutes its own
work in its own context. Anthropic's reviewer posts a finding only after
separate agents vote it real, and the share of PRs passing after one round
rose from 65% to 78% once noise was cut. The two findings Hallmark measures
as noise (`hall stats`: withdrawn after a dispute, or withdrawn by a later
review) both cost a round before they were found wrong.

## Decision

- **A verifier run**, `plugin/agents/verifier.md`, with the reviewer's
  read-only tools and `schemas/verification.json`. After the reviewer's
  verdict parses and before it is recorded, the assayer sends the
  verdict's blocking findings (blockers and majors, numbered) to the
  verifier in the same review checkout, with the commit, the merge base
  and the base to read. The verifier rules each one `real` or not, with a
  reason that names what it checked. It never adds findings, never changes
  a severity, and is told that doubt means real.
- **Refuted findings are dropped** before `record_verdict`: the verdict is
  judged on what survives. The `verified` event records the model, how
  many were checked and each dropped finding with its reason, and the
  review report lists them. `hall stats` counts them as `verifier_dropped`.
- **`verify`** in `.hallmark.toml` names the model ("sonnet"); `"off"`,
  the default, skips the run. It is off by default because it is a second
  model run on every rejected review; turn it on when `hall stats` shows
  noise worth a Sonnet pass, and judge it by the same numbers.
- **It never holds an item.** The run shares the review budget and caps
  itself at $1 and ten minutes. When it is blocked, fails, or returns an
  answer that doesn't fit the schema, the verdict stands as the reviewer
  gave it, the report says why, and `RunRequest::no_hold` keeps a second
  failure from holding the item.

## Consequences

- With the verifier on, a false blocker or major costs one cheap run
  instead of a fix, a gate run and a review.
- Only blocking findings are checked: minors and nits don't cause rounds,
  and the fixer may already leave them.
- The verifier can be wrong the other way and drop a real finding. The
  report keeps it, with the verifier's reason, so a person can see what
  was dropped; a reverted change (ADR 0061) whose dropped finding named
  the cause is the signal to turn the verifier off or change its model.
- The fake runner replays `verifier` steps like any role, so tests drive
  it without a model.
