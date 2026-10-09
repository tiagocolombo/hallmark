# 0009. Install the plugin through a local marketplace

Status: Accepted, 2026-10-09

## Context

Hallmark's skills, agents and hooks ship as a Claude Code plugin, installed at
user level so that every repo and session gets them. The plugin must match the
installed binary's version, and its hooks must find `hall` even when Claude Code
is started from a GUI with a minimal PATH.

## Decision

- The plugin's files are embedded in the binary at build time.
- `hall setup claude`:
  1. extracts them to `<data_dir>/hallmark/claude-plugin/`, as a marketplace
     named `hallmark` with the plugin at `plugins/hallmark`;
  2. rewrites the hook commands in `hooks.json` to the absolute path of the
     running `hall` binary, quoted;
  3. runs `claude plugin marketplace add <dir> --scope user`, or
     `claude plugin marketplace update hallmark` if it is already added;
  4. runs `claude plugin install hallmark@hallmark --scope user`.
- `--dry-run` prints the files and commands without running anything.
- The repo also has a `.claude-plugin/marketplace.json`, so the plugin can be
  added straight from the repository.

## Consequences

- The plugin and the binary always come from the same build. Rerunning
  `hall setup claude` after an upgrade refreshes the plugin.
- Hooks work regardless of PATH. Moving the binary means rerunning setup, and
  `hall doctor` detects the stale path.
- This relies on the `claude plugin` subcommands, which `hall doctor` checks.
