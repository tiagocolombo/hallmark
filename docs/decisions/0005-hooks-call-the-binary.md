# 0005. Hooks call `hall hook <event>`

Status: Accepted, 2026-10-09

## Context

Claude Code hooks are commands that receive JSON on stdin. Shell-script hooks
don't run on Windows without extra tooling. They are also hard to test, and they
duplicate logic that the binary already has: repo discovery, item resolution and
journal locking.

## Decision

- Each hook in the plugin's `hooks/hooks.json` is `hall hook <event>`.
- The events are `session-start`, `user-prompt-submit`, `pre-tool-use`,
  `post-tool-use`, `stop` and `notification`. PascalCase names are accepted too.
- The handlers parse the real stdin shapes recorded in Step 0. Those samples
  are test fixtures.
- **Hooks never break a session.**
  - On an internal error, they exit 0, print nothing, and append to
    `hallmark/hook-errors.log`.
  - In a repo without Hallmark, they do nothing.
- **Gate decisions** are returned as
  `hookSpecificOutput.permissionDecision` (`deny` or `ask`), with exit 0. The
  reason is Hallmark's error JSON, so Claude sees `next`.

## Consequences

- The same code runs on all three systems and is unit-tested with real
  fixtures.
- A hook runs on every tool call, so the handlers stay fast. They make no
  network calls and run at most a few `git` subprocesses.
- `hall` must be found by Claude Code even when it isn't on the GUI app's PATH.
  ADR 0009 covers this.
