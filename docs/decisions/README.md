# Architecture decisions

Short records of the larger choices, written when the choice was made. Each one
says what problem it answered, what we chose, and what follows from it. To change
a decision, add a new record that supersedes the old one. Don't edit the old one.

| # | Decision | Status |
| --- | --- | --- |
| [0001](0001-single-rust-binary.md) | One Rust binary, `hall`, for every role | Accepted |
| [0002](0002-agent-first-cli-contract.md) | The CLI is an API for agents | Accepted |
| [0003](0003-journal-jsonl-plus-sqlite-index.md) | JSONL journal, SQLite index | Accepted |
| [0004](0004-hallmark-as-git-note.md) | The hallmark is a git note | Accepted; partly superseded by 0045 |
| [0005](0005-hooks-call-the-binary.md) | Hooks call `hall hook <event>` | Accepted |
| [0006](0006-claude-p-for-every-model-run.md) | `claude -p` for every model run | Superseded in part by 0041 |
| [0007](0007-own-reviewer-not-code-review.md) | Our own reviewer, not `/code-review` | Accepted |
| [0008](0008-state-derived-from-journal.md) | Item state is folded from the journal | Accepted |
| [0009](0009-plugin-install-via-local-marketplace.md) | Install the plugin through a local marketplace | Accepted |
| [0010](0010-gate-scope.md) | Where the gates apply | Accepted; its final control amended by 0048 |
| [0011](0011-drop-any-time.md) | An item can be dropped at any time | Accepted |
| [0012](0012-config-from-the-main-checkout.md) | `.hallmark.toml` is read from the main checkout | Accepted; amended by 0053 |
| [0013](0013-one-runner-for-claude-p.md) | One runner for every `claude -p` run | Superseded in part by 0041; amended by 0053 |
| [0014](0014-daemon-workers-and-leases.md) | The daemon is a loop of workers, with leases | Accepted |
| [0015](0015-verdicts-findings-and-approval.md) | Verdicts, findings, and who decides approval | Accepted; amended by 0039 |
| [0016](0016-budgets-holds-and-pauses.md) | Budgets, holds and pauses | Accepted |
| [0017](0017-daemon-process-and-pauses.md) | The daemon process: one instance, stop by file, pauses for everyone | Accepted; amended by 0054 |
| [0018](0018-the-assay-office.md) | The Assay Office: checks, then an isolated review | Accepted |
| [0019](0019-fixer-and-shipper.md) | The fixer, the shipper, and the record | Accepted; amended by 0039, 0053 |
| [0020](0020-notifications-resume-and-budget-accounting.md) | Notifications, resuming on an answer, and what budgets count | Accepted |
| [0021](0021-the-library.md) | The Library: learnings as reviewed files, read from the base | Superseded in part by 0027 |
| [0022](0022-the-terminal-app-is-a-reader.md) | The terminal app is a reader | Accepted |
| [0023](0023-packaging-and-releases.md) | Packaging and releases | Accepted |
| [0024](0024-parallel-parts-and-follow-ups.md) | Parallel parts: the Front Desk splits, the session orchestrates, the parent is reviewed once | Accepted |
| [0025](0025-watch-ci-after-shipping.md) | Watch CI after shipping; follow-ups carry it and stack on open PRs | Accepted |
| [0026](0026-parallel-phases.md) | Every phase runs in parallel, with configurable slots | Accepted |
| [0027](0027-the-library-stays-local.md) | The Library stays local: learnings live next to the journal | Accepted |
| [0028](0028-grant-the-daily-budget.md) | Grant the daily budget for today, without changing the cap | Accepted |
| [0029](0029-adopt-open-prs.md) | Adopt open PRs: reopen shipped items, take over the rest | Accepted; amended by 0039, 0042 |
| [0030](0030-delivery-and-rebase-before-shipping.md) | The Delivery office (conflicts and PR comments), rebasing before shipping, what becomes an item, a clean inbox, auto-ready, a wrapped timeline, and a daemon that doesn't die in its sleep | Accepted; amended by 0039, 0043, 0048, 0049 |
| [0031](0031-only-escalations-reach-you.md) | Only escalations reach you; the Delivery office fixes red CI; new items start from the latest base | Accepted; amended by 0047, 0048, 0055 |
| [0032](0032-old-processes-and-the-index.md) | An older hall never rewrites a newer one's index; the terminal app restarts on update | Accepted |
| [0033](0033-configurable-setup.md) | Every setting has a command, and `hall setup --interactive` may ask | Accepted; amends 0002; amended by 0055 |
| [0034](0034-delivery-watches-closely.md) | The Delivery office watches PRs closely: its own worker, a base that moved makes its PRs due, `delivered` once | Accepted; amended by 0050 |
| [0035](0035-index-fold-marks.md) | Each index row carries a fold mark; a row nobody vouches for is re-folded | Accepted; amends 0032 |
| [0036](0036-test-first-review.md) | Test-first review: an unpinned behaviour blocks approval | Accepted |
| [0037](0037-repo-skills.md) | Repo skills: `.hallmark/skills/<name>/SKILL.md`, read from the base branch, extends or replaces Hallmark's | Accepted |
| [0038](0038-extensions.md) | Extensions: skill packs by other authors (Matt Pocock's skills first), embedded unchanged and credited, turned on per role | Accepted |
| [0035](0035-semver-from-conventional-commits.md) | Versions from conventional commits: typed PR titles, release-plz tags `vX.Y.Z` on main, `hall update` names versions and says when a release is out | Accepted; amends 0023 |
| [0039](0039-pr-description-and-review-comments.md) | The PR description is What, Why and How; every review is a PR comment (`review_comment`) | Accepted; amends 0019, 0029; amended by 0057 |
| [0040](0040-a-version-for-every-merge.md) | A version for every merge: release-plz reads tags, its release PR merges at once; a GitHub Release is published by hand | Accepted; amends 0023, 0035 |
| [0040](0040-sweep-finished-worktrees.md) | The sweeper removes the worktree of each item that is done with it: its PR merged or closed, or it shipped with no PR to watch | Accepted |
| [0041](0041-watch-runs-live.md) | Watch headless runs live: each run streams into `runs/<stamp>-<role>.jsonl`, `hall trace` and the terminal app read it | Accepted |
| [0042](0042-an-update-that-pushed-and-failed.md) | An update that pushed and then failed doesn't stick its item: the lease is the remote head when this item hallmarked it | Accepted; amends 0029 |
| [0043](0043-ready-waits-for-a-pending-rebase.md) | `hall ready` waits for a pending rebase: refused while the branch isn't on its base, and the request stays | Accepted; amends 0030 |
| [0044](0044-your-reasons-reach-the-next-agent.md) | Your reasons reach the next agent: retry, continue, pull-back and ready notes go into the next run's prompt on the item, once | Accepted |
| [0045](0045-the-gate-verifies-the-note.md) | The push gate verifies the hallmark note: it parses, names `HEAD`, and its item's journal records `hallmarked` there | Accepted; partly supersedes 0004 |
| [0046](0046-condense-command-output-never-evidence.md) | Condense command output for the context, never evidence: one filter per tool family, small output untouched, the full log on disk | Accepted |
| [0047](0047-a-ci-fix-clears-when-ci-goes-green.md) | A CI fix request clears when the PR's checks go green: the fix run reads them first, the CI watch follows held fixes, and the log is the failed job's own | Accepted; amends 0031 |
| [0048](0048-the-merger.md) | The merger: shipped PRs merge without you, one at a time per base, in dependency order; `lit` paths wait for a person; `hall queue` | Accepted; amends 0030, 0031; amended by 0049, 0050, 0051 |
| [0049](0049-a-rebase-keeps-its-approval.md) | A rebase keeps the approval it earned: the same change carries it with no review, a small difference gets a review of only that difference | Accepted; amends 0030, 0048 |
| [0050](0050-the-queue-updates-prs-at-their-turn.md) | The merge queue updates each PR once, at its turn: the watch leaves conflicts to it, a PR waits for the earlier PRs that touch its files, separate files move in parallel lanes | Accepted; amends 0034, 0048 |
| [0051](0051-merge-clean-prs-and-stop-the-line.md) | Merge clean PRs as they are (`merge.update = "conflicts"`), and stop the line: nothing merges into, or is rebased onto, a base whose tip's CI failed | Accepted; amends 0048 |
| [0052](0052-the-web-app.md) | The web app: `hall web` serves a page on 127.0.0.1 whose every call is a hall command, behind a token and a host check; it updates and restarts itself | Accepted |
| [0053](0053-the-gazette.md) | The Gazette: a newspaper page per finished item, for people. Code sets the facts, the librarian's run writes the words and a diagram outline, and `hall attach` adds plates | Accepted |
| [0053](0053-sandbox-every-headless-run.md) | Sandbox every headless run: file tools read only inside the run, never credentials; commands run in Claude Code's sandbox without skipping the allowlist; no web tools; `claude_args` can't loosen it (`[sandbox]`) | Accepted; amends 0012, 0013, 0019 |
| [0054](0054-child-processes-without-a-console-window.md) | Every child process starts in `process::command`: a `hall` without a console (the daemon on Windows) runs `git`, `gh`, `claude` and the gates without a console window | Accepted; amends 0017 |
| [0055](0055-the-setup-wizard-asks-first.md) | The setup wizard asks first: `hall init` and the first `hall setup` offer, on a terminal, to go through every setting with its default, or to skip for the defaults; `--defaults` never asks; `hall setup` creates the user config | Accepted; amends 0033 |
| [0055](0055-a-limit-pause-is-an-escalation.md) | A limit pause is an escalation: Claude's session and usage limits (and the daily budget) pause the loop until the reset, `paused` is an inbox entry, and `hall daemon resume` retries at once | Accepted; amends 0031 |
| [0056](0056-stats-for-the-loop.md) | Stats for the loop: `hall stats` folds the journals into reviews to approval, what became of each finding, the reviewer's noise by lens and how disputes went, so a change to the reviewer can be judged | Accepted |
| [0057](0057-testing-and-how-to-revert-in-the-pr.md) | The PR description also says how the change was tested (`--tested`, then the gates that passed) and how to revert it (`--revert`, then always a `git revert` line) | Accepted; amends 0039 |
| [0058](0058-review-notes-skip-paths-and-a-nit-cap.md) | The review notes say what failure looks like and which paths to skip (left out of the diff and the counts), and `max_nits` caps the nits a verdict records, enforced by the ledger | Accepted; amends 0018 |
| [0060](0060-the-last-round-asks-for-a-diagnosis.md) | On the last round before escalation the reviewer is told so and asked to make its summary a diagnosis (is the approach wrong, what instead); the escalation's inbox entry and notification carry it | Accepted; amends 0018 |
| [0061](0061-a-revert-is-a-signal.md) | A revert is a signal: the `reverts` worker watches each base for 30 days after a merge, records `reverted` on the item, puts it in the inbox, counts it in `hall stats`, and the librarian distils the item again asking which layer should have caught it | Accepted; amends 0021, 0056 |
