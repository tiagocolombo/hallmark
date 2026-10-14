# 0041. Watch headless runs live: a stream per run, read by `hall trace`

Status: Accepted, 2026-10-12

## Context

The loop's work happens in headless `claude -p` runs (ADR 0006, ADR 0013):
the reviewer, the fixer, the Delivery office's fixes, intake, the librarian
and resumed sessions. While one runs, Hallmark shows only "reviewer run
started" in the Live log and `running` in `hall watch`. The runner asked for
`--output-format json`, which prints nothing until the end, and kept that one
result in `runs/`. What the agent read, ran and said was in Claude Code's own
transcript under `~/.claude/projects/<dir>/<session>.jsonl`. Finding it meant
knowing how Claude Code names that directory and learning the session ID,
which only arrived with the result.

## Decision

- **Every run streams into the item's `runs/`.** The runner asks for
  `--output-format stream-json --verbose` and writes stdout to
  `runs/<stamp>-<role>.jsonl` as it arrives, line by line. After the
  process exits, timed out or not, it appends one line of its own:
  `{"type":"hall_run_end","t":…,"duration_ms":…,"exit_code":…,"failure":…}`
  (`trace::RunEnd`). It no longer writes `<stamp>-<role>.json`; the result
  is the stream's last `result` line, which `PrintResult::parse` already
  reads. A run with no item streams nowhere; intake, which runs before
  its item exists, saves its whole stream to the new item afterwards. The
  file is created empty when the run starts, so it is listed at once, and a
  fallback-model retry is a run of its own.
- **Hallmark owns the file.** Nothing reads `~/.claude/projects`: the
  stream is in the journal's directory, on every OS, and the fake runner
  writes the same file from its script.
- **`src/trace.rs` reads it.** It lists an item's runs (older `.json`
  results read as runs with only an end), folds lines into steps (start,
  text, tool, result, done, end) and what is known of the run (session,
  working directory, model, ended, failure), and follows a growing file a
  whole line at a time. Empty thinking blocks are skipped.
- **Four views read the module:**
  - `hall trace [<id>]` prints a run as steps; `--follow` follows the
    latest until it ends, `--role` and `--run` pick another, `--open`
    prints the `claude --resume` command for an ended run. Off a terminal
    each step is a JSON line and the reply is the last line, like
    `hall watch`.
  - `hall watch --trace` adds the running run's steps under its changes.
  - The terminal app shows the selected item's latest run, live, on `t`.
  - The now block offers `hall trace <id> --follow` while a run is going,
    and the resume command once a failed or held run has ended.
- **Not resumed while running.** The resume command appears only after the
  run's `hall_run_end`, so a person doesn't continue a session the daemon
  is still writing.

## Consequences

- A run is watchable from its first tool call, from the CLI, the terminal
  app or an agent.
- `runs/` grows with the whole conversation, not only its result. Runs are
  debugging records like `checks/`, kept with the item.
- `rate_limit_event` lines are normal in a stream. Rate-limit detection
  must read the result and stderr, not every line.
- If the daemon dies mid-run the stream has no end line. The runner holds
  an exclusive lock on a sidecar `<stamp>-<role>.lock` for as long as it
  writes the stream (the OS frees it when the process dies). A stream with
  no end line reads as still going while that lock is held, and as
  interrupted, so it can be opened, once it is free. This holds for runs
  that take no item lease (resume, `hall library distill`). A stream with
  no sidecar (older) falls back to the lease: going while a live lease that
  began before it holds the item and no newer run exists.
