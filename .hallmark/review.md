# Review notes for Hallmark

The principal engineer reads this file from the base branch before every
review. Hallmark reviews its own changes, so the code below decides what gets
approved and what may leave the machine: review it as if it were security
code, because it is.

## Sensitive paths

- `src/gate.rs` and `src/hooks.rs`: the edit and push gates
- `src/findings.rs`, `src/assayer/**`, `src/stamp.rs`: how a verdict becomes
  approval and a hallmark
- `src/state.rs`, `src/event.rs`, `src/journal.rs`: the state machine and the
  append-only journal
- `src/runner.rs`: every model run, its isolation and its budget
- `src/config.rs`, `src/repo.rs`: where settings are read from (ADR 0012)
- `plugin/**`: the agents, skills, schemas and hooks every session loads

## Invariants

- Approval is computed from open findings (0 blockers, 0 majors), never
  taken from the model's words (ADR 0015).
- The reviewer's rules, knowledge and settings come from the base branch,
  never the branch under review (ADR 0012, 0018).
- The journal is append-only; state is folded from it (ADR 0003, 0008).
- Hooks never break a session: internal errors exit 0 and are logged
  (ADR 0005).
- Every command keeps the agent contract: JSON off a TTY, `next`, errors with
  documented exit codes, no prompts, idempotent (ADR 0002). Changes to command
  output are pinned by insta snapshots and must be deliberate.
- Tests never spend model usage; they use the fake runner.
- Windows matters: no Unix-only code without a cfg gate and a reason.
