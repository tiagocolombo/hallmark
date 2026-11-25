# 0070. Threads: the agents' conversations, read like a Q&A site

Date: 2026-11-25
Status: Accepted

## Context

Agents talk to each other all through an item's life, but nothing shows that
as a conversation. A review finding is in effect a question from the
reviewer. The verifier checks it (ADR 0062), the fixer answers with a fix or
a dispute and its reason, and the next round rules on it: fixed, still open,
or withdrawn. The Front Desk answers a request with a plan (ADR 0024), and
agents ask people questions (`hall ask`). All of it is already in the journal,
but as separate events in `hall log`, mixed with commits, checks and usage.
`hall findings` shows a finding's current status, not how it got there.

People want to see what was asked, who answered, and how it ended, from the
first message to the last, the way an old developer Q&A site shows a
question, its answers and the accepted one.

## Decision

**Threads are folded from the journal. Nothing new is recorded.** Every post
in a thread is an event the journal already holds, so threads work on every
journal written before this change, and they never disagree with
`hall findings` or `hall stats`.

There are four kinds:

| Kind | ID | The question | Replies |
| --- | --- | --- | --- |
| `finding` | `H-0042/F3` | the reviewer's finding: title, scenario, suggested fix | the verifier's comment that it stands (blocking findings, when the verifier checked that round); the fixer's answers, `Fixed in <sha>` or `Disputed: <reason>`; the reviewer's rulings in later rounds |
| `refuted` | `H-0042/R2.1` | a blocking finding the verifier showed is not real (the first one it dropped in round 2) | the verifier's reason; it was never recorded as a finding |
| `planning` | `H-0042/plan` | the request as typed | the Front Desk's plan: one item, or the parallel parts and why |
| `question` | `H-0042/q-<qid>` | an agent's `hall ask`, with its options and default | the answer (yours, or the default when nobody answered in time), and its delivery to the session that asked |

**A thread has a status**: `open` (no answer yet), `answered` (a fix or a
dispute waits for the next review to rule), `resolved` (fixed, or answered),
`withdrawn` (the reviewer took the finding back), `refuted`, and `closed`
(still open or answered when the item ended, such as a nit left at approval;
nobody will answer it now). A finding's status is the ledger's, so a thread
reads the same as `hall findings`.

**The accepted answer** is the one the thread settled on: the fix the
reviewer confirmed, the dispute behind a withdrawal, the person's answer, or
the plan.

**`hall threads`** lists threads across items, live ones (open, answered)
first and then the most recently active, with a tally by status. `--kind` and
`--status` filter, and `--limit` caps the list (50 by default). An item ID
lists that item's threads. A thread ID shows the thread whole. A thread ID
that doesn't exist is `THREAD_NOT_FOUND` (exit 3).

**hall web gets a Threads page** and a Threads tab on each item, both reading
`hall threads` as the page reads every other command. The list shows each
thread's reply count, status, kind and tags. A thread shows its question,
comments beneath it, then the replies, with the accepted one marked.

## Consequences

- People can follow a dispute from the finding to the ruling without
  reading the raw journal.
- A thread is only as rich as the events behind it. The verifier records
  only how many findings it checked, so its "it stands" comment goes on each
  blocking finding of a round it checked, not on a named list.
- New conversations become threads by adding a kind and folding its events.
  When the PM and Lead (ADR 0068) and `hall consult` (ADR 0069) land, their
  story, plan decision and consultations are expected to join as kinds, with
  no change to the command or the page.
- The terminal app doesn't show threads yet. It can run `hall threads`, and
  a view can follow.
