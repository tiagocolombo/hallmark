# 0014. The daemon is a loop of workers, with leases

Status: Accepted, 2026-10-09

## Context

The review loop runs without you: checks, review, fixing, shipping,
resuming answered sessions, the librarian, notifications and the watchman.
Agents may also take work themselves (`hall next`). Work must never be
taken twice, a crash must not strand an item, and every part must be
testable alone.

## Decision

- **Workers.** Each part is a `tick(&WorkerCtx) -> Result<Progress>` that
  looks at one repository and does at most one piece of work.
  `worker::ORDER` runs them: watchman, resume, assayer, fixer, shipper,
  librarian, notify. `Progress` is idle, worked, or pause (a rate limit or
  the daily budget stops the whole pass).
- **`hall daemon run --once [--only w,…]`** runs one pass in the
  foreground. The background daemon (`hall daemon start`) repeats passes
  over every registered repo, and keeps a heartbeat and pause state in
  `<user data dir>/hallmark/daemon/state.json`. The inbox flags a daemon
  that should be running but has no fresh heartbeat.
- **Leases.** `lease::claim` writes `hallmark/leases/<id>.json` under the repo
  lock: one holder per item, with an expiry. Another live holder means
  `CLAIMED` (exit 7). `lease::candidates` picks work per role:
  - assayer: items in checks or review, oldest ticket first, and only one
    at a time per repo (test runs share caches and ports);
  - fixer: items in fixing; shipper: hallmarked items.
  Held items and child items are skipped.
- **`hall next --role assayer|fixer|shipper`** gives agents the same leases.
- Every worker writes what it is doing to `hallmark/live.log`, which the
  terminal app's Live view tails.

## Consequences

- The daemon and agents can share a repo safely; the worst case after a
  crash is waiting for a lease to expire.
- Workers are tested alone through `--only`, with the fake runner.
- Separate repositories can run in parallel; one repo's assayer is serial.
