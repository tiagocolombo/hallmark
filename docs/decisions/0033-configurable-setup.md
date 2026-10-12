# 0033. Every setting has a command, and `hall setup --interactive` may ask

Status: Accepted, 2026-10-11

Amends [0002](0002-agent-first-cli-contract.md) ("never prompts") for one
opt-in flag.

## Context

Hallmark is public now, and the people setting it up mostly don't share its
maintainer's tooling. Every setting already had a generic default, but
changing one meant editing TOML by hand:

- `hall init` took only `--base` and `--capture`. The worktree, branch, ship
  and update commands, models, budgets, parallelism, the Library and
  notifications were reachable only in `.hallmark.toml`.
- Where `claude`, `gh`, `qmd` and `cargo` live could be set only with
  `HALL_*` environment variables. The daemon's login service starts with a
  bare environment, so it saw them only when `hall setup` copied them in.
- A repo whose toolchain comes from a shell of its own (`nix develop`,
  devbox, mise, direnv) needs its gates run inside that shell. The daemon
  runs gates with `sh -c`, outside any shell you entered. Nothing said so
  until a gate failed.

## Decision

### `hall config` sets every key, without questions

- `hall config list [prefix]`, `get <key>`, `set <key> <value>…` and
  `unset <key>`.
  - They work on the main checkout's `.hallmark.toml` (ADR 0012).
  - With `--user`, they work on `<user data dir>/hallmark/config.toml`.
    Keys only the user config has, such as `tools.*`, go there without the
    flag.
- The keys, their kinds and their help come from walking the JSON Schemas of
  `Config` and `UserConfig` (`src/settings.rs`). A new field is settable as
  soon as it exists, and `hall config` can't offer a key Hallmark doesn't
  read.
- Values are read by kind:
  - a list takes several values, or one TOML array;
  - a choice must be one of its values;
  - durations, token counts and caps are checked like the code that reads
    them.
- Edits go through toml_edit, so comments and layout are kept. The whole file
  must still parse, or nothing is written.
- `list` reports where each value comes from: `default`, `repo`, `user`,
  `env`, or `path` (a tool found on PATH).
- `set` and `unset` refuse inside Hallmark's own headless runs
  (`HALL_HEADLESS`). The fixer and the reviewer work under these settings, so
  they don't get to change them.
- It follows ADR 0002 in full: JSON off a terminal, `next`, the error
  envelope, idempotent, no prompts. Its reply schema is
  `hall schema config-reply`, because `hall schema config` already describes
  the file.

### `[tools]` in the user config

- `tools.claude`, `tools.gh`, `tools.qmd` and `tools.cargo` say where those
  programs are. `HALL_CLAUDE`, `HALL_GH`, `HALL_QMD` and `HALL_CARGO` still
  override them.
- The setting is in a file, not the environment, so the daemon's login
  service finds the programs too.

### `hall setup --interactive` asks, and only then

- The wizard asks about the settings people change most:
  - the base branch;
  - the gates, and the shell they run in. It offers `nix develop`, devbox,
    mise or direnv when the repo has their files, or any prefix you type.
  - the capture mode;
  - the worktree, branch, ship and update commands;
  - the models and budget caps;
  - where `claude` and `gh` are;
  - desktop and phone notifications;
  - whether the daemon starts at login.

  Then it offers every other key, one by one.
- Nothing is written until you confirm the list of changes. Cancelling, or
  running out of input, changes nothing, and the rest of setup doesn't run.
- The answers are applied after `hall init` writes the file, through the same
  code as `hall config set`.
- It runs only when asked, and only when stdin, stdout and stderr are
  terminals and `--json` isn't given. Otherwise it's a `USAGE` error whose
  `next` points at `hall config`. Questions go to stderr.

### Amending ADR 0002

"Never prompts" becomes "never prompts, except `hall setup --interactive` on
a terminal". `hall help --json` states the exception.

## Consequences

- An agent can configure Hallmark for a person through `hall config`. The
  plugin's `hallmark-setup` skill does exactly that: it asks the person in
  conversation and runs `hall config set`.
- A person can answer the same questions in a terminal, without Claude Code.
- Nothing in the code assumes rig or Nix. The maintainers' own repo sets
  `nix develop --command` gates in its `.hallmark.toml`, like any other repo
  would.
- The wizard's questions are unit-tested with scripted input. Its terminal
  check is the only part that needs a real terminal.
