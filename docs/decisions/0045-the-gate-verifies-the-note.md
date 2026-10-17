# 0045. The gate verifies the note

Status: Accepted, 2026-10-15. Partly supersedes 0004.

## Context

- **Any note passed.** ADR 0004 made the hallmark a git note on the approved
  commit, and the push gate let a push through when `HEAD` had a note. It
  never read the note.
- **Anyone can write one.** `git notes --ref hallmark add -m x HEAD` is an
  ordinary git command. An agent denied a push could write a note and push
  again, and the gate would let it through. Copying a real note onto a later,
  unreviewed commit worked too.

## Decision

The push gate verifies the note on `HEAD` (`stamp::verify`). The push passes
only when all of these hold:

1. The note parses as the hallmark's JSON (`stamp::Note`).
2. Its `hallmark` version is 1.
3. Its `sha` is `HEAD`'s full SHA.
4. The journal of the item it names has a `hallmarked` event at `HEAD`. Only
   the assayer's approval (`stamp::strike`) and the Dispatch Dock's carry
   across a clean rebase (`stamp::carry`, ADR 0030) write one.

Otherwise the gate denies with `GATE_DENIED`. The message names the check
that failed: no note, a note that doesn't parse, an unknown version, a note
for another commit, or an item whose journal records no hallmark there.

The check is the same for every actor. A headless run of the assayer, fixer
or shipper gets no exception.

## Consequences

- Forging a hallmark now takes appending to the item's journal as well as
  writing the note. The journal lives under the git common dir, outside
  every worktree. The edit gate doesn't cover the common dir
  (`Repo::contains` is false for it), so Claude's Edit and Write tools can
  append to the journal just as a shell write can. This raises the bar; it
  doesn't make forging impossible. Denying edits under the hallmark root in
  the edit gate is the remaining gap, left for a follow-up.
- Gating `git notes` writes themselves is a separate change to the gate's
  parser. This decision doesn't depend on it.
- The rest of ADR 0004 stands: the note is still the hallmark, local and not
  pushed, and the gate still protects agent sessions, not the server.
