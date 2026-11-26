# 0073. The daemon survives a burst of open files

Status: Accepted, 2026-11-26.

## Context

The daemon stopped on its own twice in one day, with nothing in its log.
launchd recorded exit code 1, and once the state file said `pid: 0,
wanted: false`, as if `hall daemon stop` had run.

What happened:

- The login service (`dev.hallmark.daemon`) starts with launchd's soft
  limit of 256 open files.
- The loop starts every repo's worker jobs at once. That happens at
  start-up and again whenever many come due together, as when a pause
  lifts. With a dozen repos, that round holds about 290 files open.
- Out of file handles, `state::update` couldn't read `state.json`. It took
  that to mean no state, and saved a default over the file. A default
  state has `wanted = false`, so the loop then stopped cleanly.
- Or opening `state.lock` failed, and a `?` in the loop ended the process
  with exit 1.
- In both cases nothing said why. The final error goes to the stdout that
  `hall daemon start` gave the loop, which is `daemon.log`, and the loop's
  own log rotation had already renamed that file away. A login service's
  stdout goes nowhere.

## Decision

- **More file handles.** On Unix, the daemon raises its soft
  `RLIMIT_NOFILE` to its hard limit, at most 10240, when it starts, and
  logs the change.
- **Never reset the state on a failed read.** A writer reads the state with
  `state::load_strict`. A missing file is no state, but a file that exists
  and can't be read is an error, so `update` fails instead of saving a
  default over it. A file that reads but doesn't parse still starts over.
- **A failed state update doesn't end the loop.** The loop logs it and
  tries again: on the next round when it was settling a pause, and with the
  next finished job when it was recording one.
- **A loop that ends on an error writes the reason to `daemon.log`** itself,
  instead of relying on stdout.

## Consequences

- A burst now has 40 times the room before it hits the limit, and running
  into it costs a skipped round instead of the daemon.
- The loop's own check of the state file tells the two cases apart. A file
  that is unreadable is logged and skipped, however long it lasts, so the
  loop keeps running with nothing to stop it until the file can be read
  again. A file that is missing (or doesn't parse) still stops the loop
  after three misses in a row, as before. `hall daemon stop` reports the
  error, and fixing the file's permissions brings both back.
- Windows has no per-process limit like this to raise. The other changes
  apply there too.
