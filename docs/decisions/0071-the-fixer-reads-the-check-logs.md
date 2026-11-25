# 0071. The fixer reads the check logs, read-only

Status: Accepted, 2026-11-25. Amends 0053.

## Context

When checks fail, the fixer's prompt carries only the failing gate's own
section of the combined `checks/r<round>.log`, cut to a tail. A fixer that
needs more (an earlier error, another gate's output) couldn't open the log:
0053 limits file tools to the run's directories, and the item's `checks/`
directory is outside them. Fixers held instead of recovering.

## Decision

A fixer run whose item has failing checks passes `--add-dir` for the item's
`checks/` directory (`runner::allow_read_dir`). That is the only `--add-dir`
Hallmark grants to a headless run. The grant is read-only: the same call adds
`Edit` and `Write` deny rules for the directory (`//`-absolute, since a single
`/` is project-relative) to the run's `--settings`, so `acceptEdits` can't
modify the logs. It fails closed: if the run has no `--settings` deny list to
extend, the directory isn't added. Runs without failing checks get no grant.

## Consequences

- The skill no longer says the log can't be read.
- The grant is the fixer's alone; the reviewer and other roles keep reading
  only inside their own directories.
- Credentials stay out of reach: `deny_read` still applies.
