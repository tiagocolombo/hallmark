# 0012: `.hallmark.toml` is read from the main checkout

## Context

Every item works in its own worktree, and each worktree has its own copy of
`.hallmark.toml`. The edit gate lets an agent change any file inside its item's
worktree, including that copy. If `hall` read the settings from the worktree
the command runs in, an agent could loosen its own rules: rename the ship
command so the push gate no longer recognises it, or set `capture.mode = "off"`.

The reviewer has the same problem, and the design already answers it there:
its rules come from the base branch, never from the branch under review.

## Decision

- `hall` reads `.hallmark.toml` from the main checkout first, and only falls
  back to the current worktree when the main checkout has none.
- `hall init` writes the file in the current worktree's root, which is the
  main checkout in the normal case.
- In Stage 2 the assayer and the gate read settings that matter for review
  from the base branch (`git show <base>:.hallmark.toml`), which is stricter
  still.

## Consequences

- A change to `.hallmark.toml` made on an item's branch takes effect only
  once it is merged and the main checkout is updated, like any other rule.
- Testing a new gate command means editing the main checkout's file, or
  running the command by hand.
- Found by an integration test: a ship command set in the main checkout's
  file wasn't gated in a worktree whose own copy still had the default.
