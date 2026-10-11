# Contributing to Hallmark

Thanks for your interest in Hallmark. Bug reports, documentation fixes and
code are all welcome. This guide covers how to get set up, what a pull request
needs, and the conventions the codebase follows.

## Before you start

- **Bugs:** search the [issues](https://github.com/tiagocolombo/hallmark/issues)
  first. If it's new, open a bug report with `hall --version`, your OS, the
  output of `hall doctor`, and the steps to reproduce it.
- **Features and larger changes:** open an issue to discuss the idea before
  writing code. Hallmark's larger choices are recorded as
  [architecture decisions](docs/decisions/). A change that reverses one needs a
  new record, so it helps to agree on the direction first.
- **Security issues:** don't open a public issue. See [SECURITY.md](SECURITY.md).

## Development setup

You need:

- Rust 1.88 or newer (stable), with `rustfmt` and `clippy`
- a C compiler, for the bundled SQLite
- `git`
- optionally [`cargo-insta`](https://insta.rs/docs/cli/) for reviewing snapshots,
  and [`gh`](https://cli.github.com)

```sh
git clone https://github.com/tiagocolombo/hallmark.git
cd hallmark
cargo build
cargo run -- help --json
```

The maintainers use a Nix dev shell (`nix develop`, declared in `rig.toml`). It
isn't required: a standard `rustup` toolchain works.

## Checks

Every pull request must pass the same checks as CI, on Linux, macOS and
Windows:

| What | Command |
| --- | --- |
| Format | `cargo fmt --check` |
| Lint | `cargo clippy --all-targets -- -D warnings` |
| Test | `cargo test --all-targets` |

Run all three before you open a pull request.

## Conventions

### The CLI is an API

Agents are `hall`'s main users
([ADR 0002](docs/decisions/0002-agent-first-cli-contract.md)). Every command:

- prints JSON when stdout isn't a TTY;
- includes `next` in every reply;
- reports errors as `{"error":{"code","message","next"}}` with a documented
  exit code;
- never prompts, and is idempotent.

Output types derive `Serialize` and `JsonSchema`, so `hall schema` stays
accurate.

### Snapshots pin the contract

Command output is pinned with [insta](https://insta.rs) snapshots in
`tests/snapshots/`. When you change an output on purpose, update the snapshots
with `cargo insta review` (or `INSTA_UPDATE=always cargo test`) and review every
changed `.snap` file. Never accept snapshots blindly. A snapshot that changed
unexpectedly is a bug.

### Tests never spend model usage

Tests use the fixtures in `tests/fixtures/` (real hook input, `claude -p`
results and transcripts) and the scripted fake runner
(`HALL_RUNNER=fake:<script>`; see `Env::script` and `Env::hall_fake` in
`tests/common`). No test may call a real model.

### The journal is append-only

Events are never rewritten. Item state is folded from events through
`state::next` ([ADR 0008](docs/decisions/0008-state-derived-from-journal.md)),
and the SQLite index is a cache that `hall reindex` can always rebuild.

### Hooks never break a session

A hook that hits an internal error logs it and exits 0. In a repository
without `<git-common-dir>/hallmark`, hooks do nothing.

### Nothing repo-specific is hardcoded

Worktree, gate and ship commands come from `.hallmark.toml`, with generic
defaults.

### Windows matters

- Hooks call `hall hook <event>`, never shell scripts.
- Use `std::path`, and canonicalize with `dunce`.
- Run configured commands through `sh -c` or `cmd /C`.
- Gate Unix-only tests with `#[cfg(unix)]`, and say so in the pull request.

CI on Windows is the final check.

## Commits and pull requests

- Use [Conventional Commits](https://www.conventionalcommits.org): `feat:`,
  `fix:`, `docs:`, `test:`, `refactor:`, `chore:`, `ci:`.
- Keep each pull request to one change, with tests that show it works.
- Update the docs (`README.md`, `docs/`) when behaviour changes. For a larger
  design choice, add an ADR in `docs/decisions/`.
- Describe what changed and why. If it changes command output, say which
  snapshots changed.

Hallmark is developed with Hallmark, so the maintainers' own changes go through
its review loop before they reach a pull request. You don't need Hallmark to
contribute.

## Code layout

| Path | What's there |
| --- | --- |
| `src/cli.rs` | clap definitions, dispatch, exit codes |
| `src/output.rs` | the agent contract: JSON or text, `next`, the error envelope |
| `src/state.rs`, `src/event.rs`, `src/journal.rs` | the state machine, journal events, the append-only JSONL journal |
| `src/index.rs` | the SQLite index |
| `src/hooks.rs` | `hall hook <event>` handlers |
| `src/runner.rs` | every `claude -p` run, and the fake runner for tests |
| `src/worker.rs`, `src/daemon/` | the daemon and its workers |
| `src/assayer/`, `src/fixer.rs`, `src/shipper.rs`, `src/library/` | the workers |
| `src/tui/` | the terminal app |
| `src/commands/` | one module per group of commands |
| `plugin/` | the Claude Code plugin: skills, agents, schemas and hooks, embedded in the binary |
| `tests/` | integration tests and snapshots |
| `docs/` | design, decisions, spikes |

## License

By contributing, you agree that your contributions are licensed under the
[MIT License](LICENSE).
