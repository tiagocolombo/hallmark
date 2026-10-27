# Hallmark design

The interactive version, with the animated floor plan and the replay, is
[`design.html`](design.html). Where the two differ, this document wins: it
includes the decisions made after the page.

*Agentic development · automated review · a full record*

This document is a condensed version of the design page, plus the decisions made
since. Where they differ, this document wins. The reasons for the larger choices
are in [`decisions/`](decisions/), and the facts they rest on are in
[`spikes.md`](spikes.md).

The name comes from Goldsmiths' Hall in London, where silver was tested and
stamped before it could be sold.

## What it is

Hallmark is a local, agent-first development loop for any git repository, driven
from Claude Code. It works like this:

1. **You start work** with a prompt or a GitHub issue. Each piece of work becomes
   an **item** (`H-0042`) with its own branch and worktree.
2. **Dev agents build it with you** in a normal Claude Code session. Hooks record
   every prompt, turn and commit.
3. **You say it's ready.** Hallmark freezes the commit and queues the item.
4. **Checks run first.** They are the repo's own gates: tests, lint and build.
   A red build never reaches a model.
5. **An independent principal engineer** (the *assayer*) reviews the frozen
   commit. It runs headless, with a fresh context and rules read from the base
   branch.
6. **A fixer agent** addresses the findings. The loop repeats until the reviewer
   approves, or until three rejected reviews escalate the item to you.
7. **The approval is a hallmark**: a git note bound to that exact SHA. Only a
   hallmarked commit can be pushed and opened as a PR. When the PR opens, the
   item closes.

Every step is appended to the item's **journal**, so you can always see what was
rejected, fixed and passed, and why.

**Light and dark.** In Addy Osmani's terms, a *dark* software factory runs a
loop with nobody watching, and a *lit* one keeps a person where a wrong
answer is expensive. Hallmark sets the switch per loop, never for the whole
line. The checks, the review, the fixes, shipping and the Delivery office run
dark: each is judged by a script or the reviewer's rubric, which is cheap,
frequent and hard to game. A person stays in three places: deciding what to
build, any escalation, and merging. Merging goes dark only when you turn on
the merger (`[merge] mode = "auto"`, ADR 0048), and `[merge] lit` keeps chosen
paths, such as CI workflows or release settings, lit.

The product is one Rust binary, `hall` (crate `hallmark`), for macOS, Linux and
Windows:

- With no arguments, it opens the terminal app on a terminal, and prints the
  status as JSON otherwise.
- `hall web` serves the same views, and the settings, as a web app on
  127.0.0.1 whose every call is a hall command (ADR 0052).
- Its subcommands serve agents, Claude Code hooks and the daemon.

## Vocabulary

| Term | Meaning |
| --- | --- |
| `hall` | the command |
| item | one piece of work, from first prompt to PR |
| journal | the item's append-only history |
| assayer | the principal-engineer reviewer, plus the check runner |
| fixer | the agent that addresses findings; it can dispute but never approve |
| hallmark | an approval bound to one commit (`refs/notes/hallmark`) |

## The building

The docs and the terminal app describe Hallmark as a building.

| Room | What happens there | State(s) |
| --- | --- | --- |
| Front Desk | Intake: `hall new`, `hall new --issue`, `hall adopt` | (creation) |
| Workshop | Development; each bench is a worktree | Development |
| Your Office | The inbox, escalations only: questions, "ready?", escalations, holds, CI that never ran | Awaiting you, Escalated |
| Post Room | Every handoff passes here; a copy goes to Records | (all transitions) |
| Records | The journal | Dropped, Closed (history) |
| Library | Learnings distilled from closed items, kept next to the journal | — |
| Assay Office | The test bench (checks) and the principal engineer (review) | Checks, Review |
| Repair Shop | The fixer | Fixing |
| Stamping Room | The hallmark is struck | Hallmarked |
| Dispatch Dock | Rebases onto the latest base; the ship command opens the PR, whose description is What, Why and How; `update` updates an adopted or reopened one. Each review is posted as a PR comment (`review_comment`, ADR 0039). If that PR already merged with the hallmarked work in it, the item closes with nothing pushed | Closed |
| Delivery Office | Watches each shipped PR until it merges; a headless dev run rebases conflicting ones, addresses new review comments (ADR 0030) and fixes red CI (ADR 0031); a rebase that leaves the approved change as it was keeps its approval with no review (ADR 0049); a CI fix whose checks pass on a re-run goes back to its PR without a run (ADR 0047). With `[merge] mode = "auto"`, the merger merges each ready PR, one at a time per base, in dependency order (ADR 0048), and brings each PR up to date only at its turn (ADR 0050). Once the PR merges or closes, the sweeper removes the item's worktree (ADR 0040) | Closed, Development (rebasing, addressing comments) |

A **watchman** patrols the corridor. It flags any item with no new event for
`stuck_after` (30 minutes by default), and a daemon that has stopped.

Only a few rooms use a model: the Workshop, the Assay Office review, the
Repair Shop, the Delivery Office's headless dev runs (a conflict, comments,
red CI), and a little at the Front Desk and in the Library. Everything else,
the merger included, is a script.

## Item states

States are written in snake_case in JSON.

| State | Who acts | Room |
| --- | --- | --- |
| `development` | you and the dev agent | Workshop |
| `awaiting_you` | you | Your Office |
| `checks` | the assayer's gates (a script) | Assay Office |
| `review` | the principal engineer | Assay Office |
| `fixing` | the fixer | Repair Shop |
| `escalated` | you | Your Office |
| `hallmarked` | the shipper | Stamping Room |
| `closed` | nobody; terminal | Dispatch Dock |
| `dropped` | nobody; terminal | Records |

Transitions:

