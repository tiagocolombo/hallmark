# 0031. Only escalations reach you; the Delivery office fixes red CI; new items start from the latest base

Status: Accepted, 2026-10-11

## Context

- **Your office filled with things nobody had to decide.** On 2026-10-11 it
  held twelve entries. Ten were `ci_failed`, one per shipped PR. One was a
  Claude Code session that had been waiting since the night before, and one
  was 73 proposed learnings. None was an escalation. All ten CI failures had
  the same cause: GitHub never started the jobs ("recent account payments have
  failed or your spending limit needs to be increased"). Every job failed in a
  few seconds with no steps and no runner. There was no code to fix, and only
  a person could fix the cause. Each entry also went to the phone.
- **Red CI on an open PR waited for a person.** Conflicts and PR comments go
  back through the item (ADR 0030). A failing check led to an inbox entry that
  asked for `hall new --follow-up` by hand.
- **New items started behind `main`.** `hall new` ran the worktree command in
  the main checkout, so a branch started from its HEAD. That was the local
  `main` as of the last `git pull`, or whatever branch the checkout had.
  Rebasing before shipping (ADR 0030) catches this at the end. But the work,
  its checks and its review all happened on an old base, and the rebase was
  more likely to conflict.

## Decision

### Your office holds only escalations

An inbox entry is either an **escalation** or a **notice**.

- **Escalations** are what the loop gave up on, or what only a person can
  decide: questions, `escalated`, "ready?", every hold (`budget`, `run_failed`,
  `ship_failed`, `conflict`, `feedback`, `ci_failed`), red CI the Delivery
  office couldn't fix, `ci_down`, a `daemon_stopped`, and an answer the resume
  worker couldn't deliver to its session.
- **Notices** are what the loop is handling on its own, or news:
  - `stuck`, the watchman's flag;
  - a session's own "waiting for your input" (Claude Code shows it in that
    session);
  - red CI while the Delivery office fixes it;
  - red CI on a PR that already merged;
  - proposed learnings (the Library never waits for you, ADR 0027).

`hall inbox`, the count in hooks and the status line, the terminal app's
office and notifications all read escalations only. `hall inbox --notices`
lists notices, and `hall inbox` says how many there are. The entry's JSON is
unchanged: the split is a flag inside `Index`, not a new field.

### CI that never ran is one entry, and nothing to fix

When the CI watch records a failure, it asks GitHub about each failing job
(`gh api repos/{owner}/{repo}/actions/jobs/<id>`). If none of the jobs has
steps or a runner, CI never ran. The `pr_checks` event records GitHub's reason
from the check run's annotation as `not_started`. Such a failure is never a
`ci_failed` entry, and the Delivery office leaves it alone. Every open PR in
that state shares one `ci_down` escalation, which names the PRs and the
reason. Only a response that is a job object with an empty `steps` list
counts. Errors, other shapes and a fake `gh` mean "it ran".

### The Delivery office fixes red CI on an open PR

The watch (ADR 0030) handles conflicts first, then new comments, then CI.
When a shipped PR is open, its head is the commit Hallmark shipped, the
worktree is clean, and the CI watch recorded a code failure, the item is
reopened with `ci_fix_requested {pr_url, failing}`. The next tick runs a
headless dev run with the failing checks and the end of each log. It may edit,
commit and run the gates, but it can't push or rebase. **A script decides**,
as it does for comments:

- new commits and a clean worktree go to review (`ready`, note "fixes the PR's
  failing CI"), and after approval the shipper updates the same PR;
- no commits hold the item (`ci_failed`) with the run's last message, which
  covers an outage or a broken base;
- uncommitted changes count as a failed run, and the second one holds the
  item.

Each request adds one to the item's `ci_fixes`. A green CI result resets it.
After `CI_FIXES` (2) fixes, a PR that is still red stops being reopened and
becomes a `ci_failed` escalation.

### The Delivery office can resolve conflicts in generated files

The first live rebase (PR #26) conflicted in `src/tui/app.rs` and five insta
snapshots. The dev run resolved the source file, but every command it needed
next was refused: `git checkout --theirs`, a narrower `cargo test --test tui`,
`INSTA_UPDATE=always … cargo test` to regenerate the snapshots, and
`git -C <worktree> status`. Its rebase was aborted, and a second attempt would
have failed the same way and held the PR. So:

- The office's dev runs may also run `git checkout --ours`/`--theirs`,
  `git checkout -- <file>`, `git restore` and `git rm`. Every git command is also allowed as `git -C <its worktree> …`.
  They still can't push or abort.
- **`dev_commands`** in `.hallmark.toml` lists commands, as prefixes, that the
  fixer and the office's dev runs may run besides the gates. The default is
  empty. Hallmark's own list allows test subsets, `cargo fmt`, and
  `INSTA_UPDATE=always` to regenerate snapshots.
- The rebase prompt says not to merge generated files by hand: take one side,
  regenerate them, and run each allowed command on its own.

### New items start from the latest base

`hall new` fetches `origin <base>` and starts a top-level item's branch at
`origin/<base>`, or at the local `<base>` when there is no remote. It uses the
same resolution as the Dispatch Dock (`delivery::base_ref`). It starts from
the commit, not the ref, so the new branch tracks nothing; the ship command
sets its upstream. The Front Desk's parts start from their parent's branch,
which is now fresh. A follow-up stacked on an open PR still starts from that
PR's branch. Only when neither ref exists does the worktree command start
from the main checkout's HEAD, as before.

## Consequences

- Your office stays empty unless something needs you, and so does your phone.
- A red PR is fixed without you, at most twice, on the fixer's model and
  budget. Each fix goes through review like any other change.
- An Actions outage or a billing stop is one entry, not one per PR. Once it is
  fixed, re-run the checks. The CI watch keeps reading an open red PR, so a
  green re-run clears the entry.
- `hall new` needs a fetch, which costs one network round trip. Offline, it
  falls back to the local base.
- The main checkout's `main` can stay behind without anything starting,
  reviewing or shipping on an old base. Reviews read the base's rules from
  the local `<base>` (ADR 0012), so `git pull` in the main checkout still
  matters for rule changes.
- The fix is in the binary. `hall update` and `hall daemon start` pick it up.
