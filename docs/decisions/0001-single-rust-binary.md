# 0001. One Rust binary, `hall`, for every role

Status: Accepted, 2026-10-09

## Context

Hallmark has five kinds of caller:

- a person at a terminal
- agents running commands
- Claude Code hooks, on every tool call
- a long-running daemon
- a terminal app

It must run on macOS, Linux and Windows. Hooks are on the hot path, so they must
start fast and must not depend on bash, Python or Node being installed.

## Decision

Ship one Rust binary, `hall`, from the crate `hallmark`.

- With no arguments it opens the terminal app (Stage 3). Until then, it prints
  the status.
- Subcommands serve agents, hooks (`hall hook <event>`) and the daemon
  (`hall daemon`).
- The crates are ratatui and crossterm (terminal app), clap (commands), serde
  and schemars (JSON and schemas), rusqlite with bundled SQLite (index), and fs4
  (file locks). notify, service-manager and notify-rust come later.
- External tools are called as subprocesses: `git`, `gh` and `claude`.

## Consequences

- One install step puts everything on PATH. A single cargo-dist pipeline later
  covers all three systems.
- Hooks start in milliseconds and behave the same on Windows.
- Bundled SQLite adds compile time, but it means there is no system library to
  find.
- Anything the binary shells out to must be found portably. For example, `gh`
  is taken from `HALL_GH` when set, then looked up on PATH, then in the Nix store.
