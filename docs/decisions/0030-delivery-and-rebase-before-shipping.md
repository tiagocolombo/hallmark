# 0030. The Delivery office, rebasing before shipping, what becomes an item, a clean inbox, and a daemon that doesn't die in its sleep

Status: Accepted, 2026-10-11

## Context

- **PRs conflict after they ship.** PRs #7 and #9 shipped and then conflicted
  with `main` (ADR 0029). Reopening them needed `hall adopt` by hand, and
  nothing in Hallmark noticed the conflict. Once a PR opened, nothing watched
  it until it merged, apart from its CI (ADR 0025).
- **The Dispatch Dock opened PRs from whatever base the branch had.** A branch
  approved hours earlier opened its PR behind `main`, sometimes already in
  conflict.
- **Everything became an item.** The edit gate denied every edit in the repo
  outside an item. Asking Claude a question that led to a doc edit, updating
  an ADR, or writing an architecture or a requirement turned into `hall new`,
  review rounds and a PR.
- **Review comments on an open PR waited for a person.** Each one needed
  `hall new --follow-up` or `hall adopt` by hand.
- **The inbox only emptied when each entry was settled.** There was no way to
  say "seen, not now".
- **The daemon died several times a day.** Its log ended in `panicked at
  library/std/src/sys/thread/unix.rs:581: assertion left == right failed,
  left: 35, right: 4`. That line is in `std::thread::sleep`, which asserts that
  `nanosleep` only fails with `EINTR` (4). On macOS it sometimes fails with
  `EAGAIN` (35), and the main loop's 200 ms nap panicked. The login service
  doesn't restart the daemon (on purpose), so the loop stayed down until
  someone ran `hall daemon start`.

## Decision

### The Dispatch Dock rebases before every ship or update

Before the `ship` or `update` command runs, the shipper fetches
`origin <base>` and rebases the hallmarked branch onto `origin/<base>` (or the
local `<base>` when the remote doesn't have it or can't be reached). A
stacked item whose lower PR merged replays only its own commits.

- **Already on top:** it ships as before.
- **Clean, with the same patch** (`git patch-id --stable` of the item's diff
  before and after): the gates run again at the rebased commit, in the assay
  pool. If they pass, the hallmark is **carried**: the note on the new
  commit names the approved one (`rebased_from`), `rebased {carried: true}`
  and `hallmarked` are recorded, and the new commit ships. The reviewer
  approved this exact change; only the base moved, and the gates check that
  it still works there. No review tokens are spent.
- **Clean, with a changed patch, or gates failing:** `rebased {carried:
  false}`, then `pulled_back` and `ready` at the rebased commit. It goes
  through checks and review again.
- **Conflicts:** the rebase is undone, and the item goes back to development
  (`pulled_back`, then `rebase_requested {onto, reason, files}`).

If every check slot is busy, the branch is reset to the hallmarked commit and
the shipper tries again on a later pass.

### The Delivery office

A new worker, `delivery`, runs after the shipper. Each tick does one thing:

1. **Rebase.** It claims an item in development with a rebase request (lease
   role `delivery`, one at a time), checks that the worktree is clean and no
   rebase is in progress, and rebases it onto the latest base. A clean rebase
   is sent to review at once (`ready`, actor `delivery`). On conflicts, a
   headless dev run (`claude -p`, the fixer's model and budget, actor
   `delivery`) gets the conflicted files and the item's request, resolves
   them, and runs `git rebase --continue`. Its tools can edit, stage,
   continue, commit and run the gates. They can't push or abort.
   **A script decides:** the rebase finished, the worktree is clean, and the
   base is an ancestor of HEAD. Then the item goes to review. Otherwise the
   rebase is aborted and the attempt counts. The second failure in a row holds
   the item, with the new hold kind `conflict`, which reaches the inbox and
   notifications like an escalation. `hall retry` lets it try again, and
   `hall ready` sends a branch you rebased yourself.
