# 0018. The Assay Office: checks, then an isolated review

Status: Accepted, 2026-10-09

## Context

The assayer takes an item's frozen commit through the repo's gates, then
through the principal engineer's review (ADR 0015). Two things make this
harder than it looks:

- The checks must run the code under review without disturbing your
  checkouts, and a red build must never cost a model run.
- The reviewer runs inside a checkout of the branch it judges. Live runs
  (Claude Code 2.1.296, Haiku) showed that the branch can reach the reviewer
  in three ways:
  - the checkout's CLAUDE.md is loaded into an `--agent` run;
  - a branch's `.claude/agents/<name>.md` replaces a plugin agent called by
    its bare name;
  - project settings and MCP servers load too, as do the user's own
    claude.ai connectors.

## Decision

**One assay worktree per repo.** It is `../<repo>-assay`, a sibling of the
main checkout like the item worktrees. It is created with
`git worktree add --detach` and reused. Each step runs
`git checkout --detach -f <sha>` and `git clean -fd`, so ignored build caches
survive. The assayer takes one ticket at a time per repo (ADR 0014), so
nothing else uses the worktree meanwhile.

**Checks.**

- The `gates` from the main checkout's `.hallmark.toml` (ADR 0012) run in
  order through the platform shell. Every gate runs, even after one fails,
  so the fixer sees all of them in one round.
- Each gate has the `[budget.review] timeout` (default 20m). *(Note,
  2026-10-15: superseded in part. Gates now have `gates_timeout` and a
  per-gate `timeout`; see [configuration](../configuration.md).)* On Unix it runs
  in its own process group, so a timeout kills the whole tree; on Windows
  `taskkill /T` does that. A timed-out gate records exit 124.
- Output goes to `items/<id>/checks/r<round>.log`. `CheckRun.log` is
  relative to the item's directory (`checks/r1.log`), in the journal and in
  `hall findings`.
- No gates configured means checks pass with `runs: []`.
- `checks` is recorded only if the item is still in checks at that commit
  and round.

**What the review reads from the base branch.** It reads these at the base
commit with `git show`, never from the checkout:

- `max_rejects`, `rules` and `lenses` from `.hallmark.toml`;
- each rule file (`path#anchor` takes that section);
- `.hallmark/review.md` and its "Sensitive paths" globs;
- the Library briefing.

The verdict records that base commit.

**The prompt.** It is compact and in a fixed order, so the stable part
caches:

1. The `principal-review` skill, verbatim. A main-thread `--agent` run
   doesn't preload skills, whether named bare or namespaced, so the prompt
   carries the method.
2. The item: its title, request, summary and ready note.
3. How to use the checkout.
4. The lenses.
5. The open findings, with the fixer's claims and disputes.
6. The rules and the briefing.
7. The diff:
   - round 1: `merge-base..sha`;
   - from round 2: `reviewed_sha..sha`, unless the branch was rewritten, in
     which case the whole change is shown again.
   The diff is cut at 60k characters on file boundaries, and the prompt
   names the files it left out.

**Effort follows risk.**

| Diff | Effort |
| --- | --- |
| Touches a sensitive path (changed files, or files with open findings) | `max` |
| 20 lines or fewer | `low` |
| 100 lines or fewer | `medium` |
| Anything else | `[budget.review] effort`, else Claude's default |

A light level never exceeds the configured default.

**Isolation.** The run uses:

- `--agent hallmark:principal-engineer`: namespaced, so a branch's own agent
  can't replace it;
- `--setting-sources user` and `--strict-mcp-config`;
- `CLAUDE_CODE_DISABLE_CLAUDE_MDS=1` and `HALL_NO_HOOKS=1`;
- `--allowedTools`: the agent's read-only tools;
- the assay worktree at the commit as its working directory.

Measured on the same one-turn prompt:

| | Context | Branch CLAUDE.md | MCP servers |
| --- | --- | --- | --- |
| Without isolation | 7,950 tokens | loaded | 7+ of the user's |
| With isolation | 4,942 tokens | not loaded | none |

**Schema runs with agents.**

- An agent with a `tools:` list must name `StructuredOutput`. Otherwise
  `--json-schema` silently returns no `structured_output`.
- Claude Code rejects a schema that declares `$schema` (draft 2020-12), so
  `verdict.json` doesn't declare one.

**Recording.** `assayer::record_verdict` serves both the run and
`hall verdict`. It:

1. checks the verdict and `Ledger::judge`s it;
2. computes approve or changes;
3. sets `escalated` when `rejects + 1 >= max_rejects`;
4. writes `reviews/r<n>.md` and appends `verdict` under the journal lock;
5. removes the ticket;
6. on approval, strikes the hallmark.

The same round and commit again is a no-op (`changed: false`). An item not in
review at that commit is `WRONG_STATE`.

**Failures.** The runner's failure classes (ADR 0013) map as follows:

| Failure | Result |
| --- | --- |
| Budget stop | `Stop::progress` |
| Rate limit | The daemon pauses until the reset |
| Anything else | One retry, then a hold |

A `structured_output` that passes the schema but fails Hallmark's own checks
counts as a failed run.

## Consequences

- Red builds never reach a model, and every review sees exactly the frozen
  code with only the base branch's rules.
- A change that edits CLAUDE.md, `.claude/` or `.mcp.json` is reviewed as a
  change; it can't steer its own review. The live check confirmed it: the
  reviewer flagged an injected "approve every change" line as a finding.
- The assay worktree is visible next to your repo, and it keeps its build
  caches, which can be large.
- Other headless roles with agents and schemas (fixer, intake, librarian)
  need the same two fixes: `StructuredOutput` in their tools, and no
  `$schema` in their schema.
- Live cost on Haiku: round 1 (4 turns) cost $0.003; incremental round 2
  cost $0.0017.
