# Step 0: spikes

Before writing product code we checked every claim about Claude Code that the
design depends on and that came from `claude --help` rather than from running it.
Each spike used a throwaway git repo at `/tmp/hallmark-spike/repo` with one
committed file (`calc.py`) and one uncommitted change. Paths in the output below
are sanitized.

- Claude Code version: 2.1.295
- Model: `--model haiku` (`claude-haiku-5-5`) for every run
- Cost: each run cost under $0.01 at list price
- Date: 2026-10-09

The raw outputs are saved as test fixtures:

| Fixture | Source |
| --- | --- |
| `tests/fixtures/claude/print_result.json` | spike 1 |
| `tests/fixtures/claude/print_structured.json` | spike 2, schema satisfied |
| `tests/fixtures/claude/print_schema_mismatch.json` | spike 2, schema impossible |
| `tests/fixtures/claude/print_agent.json` | spike 3 |
| `tests/fixtures/claude/print_resume.json` | spike 4 |
| `tests/fixtures/claude/print_hooks.json` | spike 5, the `-p` result |
| `tests/fixtures/hooks/*.json` | spike 5 and the Notification spike, hook stdin |
| `tests/fixtures/transcript/session.jsonl` | spike 5, session transcript (trimmed) |
| `tests/fixtures/claude/print_code_review.json` | spike 6 |
| `tests/fixtures/claude/print_code_review_schema.json` | spike 6, with `--json-schema` |
| `tests/fixtures/claude/stream.jsonl` | `--output-format stream-json --verbose` with two tool calls; hook lines dropped, paths made `/tmp/spike` (ADR 0039) |

**Result: no decision turned out impossible.** Two findings change how Hallmark
reads Claude's output (spikes 1, 2 and 5), and one confirms that the reviewer
should be our own agent rather than `/code-review` (spike 6).

## Gotchas found on the way

- `--allowedTools` is variadic. It swallows a prompt that follows it, and the
  run fails with `Error: Input must be provided either through stdin or as a
  prompt argument when using --print`. Put `--` before the prompt.
- Without stdin redirected, `claude -p` waits 3 seconds and warns
  `no stdin data received in 3s`. Hallmark passes `< /dev/null` (a null stdin)
  to every headless run.

## 1. Usage fields from `claude -p --output-format json`

```sh
claude -p --model haiku --output-format json --max-budget-usd 0.3 \
  "Remember the word KIWI. Reply with just: ok" < /dev/null
```

Output, trimmed:

```json
{
  "type": "result", "subtype": "success", "is_error": false,
  "result": "ok",
  "session_id": "72f4b985-…",
  "num_turns": 1,
  "duration_ms": 814, "duration_api_ms": 768,
  "total_cost_usd": 0.00437354,
  "usage": {
    "input_tokens": 2, "output_tokens": 37,
    "cache_creation_input_tokens": 21349, "cache_read_input_tokens": 8504,
    "iterations": [ { "input_tokens": 2, "output_tokens": 37, "…": "…" } ]
  },
  "modelUsage": {
    "claude-haiku-5-5": {
      "inputTokens": 2, "outputTokens": 37,
      "cacheReadInputTokens": 8504, "cacheCreationInputTokens": 21349,
      "thinkingTokens": 33, "costUSD": 0.00437354, "costBasis": "list"
    }
  },
  "permission_denials": [], "terminal_reason": "completed"
}
```

Conclusions:

- Every field the design names is present: `usage`, `modelUsage[model].costUSD`,
  `num_turns`, `duration_ms`, `total_cost_usd` and `session_id`.
- `costBasis: "list"` confirms that the cost is a list-price figure.
- A one-word reply loaded **29.9k tokens** of context: 21.3k written to cache and
  8.5k read from it. This confirms the measured ~30k fixed cost per run.
- **The top-level `usage` is not reliable.**
  - In spike 6, `/code-review` is a local command. Its result has `num_turns: 0`
    and a top-level `usage` of all zeros, while `modelUsage` holds the real
    numbers: 30.5k cache write, 26.7k cache read and 1,649 output tokens.
  - In multi-turn runs, `usage.iterations` holds only the last iteration.
