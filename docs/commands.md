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
| `hall setup` | One-time setup: the Claude Code plugin and status line, this repo, the daemon at login, a phone topic, then `hall doctor`. Safe to repeat. `--interactive` (`-i`) asks about every setting first, on a terminal. `--dry-run`, `--no-statusline`, `--no-daemon`, `--no-notify`, `--force` |
| `hall config` | Reads and changes settings without editing TOML: `list [prefix]`, `get <key>`, `set <key> <value>…`, `unset <key>`. `--user` for your user config (`tools.*`, notifications). Keeps comments and checks values. |
| `hall setup claude` | Installs only the Claude Code plugin. `--statusline`, `--dry-run` |
| `hall init` | Sets up a repo. `--capture gate\|ask\|off`, `--base`, `--no-claude-md` |
| `hall doctor` | Checks the install. Each failed check names the command that fixes it. |
| `hall update` | Installs the latest `hall` from main, or says it's already current, naming the installed version and the latest version tagged on main ([ADR 0035](decisions/0035-semver-from-conventional-commits.md), [ADR 0040](decisions/0040-a-version-for-every-merge.md)). `--check`, `--source`, `--branch`, `--no-setup` |

## Extensions

Skill packs by other authors, embedded in `hall` unchanged and turned on per
role ([ADR 0038](decisions/0038-extensions.md)).

| Command | Does |
| --- | --- |
| `hall extensions [list]` | Every extension with its credit (author, source, license, the upstream commit) and, for each role it serves, the skills and whether they're on here. Works outside a repo too. |
| `hall extensions enable\|disable <name>` | Turns an extension on or off for every role it serves, or only `--role planner\|developer\|reviewer\|fixer` (repeatable). Writes `[extensions]` in the main checkout's `.hallmark.toml` and is idempotent. The reviewer reads that file from the base branch: the reply's `note` says so until it's committed there. Refused inside headless runs. |
| `hall extensions show <name> [<skill>]` | The extension's credits and full license, or one skill as Hallmark's agents read it. Development sessions use it to read a skill when the work calls for it. |

The extensions, and what each role gets:

