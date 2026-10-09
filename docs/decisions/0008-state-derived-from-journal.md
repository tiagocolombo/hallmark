# 0008. Item state is folded from the journal

Status: Accepted, 2026-10-09

## Context

An item's state could be stored, in `item.json` or SQLite, or it could be
derived from its events. A stored state can disagree with the journal after a
crash or a concurrent write. Then the record and the behaviour drift apart.

## Decision

The journal is the only source of truth for state.

- **Folding.**
  - The `created` event starts an item in `development`.
  - Each transition event maps to a state-machine action:
    - `proposed_ready` → ProposeReady
    - `ready` → Ready
    - `pulled_back` → PullBack
    - `dropped` → Drop
    - `retried` → AllowRound
    - `checks` → ChecksPass or ChecksFail
    - `verdict` → Approve or RequestChanges
    - `fix_submitted` → Resubmit
    - `shipped` → Ship
  - `state::next(state, action)` gives the next state.
- **Escalation.** The verdict event records whether it escalated. It is decided
  when written, using `max_rejects`, so folding needs no config.
- **Commands.** A state-changing command locks the item's journal, folds it,
  checks the transition, and appends. It does all of this under one lock.
- **Caches.** `item.json` and the SQLite `items` table are caches, rewritten
  after each change.
- **The transition table** is unit-tested over every state × action pair.

## Consequences

- State can't drift from the journal, and `hall reindex` rebuilds it exactly.
- Folding reads the item's whole journal. Journals are small (hundreds of lines),
  so this is cheap.
- An invalid transition found while folding (a corrupted journal) is reported as
  a warning, and the state is left unchanged. It doesn't crash.