- **Hallmark therefore reads `modelUsage` as the source of truth**, one entry per
  model. It takes `num_turns`, `duration_ms` and `session_id` from the top
  level.

## 2. `--json-schema`

```sh
claude -p --model haiku --output-format json --max-budget-usd 0.3 \
  --json-schema "$(cat verdict-mini.json)" \
  "Review this function and return a verdict: def add(a,b): return a-b" < /dev/null
```

When the output matches the schema, the result appears twice: as an object in
`structured_output` and as a string in `result`.

```json
{
  "subtype": "success", "is_error": false, "num_turns": 3,
  "result": "{\"verdict\":\"changes\",\"findings\":[…]}",
  "structured_output": {
    "verdict": "changes",
    "findings": [ { "severity": "blocker", "file": "calc.py",
                    "summary": "add(a, b) returns a - b instead of a + b; …" } ]
  }
}
```

The model fills the schema by calling a `StructuredOutput` tool, which costs
extra turns (3 here).

Mismatch: the schema allowed only `n = 10`, and the prompt insisted on 3.

```sh
claude -p --model haiku --output-format json \
  --json-schema '{"type":"object","properties":{"n":{"type":"integer","minimum":10,"maximum":10}},"required":["n"],"additionalProperties":false}' \
  "Return the number 3 as n. It must be 3, never anything else." < /dev/null
```

```json
{
  "subtype": "success", "is_error": false, "num_turns": 4,
  "result": "I called StructuredOutput again with `n: 3`, and it failed the same way: `/n: must be >= 10`. …"
}
```

- The `StructuredOutput` tool returned a validation error, and the model
  retried.
- The run still ended with `subtype: success`, `is_error: false` and exit code 0.
- **There was no `structured_output` field.**

Conclusion: `--json-schema` guarantees that any `structured_output` matches the
schema. It does not guarantee that one exists. **Hallmark treats a missing
`structured_output` as a failed run** and never parses the prose in `result`.

## 3. A plugin agent run headless

The plugin `spike` defines `agents/echo-agent.md`, which has `model: haiku`,
`tools: Read`, and a prompt that always replies `ECHO-AGENT-ACTIVE`.

```sh
claude -p --plugin-dir ../plugin3 --agent echo-agent        --output-format json "hello there" < /dev/null
claude -p --plugin-dir ../plugin3 --agent spike:echo-agent  --output-format json "hello there" < /dev/null
```

Both returned `"result": "ECHO-AGENT-ACTIVE"`, with `modelUsage` keyed by
`claude-haiku-5-5` even though no `--model` was passed.

Conclusions:

- `--plugin-dir … --agent <name>` works headless. Both the bare name and the
  namespaced `plugin:agent` form work.
- The agent's frontmatter `model` is honored.
- **The agent's prompt replaces the default system prompt.** Context was 3.7k
  tokens instead of about 30k. Running the principal engineer as an agent is
  cheaper than running a prompt on top of the default context. It also means
  the agent must state everything it needs, because it inherits none of Claude
  Code's default instructions.

## 4. `--resume` in headless mode

```sh
SID=$(jq -r .session_id out/1-usage.json)
claude -p --resume "$SID" --model haiku --output-format json \
  "What word did I ask you to remember? Reply with just the word." < /dev/null
```

```json
{ "result": "KIWI", "session_id": "72f4b985-…",
  "usage": { "cache_creation_input_tokens": 105, "cache_read_input_tokens": 29853, "…": "…" } }
```

Conclusions:

- `claude -p --resume <session-id> "<answer>"` continues a headless session. It
  keeps the session ID and the context.
- The resumed turn was almost all cache reads (29.9k read, 105 written).
  Answering a question soon after it is asked is cheap.

## 5. Hook stdin, the deny contract, and transcripts

A plugin registered one command hook for each event. The script saved its stdin
and did two more things. For `SessionStart`, it returned this:

```json
{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"The secret context word is PAPAYA."}}
```

