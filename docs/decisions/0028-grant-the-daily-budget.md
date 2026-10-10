# 0028. Grant the daily budget for today, without changing the cap

Status: Accepted, 2026-10-10

## Context

`[budget] daily` pauses the daemon once the loop's runs reach it, until
00:00 UTC (ADR 0016). That is the right default, but the cap is a guess made
in advance. On a busy day the loop stops with Claude quota still left, and
the only way on was to raise `daily` in `.hallmark.toml`. That changes the
cap for every later day, and the edit has to be undone by hand. `hall daemon
resume` doesn't help either: the next model run checks the budget again and
pauses at once.

## Decision

- **`hall budget --grant [<tokens>]`** lets the loop go past the daily cap for
  today (UTC). It records `budget_granted {day, tokens, reason}` in the repo
  journal. With no amount, the grant is another `daily`.
- **Today's cap is `daily` plus today's grant.** The latest grant for a day
  is the one that counts, so the command is idempotent. Repeating it with
  the same amount writes nothing; a larger amount replaces the grant rather
  than adding to it. Grants for other days don't count, so the extra
  allowance ends at 00:00 UTC on its own. `daily` itself never changes.
- **The pause lifts at once.** If the grant covers today's spend and the
  daemon is paused on the daily budget, the pause ends now: `unpaused` is
  written to the journals that recorded it. A pause for another reason (a
  rate limit) is left alone. If another repo is still over its own cap, its
  next model run pauses the loop again.
- **Everything that reads the daily cap sees the grant**: the budget checks
  before each run, `hall budget` (the daily cap's `granted`), the status
  line, the HUD and the session hooks. When the cap is reached, the stop's
  reason, `hall budget`'s `next` and the hook note all name
  `hall budget --grant`.

## Consequences

- A spent day can be continued with one command, and that day's journal
  shows who granted how much and why.
- `--grant` is per repository, like `daily`.
- Per-item caps are separate: a budget hold on one item is released with
  `hall retry` (ADR 0016).
