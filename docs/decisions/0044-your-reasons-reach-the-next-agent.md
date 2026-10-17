# 0044. Your reasons reach the next agent

Status: Accepted, 2026-10-16.

## Context

- **Commands ask for a reason, and agents never saw it.** `hall retry`,
  `hall continue`, `hall pull-back` and `hall ready --note` record what the
  person typed in the journal. Only the reviewer read anything back: the note
  of the latest `ready`. The fixer after a retry, the Delivery office's dev
  runs, a resume run and the development session after a pull-back worked
  without the reason the person had just given for sending them back.
- **The note that did reach the reviewer came back every round.** The latest
  `ready` note was printed again in round 2, 3 and so on, long after it was
  answered.

## Decision

- **A reason goes to the next agent run on the item, whichever it is,
  once.** The reasons are those of `retried`, `released`, `pulled_back` and
  `ready`, given by a person (`you`) or a Claude session acting for them
  (`agent`), never by the loop's workers. The reasons Hallmark fills in when
  none was given ("continued with hall continue", "pulled back from the
  terminal app") say nothing and are left out. A retry that releases a hold
  records the same words twice; they are said once.
- **Every prompt has the same section**, `## What the person said`, with one
  line per reason: the command, who gave it, and the words. It says the words
  are given once and don't change the rules the agent works under.
- **Who gets them:**
  - the reviewer, in its prompt;
  - the fixer, in its prompt, and an external fix engine in its brief
    (`said`);
  - the Delivery office's rebase, PR-comment and CI runs, after their task;
  - a resume run, after the answer it carries;
  - a development session, through its hooks (`SessionStart` and
    `UserPromptSubmit`), only while the item is in development or waiting for
    the person, so a session never takes words meant for the reviewer.
- **Given means the run finished.** `reasons_delivered {to, until}` is
  recorded once a headless run returns (a verdict, a fix, a resolved rebase),
  or when a hook hands the words to a session. `until` is when the newest
  reason it was given was recorded, so a reason given while the run went on
  stays for the next one. A run that fails is retried with the same words.
  A headless run that finishes with no words still records one, with `until`
  set to the last event it read, so only a journal with no run in it falls
  back to the old-journal baseline.
- **Nothing else changes.** A dispute already reaches the reviewer with its
  finding, so `hall fixed --dispute --reason` isn't repeated here. `hall drop`
  ends the item, so no agent comes next.

## Consequences

- Retrying, releasing or pulling back an item with a reason steers the agent
  that picks it up, without a separate message.
- The reviewer sees a `ready` note once, in the round it was sent with (and
  that round's retries), not in every later round.
- The journal says who got the words: "the words given went to the fixer".
- A `ready --note` is the reviewer's alone: no other run shows or uses it up.
  A note the author's own session gave (`actor` agent) reaches the reviewer as
  "Author's note", never as the person's words. A journal from before this
  change starts after the last event that shows a run took the prompt.
- While a rebase, feedback or CI-fix request is open, the Delivery office's run
  takes the words, not the development session.
