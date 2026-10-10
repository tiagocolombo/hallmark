# 0024. Parallel parts: the Front Desk splits, the session orchestrates, the parent is reviewed once

Status: Accepted, 2026-10-09

## Context

A request often holds work that could be built at the same time: two
unrelated fixes, the same change in separate packages. Building it as one
item serialises that work. Splitting it into separate top-level items would
mean several reviews and several PRs for one request. We want the parts built
in parallel and the principal engineer to review once, the integrated whole.

Claude Code can already run subagents in parallel (the Agent tool), so the
session that started the work can orchestrate; Hallmark doesn't need its own
scheduler for this.

## Decision

- **The Front Desk plans** (`src/intake.rs`). `hall new "<request>"` makes one
  `claude -p` run through the runner (ADR 0013): agent `intake` (Haiku,
  `Read`/`Grep`/`Glob` only, `HALL_NO_HOOKS=1`, the 2-minute intake timeout),
  `--json-schema plugin/schemas/plan.json`, cwd the main checkout. The prompt
  carries the request, `intake.max_parts`, and a compact map of the repo (top
  level with file counts, the first 150 tracked files, the base branch's
  `CLAUDE.md`/`AGENTS.md` headings). The agent's prompt
  (`plugin/agents/intake.md`) splits only truly independent work: disjoint
  files, no ordering, each part testable alone.
- **What the live runs taught** (Claude Code 2.1.296, Haiku), pinned by unit
  tests:
  - a schema with `"$schema": ".../draft/2020-12/schema"` is rejected by
    `--json-schema` ("no schema with key or ref"), so `plan.json` has none;
  - an agent whose frontmatter lists `tools:` must include
    `StructuredOutput`, or the model writes the JSON as prose and the run
    has no `structured_output`;
  - Haiku omitted an optional `parts` array, and kept omitting it under an
    `if/then` rule (5 retries, then `error_max_structured_output_retries`).
    `parts` is therefore required, listed second, and described as "always
    present; empty when not splitting".
- **Hallmark checks the plan**, not the model: 2 to `max_parts` parts, each
  with a title, a request and paths; no absolute or `..` paths; and **no two
  parts may overlap**. Paths are compared component-wise, and globs through
  their literal prefix with `globset`, conservatively (when unsure, they
  overlap). Anything else means one item.
- **Intake fails open.** It is skipped when `[intake] split = "off"`, with
  `--no-split`, `--parent` or `--follow-up`, without a request (`--title`
  only), and when no `claude` binary exists (no run can start, so `hall new`
  doesn't wait). A failed run, a budget stop or an invalid plan give one
  item. The reply's `plan` (`ran`, `parallel`, `reason`) always says why.
- **A split creates a parent and children**, all under the repo lock so IDs
  stay sequential: the parent first (as today, with the full request), then
  one child per part (`created` with `parent`, the part's title and request,
  branch from the `branch` template, e.g. `hm/H-0043-<slug>`), then
  `planned { parallel, children, reason }` on the parent. A plan that keeps
  one item still records `planned { parallel: false }` when intake ran.
- **Children start from the parent's branch.** `hall new` fills a new
  `{start}` placeholder in the `worktree` command with the parent's branch.
  The default command (`git worktree add ../{repo}-{id} -b {branch}`) gets
  ` {start}` appended for children; a custom command may use `{start}`, and
  if it doesn't, `hall new` fast-forwards the fresh child to the parent's
  tip. For top-level items `{start}` is empty (the main checkout's HEAD, as
  before). `hall new --parent <id>` adds one child by hand; the parent must
  be a top-level item in development. Parts don't nest.
- **The session's Claude orchestrates.** The `hall new` reply's `next`, the
  skill's "Parallel parts" section and the hook context say: one subagent per
  child in parallel, each confined to its child's worktree; `hall merge
  <child>` as each finishes; `hall ready <parent>` at the end.
- **`hall merge <child>`** requires a child in development or awaiting you
  with a clean worktree, and a parent in development with a clean worktree.
  Under the repo lock it runs `git merge --no-ff <child-branch> -m "Merge
  <id>: <title>"` in the parent's worktree, then records `merged { into, sha }`
  on the child (state machine: → closed) and `child_merged { child, sha }` on
  the parent. A child already contained in the parent's branch (no commits,
  or a crash after the merge) is recorded without a new commit. On a
  conflict, Hallmark runs `git merge --abort`, records nothing, and returns
  `MERGE_CONFLICT` (exit 4) whose `next` says: merge the parent's branch into
  the child in the child's worktree, fix, commit, retry. Afterwards the
  child's worktree is removed (never the main checkout; `--keep-worktree`
  keeps it); its branch stays. Repeating the command returns the recorded
  merge with `changed: false`.
- **The parent waits for its parts.** `hall ready` on a parent with open
  (non-terminal) children fails with `CHILDREN_OPEN` (exit 4) listing `hall
  merge`/`hall drop` for each. `hall ready` on a child fails with
  `WRONG_STATE`: parts are never reviewed alone. `propose-ready` on a child is
  allowed (the orchestrator's "this part looks done"). `hall drop` on a parent
  with open children fails with `CHILDREN_OPEN`; there is no cascade: drop or
  merge the children first, so nothing is thrown away by accident. The
  daemon already skips children (`lease::candidates`).
- **Hooks bind the orchestrator to the parent.** An edit in a child's
  worktree from a session outside it binds the session to the parent, so a
  session running several subagents doesn't flap between children. A session
  started inside a child's worktree works on that child. Commits are still
  journaled to the item whose worktree they're in. The SessionStart context
  of a parent lists its children and states and the flow; UserPromptSubmit
  repeats it while children are open; a child's context says it merges.
- **Usage.** The intake run happens before any item exists, so its usage is
  booked at the repo level, room `front` (`hall usage --by room` shows it).
  The raw result is saved under the new item's `runs/` directory.
- **Follow-ups.** `hall new --follow-up <pr-url|number>` reads the PR with
  `gh pr view --json number,url,title,body,reviews,comments` plus its line
  comments (`gh api repos/<o>/<r>/pulls/<n>/comments`, best effort), builds
  the request from the review feedback, names the item that shipped it when
  the index has one with that `pr_url`, and creates a new top-level item with
  `follow_up` set, titled "Follow-up: <PR title>". Closed stays the end.
  Follow-ups are not split.

## Consequences

- One request, one review, one PR, even when it was built by several agents
  at once. Integration problems show up in the parent's worktree before
  review, where the orchestrator runs the gates on the merged whole.
- Splitting costs one short Haiku run per request; it can be turned off per
  repo (`split = "off"`) or per request (`--no-split`).
- A bad split is cheap to undo: drop the children and keep working in the
  parent. Overlapping work is caught at merge time as `MERGE_CONFLICT`
  without touching the journal.
- Concurrent `hall merge` calls serialise on the repo lock.
