# 0016. Budgets, holds and pauses

Status: Accepted, 2026-10-09

## Context

Every review round spends a test run and a long model run against your
plan's limits. The design has four budget levels plus the plan's own rate
limit, and each stops something different.

## Decision

| Level | Set by | When reached |
| --- | --- | --- |
| Per run | `[budget.<role>] max_usd` → `--max-budget-usd`, and `timeout` (default 20m) | The run stops (`run_failed`). The next tick retries once; a second failure holds the item. |
| Per item | `[budget] per_item` | The item is **held** (kind `budget`) and shows in the inbox. |
| Per room per day | `[budget.<role>] daily` | That role's runs wait until tomorrow; tickets keep their order. |
| Daily total | `[budget] daily` | The daemon pauses until tomorrow. |
| Plan limit | Claude | The daemon pauses until the reset time Claude reports, and records `paused`. |

- **A hold** (`held` event, `Item.hold`) stops the loop for one item
  without changing its state, so its place in the machine is kept. `hall
  retry` releases it (`released`); any state change clears it too.
- **Going on past a budget hold** doesn't change the budget. Released
  as-is, the item would be held again on its next run, so `hall retry`
  on a budget hold (or `g` in the terminal app) records
  `granted_tokens` on the `released` event: another `per_item` by
  default, or `--tokens <n>`. The item's cap is `per_item` plus its
  grants; every other item, and the room and daily caps, are unchanged.
- **A pause** (`paused`/`unpaused`, repo journal; daemon state file) stops
  the daemon, not your sessions.
- `budget::check(repo, item, role)` is the single gate; `run_metered`
  calls it before every run.

## Consequences

- Only the item that is over budget waits; the rest of the building
  carries on.
- Letting one item finish never means raising `per_item` for all of
  them, and each grant is in that item's journal.
- Every stop is in the journal, so `hall log` explains why nothing moved.
