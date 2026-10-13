---
name: hallmark
description: Track changes in this repo with Hallmark (the `hall` CLI). Use when starting work on a clear requirement to build or change the app ("/hallmark <request>", "fix the flaky login test", "pick up issue #212"), adopting an existing branch, saying the work is done or "send it to review" / "ready", "pull it back", "drop it", asking "what's in review?", "what needs me?" or "why was H-0042 rejected?", answering an agent's question, checking usage, or when an edit or push was denied by the Hallmark gate.
---

# Hallmark

Every change in a Hallmark repo is an **item** (`H-0042`) with its own branch,
worktree and journal. You build it with the human; an independent reviewer
approves it later. Only a commit with a hallmark (an approval bound to that
SHA) may be pushed, so **never push** and never open PRs yourself. The gate
denies `git push`, `gh pr create` and the repo's ship command without one.

## What becomes an item

Only a **clear, concise requirement to develop something in the app**: a
feature, a fix, a refactor, a test, an issue to pick up. Everything else is
not an item, so don't run `hall new` for it:

- questions, explanations, investigations ("why does X…?", "how would we…?");
- writing or updating documents, READMEs, ADRs, architecture or designs;
- drafting requirements, specs, plans or estimates.

Answer those directly. Edits under `[capture] free` (by default `docs/` and
Markdown files) need no item. When a request is vague, clarify it first; it
becomes an item once it is a clear requirement.

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

**Start work** (`/hallmark <request>` or a clear requirement to change the app's code):

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
- To rebase, resolve conflicts in or change an open PR, Hallmark's or not:
  `hall adopt --pr <pr>`, or `hall adopt` in its worktree. A shipped item comes
  back to development; after review the shipper updates the PR (a leased
  force-push). Never push the rebase yourself. `--base <branch>` accepts only
  the configured base or the branch of another hallmarked open PR; a PR based
  elsewhere is adopted with `--base <configured base>`, which retargets it.
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
     Don't push and don't run `hall ready`. Write a test for each behaviour
     you add or change (main path, boundaries, error paths) and check each
     one fails without your change (write the test before the code, or break
     the covered line in place and restore it; never `git stash`, the stash is
     shared by every worktree). Run the repo's gates in the worktree.
     Report what you did, the tests you added and what you checked."
2. **As each subagent finishes**, check its report (send it back if a
   behaviour has no test), then merge the part into the parent's branch:
   ```
   hall merge H-0043
   ```
   On `MERGE_CONFLICT` nothing was merged: in the child's worktree, merge the
   parent's branch (`git -C <child-worktree> merge <parent-branch>`), fix the
   conflicts, commit, and run `hall merge` again, as the error's `next` says.
   A part that turns out unnecessary: `hall drop H-0044 --reason "..."`.
3. **When every part is merged or dropped**, run the gates in the parent's
   worktree on the merged whole, add tests where the parts meet, and fix
   anything that broke there (commit in the parent's worktree). Summarise,
   run `hall propose-ready <parent> --summary "..." --why "..." --how "..."`
   for the whole change. With nothing left to
   confirm it goes to review on its own; with `--ask` it waits, and when the
   human says ready, `hall ready <parent>`.

`hall ready` on a parent with open parts fails with `CHILDREN_OPEN`; on a part
it fails with `WRONG_STATE` (parts merge, they aren't reviewed alone). To add
a part by hand: `hall new "<request>" --parent <parent>`.

**Edit gate.** If an edit or commit is denied with `NO_ITEM` (or similar), run
the command in the denial's `next`, move into the worktree, and carry on. Don't
try to work around the gate.

**Tests before review.** The reviewer audits your tests before anything
else, and rejects a change whose behaviour they don't pin. Before you call it
done, act as the change's test engineer:

1. List each behaviour the change adds or alters, and its cases: the main
   path, boundaries (empty, one, many, the limit), each error path, and
   repeated, concurrent or interrupted runs where they can happen.
2. Write a test for each case that asserts the outcome, not just that the
   code ran. For a bug fix, first write the test that reproduces the bug.
3. Prove each new test can fail: write it before the code, or break the
   line it covers in place and restore it, and watch it go red. Never
   `git stash`: the stash is shared by every worktree. A test that can't fail is not
   evidence.
4. Keep tests deterministic: no real sleeps, clocks, network or model runs,
   no shared temp paths, no order dependence. Use the repo's fakes and
   fixtures.
5. Never delete, skip or loosen a test to get green. If a test is wrong, fix
   it and say why in the commit message. Review every changed snapshot.

**Looks done.** Commit everything, run the gates, then send it with a
summary that names the tests, and why and how, for the PR:

```
hall propose-ready \
  --summary "retries with jitter; 14 new tests (give-up, jitter bounds, concurrent claim), each red without the change" \
  --why "receivers that blip for a second lose the event today; #88 lost a day of orders" \
  --how "a retry loop around deliver(), backoff with full jitter capped at 5 tries; the claim is a row lock so two workers can't send twice"
```

The PR description is `## What` (the summary), `## Why` and `## How`, and
nothing else: the review goes in PR comments. Write `--why` as the reason
the change is needed (the problem, who it hurts), and `--how` as the
approach and the choices a reader of the diff should know. Leave out the
review and the tests' details. Without `--why`, the request is used; without
`--how`, the commit subjects.

When nothing is left to confirm (no open question, a clean worktree, no open
parts), it goes **straight to review** on its own: the reply's `state` is
`checks`. Tell the human what you did and that it's in review. When something
does need their decision (a trade-off, a scope question, a risky change), add
`--ask`: it waits for their call (`awaiting_you`), and you ask them.

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
hall inbox                      # escalations: what only the human can settle
hall usage --by item            # tokens and cost
```

To answer "why was it rejected?", read the item's log and report the findings
with their severity, file and reason.

## When something waits for the human

Hallmark's hooks tell you when entries wait in the human's inbox (a
question, "ready?", an escalation, a hold, a failed CI, a stuck item). When
they do:

1. **Say so first**, before anything else in your reply: one or two lines per
   entry, in plain words, with what it needs from them.
2. **Offer the actions** the hook listed, with what each does, and ask which
   one they want. `hall inbox <n>` shows an entry's evidence (findings,
   failing checks, the question's options) when they want details.
3. **Run the action they pick** for them (`hall ready H-0042`,
   `hall answer Q-3 a`, `hall retry H-0039 --reason "…"`), then report the
   result. Don't pick for them.

To see what an item is doing at any time: `hall status <id>` has a **now**
block (queued and what is ahead, which gate is running, the reviewer's run,
the fixer, a hold and how to release it), and `hall watch <id>` follows it
live.

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
Once a closed item's PR merges or closes, the daemon removes its worktree
(the branch and journal stay), so never leave work there.
