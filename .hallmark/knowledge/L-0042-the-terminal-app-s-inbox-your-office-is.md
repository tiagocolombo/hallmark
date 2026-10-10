---
id: L-0042
kind: map
scope: ["src/tui/**", "src/commands/library.rs", "src/cli.rs"]
lens: repo_rules
source: ["H-0022"]
status: proposed
used: 0
---
# The terminal app's Inbox ('your office') is a reader: it shows each entry in full and runs actions only through the CLI's own parser, restricted to an allowlist of commands.

The TUI in src/tui renders inbox entries untruncated, with the reason they wait, evidence, and runnable actions. Actions are not executed freely; they are parsed by the same parser the CLI uses and limited to an allowlisted set of commands. The design is recorded in docs/decisions/0022-the-terminal-app-is-a-reader.md. Snapshot tests for these views live in tests/snapshots/tui__inbox_*.snap, so changes to inbox layout need snapshot updates.