| From | Action | To |
| --- | --- | --- |
| development | `propose-ready` (agent thinks it's done), nothing left to confirm (`auto_ready`) | checks (`proposed_ready`, then `ready`) |
| development | `propose-ready --ask`, or something left to confirm | awaiting_you |
| development, awaiting_you | `ready` (freeze SHA, queue) | checks |
| awaiting_you | `pull-back` ("not yet") | development |
| checks | checks pass | review |
| checks | checks fail | fixing |
| checks | checks fail for the `max_check_failures`-th time in a row (3) | fixing, held for you (`checks`) |
| review | approve (0 blockers, 0 majors) | hallmarked |
| review | changes requested | fixing |
| review | changes requested, `max_rejects` reached (3) | escalated |
| fixing | fix submitted | checks |
| escalated | `retry` (allow one more round) | fixing |
| awaiting_you, checks, review, fixing, escalated, hallmarked | `pull-back` | development |
| hallmarked | shipped (PR opened) | closed |
| hallmarked | the rebase onto the base conflicts, or changes the patch (`pulled_back`; the review then reads only what changed, ADR 0049) | development |
| closed | the PR conflicts, has new review comments or red CI (the Delivery office reopens it), or it is behind its base on files it touches, or conflicts, and its turn in the merge queue has come (the merger reopens it, ADR 0048, ADR 0050; with the merger on, the watch leaves conflicts to it) | development |
| development | reopened for red CI, and the checks passed on a re-run with nothing moved (`restored`, ADR 0047) | closed |
| any non-terminal | `drop --reason` | dropped |

Rules:

- **Closed is the end.** Later PR feedback starts a new follow-up item.
- **`ready` is idempotent.** On an item already in the loop at the same SHA,
  `ready` returns the current result. At a different SHA, it fails with
  `ITEM_IN_REVIEW` (exit 4) and suggests `hall pull-back`.
- **A pending rebase blocks `ready`.** While the Delivery office's rebase
  request stands and the base isn't an ancestor of HEAD (or a rebase is
  half-done), `ready` and `propose-ready` (`--ask` too) refuse with
  `WRONG_STATE` and keep the request, so the conflicting commit never reaches
  review (ADR 0043).
- **A script decides when to stop.** Loops end when the hallmark exists or the
  round limit is reached, never because an agent printed "done". Failed
  checks count too: after `max_check_failures` in a row the item is held, so
  fixes that never turn the gates green don't loop.
- **The state is derived, not stored.** The state machine folds the item's
  events to get its state. `item.json` is only a cache (ADR 0008).

## The journal

```text
$(git rev-parse --git-common-dir)/hallmark/   shared by every worktree, never committed
├─ items/H-0042/
│  ├─ events.jsonl        the journal: append-only, source of truth
│  ├─ item.json           cached snapshot (title, branch, worktree, state)
│  └─ runs/               each headless run, streamed as it happens:
│                         <stamp>-<role>.jsonl, plus .lock, .stderr and prompts
├─ events.jsonl           repo-level events: prompts and notifications from
│                         sessions with no item, gate denials
├─ sessions/<id>.json     session → item binding, transcript byte offset
├─ queue/H-0042.json      tickets waiting for the assayer
├─ checks-cache/<key>.jsonl  every gate run, red or green, by tree, command,
│                         gates and OS; passes are served instead of rerun
├─ counters.json          next item and question numbers
└─ index.sqlite           derived; `hall reindex` rebuilds it
```

- The journal lives in the git common directory, so it outlives worktrees and
  every worktree sees the same journal. So does the Library. A worktree holds
  only the item's checkout, and the sweeper removes it once the item is done
  (ADR 0040).
- It is private, because it includes your prompts, and it is never committed.
- Writes take an exclusive file lock (fs4) and append one line each.
- State changes lock the item's file, fold its events, check the transition and
  append, all as one step.
- The SQLite index (rusqlite, bundled) catches up from the files on read.
  Usage, inbox and history queries use it. Deleting it loses nothing.
- Each headless run with an item streams into `runs/<stamp>-<role>.jsonl`
  as it happens: Claude's stream-json, line by line, then one
  `{"type":"hall_run_end",…}` line from the runner once the process has
  exited, with its duration, exit code and failure (ADR 0041). The result is
  the stream's last `result` line; stderr, when there is any, sits beside it
  as `<stamp>-<role>.stderr`. A fallback-model retry is a run of its own,
  with its own file. Runs are debugging records, like `checks/`, not events.

Every line is one event:

```json
{"v":1,"t":"2026-10-09T10:04:12Z","actor":"you","event":"ready","item":"H-0042","sha":"7c2d018","round":1,"note":"cap at 6 attempts done"}
```

The journal records:

- prompts and turn summaries
- every commit, and which findings it claims to fix
- every check run, with its exit code, duration and failing output, or
  `cached` when a pass on the same tree was reused
- every verdict, with each finding's lens, severity, file, line and scenario
- your decisions, with reasons
- questions and answers
- notifications
- usage for every model run
- the hallmark, the PR link, and the time spent in each state

## The hallmark and the gates

The hallmark is a git note under `refs/notes/hallmark` on the approved SHA. Any
later commit has no note. The note is JSON: the item, the commit, the round, the
reviewer's model, the findings still open, and `actor`, who approved it:
`assayer`, or `you` when a person recorded the verdict with `hall verdict` (a
human override). Notes struck before `actor` existed don't have it. Agents may
not run `hall verdict`, and the fixer's headless run may not run `hall ready`,
`hall retry` or `hall drop` either.

A note on `HEAD` is a hallmark only when Hallmark struck it (ADR 0045), because
anyone with git can write a note. The push gate checks three things:

