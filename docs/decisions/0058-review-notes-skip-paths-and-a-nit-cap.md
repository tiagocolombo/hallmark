# 0058. Review notes say what failure looks like and which paths to skip; a nit cap

Status: Accepted, 2026-10-27. Amends 0018.

## Context

The review notes stub (`hall init`) asked for sensitive paths and
invariants. Anthropic's advice for the same file (`REVIEW.md`, written by
code owners) adds three things that made their reviewer quieter and more
useful: say what failure looks like in that repository, list the paths to
skip, and cap nits. The first two are prompt content the notes already
carry to the reviewer. The nit cap has to be enforced somewhere: a prompt
that says "no more than three" is advice, and with an agent author every
extra nit is a commit, a gate run and another review round.

## Decision

- **The stub has four sections**: What failure looks like here, Sensitive
  paths, Paths to skip, Invariants. The examples sit in comments, as before,
  so they never count as globs.
- **Paths to skip are left out of the review.** `base::skip_globs` reads
  the section the way `sensitive_globs` does. A changed file that matches
  leaves the diff (whole chunks, `prompt::drop_files`), the file and line
  counts that set the effort, and the Library briefing's changed paths.
  The prompt names what was skipped and says those changes aren't the
  reviewer's to judge. A rebase review (ADR 0049) is already only what the
  rebase changed, so it skips nothing.
- **`max_nits`** (3 by default, in `.hallmark.toml`, read from the base
  like `max_rejects`) caps the nits a verdict records. The prompt tells the
  reviewer the cap. `Ledger::judge` keeps the first `max_nits` nits in the
  reviewer's order, counts the rest in `dropped_nits`, and never records
  them; the review report says how many were dropped. Blockers, majors and
  minors are never capped.
- The `principal-review` skill says both: the skipped paths aren't in
  scope, and the cap is real.

## Consequences

- A repository can keep generated files, vendored code and fixtures out of
  every review by listing them once, and a review of a change that is
  mostly generated output costs what the hand-written part costs.
- A reviewer that pads a verdict with nits loses them, and the fixer never
  sees them. The cap is per verdict, not per item: a later round may
  record more.
- The notes' first section is prose the reviewer reads as a rule; nothing
  parses it.
