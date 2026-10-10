---
name: hallmark
description: Track changes in this repo with Hallmark (the `hall` CLI). Use when starting work on a change ("/hallmark <request>", "fix the flaky login test", "pick up issue #212"), adopting an existing branch, saying the work is done or "send it to review" / "ready", "pull it back", "drop it", asking "what's in review?", "what needs me?" or "why was H-0042 rejected?", answering an agent's question, checking usage, or when an edit or push was denied by the Hallmark gate.
---

# Hallmark

Every change in a Hallmark repo is an **item** (`H-0042`) with its own branch,
worktree and journal. You build it with the human; an independent reviewer
approves it later. Only a commit with a hallmark (an approval bound to that
SHA) may be pushed, so **never push** and never open PRs yourself. The gate
denies `git push`, `gh pr create` and the repo's ship command without one.

## Talking to `hall`

- Output is JSON automatically when you run it (stdout is not a TTY).
- Every reply has `next`: the commands that make sense now. Read it.
- Errors look like `{"error":{"code","message","next"}}`.
- Inside an item's worktree the item ID is optional.
- Repeating a command is safe; it returns the current result.
- Unsure? `hall help --json` lists commands; `hall schema <cmd>` gives a reply's shape.

| Exit | Meaning |
| --- | --- |
| 0 | ok |
| 1 | internal error |
| 2 | bad arguments |
| 3 | not found |
| 4 | not allowed in the item's current state |
| 5 | denied by the gate |
| 6 | checks failed |
| 7 | claimed by another agent |

## Flows

**Start work** (`/hallmark <request>` or any request to change code):

```
hall new "<request>"            # or: hall new --title "<short title>"
```

Then do all edits and commits in the returned `worktree`:

- Edit files there with the Edit and Write tools, using absolute paths. Don't
  write files through shell redirects.
- Run git there with `git -C <worktree> ...`.
- The worktree is usually a sibling of the main checkout, outside this
  session's directory. If Claude Code asks for permission to write there, say
  why. If writes are refused, ask the human to approve them or run
  `/add-dir <worktree>`, or to start a session in the worktree.

- From a GitHub issue: `hall new --issue 212`. The PR will close it.
- On an existing branch or worktree: `hall adopt`.
- From review feedback on a shipped PR: `hall new --follow-up <pr-url>`.
- To keep a request as one item, never split: `hall new "<request>" --no-split`.

`hall new` asks the Front Desk whether the request splits into independent
parts. The reply's `plan` says what it decided and why. If the reply has
`children`, follow **Parallel parts** below instead.

## Parallel parts

When `hall new` returns `children`, the item it returned is the **parent**
and each child is one independent part with its own branch and worktree. You
are the orchestrator; the principal engineer reviews the parent once, when
every part is in.

1. **Launch one subagent per child, all in the same message**, so they run in
   parallel (the Agent tool). Give each one:
   - the child's id, its absolute `worktree` path and its `request`;
   - "Edit files only inside this worktree, with the Edit and Write tools and
     absolute paths. Run git there with `git -C <worktree>`. Commit your work.
     Don't push and don't run `hall ready`. Run the repo's gates in the
     worktree. Report what you did and what you checked."
2. **As each subagent finishes**, check its report, then merge the part into
   the parent's branch:
   ```
   hall merge H-0043
   ```
   On `MERGE_CONFLICT` nothing was merged: in the child's worktree, merge the
   parent's branch (`git -C <child-worktree> merge <parent-branch>`), fix the
   conflicts, commit, and run `hall merge` again, as the error's `next` says.
   A part that turns out unnecessary: `hall drop H-0044 --reason "..."`.
3. **When every part is merged or dropped**, run the gates in the parent's
   worktree on the merged whole and fix anything that broke there (commit in
   the parent's worktree). Summarise, run
   `hall propose-ready <parent> --summary "..."` and ask the human; when they
   say ready, `hall ready <parent>`.

`hall ready` on a parent with open parts fails with `CHILDREN_OPEN`; on a part
it fails with `WRONG_STATE` (parts merge, they aren't reviewed alone). To add
a part by hand: `hall new "<request>" --parent <parent>`.

**Edit gate.** If an edit or commit is denied with `NO_ITEM` (or similar), run
the command in the denial's `next`, move into the worktree, and carry on. Don't
try to work around the gate.

**Looks done.** Summarise and ask the human; don't send it yourself:

```
hall propose-ready --summary "retries with jitter, 14 new tests"
```

**Human says ready.** Commit everything first (`ready` refuses a dirty
worktree), then:

```
hall ready                      # freezes HEAD and queues it for review
```

After that, stop editing the item. If the human wants changes:
`hall pull-back`.

**Abandon:** `hall drop --reason "superseded by H-0044"`. Ask the human first.

## Seeing where things are

```
hall status                     # items by state, or one item in detail
hall log H-0042 --kind verdict  # the journal, filtered by event
hall inbox                      # everything waiting for the human
hall usage --by item            # tokens and cost
```

To answer "why was it rejected?", read the item's log and report the findings
with their severity, file and reason.

## Questions

When you need a decision from the human, ask through Hallmark so it reaches
them wherever they are:

```
hall ask "billed usage or raw events?" --options billed,raw --default billed --wait 4h
```

When the human answers in chat, record it: `hall answer Q-3 billed` (or the
option letter, `a`).

## States

`development` → `awaiting_you` → `checks` → `review` → `fixing` → back to
`checks`; `escalated` after 3 rejected reviews; `hallmarked` → `closed` when
the PR opens; `dropped` at any time. You work only in `development`. A
parallel part goes `development` → `closed` when `hall merge` merges it into
its parent. Later PR feedback starts a new item (`hall new --follow-up`).
