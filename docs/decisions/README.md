# Architecture decisions

Short records of the larger choices, written when the choice was made. Each one
says what problem it answered, what we chose, and what follows from it. To change
a decision, add a new record that supersedes the old one. Don't edit the old one.

| # | Decision | Status |
| --- | --- | --- |
| [0001](0001-single-rust-binary.md) | One Rust binary, `hall`, for every role | Accepted |
| [0002](0002-agent-first-cli-contract.md) | The CLI is an API for agents | Accepted |
| [0003](0003-journal-jsonl-plus-sqlite-index.md) | JSONL journal, SQLite index | Accepted |
| [0004](0004-hallmark-as-git-note.md) | The hallmark is a git note | Accepted |
| [0005](0005-hooks-call-the-binary.md) | Hooks call `hall hook <event>` | Accepted |
| [0006](0006-claude-p-for-every-model-run.md) | `claude -p` for every model run | Accepted |
| [0007](0007-own-reviewer-not-code-review.md) | Our own reviewer, not `/code-review` | Accepted |
| [0008](0008-state-derived-from-journal.md) | Item state is folded from the journal | Accepted |
| [0009](0009-plugin-install-via-local-marketplace.md) | Install the plugin through a local marketplace | Accepted |
| [0010](0010-gate-scope.md) | Where the gates apply | Accepted |
| [0011](0011-drop-any-time.md) | An item can be dropped at any time | Accepted |
| [0012](0012-config-from-the-main-checkout.md) | `.hallmark.toml` is read from the main checkout | Accepted |
| [0013](0013-one-runner-for-claude-p.md) | One runner for every `claude -p` run | Accepted |
| [0014](0014-daemon-workers-and-leases.md) | The daemon is a loop of workers, with leases | Accepted |
| [0015](0015-verdicts-findings-and-approval.md) | Verdicts, findings, and who decides approval | Accepted |
| [0016](0016-budgets-holds-and-pauses.md) | Budgets, holds and pauses | Accepted |
| [0017](0017-daemon-process-and-pauses.md) | The daemon process: one instance, stop by file, pauses for everyone | Accepted |
| [0018](0018-the-assay-office.md) | The Assay Office: checks, then an isolated review | Accepted |
| [0019](0019-fixer-and-shipper.md) | The fixer, the shipper, and the record | Accepted |
| [0020](0020-notifications-resume-and-budget-accounting.md) | Notifications, resuming on an answer, and what budgets count | Accepted |
| [0021](0021-the-library.md) | The Library: learnings as reviewed files, read from the base | Superseded in part by 0027 |
| [0022](0022-the-terminal-app-is-a-reader.md) | The terminal app is a reader | Accepted |
| [0023](0023-packaging-and-releases.md) | Packaging and releases | Accepted |
| [0024](0024-parallel-parts-and-follow-ups.md) | Parallel parts: the Front Desk splits, the session orchestrates, the parent is reviewed once | Accepted |
| [0025](0025-watch-ci-after-shipping.md) | Watch CI after shipping; follow-ups carry it and stack on open PRs | Accepted |
| [0026](0026-parallel-phases.md) | Every phase runs in parallel, with configurable slots | Accepted |
| [0027](0027-the-library-stays-local.md) | The Library stays local: learnings live next to the journal | Accepted |
| [0028](0028-grant-the-daily-budget.md) | Grant the daily budget for today, without changing the cap | Accepted |
| [0029](0029-adopt-open-prs.md) | Adopt open PRs: reopen shipped items, take over the rest | Accepted |
| [0030](0030-delivery-and-rebase-before-shipping.md) | The Delivery office (conflicts and PR comments), rebasing before shipping, what becomes an item, a clean inbox, auto-ready, a wrapped timeline, and a daemon that doesn't die in its sleep | Accepted |
| [0031](0031-only-escalations-reach-you.md) | Only escalations reach you; the Delivery office fixes red CI; new items start from the latest base | Accepted |
| [0032](0032-old-processes-and-the-index.md) | An older hall never rewrites a newer one's index; the terminal app restarts on update | Accepted |
| [0033](0033-configurable-setup.md) | Every setting has a command, and `hall setup --interactive` may ask | Accepted; amends 0002 |
| [0034](0034-delivery-watches-closely.md) | The Delivery office watches PRs closely: its own worker, a base that moved makes its PRs due, `delivered` once | Accepted |
