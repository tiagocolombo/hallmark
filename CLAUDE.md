# Hallmark: working in this repo

Hallmark (crate `hallmark`, binary `hall`) is a local, agent-first development
loop. See [README.md](README.md) and [docs/DESIGN.md](docs/DESIGN.md). Decisions
are in [docs/decisions/](docs/decisions/), and the Claude Code behaviour they
rely on is in [docs/spikes.md](docs/spikes.md).

## Toolchain

The Rust toolchain (cargo, rustc, clippy, rustfmt, rust-analyzer) and `gh` come
from the Nix shell declared in `rig.toml`:

- with direnv: `direnv allow`, once;
- otherwise: `nix develop`, or `nix develop --command <cmd>`.

`gh` may not be on PATH outside the shell. It lives under
`/nix/store/*-gh-*/bin/gh`, and `hall` also honors `HALL_GH`.

## Commands

Run all three checks before calling anything done, and report their real output.

| What | Command |
| --- | --- |
| Build | `cargo build` |
| Test | `cargo test --all-targets` |
| Format check | `cargo fmt --check` |
| Lint | `cargo clippy --all-targets -- -D warnings` |
| Review snapshots | `cargo insta review` (or `INSTA_UPDATE=always cargo test`, then inspect the diff) |
| Run | `cargo run -- <args>`, e.g. `cargo run -- help --json` |

## Conventions

- **Conventional commits:** `feat:`, `fix:`, `docs:`, `test:`, `chore:`,
  `ci:`, `refactor:`. Commit at each green step. Don't push, publish crates or
  create GitHub repos without asking.
- **The CLI is an API** (ADR 0002).
  - JSON output when stdout isn't a TTY.
  - Every reply has `next`.
  - Errors are `{"error":{"code","message","next"}}` with documented exit codes.
  - Commands never prompt and are idempotent.
  - Output types derive `Serialize` and `JsonSchema`, so `hall schema` stays
    true.
- **Snapshots pin the contract.** Update them deliberately and review every
  changed `.snap`. Never accept them blindly.
- **Docs ship with the change.** A change that adds or changes behaviour
  updates the docs in the same item, never in a follow-up. Docs that describe
  the old behaviour are a defect.
  - A command, subcommand or flag: `docs/commands.md`, and `README.md` when
    it's in the quick start.
  - A setting or default: `docs/configuration.md`.
  - Rooms, workers, states or an item's life: `docs/how-it-works.md` and
    `docs/DESIGN.md`.
  - Install, setup or upgrading: `docs/install.md`.
  - A design decision: a new ADR in `docs/decisions/`; update an old one only
    to mark it superseded.
  - What agents are told: the skill or agent under `plugin/`.
  - `tests/docs.rs` fails when a command or setting is missing from the
    reference docs. Everything else is checked in review.
- **Nothing repo-specific is hardcoded.** Worktree, ship and gate commands come
  from `.hallmark.toml`, with generic defaults.
- **Windows matters.**
  - Hooks call `hall hook <event>`, never shell scripts.
  - Use `std::path`, and canonicalize with `dunce`.
  - Run configured commands through `sh -c` or `cmd /C`.
  - Flag anything Unix-only, and gate Unix-only tests with `#[cfg(unix)]`.
  - CI on Windows is the check.
- **The journal is append-only.**
  - Never rewrite events.
  - State is folded from events through `state::next` (ADR 0008).
  - The SQLite index is a rebuildable cache.
- **Hooks never break a session.**
  - On an internal error, exit 0 and log it.
  - In repos without `<git-common-dir>/hallmark`, do nothing.
- **Don't spend model usage in tests.** Use the fixtures in `tests/fixtures/`
  and the fake runner (`HALL_RUNNER=fake:<script>`; `Env::script` and
  `Env::hall_fake` in `tests/common`).

## Layout

```text
src/
  main.rs        entry point
  lib.rs
  cli.rs         clap definitions, dispatch, exit codes
  output.rs      the agent contract: JSON/text, `next`, error envelope
  error.rs       error codes and exit codes
  item.rs        item IDs (H-0001) and states
  state.rs       the state machine and its transition table
  event.rs       journal event types
  journal.rs     append-only JSONL with fs4 locks
  index.rs       SQLite index (rusqlite, bundled)
  repo.rs        git dirs, worktrees, item resolution from the cwd
  config.rs      .hallmark.toml and its defaults
  git.rs, gh.rs  subprocess wrappers
  paths.rs       path normalization that behaves the same on every OS
  detect.rs      gate detection for `hall init`
  usage.rs       claude -p results and transcript usage, pricing
  gate.rs        shell parsing for the push and commit gates
  hooks.rs       `hall hook <event>` handlers
  plugin.rs      the embedded Claude Code plugin, for `hall setup claude`
  runner.rs      every `claude -p` run: command shape, timeouts, failures,
                 usage; the scripted FakeRunner for tests (ADR 0013)
  findings.rs    verdicts and findings; approval is computed (ADR 0015)
  lease.rs       work leases and `hall next` candidates (ADR 0014)
  worker.rs      the daemon's worker ticks, in order
  daemon/        the loop, state file, pauses, the login service
  watchman.rs, assayer/, fixer.rs, shipper.rs, resume.rs, notify/,
  library/       the workers
  intake.rs      the Front Desk's plan: parallel parts (ADR 0024)
  budget.rs      budget checks at four levels (ADR 0016)
  stamp.rs       the hallmark note
  skills.rs      repo skills from .hallmark/skills on the base (ADR 0037)
  condense/      condensing command output for an agent's context: one
                 filter per tool family, evidence never (ADR 0046)
  tui/           the terminal app (a reader)
  web/           `hall web`: the server and its embedded page (ADR 0052)
  commands/      one module per group of commands; meta.rs has
                 `hall help --json` and `hall schema`
plugin/          the Claude Code plugin: skills, agents, schemas, hooks
                 (every file is embedded via plugin::FILES)
tests/           integration tests (assert_cmd + tempfile), insta snapshots
tests/fixtures/  real hook stdin, claude -p results and transcripts from Step 0;
                 command output logs under output/
docs/            design, ADRs, spikes
```

<!-- hallmark:start -->
## Hallmark
Changes to this repo's code are tracked with Hallmark.
Only a clear, concise requirement to build or change
something in the app becomes an item: start it with
`hall new`, or `hall adopt` for an existing branch.
Questions, documents, architecture, requirements and
plans are not items: answer or write them directly.
Never push: work reaches GitHub only through review.
`hall help --json` lists every command.
<!-- hallmark:end -->
