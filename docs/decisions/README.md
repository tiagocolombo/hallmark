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
