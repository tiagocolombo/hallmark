# 0054. Every child process starts in `process::command`: no console window on Windows

Status: Accepted, 2026-10-23. Amends 0017.

## Context

- The daemon runs without a console on Windows: `hall daemon start` launches
  it with `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW`
  (ADR 0017), so it has no window of its own.
- On Windows, a process without a console gets a new, visible console for
  every console program it starts. The daemon's passes run `git` several
  times a second, plus `gh`, `claude -p` and the gates, and every one of them
  flashed a console window (issue #119). Those `git` children also stopped
  exiting, the daemon's heartbeat stopped, and `hall daemon stop` could only
  clear `wanted` and wait.
- `Command::new` was called in some forty places, in sixteen files, and only
  the daemon's own launch set a creation flag.

## Decision

- `process::command(program)` is the one way to make a `Command`. It is
  `Command::new`, and on Windows it adds `CREATE_NO_WINDOW` when this process
  has no console (`GetConsoleWindow()` is null). The child then gets an
  invisible console instead of a visible one, and the child's own children
  (`cmd /C claude.cmd`, `cmd /C <gate>`) inherit it.
- With a console (`hall` in a terminal, a hook under Claude Code), the flag
  is left off: children inherit the terminal's console, open no window, and
  stay in reach of its Ctrl-C.
- `tests/sources.rs` rejects a `Command::new` anywhere else under `src/`.

## Consequences

- The daemon is silent on Windows, as it is elsewhere. Nothing changes in a
  terminal, or on Unix.
- A new call site goes through `process::command`, or the tests fail.
- `hall daemon stop` still waits for the pass in progress; a child that never
  exits still keeps the daemon alive. That is a separate matter.