- The note parses as the hallmark's JSON, version 1.
- Its `sha` is `HEAD` itself, so a note copied from an approved commit doesn't
  count.
- The journal of the item it names records `hallmarked` at `HEAD`.

A denial says which check failed, for example *"its note is for 3f2a9c1, not
e87f7ed"*.

One `PreToolUse` hook, `hall hook pre-tool-use`, enforces two gates. Both apply
only in repos where `hall init` has created `<git-common-dir>/hallmark`
(ADR 0010).

**The push gate.** It denies a push unless `HEAD` has a hallmark: `git push`,
`git send-pack`, `git http-push`, `git subtree push`, `gh pr create`,
`gh pr merge`, a `gh api` write to a PR, ref, merge or contents path, and the
configured ship command.

- Compound shell commands (`&&`, `||`, `;`, `|`, newlines) are split, and each
  part is checked. Heredoc bodies are data, except when a shell reads them.
- Leading `cd …` steps, `VAR=value` assignments and shell keywords are skipped,
  and so are git global options such as `git -C dir push`.
- It sees through wrappers (`env`, `command`, `exec`, `sudo`, `nohup`, `time`,
  `timeout`, `nice`, `ionice`, `caffeinate`, `stdbuf`, `xargs`,
  `nix develop --command`, `direnv exec`), `sh|bash|zsh -c` with any option
  cluster holding `c` (`bash -ec`), `eval`, command substitutions, git aliases
  (`git -c alias.p=push p`, and aliases in git config), and the commands git
  runs itself (`rebase -x`, `bisect run`, `submodule foreach`).
- `[capture] push_guard = "strict"` (the default) covers what it can't read:
  inside an item's worktree, a command that may push but can't be followed (a
  script mentioning `push` or `release`, an unknown git subcommand, a `gh`
  command that changes GitHub, a computed command word) is denied too, unless
  `[capture] push_allow` lists it. `parse` turns this off.

**The note gate.** The hallmark is a git note, so `git notes` writes (`add`,
`append`, `edit`, `copy`, `remove`, `merge`, `prune`) and `git update-ref` on a
notes ref are denied unless `HALL_ACTOR` is `assayer` or `shipper`. Reading
notes (`git notes show`, `list`) passes.

**The edit gate.** It denies an `Edit`, `Write`, `MultiEdit` or `NotebookEdit`
inside the repo but outside an active item's worktree, and also a Bash
`git commit` outside one. The denial's reason is Hallmark's error JSON, which
names what to run: `hall new --title "…"` or `hall adopt`. Claude reads it,
creates the item and carries on. Files outside the repo are never gated.

`[capture] mode` sets how the edit gate behaves:

- `gate` (default): deny.
- `ask`: return `permissionDecision: "ask"`, so you decide.
- `off`: only `/hallmark` starts items.

`[capture] free` (default `["docs/", "*.md"]`) lists paths that need no
item: questions, documents, architecture and requirements aren't development
work (ADR 0030). Edits there, and commits of only those paths, aren't gated.

## How Claude knows to use Hallmark

From most explicit to the one that can't be skipped:

1. **The `/hallmark` skill.** It covers starting work, ready, pull-back and
   status.
2. **The skill description.** It matches natural requests such as "pick up #212"
   or "what's in review?".
3. **The `SessionStart` hook.** It injects the item's context, or how to start
   one.
4. **A short Hallmark section in CLAUDE.md or AGENTS.md.** `hall init` writes it
   between `<!-- hallmark:start -->` and `<!-- hallmark:end -->`.
5. **The edit gate.** This is the guarantee.

## Hooks

Each hook runs `hall hook <event>`, never a shell script, so Windows works.

- `hall setup claude` writes the absolute path of the `hall` binary into the
  installed `hooks.json`.
- Hooks never break a session: on an internal error, they exit 0, print nothing,
  and log the error.
- Outside an initialized repo, they do nothing.

| Event | What Hallmark does |
| --- | --- |
| `SessionStart` | Injects context: the item, its state and `next`, the inbox count, or how to start an item. While the item is in development or waiting for you, it adds what you said since the last agent run (ADR 0044). |
| `UserPromptSubmit` | Records the prompt. If the item is in the review loop, it adds a note that offers `pull-back`. While it is in development or waiting for you, it adds what you said since the last agent run, once (ADR 0044). |
| `PreToolUse` | Runs the edit gate and the push gate. |
| `PostToolUse` | Records commits. It binds the session to the item when `hall new`, `hall adopt` or `hall ask` run, by reading their JSON output. |
| `Stop` | Records the turn summary (`last_assistant_message`). It reads new transcript usage from the stored offset, deduplicated by `requestId`. |
| `Notification` | Files "session waiting" as a notice, under the session's item. |

## The agent contract

Agents are the primary users of `hall`, so the command line is an API.

- **Output.**
  - JSON when stdout isn't a TTY; `--json` forces it.
  - Readable text only on a TTY.
- **Never prompts.**
  - Destructive actions need explicit flags, such as `drop --reason`.
- **Idempotent.**
  - Repeating a command returns the current result.
- **`next`.**
  - Every reply has a `next` list of the commands that make sense now.
- **Errors are data.**
  - The shape is `{"error":{"code":"…","message":"…","next":["…"]}}`.
  - Each error has a documented exit code.
- **Self-describing.**
  - `hall help --json` lists the commands.
  - `hall schema <command|error|event|config>` prints JSON Schema generated
    from the Rust types with schemars.
- **Knows where it is.**
  - Inside an item's worktree, the item ID is optional.
  - IDs accept `42`, `H-42` or `H-0042`.

