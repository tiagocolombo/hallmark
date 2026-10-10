# 0026. Every phase runs in parallel, with configurable slots

Status: Accepted, 2026-10-10. Supersedes "one ticket at a time per repo" in
ADR 0014 and 0018.

## Context

The assayer took one item at a time per repository, so that test runs
wouldn't share build caches or ports, and the daemon waited for every repo's
pass before the next one. With several repos and many items, the review loop
crawled: items waited minutes for a slot while the machine and the model plan
sat mostly idle. Agents are cheap to run side by side; the loop should use
that.

## Decision

- **`[parallel]`** in `.hallmark.toml` says how many items each phase works on
  at once, per repo: `checks` (2), `review` (4), `fix` (4), `ship` (2).
- **Leases count per phase:** `lease::candidates` offers an item of a phase
  only while fewer than that many items of the phase are leased by others.
  Agents using `hall next` get the same limits.
- **Checks** run in a pool of assay worktrees, `../<repo>-assay`,
  `../<repo>-assay-2`, …, one lock per slot, reused so build caches stay warm.
  A repo whose tests share ports sets `checks = 1`.
- **Reviews** each get their own checkout at their commit,
  `../<repo>-review-<id>`, removed once the verdict is recorded. Fixers already
  work in the item's own worktree.
- **The daemon schedules every repo, worker and slot on its own** (building on
  H-0007's per-repo schedule): a job's next run starts one poll interval after
  it ended. Each slot has its own lease holder (`daemon:<pid>:<worker>.<slot>`),
  so two slots never take the same item. A worker's extra slots wake only
  while it has work (a job runs long, or one did something lately), so an idle
  repo costs one job per worker per poll.

## Consequences

- N items move through checks, review, fixing and shipping at once, per repo
  and across repos, up to the limits and the budgets.
- Each extra check slot costs a worktree and its build cache on disk.
- Budgets and rate limits still apply per run; a pause stops every model slot.
