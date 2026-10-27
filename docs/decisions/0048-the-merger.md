# 0048. The merger: shipped PRs merge without you, in dependency order

Status: Accepted, 2026-10-17. Amends 0030 and 0031. Amended by 0063: risk tiers
(`one_approval`, `two_approvals`, the reviewer's `needs_person`) make a PR wait
for people's approvals the way `lit` waits for you.

## Context

Hallmark closed every loop but the last. The Delivery office watches each
shipped PR, rebases it when it conflicts, fixes red CI and addresses review
comments (ADR 0030, 0031). Then the PR waits for a person to press Merge. On
2026-10-17, fourteen PRs (#64 to #77) were green and hallmarked and waited
for that, some for a day. The order mattered: twelve of them touch
`docs/DESIGN.md`, and several touch `src/config.rs`, so each merge could put
the next PR behind its base.

Addy Osmani's "Software factories, light and dark" names the choice. A
*dark* loop runs with nobody watching; a *lit* one keeps a person where a
wrong answer is expensive. A loop earns dark only when its check is cheap,
frequent, hard to game and stable, and the switch is set per loop, never for
the whole line.

## Decision

### A worker, `merger`, after `delivery-watch`

It is off unless `[merge] mode = "auto"`. At most every 20 seconds it walks
the open shipped PRs (closed items, not parts, not yet `delivered`), grouped
by base and ordered by PR number, oldest first. **A script decides**, never a
model. A PR is ready when all of these hold, cheapest first, so a PR that
can't merge yet costs no GitHub call:

- the merger hasn't merged it already, and it isn't held;
- its recorded head (`pr_head`) carries a hallmark note;
- CI passed at that head: the last `pr_checks` is green and came after the
  last ship. `[merge] require_ci = false` skips this;
- GitHub's head is that commit (`gh pr view --json
  state,mergeable,headRefOid,body,reviews`);
- it waits for no open PR: the one it is stacked on (its base is another
  item's branch), and any its description or last commits name with
  `Depends on #N`, `Blocked by #N` or `After #N` (or a PR URL). A PR that
  isn't one of Hallmark's counts once GitHub says it merged;
- no person's latest review asks for changes (bots don't count; a later
  approval by the same person clears it);
- `mergeable` is `MERGEABLE`: `UNKNOWN` waits, `CONFLICTING` is the Delivery
  office's;
- it touches no `[merge] lit` path, or you let this head merge;
- it is **fresh**: the base is in it, or the base's commits since the merge
  base touch none of the files the PR touches;
- its worktree is clean.

The first ready PR on a base merges. The first one that is only behind on its
own files is reopened (actor `merger`) with `rebase_requested {files}`, and
the Delivery office rebases it, runs the gates, carries the hallmark when the
patch is the same, and updates the PR. Only the PR next in line is reopened,
never the queue, and only one per base at a time: until it ships again (or
is dropped), the other PRs that are behind wait (`queued`), while fresh ones
keep merging. On 2026-10-17 seven of the fourteen open PRs were behind on
`docs/DESIGN.md`; rebasing them all at once would have made each later merge
send them back again. The merger keeps the one in flight in `merger.json`.

### Fresh by overlap, not by ancestry

Requiring every PR to contain its base would rebase every open PR after
every merge. On this repo each merge is followed by a release commit that
changes `Cargo.toml`, `Cargo.lock` and `CHANGELOG.md`, so the whole queue
would go back through gates and CI twice per merge. A PR whose files the base
didn't touch merges as it is; GitHub's own `mergeable` and CI on the merge
still guard it. A PR the base moved under, on its own files, is rebased
first.

### One at a time

The merge runs `[merge] command` in the main checkout under a lease, with
`{pr}` `{pr_head}` `{branch}` `{base}` `{id}` `{title}`. The default is
`gh pr merge {pr} --squash --match-head-commit {pr_head}`, so GitHub refuses
the merge if the head moved. Success records `pr_merged {pr_url, sha}`
(`merged` is a part's merge into its parent, ADR 0024). The watch records
`delivered` as before, and the sweeper removes the worktree. Until then,
nothing else on that base merges, so each PR is judged against the base as
it really is.

### Holds

- **`merge_failed`**: the command failed. The output is in the item's
  `runs/<stamp>-merge.log`, and the reason in the hold. `hall retry` lets the
  merger try again.
- **`lit`**: the PR is ready but touches a `[merge] lit` path. It waits for
  you, and doesn't hold up the PRs behind it. Merge it yourself, or
  `hall retry`, which records your approval of that head
  (`merge_approved`): the merger merges it, and a new head needs you again.

Both are escalations on a shipped item. The inbox now lists holds on shipped
items whose PR is still open.

### The author can't approve

GitHub refuses an approving review from a PR's author, and Hallmark ships
under the person's own account. So Hallmark posts no approval: the hallmark
is the approval. A base branch that requires a review needs a merge command
that may bypass it, such as the repository owner's `--admin`, which is how
release-plz merges its release PRs (ADR 0040). `--admin` also skips required
checks, so with `require_ci = false` a red PR merges, and only the base's own
CI reports it.

### `hall queue`

Read-only: every open shipped PR, by base and in order, with its status and
why. It works with the merger off. `hall status <id>` on a shipped item says
what the merger last decided about it.

## Consequences

- Nobody has to press Merge. A queue of green PRs drains in order, and a PR
  the base moved under is rebased before it merges, not after it conflicts.
- Merging is dark only by choice, per repo, and `lit` keeps chosen paths
  with a person. This repo merges with `--admin` and keeps `.github/`,
  `release-plz.toml`, `rig.toml` and `flake.nix` lit.
- The merger reads GitHub at most every 20 seconds, once per PR that passes
  the local checks.
- Merging squashes by default, so the PR title is the commit release-plz
  reads (ADR 0035).
- Item JSON gains `merged` and `merge_approved`; hold kinds gain
  `merge_failed` and `lit`; events gain `pr_merged`.
