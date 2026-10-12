# 0006. `claude -p` for every model run

Status: Accepted, 2026-10-09. Superseded in part by
[ADR 0039](0039-watch-runs-live.md): runs ask for
`--output-format stream-json --verbose`, and the result is the stream's last
`result` line. The flags and the two rules below stand.

## Context

Hallmark needs headless model runs for the reviewer, the fixer, intake and the
librarian. These runs should use the user's Claude plan and Claude Code's tools,
plugins and permissions, and each one must report its usage.

## Decision

Every model run is `claude -p --output-format json`, with:

- `--plugin-dir` and `--agent <name>` for role agents;
- `--json-schema` where the output is structured;
- `--max-budget-usd` and a timeout, for the per-run budget;
- `--resume <session-id>` to continue after a question is answered;
- `--` before the prompt, and a null stdin.

Step 0 confirmed every flag (see `docs/spikes.md`). It also found two rules we
now follow.

1. **`modelUsage` is the source of truth for usage.**
   - The top-level `usage` is all zeros for local commands such as
     `/code-review`, and holds only the last iteration in multi-turn runs.
   - Hallmark records, from each `modelUsage[model]`: input, output, cache read
     and cache write tokens, and `costUSD` (`costBasis: "list"`).
   - From the top level it records `num_turns`, `duration_ms` and `session_id`.
2. **A run with `--json-schema` must have `structured_output`.**
   - When the model can't satisfy the schema, the run still ends
     `subtype: success` with exit 0, but without `structured_output`.
   - Hallmark treats that as a failed run, and never parses prose.

Interactive sessions get their usage from the transcript instead, through the
`Stop` hook. Lines are deduplicated by `requestId`, because each content block
repeats the message's usage.

## Consequences

- Hallmark never holds an API key and never calls the API directly.
- Every run pays a fixed context cost: about 30k tokens for a plain run, about
  4k for an agent run. So we prefer fewer, larger runs.
- If the CLI's output changes shape, the fixture tests catch it.
