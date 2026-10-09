# 0004. The hallmark is a git note

Status: Accepted, 2026-10-09

## Context

An approval must be bound to exactly the commit that was reviewed. A later
commit, even a one-line one, must not inherit it. The gate must be able to check
the approval cheaply, from any worktree.

## Decision

- **The hallmark** is a git note under `refs/notes/hallmark` on the approved SHA.
  - Its body is JSON: the item, round, reviewer, finding counts and time.
- **The PreToolUse gate** denies these unless `HEAD` has a hallmark:
  - `git push`
  - `gh pr create`
  - the configured ship command
- **Who writes it.** Only the assayer writes hallmarks (Stage 2). Stage 1 only
  reads them.

## Consequences

- Any new commit has no note, so it is blocked until it is reviewed again.
- Notes live in the shared object store, so every worktree sees them.
- Notes are not pushed by default. That suits us, because the hallmark is a
  local gate, not a published signature.
- A person can still push from a terminal outside Claude Code. The gate protects
  agent sessions; it is not a server-side control. Branch protection on GitHub
  remains the human's final merge gate.
