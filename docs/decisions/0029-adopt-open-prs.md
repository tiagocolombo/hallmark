# 0029. Adopt open PRs: reopen shipped items, take over the rest

Status: Accepted, 2026-10-10

## Context

PRs #7 and #9 shipped from H-0011 and H-0009, and then `main` moved on: both
conflicted with it. Fixing them meant rebasing each branch, resolving the
conflicts, stacking #9 on #7, and force-pushing. Hallmark had no way to do
any of that:

- a closed item can't move (`closed` was terminal), so the rebase couldn't go
  back through review;
- `hall adopt` on the item's branch made a second, unrelated item;
- the ship command opens a PR (`git push -u` and `gh pr create`), which fails
  on a branch that already has one and can't replace rewritten history;
- the push gate rightly refused the rebased commits, which have no hallmark.

So the only way out was a hand push that skipped review. The same applies
to a PR Hallmark didn't open (a teammate's branch, or one from before
`hall init`): there was no way to bring it under review and update it in
place.

## Decision

- **`closed → development` through the open PR.** A new action, `Reopen`
  (event `reopened {pr_url, pr_head, base, worktree}`), is the only move out
  of `closed`. It is allowed for an item that shipped its own PR (not a part
  merged into its parent), and only while that PR is open. A merged or closed
  PR is history: work on it starts a follow-up (ADR 0025). `dropped` never
  moves.
- **`hall adopt` reopens.** On a branch with no open item, `hall adopt` looks
  for a closed item on that branch. If its PR is still open (`gh pr view`),
  the item goes back to development, keeps its ID, journal, findings and
  rounds, and is reviewed again like any other change. A rewritten branch
  gets a full review: the assayer already drops the incremental diff when the
  last reviewed commit isn't an ancestor.
- **`hall adopt --pr <n|url>` takes over any open PR.** If an item holds the
  PR, it is returned (or reopened). Otherwise a new item is made on the PR's
  own branch, which is checked out in a worktree through the configured
  `worktree` command (started at `origin/<branch>`) if no worktree has it.
  The item records the PR (`created.pr_url`, `created.pr_head`), and its base
  is the PR's base when that is an allowed base (see `--base`). Plain `hall adopt` on a branch with an open PR does the
  same, best effort: without `gh`, the item opens a PR of its own as before.
  PRs from forks are refused, since Hallmark can't push to them.
- **`--base <branch>` stacks.** On reopening or creating, the item can be
  based on another branch: it is reviewed against that branch, and its PR is
  retargeted to it on update. Only the configured base, or the local branch
  of an open PR whose item is hallmarked and still at its hallmarked commit,
  is accepted, so no unreviewed commits ship. With `--pr`, a PR based
  elsewhere (say `release-1.2`) is refused unless that rule holds; adopt it
  with `--base <configured base>`, which retargets it on update. An open
  item's base doesn't move. A worktree rebased or amended since the item
  shipped can be adopted while the PR is still at the commit the item shipped.
- **The shipper updates an existing PR.** An item with a PR runs the new
  `update` command instead of `ship`. Placeholders are those of `ship` plus
  `{pr}` (the PR's URL) and `{pr_head}` (the commit the PR's head was at when
  the item took it over, or last shipped). The default:

  ```sh
  git push --force-with-lease={branch}:{pr_head} origin {branch} \
    && gh pr edit {pr} --base {base} \
    && gh pr comment {pr} --body-file {body_file}
  ```

  The push replaces exactly the commit Hallmark knew about. If anyone pushed
  to the PR since, the lease fails and the item is held (`ship_failed`)
  instead of losing their commits. The PR body isn't rewritten, since a
  teammate's description is theirs; the review summary goes in a comment.
  The push gate treats `update`'s `git push` like `ship`'s; `gh pr edit` and
  `gh pr comment` aren't pushes and stay outside it.
- **What the item shows.** `pr_head` is folded onto the item. `reopened`
  clears `ci`, so the CI watch follows the PR again after the update. A
  closed item's `next` and `now` actions offer `hall adopt --pr <url>`, and
  `hall pull-back` on a shipped item points there.

## Consequences

- Rebases, conflict fixes and late changes to a shipped PR go through the
  same review as everything else, and the PR keeps its number and discussion.
- Stacking two open PRs is `hall adopt --base <other-branch>` on the upper
  one.
- `closed` is no longer strictly terminal. Code that treats it as "done" (the
  board, `hall status`, the library) sees a reopened item as live again until
  it ships. The librarian distils an item once per arrival in `closed`, so a
  reopened item is distilled again when it ships again, and lessons from the
  rebase reach the Library too.
- Adopting a PR, reopening and updating need `gh`. Without it, `hall adopt`
  behaves as before.
- Repositories with a custom `ship` command (not GitHub PRs) should set
  `update` too; the default assumes `git` and `gh`.