2. **Watch.** Each shipped PR (closed item, not a part, no `delivered` yet) is
   read at most every two minutes (`gh pr view --json
   state,mergeable,headRefOid`):
   - `MERGED` or `CLOSED`: `delivered {pr_url, outcome}`, and the watch ends.
   - `OPEN` and `CONFLICTING`: the item is reopened (`reopened`, as in ADR
     0029, actor `delivery`) with a rebase request, so step 1 takes it next.
     After review the shipper updates the same PR with a leased force-push.
   - `OPEN` with **new review comments**: comments, reviews and line comments
     written after the last ones it handled (or after the PR first shipped),
     leaving out approvals, empty bodies, bots (`*[bot]`, `github-actions`)
     and Hallmark's own update comment. The item is reopened with
     `feedback_requested {pr_url, comments, until}`. On the next tick a
     headless dev run gets the comments and the item's request, and
     addresses each requested change with commits. **A script decides:** new
     commits and a clean worktree go to review (`ready`, note "addresses the
     PR's comments"), and after approval the shipper updates the same PR. No
     commits means the comments were questions, or the run disagrees: the
     item is held (`feedback`) with the run's last message, and you answer on
     the PR. `until` (the newest comment's time) keeps handled comments from
     coming back.
   - If someone else pushed to the PR (its head isn't the one Hallmark
     shipped), or the worktree is missing or dirty, the office logs it once
     and leaves the PR for `hall adopt --pr`.

   Conflicts are handled before comments, so comments are always addressed on
   the current base.

The CI watch (ADR 0025) stays its own worker and belongs to the same office.

### Only clear development requirements become items

- `[capture] free` lists paths that need no item, as gitignore-style globs
  relative to the repo root. The default is `["docs/", "*.md"]`. An edit to
  such a path is never gated. A `git commit` whose staged and tracked changes
  are all free paths isn't gated either. Pushes still need a hallmark.
  `free = []` gates everything, as before.
- The CLAUDE.md section, the skill and the session context say it plainly:
  only a clear, concise requirement to build or change something in the app
  becomes an item. Questions, documents, architecture, requirements and plans
  are answered or written directly.

### Dismissing inbox entries

- `hall inbox <n|Q-id|H-id> --dismiss` hides one entry. `hall inbox --clear`
  hides every entry shown now. `hall inbox --all` shows dismissed entries too.
  Every explained entry (`hall inbox <n>`) ends with its dismiss action. In
  the terminal app's office, `D` dismisses the selected entry and `C` clears
  them all.
- A dismissal hides the entry **as it is**. Its key is the kind, the
  question, item or session, the time it started waiting, and a hash of its
  text. A new hold, a new failure or a new "looks done" makes a new key, so
  the entry comes back. Nothing is answered, retried or dropped.
- Dismissals are `inbox_dismissed {keys}` events in their own append-only
  file, `inbox-dismissed.jsonl`. The index isn't involved, so an older daemon
  sharing the index can't lose them. Hooks, notifications and the terminal app
  all read the filtered inbox.

### The daemon never sleeps through `std::thread::sleep`

`hall::daemon::nap` sleeps with `std::thread::park_timeout` in a loop until
the time is up. A timed park has no assertion and may only wake early. Every
sleep in the binary and the tests uses it. `clippy.toml` disallows
`std::thread::sleep`, so `cargo clippy -D warnings` (and CI) rejects any new
use.

## Consequences

- PRs open on the latest base, and stay mergeable until they merge, without a
  person rebasing them. A clean base move costs a gate run, not a review.
- The hallmark's guarantee changes in one way: a carried hallmark sits on a
  commit the reviewer never saw, but its patch is identical to one they
  approved, and its gates passed. The note says so (`rebased_from`), and the
  PR body says so too. A changed patch is always reviewed again.
- Rebasing needs a remote named `origin`, or a local base branch. Without
  `gh` the watch logs once and does nothing.
- Docs and Markdown edits no longer start items by default. In repos where
  Markdown is code (this one embeds its plugin's skills), set `free` to suit.
- Item JSON gains `rebase`, `feedback` and `delivered`, and the item view
  shows them. `hall status <id>` says what the Delivery office is doing.
- Follow-ups (ADR 0025) are still how you address feedback on a merged or
  closed PR. An open PR's comments go back through the same item.
- The daemon no longer dies of a macOS `nanosleep` quirk. The fix is in the
  binary, so `hall update` and a restart are needed to pick it up.