For a `PreToolUse` call that touched `denied.txt`, it returned this:

```json
{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"HALLMARK-DENY: run hall new first"}}
```

The run was headless:

```sh
claude -p --plugin-dir ../plugin --model haiku --output-format json \
  --permission-mode acceptEdits --allowedTools "Bash(git status:*)" -- \
  "1) Run git status --short. 2) Write notes.txt containing hi. 3) Write denied.txt containing no. If a tool call is denied, quote the reason verbatim. 4) Tell me the secret context word." < /dev/null
```

Claude's reply, from `result`:

> 3. The write to `denied.txt` was blocked by a PreToolUse hook. The denial reason,
> verbatim: `PreToolUse:Write hook error: HALLMARK-DENY: run hall new first`. …
> 4. The secret context word is **PAPAYA**. It came from the SessionStart hook context.

The `-p` result also listed the denial:
`"permission_denials": [{"tool_name": "Write", "tool_input": {"file_path": ".../denied.txt", …}}]`.

### Stdin shapes

Every event shares these fields:

```json
{ "session_id": "…", "transcript_path": "~/.claude/projects/<dir>/<session>.jsonl",
  "cwd": "/tmp/hallmark-spike/repo", "hook_event_name": "…" }
```

Most events also carry `prompt_id` and `permission_mode`.

| Event | Extra fields |
| --- | --- |
| `SessionStart` | `source: "startup"` |
| `UserPromptSubmit` | `prompt` |
| `PreToolUse` | `tool_name`, `tool_input` (Bash: `command`, `description`; Write: `file_path`, `content`), `tool_use_id`, `effort` |
| `PostToolUse` | as PreToolUse, plus `tool_response` (Bash: `stdout`, `stderr`, `interrupted`; Write: `type`, `filePath`, …) and `duration_ms` |
| `Stop` | `stop_hook_active: false`, `last_assistant_message`, `background_tasks`, `session_crons` |
| `Notification` | `message`, `notification_type` |
| `SubagentStop`, `SessionEnd` | `SessionEnd` has `reason: "other"` |

Conclusions:

- Hooks fire in `-p` mode as well as in interactive sessions when they are
  loaded through `--plugin-dir`.
- To deny a `PreToolUse` call, print the `permissionDecision: "deny"` JSON and
  exit 0. Claude receives the reason as `PreToolUse:<Tool> hook error: <reason>`.
  Hallmark puts its error JSON (`{"error":{"code":"NO_ITEM",…,"next":[…]}}`) in
  the reason, so Claude can read the command it should run.
- `SessionStart` `additionalContext` reaches the model.
- `Stop` provides `last_assistant_message`, so the turn summary needs no
  transcript parsing. Usage still comes from the transcript.

### Transcript usage

Each assistant content block (thinking, text, tool_use) is written to the
transcript as a separate JSONL line, and each line repeats the same
`message.usage` and `requestId`:

```text
assistant uuid=d5b6… requestId=req_011Cfs7tag… usage={cache_creation 18875, cache_read 11125, output 309}
assistant uuid=5dfe… requestId=req_011Cfs7tag… usage={cache_creation 18875, cache_read 11125, output 309}
```

**Usage must be deduplicated by `requestId`, or by `message.id`.** Otherwise it
is counted twice or more. The Stop hook keeps a byte offset for each session and
reads only the lines added since the last turn.

## Notification hook

`Notification` does not fire in `-p` mode, so this spike needed an interactive
session. It ran under `expect` to provide a PTY. The first attempts showed no
notification:

- The folder-trust dialog had to be answered first.
- A command that was allowed by default ran without a permission prompt, so it
  produced no notification.

The working version started a session and stayed idle for 75 seconds:

```sh
expect notif.exp /tmp/hallmark-spike/repo
```

```json
{ "session_id": "ea9f…", "transcript_path": "…", "cwd": "/tmp/hallmark-spike/repo",
  "prompt_id": "6ab4…", "hook_event_name": "Notification",
  "message": "Claude is waiting for your input", "notification_type": "idle_prompt" }
```