| Extension | Credit | planner | developer | reviewer | fixer |
| --- | --- | --- | --- | --- | --- |
| `mattpocock` | [Matt Pocock's skills](https://github.com/mattpocock/skills) by Matt Pocock, MIT | `to-tickets` | `tdd`, `diagnosing-bugs`, `codebase-design` | `code-review` | `tdd`, `diagnosing-bugs` |

## Items

| Command | Does |
| --- | --- |
| `hall new [<request>]` | Creates an item and its worktree; splits into parallel parts when it can. `--title`, `--issue <n>`, `--no-split`, `--parent <id>`, `--follow-up <pr>`, `--open`, `--branch` |
| `hall adopt [--pr] [--base]` | Turns the current branch and worktree, or an open PR, into an item. A shipped item whose PR is still open is reopened: rebase it, resolve conflicts or change it, and the shipper updates the PR after review ([ADR 0029](decisions/0029-adopt-open-prs.md)). A checkout you made yourself is never removed when the item closes; only worktrees Hallmark made are swept ([ADR 0040](decisions/0040-sweep-finished-worktrees.md)). |
| `hall merge [<child>]` | Merges a parallel part into its parent's branch and closes it. |
| `hall ready [<id>]` | Freezes the commit and queues the item for review. While the Delivery office has a rebase pending and the branch isn't on its base yet, it refuses (`WRONG_STATE`) and keeps the request: release a hold so the office rebases it, or rebase yourself and run it again ([ADR 0043](decisions/0043-ready-waits-for-a-pending-rebase.md)). `hall propose-ready` waits the same way. Refused (`GATE_DENIED`) inside the fixer's headless run. |
| `hall propose-ready [<id>] --summary <s> [--why <w>] [--how <h>] [--ask]` | The agent says the work is done. With nothing left to confirm (no open question, a clean worktree, no open parts) it goes straight to review; `--ask` (or `auto_ready = false`) waits for your call. `--summary`, `--why` and `--how` become the PR description's What, Why and How ([ADR 0039](decisions/0039-pr-description-and-review-comments.md)). |
| `hall pull-back [<id>]` | Returns the item to development. |
| `hall drop [<id>] --reason <r>` | Abandons the item. The branch and journal stay. `--keep-worktree`. Refused (`GATE_DENIED`) inside the fixer's headless run. |
| `hall retry [<id>] --reason <r> [--tokens <n>]` | Allows an escalated item one more round, or releases a held item. A budget hold also grants that item another `per_item` (or `--tokens`); the budget is unchanged. On a shipped item the merger held, it lets the merger try again (`merge_failed`), or merge a PR that touches a `[merge] lit` path at its current head (`lit`) ([ADR 0048](decisions/0048-the-merger.md)). Refused (`GATE_DENIED`) inside the fixer's headless run. |
| `hall continue [<id>]` | Moves an item on from wherever it waits: releases a hold, retries an escalation, sends "ready?" to review, or reopens its Claude session. |

## Seeing what's happening

| Command | Does |
| --- | --- |
| `hall` | Opens the terminal app on a TTY; prints the status as JSON otherwise. |
| `hall status [<id>]` | Lists items by state, or shows one item with a **now** block: queued (and what is ahead), the gate running, the reviewer's run, the fixer, a hold, and what you can do. While a model run is going, it shows the run's last step and offers `hall trace <id> --follow`; on a hold after failed runs, once the last run has ended, it offers that run's `claude --resume` command and `hall trace <id>`. `--all` includes closed and dropped items. |
| `hall log [<id>]` | Prints the journal. `--kind <event>`, `--limit <n>` |
| `hall watch [<id>]` | Follows an item live until it leaves the loop or needs you. `--timeout` (default 30m); `--trace` also prints the steps of the item's run while one is going. |
| `hall trace [<id>]` | A headless run (reviewer, fixer, intake, librarian, resume) step by step: what the agent read, ran and said, and how it ended. The latest run by default; `--role <role>` takes that role's latest, `--run <n>` the n-th (1 = oldest), and `--list` numbers them. `--follow` keeps printing until the run ends or `--timeout` (default 30m) passes. `--open` prints the `claude --resume` command for an ended run (with `cd <dir> &&` only while the run's directory still exists; a review checkout is removed when its run ends); it never starts anything, and a run still going is `WRONG_STATE`. A stream with no end line is *interrupted* once its writer's `<stamp>-<role>.lock` is free (older streams without that file fall back to: a newer run exists or no live lease began before it), as when the daemon died mid-run: `--list` and the terminal app say so, `--follow` stops, and `--open` works. Off a terminal each step is a JSON line (`hall schema trace-line`) and the reply is the last line ([ADR 0041](decisions/0041-watch-runs-live.md)). |
| `hall history` | Closed and dropped items, and why things were rejected. `--rejected`, `--lens`, `--since`, `--all` |
| `hall queue` | The merge queue: every open shipped PR, by base, in the order the merger merges them, with its status (`ready`, `queued`, `merging`, `waits_for`, `behind`, `ci_pending`, `ci_red`, `conflicting`, `unknown`, `lit`, `changes_requested`, `held`, `not_ours`, `unreadable`) and why. It reads each PR from GitHub and never acts; it works with the merger off too ([ADR 0048](decisions/0048-the-merger.md)). |
| `hall export [<id>]` | The item's whole record as Markdown, with every model run's steps. `--out` |
| `hall usage` | Tokens and list-price cost. `--by item\|room\|role\|model\|day`, `--since`, `--item` |
| `hall statusline` | One line for Claude Code's status line. |
| `hall hud` | Everything the Claude Code heads-up display shows, in one call. `--session`, `--lines` |

## Your inbox

| Command | Does |
| --- | --- |
| `hall inbox [<n>]` | Only escalations: questions, "ready?", escalations, holds, CI that never ran. `hall inbox --notices` lists what the loop handles on its own (stuck items, waiting sessions, red CI being fixed, proposed learnings). `hall inbox 1` explains one: why it waits, the evidence, and each action. `hall inbox 1 --dismiss` hides one, `--clear` hides them all (each comes back if it changes), `--all` shows them. |
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
| `hall verdict --file <json>` | Records a schema-checked verdict. Only the assayer (`HALL_ACTOR=assayer`) and you on a terminal may run it; an agent (a Claude Code session, or any headless run but the assayer's) gets exit 5, `GATE_DENIED`, so an agent can't approve its own item. Your verdict is recorded with `actor: "you"` and shown as a human override in the report, the hallmark's note and the PR's review comment. |
| `hall reindex` | Rebuilds the SQLite index from the journals. |
| `hall help --json` | Lists the commands. |
| `hall schema <name>` | Prints a JSON Schema: any command's output, `error`, `event`, `config` or `verdict-input`. |
| `hall hook <event>` | The entry point for Claude Code hooks. |
