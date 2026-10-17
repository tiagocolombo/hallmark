---
name: hallmark-fix
description: How Hallmark's fixer addresses failing checks and review findings - fix, commit, record with `hall fixed`, or dispute with a reason.
---

# Fixing for Hallmark

An item lands in the Repair Shop (`fixing`) when its checks failed or the
principal engineer asked for changes. You fix it in the item's worktree, then
send it back to review. You can dispute a finding, but you can never approve:
only the next review settles findings.

## The loop

1. **Read what's open.**

   ```
   hall findings <id>
   ```

   `failing_checks` lists each failing gate with its log path; `findings`
   lists the open findings (ID, severity, lens, file, line, title, scenario,
   fix). A `dispute` or a reviewer's `note` on a finding is the history of an
   earlier round.

2. **Fix the failing checks first.** Read the end of each log, reproduce the
   failure by running the gate, fix it, and run the gate again.

3. **Fix the findings, most severe first** (blocker, major, minor, nit). For
   each one:
   - read the file around the line and the scenario that breaks;
   - for a correctness or evidence finding, first write a test that
     reproduces the scenario and watch it fail;
   - make the smallest change that fixes the scenario, and watch the test
     pass;
   - commit it, naming the IDs: `git commit -m "fix F2: check the tenant before reading"`.

4. **Run the gates** listed in your prompt (or in `.hallmark.toml`) and make
   sure they pass. Leave nothing uncommitted.

5. **Record the fix.**

   ```
   hall fixed F1 F2 --item <id>                      # findings the commits fix
   hall fixed --item <id>                            # only the checks were fixed
   hall fixed --dispute F3 --reason "..." --item <id>  # a finding that is wrong
   hall fixed F1 --dispute F3 --reason "..." --item <id>
   ```

   The item goes back to checks, then review, as round N+1. Repeating the
   same `hall fixed` is safe.

6. **Return the fix report**: `summary`, `fixed`, `disputed`.

The next review checks that each fixed correctness or evidence finding now
has a test that fails without the fix. A fix without one stays open.

## Disputes

Dispute only when the finding is wrong, with a reason the reviewer can check:
the code path can't happen (say why), an existing test already covers it
(name it), or the repo's rules ask for exactly this. Disagreeing on taste is
not a dispute; fix it or leave a minor open. A disputed blocker or major
still blocks until the reviewer withdraws it.

## Errors from `hall fixed`

| Code | Means | Do |
| --- | --- | --- |
| `DIRTY_WORKTREE` | uncommitted changes | commit them, then retry |
| `WRONG_STATE` | nothing new was committed, or the item isn't in fixing | commit the fix; check `hall status <id>` |
| `USAGE` | an ID isn't an open finding, or a dispute has no reason | use the IDs from `hall findings <id>` |

## Never

- push, open a PR, or run the ship command;
- run `hall ready`, `hall verdict`, `hall retry`, `hall pull-back` or `hall drop`
  (`hall` refuses all but `hall pull-back` in your run);
- edit `.hallmark.toml`, `.hallmark/`, CLAUDE.md or AGENTS.md to make a
  finding go away;
- weaken, skip or delete tests, loosen assertions, or accept snapshots you
  haven't read, to make a gate pass.
