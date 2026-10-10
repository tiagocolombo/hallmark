# 0017. The daemon process: one instance, stop by file, pauses for everyone

Status: Accepted, 2026-10-09

## Context

ADR 0014 defines the daemon as a loop of workers with a heartbeat in
`<user data dir>/hallmark/daemon/state.json`. ADR 0016 says a rate limit or
the daily budget pauses it. That leaves open how the process starts, stops
and proves it is alive on three operating systems, and how a pause behaves
across repositories, restarts and `hall daemon run --once`.

## Decision

- **One instance.** `hall daemon run` holds an fs4 lock on
  `daemon/daemon.lock` for its whole life. A second `run` gets `CLAIMED`
  (exit 7).
  - "Running" means the lock is held. A crashed loop releases it at once,
    so this is more exact than the heartbeat.
  - The heartbeat is still written every few seconds, including during a
    long pass, because the inbox reads it (`daemon_stopped` after
    `STALE_AFTER_SECS`).
- **Stop by file, not by signal.**
  - `hall daemon stop` sets `wanted = false`. The loop reads the state
    file every second, finishes the passes in progress, clears its
    heartbeat, records `stopped_at` and exits.
  - `stop` waits up to 10s for the lock to be released. This works the
    same on Windows, where there is no SIGTERM.
  - Every read-modify-write of the state file goes through
    `state::update`, under `daemon/state.lock`, so the heartbeat never
    undoes a `stop`.
- **`wanted`.** The loop sets it when it starts, `start` sets it before
  launching, and `stop` clears it. `run --once` never touches it.
- **Start.** `hall daemon start` launches `<this binary> daemon run`
  detached from the terminal:
  - on Unix, in a new process group;
  - on Windows, with `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP |
    CREATE_NO_WINDOW`.

  stdin is null and the output goes to `daemon/daemon.log`. `start` waits
  for the new pid's first heartbeat. If a daemon is already running, it
  answers `changed: false`.
- **At login.** `--at-login` installs a user-level service labelled
  `dev.hallmark.daemon`. It runs the absolute path of the binary with
  `daemon run`, with `RestartPolicy::Never`, so `stop` really stops it.
  - macOS and Linux use service-manager (a launchd agent or a
    `systemd --user` unit). The service gets `PATH`, `HALL_HOME`,
    `HALL_CLAUDE` and `HALL_GH`, because those managers start with a bare
    environment.
  - On Windows, service-manager only has system services, so it uses a
    `schtasks /SC ONLOGON` task.
  - `HALL_DAEMON_SERVICE_DRY=1` reports the definition and installs
    nothing, for tests and CI.
- **Passes.**
  - The registry (`repos.json`) is re-read every poll interval, so a newly
    initialized repo joins without a restart.
  - Each repository has its own schedule and thread: its next pass starts
    one poll interval after its own last pass ended, so a slow repo never
    holds another's queue. One repo's workers run in order.
  - `passes` in the state counts finished per-repo passes.
  - A stop waits for the passes in progress, so a long model run delays it.
  - A missing or uninitialized repo is skipped, and only noted in its
    last-pass summary. Nothing is written to its journal.
  - The poll interval is 5s (`HALL_DAEMON_POLL` overrides it).
- **Pauses are daemon-wide.** A `Progress::Pause` from any repo's worker
  starts one pause:
  - `paused` is recorded in the journal of every repository that pass
    reached, so `hall log` in any of them explains why nothing moved;
  - the pause goes into `state.json` (reason, until, the journals), so a
    restart honors it;
  - while it lasts, the model workers (resume, assayer, fixer, librarian)
    are skipped, and the watchman, shipper and notify keep running;
  - with no `until`, it lasts 30 minutes;
  - when it ends, `unpaused` goes to the same journals.
- **`run --once`** settles an expired pause, honors a pause in force and
  records a new one.
  - The exception is `--only`: naming workers drives them by hand, pause
    or not.
  - With no `--repo`, it covers the current repository, or every
    registered one when run outside a repository.
- **The watchman** flags items in checks, review, fixing or hallmarked
  whose journal has been silent for `stuck_after`. It skips held items
  (already in the inbox) and items already flagged. It decides under the
  item's journal lock and appends `stuck` (actor `watchman`), which any
  later event clears.

## Consequences

- `status`, `start` and `stop` agree on whether the daemon runs, without
  pids or signals, on every OS.
- One rate limit pauses every repository, which matches how Claude's plan
  limit works. A daily budget configured in one repo also pauses the
  others until tomorrow.
- A pass with a long model run delays `stop` until that run ends. `stop`
  then answers `running: true` with a note, and the loop exits after the
  pass.
- Tests drive the real binary in the background with a 200ms poll and a
  dry service. That test is Unix-only. Windows CI covers `run --once`,
  pauses, the watchman and the state file.
