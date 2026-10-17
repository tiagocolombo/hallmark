# 0050. The merge queue updates each PR once, at its turn

Status: Accepted, 2026-10-17. Amends 0034 and 0048.

## Context

With the merger on (ADR 0048), two parts of the Delivery office sent shipped
PRs back to be rebased, and only one of them knew about the queue:

- the merger reopened the next PR in line when it was behind its base on its
  own files, one per base at a time;
- the delivery watch (ADR 0030, 0034) reopened every PR the moment GitHub
  called it `CONFLICTING`.

On 2026-10-17, when #82 merged, the watch reopened six PRs in the same second
(H-0066, 67, 68, 69, 71 and 73), and the merger reopened H-0062. Almost every
item touches the same documents (`docs/DESIGN.md`, `docs/decisions/README.md`,
`docs/commands.md`) and snapshots, because docs ship with the change. So each
merge made every PR still waiting conflict again, and the watch reopened all
of them again. For N PRs that is about N²/2 rebases, each with a full review.
By that afternoon H-0062 was on review round 6 and H-0068 on round 5, and
H-0066, 67 and 68 had spent $10 to $16 of reviews each, mostly re-approving
the same change on a new base. The review slots filled with re-reviews and
new work waited behind them.

The merger had a smaller gap of the same kind: while one PR was being
rebased, a fresh PR behind it could merge even when it touched the same
files, making that rebase stale before it finished.

Merge queues elsewhere agree on three rules:

- **The queue alone updates a PR.** GitHub's merge queue tests each PR on the
  latest base plus the PRs ahead of it, and authors never update a waiting
  branch; a conflicting PR leaves the queue.
- **Just in time, near the head.** Zuul tests each change as if the changes
  ahead had merged, and starts work only for a window at the head of the
  queue; the changes behind it wait without running anything.
- **Parallelism comes from knowing what conflicts.** Uber's SubmitQueue
  analyses which changes conflict, builds the independent ones together, and
  orders only the ones that conflict.

None of them resolves conflicts; Hallmark's Delivery office can. That makes
the second rule worth more here: a conflict is resolved once.

## Decision

With `[merge] mode = "auto"`, only the merge queue sends a shipped PR back to
be brought up to date, and only at its turn. With the merger off, nothing
changes.

### The watch leaves conflicts to the queue

The delivery watch no longer reopens a PR GitHub calls `CONFLICTING`. It
still records `delivered`, addresses new review comments and red CI. The
merger treats a conflicting PR like one that is behind its base: it is
reopened with a rebase request at its turn, and `hall queue` shows it as
`conflicting`. Freshness stays the merger's own git check (ADR 0048).
GitHub's `mergeable` only says when to wait (`UNKNOWN`) or to look closer
(`CONFLICTING`).

### A PR waits for the earlier PRs that touch its files

The merger plans each base in queue order: stacked and `Depends on` first,
then the oldest PR. Files are **claimed**:

1. first by every item of that base already reopened through its PR and
   being brought back by the loop (a rebase, comments or a CI fix; in
   development with that request, checks, review, fixing or hallmarked), with
   no hold;
2. then, walking the queue, by every PR still on its way to a merge on its
   own: ready, merging, behind, conflicting, CI pending, `UNKNOWN`, queued, or
   waiting for a PR in this queue.

A PR that touches a claimed file neither merges nor is rebased. It shows
`waits_for` the PR that claims the file, and why ("first waits for #64
(docs/DESIGN.md)"). So a PR is rebased only after every earlier PR it
overlaps with has merged: once, onto a base that already has them.

### Lanes

A PR that shares no files with anything ahead of it merges, or is rebased,
without waiting. Several rebases may be in flight on one base when they touch
different files. Merges stay one per base until the watch records
`delivered` (ADR 0048).

### A PR waiting on a person holds up nothing

A PR that is held, lit, has a standing request for changes, isn't the shipped
head, has red CI, can't be read, or waits for a PR outside the queue claims
nothing. The PRs behind it go first, and it is brought up to date at its own
turn later. A reopened item that is held, or that a person took back, claims
nothing either.

### The plan is the queue

`hall queue` and the worker share one planning function, so the queue shows
exactly what the worker will do. Reopened items appear in it as `updating`,
or `held`. The `rebasing` map in `merger.json` is gone: what is in flight
comes from the journal.

## Consequences

- Each PR is rebased at most once per pass through the queue instead of once
  per merge ahead of it: for N PRs that touch the same files, N rebases
  instead of about N²/2.
- PRs on hot files go one after another: rebase, checks, review, ship, CI,
  merge. PRs on separate files run in parallel lanes.
- A rebase still costs a review today. ADR 0049 (a rebase keeps its
  approval) makes most of them cost only the gates.
- A PR stays `CONFLICTING` on GitHub until its turn, and its CI may be stale
  until then. That is expected: it merges only after it is brought up to
  date.
- Not done: Zuul-style speculation, which would resolve and review the next
  PR on top of the one being merged, while that one runs CI. It means pushing
  queue branches and trusting CI by tree rather than by commit, and squash
  merges change commit hashes. Worth it only if the queue is still too slow.
