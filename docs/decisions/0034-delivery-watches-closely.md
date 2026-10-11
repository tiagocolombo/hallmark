# 0034. The Delivery office watches PRs closely

Status: Accepted, 2026-10-11

## Context

ADR 0030's watch read each shipped PR at most every two minutes, one PR per
tick, in the same worker as the office's rebases. On 2026-10-11:

- PRs reached `delivered` 5 to 96 seconds after they merged (#29: 96 s, #35:
  92 s). The item stayed in the terminal app's Delivery lane until then.
- A rebase's dev run (`claude -p`, H-0030: three and a half minutes) held up
  the watch for every other PR, because the worker had one slot and each tick
  did one thing.
- `delivered` was recorded two or three times for one PR (H-0034, H-0037),
  two minutes apart. The watch picks PRs from the index, and an older `hall`
  process had rewritten the rows without the new field (ADR 0032), so the
  item still looked watched.
- Right after a base moves, GitHub reports `mergeable: UNKNOWN` for a while.
  A PR the merge put in conflict then waited two more minutes.

Reading every PR more often would spend GitHub API quota on PRs that haven't
changed.

## Decision

- **The watch is its own worker, `delivery-watch`.** It runs after
  `delivery`, which now only rebases. A dev run no longer delays the watch.
- **A base that moved makes its PRs due.** At most every ten seconds the
  watch asks `origin` where each watched base is (`git ls-remote origin
  refs/heads/<base>`, no prompt, no fetch, no GitHub API call). A merge
  always moves its base, so when a base moves, every PR on it is read on that
  tick: the merged one is delivered, and the ones left behind are checked for
  conflicts. Without `origin`, or offline, nothing changes and the two-minute
  read still applies.
- **Each tick reads every due PR**, oldest read first, instead of one.
- **`UNKNOWN` is read again in 15 seconds**, not two minutes, until GitHub
  settles on mergeable or conflicting.
- **`delivered` is recorded once.** It is appended under the item journal's
  lock, only when the folded journal has no `delivered` yet, whatever the
  index says.
- `delivery-watch.json` gains `soon`, `bases` and `bases_at`. A file from
  before loads with them empty.

## Consequences

- A merge reaches `delivered` within about ten seconds plus one daemon poll
  (5 s by default). A PR closed without merging, or new review comments, are
  still seen within two minutes.
- The watch makes one small `git ls-remote` per watched base every ten
  seconds while a PR is watched, and none when nothing is.
- `hall daemon run --only delivery` no longer reads PRs; use
  `--only delivery-watch`.
