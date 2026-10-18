# 0051. Merge clean PRs as they are, and stop the line on a red base

Status: Accepted, 2026-10-18. Amends 0048.

## Context

ADR 0048 merges a PR only when it is fresh: it contains its base, or the
commits it lacks touch none of its files. Anything else is rebased first, so
CI tests exactly the code that lands. ADR 0050 made that rebase happen once,
at the PR's turn, but on shared files the PRs still go one after another:
rebase, gates, review (carried, ADR 0049), ship, CI, merge, about 20 to 30
minutes each.

In this repo nearly every PR touches the same documents, because docs ship
with the change. On 2026-10-18 nine PRs waited behind #66, all in one lane:
about four hours for changes that GitHub said merge cleanly and whose CI was
green. What the rebase guards against is a semantic conflict: two PRs that
are green alone and merge without a textual conflict, but break together
(one renames a function, the other adds a call to its old name). Here that is
rare, and CI runs on every push to `main`, so a bad combination shows within
one CI run of the merge.

GitHub doesn't do this check for us. Without a merge queue (only for
organization-owned repos) or the "require branches to be up to date" rule,
GitHub merges the combination untested.

## Decision

### `merge.update`

A new setting says when the merger brings a PR up to date:

- `files` (the default): as before. The base moved on a file the PR touches,
  so it is rebased at its turn.
- `conflicts`: only when GitHub says the PR conflicts. A PR GitHub calls
  `MERGEABLE`, with green CI at its shipped head and every other check
  holding, merges as it is. `hall queue` says so: "behind main on
  docs/DESIGN.md, and merges cleanly as it is".

Everything else stays: the queue order, claims on files (a PR still waits for
an earlier PR on its files, so they merge in order), one merge per base until
the watch records `delivered`, `lit` paths, and `require_ci`.

### Stop the line

In either mode, before the merger merges into a base, rebases a PR onto it,
or asks a person about a `lit` PR, it reads the check runs on the base's tip
(`gh api repos/<o>/<r>/commits/<sha>/check-runs`). If any finished as
`failure` or `timed_out`, nothing merges into that base and nothing is
rebased onto it. The PRs show `queued` (or stay `behind`, `conflicting`) with
"main is red (… failed); the merger waits for it to be green". Your office
gets one `base_red` entry per base, naming the tip and the checks.

A cancelled run (a newer run replaced it) is not a failure. A run still
queued or going isn't one either, but the merger doesn't merge into a base
whose tip is still running checks (rebases go on): otherwise `main`'s
`cancel-in-progress` CI cancels each run as the next merge lands, a break is
never seen, and every queued PR lands on it. Each merge's CI finishes before
the next lands.

When GitHub can't be read the merger goes on, because the guard must never
stop the queue on an outage; but `hall queue` and the log say the guard is
off, and a base already recorded red at the same tip stays stopped. A base
that is red is read every pass, even with nothing ready, so the queue starts
again on the first pass after the tip is green, and the entry leaves.

The PR that fixes a red base can't pass the stopped queue (the base would
have to be green first). It is merged by hand with `gh pr merge <pr> --squash
--admin`; the `base_red` text and the queue say so.

## Consequences

- With `conflicts`, PRs on shared files merge one CI run apart instead of
  half an hour apart, and most of them are never rebased, gated or reviewed
  again.
- A bad combination isn't prevented; it is caught. It lands on the base, the
  base's CI fails, and the queue stops. The next merge waits for that CI, so
  nothing lands on top of the break and it stays one commit to find.
- With `files`, stop the line adds a guard that was missing: a red base
  stopped nothing before, and the merger rebased PRs onto it, which turned
  their CI red and sent them to the Delivery office for fixes they didn't
  need.
- A flaky check on the base stops the queue until it is re-run. Every check
  run on the tip counts, not only the ones that gate PRs: a red security
  audit on `main` is worth stopping for.
- The guard costs one GitHub call per base per pass, and only on a pass with
  something to merge or rebase, or a base already red.