| Exit | Meaning | Error codes |
| --- | --- | --- |
| 0 | done | — |
| 1 | internal error | `INTERNAL`, `COMMAND_FAILED`, `GH_UNAVAILABLE`, `RUN_FAILED` |
| 2 | bad arguments | `USAGE`, `INVALID_VERDICT` |
| 3 | not found | `NOT_A_REPO`, `NOT_INITIALIZED`, `ITEM_NOT_FOUND`, `NO_ITEM`, `QUESTION_NOT_FOUND` |
| 4 | wrong state | `WRONG_STATE`, `ITEM_IN_REVIEW`, `DIRTY_WORKTREE`, `ALREADY_ANSWERED`, `CHILDREN_OPEN`, `MERGE_CONFLICT`, `BUDGET_EXCEEDED` |
| 5 | gate denied | `GATE_DENIED` |
| 6 | checks failed | `CHECKS_FAILED` |
| 7 | claimed by another agent | `CLAIMED` |

```console
$ hall ready
{"error":{"code":"ITEM_IN_REVIEW","message":"H-0042 is in review, round 2.",
  "next":["hall pull-back H-0042","hall log H-0042 --kind verdict"]}}
$ echo $?
4
```

## Commands

### Stage 1

| Command | Does |
| --- | --- |
| `hall` | The terminal app on a TTY; JSON status otherwise. |
| `hall init [--defaults] [--capture] [--base] [--no-claude-md]` | Creates the journal directory. Writes `.hallmark.toml` from detected gates, a stub `.hallmark/review.md`, and the CLAUDE.md section. Registers the repo. On a terminal, a repo without `.hallmark.toml` is first asked about every setting, or to skip (ADR 0055); `--defaults` doesn't ask. |
| `hall doctor` | Checks git, gh, claude, the plugin, the hooks and the repo. Each failed check names its fix. |
| `hall setup claude [--dry-run]` | Installs the Claude Code plugin at user level. |
| `hall new [<request>] [--title] [--issue <n>] [--open] [--branch]` | Creates an item, its branch and its worktree. `--open` starts Claude there. |
| `hall adopt [--title] [--pr] [--base]` | Turns the current branch and worktree, or an open PR (`--pr`, Hallmark's or not), into an item that updates that PR. On the branch of a shipped item whose PR is still open, reopens the item (`closed → development`). `--base` stacks it on another branch (ADR 0029). |
| `hall status [<id>] [--all]` | Lists items by state, or shows one item in detail. |
| `hall log [<id>] [--kind] [--limit]` | Prints the item's journal. |
| `hall inbox` | Lists the escalations waiting for you, oldest first. `<n> --dismiss` hides one entry and `--clear` hides them all, until each changes; `--all` shows them. `--notices` lists what the loop handles on its own. |
| `hall ready [<id>] [--note]` | Freezes HEAD and queues the item. A dirty worktree is refused, and so is a branch the Delivery office was asked to rebase that isn't on its base yet (`WRONG_STATE`, ADR 0043). |
| `hall pull-back [<id>] [--reason]` | Returns the item to development. |
| `hall drop [<id>] --reason [--keep-worktree]` | Abandons the item. The branch and journal stay. |
| `hall retry [<id>] --reason [--tokens <n>]` | Allows an escalated item one more round, or releases a held item. A budget hold also grants that item another `per_item` (or `--tokens`); the budget is unchanged. |
| `hall propose-ready [<id>] --summary [--why] [--how] [--ask]` | The agent says the work is done: straight to review when nothing is left to confirm, else (or with `--ask`) it waits for you. Refused like `ready` while a rebase is pending and the branch isn't on its base (ADR 0043). |
| `hall ask <q> [--item] [--options] [--default] [--wait] [--session]` | Records a question. |
| `hall answer <qid> <answer>` | Records the answer; the daemon resumes the asking session with it. |
| `hall usage [--by item\|room\|role\|model\|day] [--since] [--item]` | Shows tokens and list-price cost. |
| `hall reindex` | Rebuilds the SQLite index from the journals. |
| `hall help --json`, `hall schema <cmd>` | Self-description for agents. |
| `hall hook <event>` | The entry point for Claude Code hooks. |

### Stages 2 and 3

| Command | Used by | Does |
| --- | --- | --- |
| `hall new … [--no-split] [--parent] [--follow-up <pr>]` | you, your session | The Front Desk may split a request into parallel parts; `--follow-up` starts from PR feedback. |
| `hall merge [<child>]` | your session | Merges a parallel part into its parent's branch and closes it. |
| `hall daemon start\|stop\|status\|run [--at-login]` | you | The review loop for every registered repo (ADR 0014, 0017). |
| `hall next --role assayer\|fixer\|shipper` | agents | Claims work with a lease (exit 7 `CLAIMED`). |
| `hall findings [<id>]` | fixer | Open findings and failing checks. |
| `hall fixed <ids> [--dispute <id> --reason]` | fixer | Records what the commit fixes, resubmits. |
| `hall verdict --file <json>` | assayer, you | Records a schema-checked verdict. Refused to agents (`GATE_DENIED`); yours is a human override, named as one in the report, the note and the PR comment. |
| `hall history [--rejected] [--lens] [--since]` | you | Closed and dropped items, and why things were rejected. |
| `hall export [<id>]` | you, shipper | The whole record as Markdown, with what each model run did, step by step. |
| `hall budget [--set k=v] [--grant [<tokens>]]` | you | The caps and today's spend. `--grant` lets the loop go past the daily cap for today (default: another `daily`) and lifts the budget pause; the cap is unchanged. |
| `hall notify test` | you | A test notification on every channel. |
| `hall web [--port] [--no-open]` | you | The web app on 127.0.0.1, behind a token: every call runs a hall command a person may run from a page (ADR 0052). |
| `hall statusline` | Claude Code | One line for the status line. |
| `hall library search\|review\|retire\|distill\|import\|export` | you, agents | The Library. |

## Configuration

`.hallmark.toml` at the repo root is optional. Every key has a default, and
nothing repo-specific is hardcoded. `hall` reads it from the main checkout, so
an item can't loosen its own gate from its worktree (ADR 0012).

```toml
base        = "main"            # hall init detects origin/HEAD
worktree    = "git worktree add ../{repo}-{id} -b {branch}"
branch      = "hm/{id}-{slug}"
ship        = "git push -u origin {branch} && gh pr create --base {base} --head {branch} --title {title} --body-file {body_file}"
review_comment = "gh pr comment {pr} --body-file {review_file}"   # each review is a PR comment
gates       = []                # hall init fills these
gates_no_cache = []             # gates the checks cache never serves (network tests)
dev_commands = []               # what headless dev runs may run besides the gates
fixer       = "claude -p"       # or an external fix engine (Ralph Orchestrator…)
claude_args = []                # extra arguments for every headless run
max_rejects = 3
max_check_failures = 3          # failed checks in a row before a `checks` hold
stuck_after = "30m"
lenses      = "all"
rules       = ["CLAUDE.md", "AGENTS.md"]

[capture]
mode = "gate"                   # gate | ask | off
free = ["docs/", "*.md"]        # paths that need no item
push_guard = "strict"           # or "parse": deny unreadable pushes in worktrees
push_allow = []                 # commands the strict guard lets through

[merge]                         # the merger (ADR 0048)
mode       = "off"              # or "auto"
command    = "gh pr merge {pr} --squash --match-head-commit {pr_head}"
require_ci = true
lit        = []                 # paths a person merges

[intake]
split     = "auto"              # the Front Desk splits requests when it can; or "off"
max_parts = 4

[budget]                        # runs are capped by default too (per-run max_usd)
daily           = "4M"          # default caps; "none" turns one off
per_item        = "800k"
review_model    = "opus"
fix_model       = "sonnet"
intake_model    = "haiku"
librarian_model = "haiku"
# [budget.review] max_usd = 3.00, daily = "2M"

[library]
search         = "builtin"      # or "qmd"
briefing_limit = "6k"
auto_accept    = []
retire_after   = "90d"

[extensions]                    # skill packs by other authors (ADR 0038)
# mattpocock = ["planner", "developer", "reviewer", "fixer"]

# [notify] usually lives in <user data dir>/hallmark/config.toml
```

**Templates.**

- The placeholders are `{repo}`, `{id}`, `{branch}`, `{slug}` and `{title}`.
- Commands run through the platform shell: `sh -c`, or `cmd /C` on Windows.
- When a custom worktree command doesn't say where it put the worktree, Hallmark
  finds it with `git worktree list --porcelain` by branch.

**An example override** for a repo with its own tooling:

```toml
worktree = "rig wt new {branch}"
ship     = "rig submit --pr"
gates    = ["rig test", "rig run build"]
```

**Gate detection.** `hall init` detects the gates from the files it finds:

| Found | Gates |
| --- | --- |
| `package.json` scripts | run through npm, pnpm, yarn or bun, chosen by lockfile |
| Cargo | `cargo test`, `cargo clippy` |
| Go | `go test ./...`, `go vet ./...` |
| pytest | `pytest` |
| Makefile | its targets |
| justfile | its recipes |

## The principal engineer (Stage 2)

The role is defined once, in the plugin:

- `agents/principal-engineer.md`: the role, with read-only tools.
- `skills/principal-review/`: how it reviews.
- `schemas/verdict.json`: the exact shape of the verdict.

It runs headless with a fresh context:

```sh
claude -p --plugin-dir "$PLUGIN" --agent principal-engineer \
  --json-schema "$PLUGIN/schemas/verdict.json" \
  --output-format stream-json --verbose \
  --max-budget-usd 6 "Review H-0042 round 2: commit b81f3e4 against main" < /dev/null
```

**Lenses.** Every review applies seven lenses:

| Lens | Asks |
| --- | --- |
| First principles | Is this the simplest change, in the right layer? |
| Correctness | Edge cases, errors, concurrency, idempotency, retries |
| Security & tenancy | Authorization, isolation, secrets, injection |
| Data & migrations | Reversible and safe on live data? |
| Evidence | Do the tests prove it? Did the test count drop? |
| Operability | Can someone on call see it fail and recover? |
| Repo rules | CLAUDE.md, AGENTS.md and the rule files they list |

**Severities.**

- There are four: blocker, major, minor and nit.
- Approval means 0 blockers and 0 majors.

**Findings.** Each finding names its lens, file, line, severity, and the
concrete scenario that breaks.

**What the reviewer reads.**

- Its rules and knowledge come from the base branch, never the PR branch, so a
  branch can't soften its own review.
- From round 2, it reads only the diff since the last round, plus the open
  findings.
- A rewrite of the hallmarked commit, such as a rebase, is compared with the
  approved change, file by file with its context lines (ADR 0049). The
  same change keeps its approval with no review run: `rebased {carried:
  true}`, a verdict by `carried`, and a note that names the approved commit.
  A difference of up to 100 lines gets a rebase review, which reads only that
  difference, at the effort its size calls for. A larger one gets a full
  review.

**Failures.** A run without `structured_output` counts as failed.

**Isolation** (live checks, ADR 0013 addendum and ADR 0018). Every headless run
calls the plugin's agent by its namespaced name (`hallmark:principal-engineer`),
because a branch's `.claude/agents/` would otherwise replace it. The reviewer,
intake and the librarian run with `--setting-sources user`,
`--strict-mcp-config`, no CLAUDE.md from the checkout
(`CLAUDE_CODE_DISABLE_CLAUDE_MDS=1`) and no hooks. The review prompt carries the
`principal-review` skill and the base branch's rules itself, because a headless
`--agent` run doesn't preload skills.

**Confinement** (ADR 0053, `[sandbox]`). Isolation keeps a run on its task;
confinement keeps it off the host. Every headless run, the resume included,
gets `--settings` from `runner::confinement`:

- its file tools read only inside the run's directories, and never the
  credential paths in `deny_read`;
- its commands run in Claude Code's sandbox (macOS, Linux, WSL2). They write
  only the run's directory, the temp dir and the worktree's git directory,
  never its hooks or config. They reach only the repo's `domains` and
  `unix_sockets`, can't skip `--allowedTools`, and can't retry unsandboxed.

Every run also drops WebFetch, WebSearch and the credentials in `remove_env`.
The fixer and dev runs have no bare `Edit` or `Write` rule, so `acceptEdits`
keeps their edits in the worktree. `claude_args` that would loosen any of
this make `.hallmark.toml` invalid. The Assay Office's own gate runs and a
non-`claude` fixer engine are not confined.

**The fixer.** `agents/fixer.md`:

- runs in the item's worktree;
- commits, and records which findings each commit fixes;
- can dispute a finding with a reason;
- can never approve.

We built our own reviewer rather than use the built-in `/code-review`. The
reasons are in spike 6 and ADR 0007.

## Usage and budgets

Every model run is recorded with its tokens (input, output, cache read and cache
write), turns, duration and list-price cost.

**Sources.**

- **Headless runs.** Usage comes from the `result` line that ends
  `claude -p --output-format stream-json --verbose`.
  - `modelUsage` is the source of truth; the top-level `usage` can be zero or
    partial.
  - `num_turns`, `duration_ms` and `session_id` come from the top level.
- **Interactive sessions.**
  - The `Stop` hook reads the new `message.usage` entries from the session
    transcript.
  - Entries are deduplicated by `requestId`.
  - Cost comes from a list-price table.

**Models by role.**

| Role | Model |
| --- | --- |
| Review | Opus |
| Fix | Sonnet |
| Intake and librarian | Haiku |

**Fewer, larger runs.** Even a trivial headless run loads about 30k tokens of
context. An agent run loads about 4k, because its prompt replaces the default
system prompt.

**Budgets.** There are four levels, plus the plan limit:

| Limit | Set by | When it's reached |
| --- | --- | --- |
| Per run | `max_usd` → `--max-budget-usd` (default: review $6.00, fix $2.00, intake $0.10, librarian $0.20; `"none"` for no cap), plus a timeout | The run stops. The item is retried once, then goes to you. |
| Per item | `per_item` | The item goes to Your Office, marked "budget". |
| Per room per day | `[budget.review] daily`, … | The room closes for the day. Items wait in order. |
| Daily total | `daily` | The daemon pauses. Your session isn't blocked. |
| Plan limit | Claude | The daemon pauses until the limit resets. |

**Views.** `hall usage` totals by item, room, role, model or day.

## The inbox

Your Office holds only escalations, oldest first: what the loop gave up on, or
what only a person can decide (ADR 0031). It is derived from the journal. A
dismissed entry (`hall inbox <n> --dismiss`, `--clear`) stays hidden until it
changes (ADR 0030).

| Kind | Comes from | What waits |
| --- | --- | --- |
| question | `hall ask` without an answer | that item |
| escalated | the item is `escalated` | that item |
| ready? | the item is `awaiting_you` | nothing |
| budget, run_failed, checks, ship_failed, conflict, feedback, ci_failed | a hold: a per-item cap, two failed runs in a row, `max_check_failures` failed checks in a row, a failed ship, a rebase the Delivery office couldn't finish, PR comments or failing CI its dev run didn't act on | that item, until `hall retry` (which, for a budget hold, grants the item more tokens); a `ci_failed` hold also clears when a re-run of the PR's checks passes (ADR 0047) |
| merge_failed, lit | a hold on a shipped item: the merger's merge command failed, or the PR is ready but touches a `[merge] lit` path (ADR 0048) | that item, until `hall retry` (try again; for `lit`, merge this head) or the PR merges or closes |
| ci_failed | an open PR's CI still fails after the Delivery office's fixes | a follow-up item |
| base_red | a failed check on a base's tip (ADR 0051) | that base's merge queue, until its CI is green; a PR that fixes it is merged by hand |
| ci_down | CI jobs that never started (an Actions billing limit, no runners): one entry for every PR | GitHub |
| session waiting | an answer the loop couldn't deliver to the asking session | that session |
| daemon stopped | a daemon that should run has no heartbeat | the loop |

**Notices** (`hall inbox --notices`) are what the loop handles on its own, or
news. They never reach your office and are never notified:

| Kind | Comes from |
| --- | --- |
| ci_failed | an open PR's red CI, while the Delivery office fixes it; a PR merged with red CI |
| stuck | the watchman |
| session waiting | a `Notification` hook, cleared by the next prompt in that session |
| learnings | proposed learnings in the Library |

**Asking a question.** An agent runs:

```sh
hall ask "billed or raw?" --options billed,raw --default billed --wait 4h
```

The answer comes with `hall answer Q-12 a` (or the option's text). The daemon
then resumes the asking agent's session with
`claude -p --resume <session-id> "<answer>"`.

- If a question with a default isn't answered within its wait, the agent goes
  ahead with the default, and the journal says so.
- Escalations never use a default.

**Notifications** (ADR 0020):

- hooks and the status line (`hall statusline`)
- the terminal app (and its bell), and the web app's office (`hall web`)
- the desktop (notify-rust)
- the phone: ntfy by default (a random topic), Telegram, or a Slack webhook.
  Only the item, the question and its options are sent, never code.
- a morning digest, quiet hours for the phone, a reminder every `remind`, and
  routes per inbox kind under `[notify.route]`

## The Library (Stage 2)

**What a learning is.** Learnings are Markdown files in the local store,
`<git-common-dir>/hallmark/library/learnings/`, next to the journal (ADR 0027).
They are never committed: every worktree shares them, and no branch can change
what its review is told. There are four kinds:

- **rule**: comes from a finding that turned out right.
- **false-positive**: what not to flag, and why.
- **map**: what a module does.
- **decision**: your ruling on an escalation.

**How learnings are made and used.**

1. A Haiku librarian distils closed items into learnings.
2. Each review receives a briefing within a token cap. The briefing combines
   learnings scoped to the changed paths, which come first, with a search of
   the rest. The cap follows the review's effort: 2k tokens at `low`, 4k at
   `medium`, and `briefing_limit` (6k) otherwise, never more than
   `briefing_limit`.
3. Each verdict says which learnings it used. Unused or contradicted learnings
   are retired; `history/` keeps every earlier version.

**Approval is yours, locally.** Learnings of kinds outside `auto_accept` wait
as `proposed`, one notice (`hall inbox --notices`). `hall library review --accept` or
`--reject` settles them at once; rejecting an update restores what it replaced.
`hall library import` brings in learnings committed under `.hallmark/knowledge/`
by earlier versions, and `hall library export` writes the store to one file.

**Search.** Built-in search uses SQLite FTS5. tobi/qmd is an optional backend,
with one collection per repo.

**Where rules belong.** CLAUDE.md is for rules that are always true; situational
knowledge goes in the Library.

**The Gazette** (ADR 0053). The librarian's run also writes the words for the
item's Gazette, a newspaper page for people at `items/<id>/gazette.html` in
the journal: a headline, a story, a line per round, each finding in plain
words, and a diagram outline when the change has a flow worth drawing.
Hallmark sets every fact itself from the journal and lays the diagram out as
SVG. The page is one file, with no scripts and nothing from the network.
Files and links the dev agent attaches with `hall attach` become plates. The
page is reprinted from code, with no model run, when the item moves on (its
PR merges, CI reports) or a new hall prints differently. `hall gazette`, the
terminal app (<kbd>n</kbd>) and the web app's Gazette tab read it. Like the
Library, it is never committed.

## Parallel parts (Stage 2, ADR 0024)

The Front Desk's intake run (Haiku, `plugin/agents/intake.md`,
`schemas/plan.json`) decides whether a request splits into parts that are truly
independent: disjoint paths, no ordering, each testable alone. Hallmark checks
the plan (2 to `max_parts` parts, no overlapping paths) and fails open: any
doubt makes one item.

- A split makes a **parent** item and one **child** item per part, each with its
  own branch (cut from the parent's) and worktree.
- **Your session's Claude orchestrates**: the skill tells it to run one subagent
  per child in parallel, each confined to its worktree.
- `hall merge <child>` merges a finished part into the parent's branch and closes
  it (`merged`); conflicts abort the merge and say how to resolve them.
- `hall ready <parent>` is refused (`CHILDREN_OPEN`) while a part is open, and
  `hall ready <child>` is refused: the principal engineer reviews **once**, the
  integrated parent. The daemon never takes child items.

## The daemon (Stage 2, ADR 0014, 0017)

One process (`hall daemon start`, optionally at login) runs passes over every
registered repo; every repo, worker and slot is scheduled on its own (ADR
0026): watchman, resume, assayer, fixer, shipper, delivery, delivery-watch,
merger, ci, sweeper, librarian, notify. Leases
keep one worker per item and count per phase against `[parallel]` (checks,
review, fix, ship), not one assayer per repo. A rate limit or the daily budget
pauses the model workers until the reset, recorded as `paused`. Stopping is a
flag in the state file, so it works the same on every OS. On Windows a
daemon started by `hall daemon start` has no console (the `--at-login` task
gets one visible window from Task Scheduler), so every command it runs
(`git`, `gh`, `claude`, a gate) is created without a console window of its own
(`process::command`, ADR 0054).

The **merger** (ADR 0048) is off unless `[merge] mode = "auto"`. At most
every 20 seconds it walks each base's open shipped PRs, oldest first, and
judges each with a script, never a model: the shipped head with its hallmark,
`MERGEABLE`, CI green at that head (unless `require_ci = false`), no
standing request for changes, nothing it depends on still open (the PR it is
stacked on; `Depends on #N`, `Blocked by #N`, `After #N`), no `lit` path, and
fresh (the base is in it, or the base's new commits touch none of its files;
with `update = "conflicts"`, `MERGEABLE` is enough, ADR 0051).
The first ready PR merges through `[merge] command` (`pr_merged`). A PR
behind on its own files (with `update = "files"`, the default), or
`CONFLICTING`, is reopened with a rebase request
only at its turn (ADR 0050): walking the queue, every PR still on its way to
a merge claims its files (one waiting for another PR that claims too), and
so does every item already reopened and being brought back, against the PRs
newer than it (an older PR merges first, since the reopened one is rebased
onto it anyway); a PR that touches a claimed file waits (`waits_for`). So
each PR is rebased once, after the earlier PRs it overlaps with have merged,
and PRs on separate files move in parallel lanes. A PR waiting on a person
claims nothing, and neither does one waiting for it, or one whose CI the CI
watch gave up on. A conflicting PR is rebased at its turn even when its CI
hasn't reported, since GitHub runs no pull request CI on it, and a `lit` PR
is put to you only once nothing claims its files. With the merger on, the
delivery watch leaves conflicts to the queue. After a merge, nothing else on
that base merges until the watch records `delivered`. The merger stops the
line: when a check failed on a base's tip, nothing merges into it or is
rebased onto it until it is green again, and your office shows `base_red`
(ADR 0051). A merge also waits for the base tip's checks to finish (a new
tip with no runs yet counts as running for five minutes; a run waiting on a
person doesn't; six hours is the limit), so with `files` too, PRs on
separate files merge a full base CI run apart. A failed command holds the item (`merge_failed`); a `lit` path
holds it for you (`lit`), and `hall retry` lets that head merge.
`hall queue` shows the same judgement for every PR. Each pass also keeps its
whole plan in `merger.json`, which the terminal app shows on `m` while the
merger is on: a reader, it never calls GitHub (ADR 0022).

The **sweeper** removes the worktree of each item that is done with it
(ADR 0040): closed, not a part, and its PR merged or closed (`delivered`), or
shipped with no PR to watch. While the PR is open the worktree stays for the
Delivery office. It runs `git worktree remove` without `--force`, so a dirty
worktree is kept and said once in the live log. It never touches the main
checkout, a worktree on another branch, or one a live item still uses. It
records `worktree_removed`, and the branch and journal stay. `hall drop` and
`hall merge` remove their own worktrees, unless `--keep-worktree`.

**What an item is doing now.** `hall status <id>`, `hall watch`, the inbox,
the hooks, the HUD and the terminal app read one derived **now** block: the
item, its lease, the queue, the Live log, the check log, and the item's run
streams (ADR 0041). While the loop holds the item's lease and the latest run
began under it without its `hall_run_end` line, the block shows the run's last
step and offers `hall trace <id> --follow`. A stream without an end line alone
proves nothing, since a daemon that died mid-run never writes one. When an
item is held after failed runs and its last run has ended, the block offers
that run's `claude --resume` command (in the directory it ran in) and
`hall trace <id>`. `hall export` lists every run's steps, so the record
includes what the agents did.

## Condensing command output (ADR 0046)

`src/condense/` is a library that later commands (`hall run` and the gates)
use; nothing calls it yet. It turns a command's output into what an agent
should read: lossless on disk, lossy in context, with a drill-down.

- **Evidence is never condensed.** `git diff`, `git show` and `git log` pass
  through untouched. The list is hard-wired and can only grow.
- **Small output is untouched.** At most 2 KB and 60 lines passes verbatim.
- **What a fixer acts on stays:** the exit code, every failing test or error
  with its file:line, the first message block of each failure, the tool's
  summary line, and distinct warnings once with a count. Progress, ANSI,
  `\r` redraws, "Compiling" lines, passing tests, repeats, downloads and
  timing go.
- **One filter per family**, picked from the command line: `cargo test`,
  `cargo build|check|clippy`, `cargo fmt --check`, pytest, jest and vitest,
  `go test`, and a generic one (the first 20 and last 40 lines, repeats as
  `[xN]`). A family filter that doesn't recognise its output falls back to
  the generic one.
- **Measured honestly:** bytes and lines in and out; tokens saved are shown
  as an estimate (bytes/4) and labelled as one. A footer names the full log
  and how to show a part of it.

Each filter is a pure function over text, pinned by real or real-format
logs in `tests/fixtures/output/` and snapshots in `tests/condense.rs`.

## What we reuse

| Need | Reused |
| --- | --- |
| Skills, agents and hooks | Claude Code's plugin system |
| Every model run | `claude -p` |
| Git and GitHub | `git` and `gh`, as subprocesses |
| Terminal app | ratatui, crossterm |
| Command line | clap |
| Serialization and schemas | serde, schemars |
| File watching and locks | notify, fs4 |
| Index | rusqlite |
| Daemon and notifications | service-manager, notify-rust |
| Releases | dist (cargo-dist), a Nix package |

**Extensions** (ADR 0038) bring in skills by other authors, embedded
unchanged at a pinned commit with their license and credit, and turned on
per role in `[extensions]`. The first is Matt Pocock's skills (MIT):
`to-tickets` for the Front Desk, `tdd`, `diagnosing-bugs` and
`codebase-design` for development sessions, `code-review` for the reviewer,
`tdd` and `diagnosing-bugs` for the fixer. Headless roles get the skill in
full with a note on how it fits and a rule for running without a person,
and only when its trigger holds: the fixer reads `diagnosing-bugs` when a
gate failed and `tdd` when an open finding is about correctness or
evidence. Sessions get an index and `hall extensions show`.

Two outside tools fit at the edges:

- **Ralph Orchestrator** (MIT): only as an optional external fixer engine.
- **Claude Squad** (AGPL): only side by side, through `hall adopt`.

## Build stages

1. **Journal and hooks** (done)
   - the binary and the agent contract
   - items and the state machine
   - the journal and its index
   - the config
   - the Stage 1 commands
   - the hooks and both gates
   - the plugin skeleton with the `hallmark` skill
   - integration tests
   - CI on macOS, Linux and Windows
2. **The review loop** (done)
   - the daemon: checks, review, fixer, shipper and watchman
   - the principal-engineer and fixer agents, the verdict schema, and hallmarks
   - the Front Desk's intake plan and parallel parts
   - issue linking and follow-up items
   - budgets at four levels, resuming on an answer
   - the Library and the librarian
   - desktop and phone notifications
3. **The terminal app and releases** (done; releases are dry-run only)
   - views: board, journal, findings, history, live, usage and repos, and
     a run's trace on `t`, followed live (ADR 0041), and the merge queue on
     `m` while the merger is on
   - qmd as an option
   - dist releases and a Nix package
4. **The web app** (done, ADR 0052)
   - `hall web`: the terminal app's views and every setting in a browser,
     each action a hall command; updates and restarts itself
