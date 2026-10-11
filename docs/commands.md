# Command reference

`hall` is one binary. Run it with no arguments on a terminal to open the
terminal app. Everything else is a subcommand. `hall <command> --help` shows
every flag, `hall help --json` lists the commands for agents, and
`hall schema <name>` prints the JSON Schema of a command's output.

## The agent contract

Agents are the main users of `hall`, so every command follows these rules
([ADR 0002](decisions/0002-agent-first-cli-contract.md)):

- **Output.** JSON when stdout isn't a TTY; `--json` forces it.
- **No prompts.** `hall` never prompts. Destructive actions need explicit
  flags, such as `--reason`.
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

## Setup

| Command | Does |
| --- | --- |
| `hall setup` | One-time setup: the Claude Code plugin and status line, this repo, the daemon at login, a phone topic, then `hall doctor`. Safe to repeat. `--dry-run`, `--no-statusline`, `--no-daemon`, `--no-notify`, `--force` |
| `hall setup claude` | Installs only the Claude Code plugin. `--statusline`, `--dry-run` |
| `hall init` | Sets up a repo. `--capture gate\|ask\|off`, `--base`, `--no-claude-md` |
| `hall doctor` | Checks the install. Each failed check names the command that fixes it. |
| `hall update` | Installs the latest `hall` from main, or says it's already current. `--check`, `--source`, `--branch`, `--no-setup` |

## Items

| Command | Does |
| --- | --- |
| `hall new [<request>]` | Creates an item and its worktree; splits into parallel parts when it can. `--title`, `--issue <n>`, `--no-split`, `--parent <id>`, `--follow-up <pr>`, `--open`, `--branch` |
| `hall adopt [--pr] [--base]` | Turns the current branch and worktree, or an open PR, into an item. A shipped item whose PR is still open is reopened: rebase it, resolve conflicts or change it, and the shipper updates the PR after review ([ADR 0029](decisions/0029-adopt-open-prs.md)). |
| `hall merge [<child>]` | Merges a parallel part into its parent's branch and closes it. |
| `hall ready [<id>]` | Freezes the commit and queues the item for review. |
| `hall propose-ready [<id>] --summary <s> [--ask]` | The agent says the work is done. With nothing left to confirm (no open question, a clean worktree, no open parts) it goes straight to review; `--ask` (or `auto_ready = false`) waits for your call. |
| `hall pull-back [<id>]` | Returns the item to development. |
| `hall drop [<id>] --reason <r>` | Abandons the item. The branch and journal stay. `--keep-worktree` |
| `hall retry [<id>] --reason <r> [--tokens <n>]` | Allows an escalated item one more round, or releases a held item. A budget hold also grants that item another `per_item` (or `--tokens`); the budget is unchanged. |
| `hall continue [<id>]` | Moves an item on from wherever it waits: releases a hold, retries an escalation, sends "ready?" to review, or reopens its Claude session. |

## Seeing what's happening

| Command | Does |
| --- | --- |
| `hall` | Opens the terminal app on a TTY; prints the status as JSON otherwise. |
| `hall status [<id>]` | Lists items by state, or shows one item with a **now** block: queued (and what is ahead), the gate running, the reviewer's run, the fixer, a hold, and what you can do. `--all` includes closed and dropped items. |
| `hall log [<id>]` | Prints the journal. `--kind <event>`, `--limit <n>` |
| `hall watch [<id>]` | Follows an item live until it leaves the loop or needs you. |
| `hall history` | Closed and dropped items, and why things were rejected. `--rejected`, `--lens`, `--since`, `--all` |
| `hall export [<id>]` | The item's whole record as Markdown. `--out` |
| `hall usage` | Tokens and list-price cost. `--by item\|room\|role\|model\|day`, `--since`, `--item` |
| `hall statusline` | One line for Claude Code's status line. |
| `hall hud` | Everything the Claude Code heads-up display shows, in one call. `--session`, `--lines` |

## Your inbox

| Command | Does |
| --- | --- |
| `hall inbox [<n>]` | Everything waiting for you: questions, "ready?", escalations, holds, stuck items, waiting sessions. `hall inbox 1` explains one: why it waits, the evidence, and each action. `hall inbox 1 --dismiss` hides one, `--clear` hides them all (each comes back if it changes), `--all` shows them. |
| `hall ask <question>` | Asks you a question. `--options a,b`, `--default`, `--wait 4h`, `--item`, `--session` |
| `hall answer <qid> <answer>` | Answers; the daemon resumes the asking session with it. |

## The daemon, budgets and notifications

| Command | Does |
| --- | --- |
| `hall daemon start\|stop\|status\|run` | The review loop. `start --at-login`; `run --once --only <workers>` |
| `hall daemon resume` | Lifts a pause (a rate limit or the daily budget) before its time. |
| `hall budget` | The caps at four levels, with today's spend. `--set key=value`; `--grant [<tokens>]` lets the loop go past the daily cap today without changing it ([ADR 0028](decisions/0028-grant-the-daily-budget.md)) |
| `hall notify test` | Sends a test notification to every channel. `--channel` |
| `hall library search\|review\|retire\|distill\|import\|export` | The Library of learnings, kept locally next to the journal, never in the repo. |

## For agents and hooks

| Command | Does |
| --- | --- |
| `hall next --role assayer\|fixer\|shipper` | Claims work with a lease. |
| `hall findings [<id>]` | Open findings and failing checks (the fixer). |
| `hall fixed <ids>` | Records what a commit fixes, or `--dispute <id> --reason` (the fixer). |
| `hall verdict --file <json>` | Records a schema-checked verdict (the assayer). |
| `hall reindex` | Rebuilds the SQLite index from the journals. |
| `hall help --json` | Lists the commands. |
| `hall schema <name>` | Prints a JSON Schema: any command's output, `error`, `event`, `config` or `verdict-input`. |
| `hall hook <event>` | The entry point for Claude Code hooks. |
