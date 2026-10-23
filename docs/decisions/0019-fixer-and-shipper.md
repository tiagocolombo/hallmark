# 0019. The fixer, the shipper, and the record

Status: Accepted, 2026-10-09. The PR body is amended by 0039; the fixer's
tools by 0053 (no bare `Edit` or `Write` rule).

## Context

After a rejected review or failed checks, an item waits in the Repair Shop
(`fixing`). Something must fix it and send it back without ever being able to
approve it. After approval, a hallmarked item must leave as a PR, and only at
the commit the reviewer approved. And the whole story must be readable
afterwards: what was rejected, why, and what it cost.

## Decision

**`hall fixed [<F ids>…] [--item] [--dispute <id>… --reason]`** is the only
way a fix enters the journal, for agents and the fixer worker alike
(`commands::fix::submit`).

- The item must be in `fixing`, its worktree clean of tracked changes
  (`DIRTY_WORKTREE`), and every ID an **open** finding (`USAGE`, listing the
  open ones). A dispute needs `--reason`; one ID can't be both fixed and
  disputed.
- The submitted commit is the worktree's HEAD. It must differ from the
  commit under review, unless the submission only disputes and the checks
  didn't fail at that commit (`WRONG_STATE`, "nothing committed").
- It records `fix_submitted` (→ `checks`, round + 1), queues a ticket, and
  releases any fixer lease, so the assayer doesn't wait for it to expire.
- Repeating a recorded submission returns it with `changed: false`.
- There is no path from it to `hallmarked`: only a later review settles the
  findings it claims.

**The fixer worker** (`fixer::tick`) claims an item in `fixing` and runs the
`fixer` plugin agent with `claude -p` in the item's worktree:

- `--json-schema plugin/schemas/fix.json`, `--permission-mode acceptEdits`,
  and `--allowedTools` for editing, committing, `hall findings`,
  `hall fixed`, and `Bash(<gate>:*)` for each configured gate.
- Hooks stay on, so its commits are journaled; `HALL_ACTOR=fixer`; and
  `PATH` starts with this binary's directory, so `hall` resolves even when
  it isn't installed.
- The prompt carries the failing checks (with the tail of each log) first,
  then the open findings, most severe first, with earlier disputes and
  reviewer notes.

**A script decides when it's done.** After the run:

1. the item already left `fixing` (the agent ran `hall fixed`): done;
2. uncommitted changes: a failed run, and the changes are kept for you;
3. the fix report names open IDs and HEAD moved, or it only disputes: the
   worker submits through `submit`, dropping IDs that aren't open;
4. nothing committed and nothing disputed: a failed run.

Failed runs are counted like `run_metered`'s (`run_failed`, attempt from
`failures_in_a_row`): the next tick retries once, the second failure holds
the item (`held`, kind `run_failed`). A dirty worktree at the start of a
tick counts as another failure without spending a run. Rate limits pause
the daemon; budget stops map through `Stop::progress`.

**An external fix engine** (Ralph Orchestrator, or anything) is used when
`fixer` in `.hallmark.toml` isn't `claude -p`. Hallmark writes the brief to
`items/<id>/fix-r<round>.json` (the `hall findings` fields, the round, the
request, and `report_file`), runs the command through the platform shell in
the worktree with `{id}`, `{worktree}` and `{findings_file}`, `HALL_ACTOR`,
the same `PATH`, and the fixer's timeout, and then settles the same way. The
engine may run `hall fixed` itself, or write a fix report
(`plugin/schemas/fix.json`) to `{findings_file}.out`. Without a report, a
moved HEAD is submitted with no finding IDs. Hallmark can't meter an
engine's model usage; budgets are checked before it starts.

**The shipper** (`shipper::tick`) claims a hallmarked item (never a child
item) and:

- refuses to ship unless HEAD is the hallmarked commit and carries the note;
  a branch that moved past it is held (`ship_failed`), never shipped;
- writes the PR body to `items/<id>/pr-body.md`: the developer's summary,
  the approving review's summary, rounds and rejections, the open minors and
  nits (they ride along), the checks, the item's usage, `Closes #<n>` for
  issue items, and where the full record is;
- runs `ship` with `{branch} {id} {base} {title} {body_file} {issue}` in the
  worktree (10-minute limit), and takes the PR URL from its stdout: the last
  `…/pull/<n>` URL, else the last URL;
- records `shipped` (→ `closed`), unqueues the item, and writes the export.
  A failing ship command holds the item with the end of its output.

**`hall export [<id>] [--out]`** writes the whole record as Markdown, by
default to `items/<id>/export.md`: the header (state, branch, base, issue,
PR, parent and children, follow-up, hallmark), the request, every event,
each checks run with its logs, each review with its findings and rulings,
each fix and dispute, the findings ledger, questions and answers, usage by
role and model, and the hallmark note.

**`hall history [--rejected] [--lens] [--since] [--all] [--limit]`** lists
closed and dropped items (live ones too with `--all`), newest first, with
the outcome, rounds, rejections, time to close, the PR, and the blocker and
major findings behind the rejections (from the index's `findings` table).
`--lens` keeps items with findings under that lens; `--limit` defaults
to 50.

## Live checks (Claude Code 2.1.296, haiku)

- An agent whose frontmatter lists `tools` loses the `StructuredOutput`
  tool, so a `--json-schema` run never returns `structured_output`. Listing
  `StructuredOutput` in `tools` fixes it. The fixer agent does.
- `--json-schema` rejects a schema with
  `"$schema": "https://json-schema.org/draft/2020-12/schema"`
  ("no schema with key or ref"). `fix.json` has no `$schema`.
- Without this binary's directory on `PATH`, the agent couldn't find `hall`
  and never recorded its fix. With it, the fixer fixed the finding, ran the
  gate, ran `hall fixed F1 F2` itself, and returned its report ($0.005).

## Consequences

- The fixer can be the built-in agent, any agent through `hall next --role
  fixer`, or an external engine; all go through the same `submit` rules.
- No prose decides anything: the journal and the worktree do.
- Only the reviewed commit ships, and the PR always points to its record.
