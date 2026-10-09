# 0007. Our own reviewer, not `/code-review`

Status: Accepted, 2026-10-09

## Context

Claude Code has a built-in `/code-review`. If it ran headless and returned
structured findings, it could replace the principal-engineer agent.

## Decision

Keep our own reviewer:

- the `principal-engineer` plugin agent, with read-only tools;
- the `principal-review` skill, with seven lenses;
- `schemas/verdict.json`, enforced with `--json-schema`.

Step 0 (spike 6) showed what `/code-review` does headless:

- **What works.**
  - It finds real bugs. The result has `local_command: "code_review"`.
  - Its findings come as a JSON array (`file`, `line`, `summary`,
    `failure_scenario`) inside a Markdown fence in `result`.
- **Why it doesn't fit.**
  - It ignores `--json-schema`: there is no `structured_output`.
  - It has no severities, so "approve = 0 blockers and 0 majors" can't be
    computed.
  - It can't be steered to our lenses.
  - It reviews whatever working tree it finds, not a frozen SHA against a base
    branch.
  - Its rules don't come from the base branch.

## Consequences

- We maintain the agent, the skill and the schema ourselves.
- `/code-review` may later run as an extra, advisory signal, but it never
  decides a hallmark.
