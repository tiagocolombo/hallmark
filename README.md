# Hallmark

*Agentic development · automated review · a full record*

Hallmark is a local, agent-first development loop for any git repository, driven
from Claude Code.

1. You start work with a prompt or a GitHub issue.
2. Dev agents build it with you.
3. When you say it's ready, an independent "principal engineer" agent reviews
   it.
4. A fixer agent addresses the findings, and the loop repeats until the commit
   earns a **hallmark**: an approval bound to that exact SHA.
5. Only hallmarked commits are pushed and opened as PRs.

Every step is written to an append-only journal, so you can always see what was
rejected, fixed and passed, and why.

The crate is `hallmark` and the command is `hall`. It is one Rust binary, for
macOS, Linux and Windows.

> This repository is private for now.

## Status

**Stage 1: the journal and hooks.** This stage includes:

- the `hall` binary and the agent contract
- items and their state machine
- the journal and its SQLite index
- `.hallmark.toml`
- the commands below
- the Claude Code hooks, with the edit and push gates
- the plugin skeleton, with the `hallmark` skill

**Not built yet:**

- the daemon
- the review and fix agents (so nothing strikes a hallmark yet)
- the terminal app
- the Library
- phone notifications
- releases

See [`docs/DESIGN.md`](docs/DESIGN.md) for the whole design.

## How it works

The full design is an interactive page:
[`docs/design.html`](docs/design.html). It includes an animated floor plan, the
state map, a step-by-step replay of one item, and mockups of the inbox and
usage views. GitHub shows it as source, so open it from a clone
(`open docs/design.html` on macOS, `xdg-open` on Linux, `start` on Windows).

Hallmark is laid out like a building, named after Goldsmiths' Hall in London,
where silver was tested and stamped before it could be sold. Each piece of work
is an **item**, and it moves from room to room:

| Room | What happens there | Who works there |
| --- | --- | --- |
| Front Desk | A request becomes an item, with a branch and a worktree | `hall new`, `hall adopt` |
| Workshop | You and the dev agent build it; each bench is a worktree | your Claude Code session |
| Your Office | Questions, "looks done?", escalations: your inbox | you (`hall inbox`) |
| Post Room | Every handoff passes through and is recorded | the `hall` CLI, the watchman |
| Records | The journal: every prompt, commit, check, verdict and decision | `hall log` |
| Library | What closed items taught, briefed to each review | the librarian *(Stage 2)* |
| Assay Office | The checks, then the principal engineer's review | the assayer *(Stage 2)* |
| Repair Shop | A fixer addresses failing checks and findings | the fixer *(Stage 2)* |
| Stamping Room | The approved commit gets its hallmark, a git note | the assayer *(Stage 2)* |
| Dispatch Dock | The ship command opens the PR, and the item closes | the shipper *(Stage 2)* |

An item's states:

```mermaid
stateDiagram-v2
    direction LR
    [*] --> development: hall new / adopt
    development --> awaiting_you: agent - hall propose-ready
    awaiting_you --> development: not yet (hall pull-back)
    development --> checks: hall ready
    awaiting_you --> checks: hall ready
    state "review loop (runs on its own)" as loop {
        checks --> review: checks pass
        checks --> fixing: checks fail
        review --> fixing: changes requested
        fixing --> checks: fix committed
    }
    review --> escalated: 3rd rejection
    escalated --> fixing: hall retry
    review --> hallmarked: approved
    hallmarked --> closed: ship command opens the PR
    loop --> development: hall pull-back
    escalated --> development: hall pull-back
    development --> dropped: hall drop --reason
    closed --> [*]
    dropped --> [*]
```

Any live item can be dropped with a reason. Closing ends an item's life: later
PR feedback starts a new item.

One item's life, from the replay in the design page:

1. **09:12** You type `/hallmark Retry failed webhook deliveries`. Claude runs
   `hall new`, which creates H-0042 with its own branch and worktree.
2. **09:14–09:50** You and the agent iterate. Hooks record each prompt, turn
   and commit.
3. **09:51** The agent thinks it's done and runs `hall propose-ready`. You say
   "not yet: cap it at 6 attempts".
4. **10:04** You say ready. `hall ready` freezes the commit and queues it.
5. **10:09** The checks fail, so no review tokens are spent. The fixer adds tests.
6. **10:24** The principal engineer finds a blocker and a major. The fixer
   addresses both.
7. **10:46** The review approves, and the commit gets its hallmark.
8. **10:47** The shipper opens PR #191 and the item closes. The whole record
   stays in the journal.

Steps 1–4 work today. Steps 5–8 are Stage 2: until then `hall ready` queues the
item, and the push gate denies every push, because nothing writes hallmarks yet.

## Install

From source, with Rust 1.88 or newer:

```sh
cargo install --path .
```

With Nix, `nix develop` (or direnv) gives you the toolchain declared in
`dev.toml`.

## Setup

```sh
hall setup claude    # once per machine: installs the Claude Code plugin at user level
cd your-repo
hall init            # once per repo: journal, .hallmark.toml, CLAUDE.md section
git add .hallmark.toml .hallmark/ CLAUDE.md && git commit -m "Adopt Hallmark"
hall doctor          # checks everything and names the fix for each problem
```

**What `hall init` writes:**

- `.hallmark.toml`, with the gates it detects from package.json scripts, Cargo,
  Go, pytest, a Makefile or a justfile
- a stub `.hallmark/review.md`
- a short Hallmark section in CLAUDE.md or AGENTS.md

**What `hall setup claude` installs:**