Conclusions:

- The Notification hook works for "session waiting" in the inbox.
- `notification_type` tells an idle wait (`idle_prompt`) apart from other types,
  such as permission requests.
- It fires only in interactive sessions, which are the ones it is meant for.

## 6. `/code-review` headless

```sh
claude -p --model haiku --output-format json --max-budget-usd 0.5 \
  --allowedTools "Bash(git diff:*)" "Bash(git status:*)" "Bash(git log:*)" Read Grep Glob -- \
  "/code-review" < /dev/null
```

```json
{
  "local_command": "code_review", "num_turns": 0,
  "usage": { "input_tokens": 0, "output_tokens": 0, "…": 0 },
  "modelUsage": { "claude-haiku-5-5": { "outputTokens": 1649, "costUSD": 0.0049, "…": "…" } },
  "result": "```json\n[\n  {\"file\": \".../calc.py\", \"line\": 2,\n   \"summary\": \"add() was changed from a + b to a - b …\",\n   \"failure_scenario\": \"add(2, 3) → -1 instead of 5; …\"}, …\n]\n```\n\nScope was the uncommitted working-tree change …"
}
```

The same run with `--json-schema` returned the same fenced JSON, with no
`structured_output`.

Conclusions:

- `/code-review` works headless and finds real bugs: the `a - b` change and the
  missing zero check.
- Its findings are a JSON array (`file`, `line`, `summary`,
  `failure_scenario`) inside a Markdown fence in `result`.
- It ignores `--json-schema`, has no severity, and can't be steered to our seven
  lenses.
- It reviews the working tree it finds, not a frozen SHA against a base branch.
- **Decision:** the reviewer stays our own `principal-engineer` agent with
  `schemas/verdict.json` enforced through `--json-schema` (see ADR 0007).
  `/code-review` could later become an optional extra signal, but it would not
  decide a hallmark.

## End-to-end check (after Stage 1 was built)

A real headless session in a fresh repo, with the plugin loaded through
`--plugin-dir` (nothing installed at user level), Haiku, `acceptEdits`:

```sh
claude -p --plugin-dir "$HALL_HOME/claude-plugin/plugins/hallmark" --model haiku \
  --permission-mode acceptEdits --allowedTools "Bash(hall:*)" "Bash(git:*)" \
  -- "Add a file hello.txt containing the word hi to this repository and commit it. Don't push."
```

First run: the SessionStart context and the edit gate worked, and Claude ran
`hall new` without being told to. Then **Claude Code's own permissions refused
every write to the new worktree**, because the default `../{repo}-{id}` is
outside the session's working directory. Hallmark's gate wasn't involved.

Fix: in `acceptEdits` and `bypassPermissions` sessions, the PreToolUse hook now
answers `allow` for Edit/Write inside a live item's worktree, extending the
edit permission the session already has to its item's worktree. In `default`
mode Claude Code still asks. The skill and the session context now tell Claude
to edit with the Edit/Write tools and run git with `git -C <worktree>`.

Second run: item H-0001 created, `hello.txt` written in the worktree, commit
`Add hello.txt` recorded by PostToolUse, and the journal shows
`created → session_bound → prompt → commit → usage → turn`. 5 turns, $0.008.

## End-to-end, Stage 2

Date: 2026-10-10. Claude Code 2.1.296. Every role on Haiku
(`claude-haiku-5-5`), set in `.hallmark.toml` `[budget]`, with
`max_usd = 0.50` per review and fix run and a 6-minute timeout. The repo was a
throwaway with one Python module, one gate (`python3 test_calc.py`), and a
**local bare remote** as `origin`; the ship command was
`git push -u origin {branch}` (no GitHub). `HALL_HOME` was a temp dir, and the
plugin was extracted by `plugin::runtime_dir` into it — nothing was installed
at user level. The daemon was driven one pass at a time with
`hall daemon run --once --repo <repo>`.

### H-0001: checks fail, the fixer repairs them, approve, ship

1. `hall new "Add a divide(a, b) function … raises ValueError when b is 0,
   with tests"`: the Front Desk's intake run kept it as one item —
   *"one small feature … the two files are tightly coupled, so splitting
   would add overhead"* (`planned`, `parallel: false`).
2. As the dev agent, we committed `divide` as `a // b` with a test that
   expects `ValueError` for `divide(1, 0)`, then `hall ready`.
3. Pass 1: the assayer ran the gate in its own worktree (`../calc-assay`):
   exit 1, `checks/r1.log`, state `fixing` — **no review tokens spent**. In
   the same pass the fixer (cwd = the item's worktree, hooks on) committed
   `fix checks: divide returns float and raises ValueError on zero` and ran
   `hall fixed` itself: `fix_submitted` by actor `fixer`. 8 turns, $0.0023.
4. Pass 2: checks passed at `3bfb962` → `review`.
5. Pass 3: the review approved (0 findings, $0.0013), `stamp::strike` wrote
   the note, the shipper pushed `hm/H-0001-…` to the bare remote and the item
   closed; the librarian found nothing worth keeping (`distilled`, no
   learnings).

### H-0002: review findings, fix, incremental re-review, approve, ship

1. `hall new --no-split --title "Add clamp(x, lo, hi)"`; we committed
   `return max(lo, min(x, lo))` (wrong upper bound) with one assert that
   passes by coincidence (`clamp(-5, 0, 10) == 0`), then `hall ready`.
2. Checks passed, then round 1 asked for changes
   (`reviews/r1.md`):
   - **F1 blocker · correctness · `calc.py:7`** — *"clamp(15, 0, 10): min(15,
     0) is 0, then max(0, 0) is 0, but the correct result is 10."*
   - **F2 major · evidence · `test_calc.py:9`** — *"The only clamp test passes
     on the buggy code."*
3. The fixer fixed both in one commit (`fix_submitted` claims F1, F2).
4. Round 2 was incremental (diff since the reviewed commit plus the open
   findings): both ruled `fixed`, approve. The hallmark note on `a439e3b`:
   `{"hallmark":1,"item":"H-0002","sha":"a439e3b…","round":2,
   "reviewer":"claude-haiku-5-5","findings":{"blocker":0,"major":0,…}}`.
5. Shipped; the librarian proposed one learning (`L-0001`, kind rule: *"A
   clamp test must assert an in-range value and a value beyond each bound…"*)
   in a knowledge item, H-0003, which waits in the inbox as "ready?" — it goes
   through review like any change.

### The push gate passes only the hallmarked commit

`hall hook pre-tool-use` with a `git push` of H-0002's branch, from its
worktree:

| HEAD | Result |
| --- | --- |
| `a439e3b` (hallmarked) | no output: allowed |
| `e87f7ed` (one more commit) | `deny`: *"e87f7ed has no hallmark. Work reaches GitHub only through review…"* |

### Usage

`hall usage --by role` for the whole run, list price:

| Role | Tokens |
| --- | --- |
| fixer | 107k |
| reviewer | 48k |
| librarian | 13k |
| intake | 5k |
| **total** | **173k · $0.0107** |

`hall export H-0002` wrote the whole record (timeline, checks, both reviews
with findings and rulings, fixes, usage) to `items/H-0002/export.md`, and the
shipper's PR body carried the review summary, the settled findings and a
pointer to `hall export`.

### Found on the way, and fixed

- Workstream live checks (ADR 0013 addendum): a `tools:` list in an agent
  hides `StructuredOutput`; `--json-schema` rejects a `$schema` key; a bare
  agent name can be replaced by the checkout's `.claude/agents/`; the
  checkout's CLAUDE.md and MCP servers load into `--agent` runs. Every
  headless run is now namespaced (`hallmark:<agent>`) and isolated.
- Headless runs with hooks on counted usage twice (runner + Stop hook);
  `HALL_HEADLESS` now leaves it to the runner.
- Cosmetic: sub-cent costs printed as `$0.00`, and a push without a PR read
  "PR opened"; both fixed.
