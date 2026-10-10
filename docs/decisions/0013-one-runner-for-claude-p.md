# 0013. One runner for every `claude -p` run

Status: Accepted, 2026-10-09

## Context

Stage 2 adds four headless roles: the reviewer, the fixer, intake and the
librarian. Answers to questions also resume sessions headless. Each run
must use the command shape from Step 0, stop on a time and money limit,
record its usage, and fail safely. Tests must never spend model usage, on
any OS.

## Decision

- `src/runner.rs` owns the command. Callers build a `RunRequest` (role, room,
  agent, model, effort, prompt, schema, budget, timeout, cwd, tools); the
  runner turns it into
  `claude -p [--resume s] --plugin-dir d --agent a --model m [--effort e]
  --output-format json [--json-schema s] [--max-budget-usd n]
  [--permission-mode m] [--add-dir …] [claude_args…] [--allowedTools …] -- "<prompt>"`
  with a null stdin.
  - A prompt over 16k characters goes through stdin instead, because
    Windows caps a command line at 32k characters.
  - The schema is passed as text, as Step 0 did.
  - `--plugin-dir` is a copy of the plugin extracted from the binary
    (`plugin::runtime_dir`), so headless runs always match the binary.
- **Failures are classified, never parsed from prose:** timeout (the child
  is killed), budget (`--max-budget-usd`), rate limit (the daemon pauses),
  an error result, and a schema run without `structured_output`.
- **`run_metered`** wraps every run: it checks the budgets first
  (`budget::check`), saves the raw result under `items/<id>/runs/`,
  records one `usage` event per `modelUsage` model, and records
  `run_failed`. The second failure in a row for the same role holds the
  item for you (`held`, kind `run_failed`). Rate limits are not the item's
  fault and don't count.
- **The `Runner` trait** has two implementations:
  - `ClaudeRunner`, the real binary (`HALL_CLAUDE`, else `claude`);
  - `FakeRunner`, chosen with `HALL_RUNNER=fake:<script.json>`. It replays
    scripted steps per role: a structured output or a whole result, files
    to write, shell commands to run (`{hall}` is the binary), and failures.
    It logs every request to `<script>.calls.jsonl`.
- Runs that should leave no trace through hooks (reviewer, intake,
  librarian) set `HALL_NO_HOOKS=1`; the runner records them instead. The
  fixer keeps hooks, so its commits are journaled and gated like any
  session's.

## Consequences

- Workers never touch `Command::new("claude")`, so the shape is tested in
  one place.
- Integration tests drive the real `hall` binary through the daemon with
  scripted model output, on all three systems.
- A change in Claude Code's result shape breaks the fixture tests rather
  than the loop.

## Addendum: isolation, from the Stage 2 live checks

Live runs (ADR 0018, 0019, 0024) showed three things the runner now handles
for every role in `runner::request`:

- A `tools:` list in an agent hides the `StructuredOutput` tool, so every
  agent that lists tools includes it. `--json-schema` also rejects a
  `$schema` key, so the plugin's schemas have none.
- A branch's `.claude/agents/<name>.md` replaces a plugin agent called by its
  bare name, so agents are always called as `hallmark:<name>`.
- The checkout's CLAUDE.md, settings and MCP servers load into `--agent`
  runs (7,950 tokens of context against 4,942 without). Every run passes
  `--strict-mcp-config`; the read-only roles (reviewer, intake, librarian)
  also pass `--setting-sources user` and `CLAUDE_CODE_DISABLE_CLAUDE_MDS=1`,
  and run without hooks (`HALL_NO_HOOKS=1`).
- Every run sets `HALL_HEADLESS=1`. The Stop hook then leaves usage to the
  runner, which reads it from the result, so a fixer's or a resumed
  session's turn is counted once.