- the `hallmark` skill
- hooks that call `hall hook <event>`

## Using it

You don't have to mention Hallmark to Claude.

- The skill picks up requests like "pick up #212" or "what's in review?".
- A `SessionStart` hook tells Claude whether it's in an item.
- The edit gate denies the first edit outside an item's worktree, with a reply
  that says to run `hall new`.

From the terminal:

```sh
hall new "Retry failed webhook deliveries" --open   # item, branch, worktree; opens Claude there
hall new --issue 212                                # start from a GitHub issue
hall adopt                                          # turn the current branch into an item

hall status                 # items by state
hall log H-0042             # the item's journal
hall inbox                  # everything waiting for you

hall ready H-0042           # freeze HEAD and queue it for review
hall pull-back H-0042       # back to development
hall drop H-0041 --reason "superseded by H-0044"
hall usage --by model --since 7d
```

On a terminal these commands print readable text. When an agent runs them, they
print JSON.

## The agent contract

Agents are the main users of `hall`, so every command follows these rules:

- **Output.** JSON when stdout isn't a TTY; `--json` forces it.
- **No prompts.** `hall` never prompts. Destructive actions need explicit flags,
  such as `--reason`.
- **Idempotent.** Repeating a command returns the current result.
- **`next`.** Every reply includes `next`: the commands that make sense now.
- **Errors.** They are data, with a documented exit code.
- **Self-describing.** `hall help --json` lists the commands, and
  `hall schema <command>` prints the output shape.
- **Location-aware.** Inside an item's worktree, the item ID is optional.

```json
{"error":{"code":"ITEM_IN_REVIEW","message":"H-0042 is in review, round 2.","next":["hall pull-back H-0042"]}}
```

| Exit | Meaning |
| --- | --- |
| 0 | done |
| 1 | internal error |
| 2 | bad arguments |
| 3 | not found |
| 4 | not allowed in the item's current state |
| 5 | denied by the gate |
| 6 | checks failed |
| 7 | claimed by another agent |

## Commands

| Command | Does |
| --- | --- |
| `hall` | Prints the status. The terminal app arrives in Stage 3. |
| `hall init` | Sets up a repo. `--capture gate\|ask\|off`, `--base`, `--no-claude-md` |
| `hall doctor` | Checks the install. |
| `hall setup claude` | Installs the Claude Code plugin. `--dry-run` shows what it would do. |
| `hall new [<request>]` | Creates an item and its worktree. `--title`, `--issue <n>`, `--open`, `--branch` |
| `hall adopt` | Turns the current branch and worktree into an item. |
| `hall status [<id>]` | Lists items by state, or shows one item. `--all` includes closed and dropped items. |
| `hall log [<id>]` | Prints the journal. `--kind <event>`, `--limit <n>` |
| `hall inbox` | Lists questions, "ready?", escalations and waiting sessions. |
| `hall ready [<id>]` | Freezes the commit and queues the item for review. |
| `hall pull-back [<id>]` | Returns the item to development. |
| `hall drop [<id>] --reason <r>` | Abandons the item. The branch and journal stay. `--keep-worktree` |
| `hall retry [<id>] --reason <r>` | Allows an escalated item one more round. |
| `hall propose-ready [<id>] --summary <s>` | The agent says the work looks done. |
| `hall ask <question>` | Records a question. `--options a,b`, `--default`, `--wait 4h`, `--item`, `--session` |
| `hall answer <qid> <answer>` | Records the answer to a question. |
| `hall usage` | Shows tokens and list-price cost. `--by item\|room\|role\|model\|day`, `--since`, `--item` |
| `hall reindex` | Rebuilds the SQLite index from the journals. |
| `hall help --json` | Lists the commands, for agents. |
| `hall schema <name>` | Prints a JSON Schema: any command's output, `error`, `event` or `config`. |
| `hall hook <event>` | The entry point for Claude Code hooks. |

## Configuration

`.hallmark.toml` is optional, and `hall` reads it from the main checkout, so an
item can't change its own gate from its worktree. These are the defaults:

```toml
base        = "main"
worktree    = "git worktree add ../{repo}-{id} -b {branch}"
branch      = "hm/{id}-{slug}"
ship        = "git push -u origin {branch} && gh pr create --fill"
gates       = []
max_rejects = 3
rules       = ["CLAUDE.md", "AGENTS.md"]

[capture]
mode = "gate"   # gate: edits need an item · ask: Claude asks you · off

[budget]
review_model = "opus"
fix_model    = "sonnet"
intake_model = "haiku"
```

A repo with its own tooling overrides only the commands:

```toml
worktree = "dev wt new {branch}"
ship     = "dev submit --pr"
gates    = ["dev test", "dev run build"]
```

## Where things live

- **The journal** is append-only JSONL at
  `$(git rev-parse --git-common-dir)/hallmark/items/<id>/events.jsonl`.
  - Every worktree shares it.
  - It is never committed, because it includes your prompts.
  - A SQLite index next to it can be rebuilt at any time with `hall reindex`.
- **The hallmark** is a git note under `refs/notes/hallmark` on the approved
  commit.

## Documentation

- [`docs/design.html`](docs/design.html): the interactive design page.
- [`docs/DESIGN.md`](docs/DESIGN.md): the design, condensed, with the decisions.
- [`docs/decisions/`](docs/decisions/): architecture decision records.
- [`docs/spikes.md`](docs/spikes.md): Step 0, which checked how Claude Code
  actually behaves.
- [`CLAUDE.md`](CLAUDE.md): build, test and lint commands for contributors.
