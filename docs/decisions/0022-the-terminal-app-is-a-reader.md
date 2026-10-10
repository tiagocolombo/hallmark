# 0022. The terminal app is a reader

Status: Accepted, 2026-10-09

## Context

`hall` with no arguments on a terminal opens the terminal app: the human
view of the building (board, journal replay, findings, history, live,
usage, repos). The work it shows happens elsewhere: in Claude Code
sessions, in hooks, and in `hall daemon`. Several copies of the app may be
open at once, on several repos, while the daemon writes. Closing the app
must never stop or lose work, and it must run the same on macOS, Linux and
Windows.

## Decision

- **It only reads.** The app reads the journal (`Hall::read_item`), the
  SQLite index (`Index::sync`, `items`, `inbox`, `findings`, `usage`),
  `live.log`, the registry (`<user data dir>/hallmark/repos.json`) and the
  daemon state file. Its only writes are the index catching up with the
  journal, which every command does (the index is a cache, ADR 0003).
  When there's no registry, it falls back to the repo you're standing in;
  a registered repo that is missing or not initialized is listed with its
  problem, never fatal.
- **State is folded, not stored.** The board comes from the index's
  folded items. The selected item's journal is folded again with
  `event::fold` and `findings::Ledger`, both now and at the replay cursor,
  so stepping through the journal shows the board and the findings as they
  were at that event. Folds are cached per selection and recomputed only
  when the cursor or the journal moves.
- **It refreshes on change.** A `notify` watcher on each repo's
  `<git-common-dir>/hallmark/` marks the repo stale; the app re-reads it
  once writes have been quiet for 250 ms, or after 1 s while they keep
  coming. Changes to the index, lock and temp files are ignored, so the
  app's own index sync doesn't wake it. A 2 s tick refreshes the clock and
  the daemon state, and, if the watcher couldn't start, re-reads every
  repo instead. The bell rings when the inbox grows.
- **Actions go through the commands.** `r`, `p` and `x` call
  `commands::flow::{ready, pull_back, drop}` in-process with the same
  argument structs as the CLI, so they take the same journal lock, check
  the same states and write the same events as `hall ready`,
  `hall pull-back` and `hall drop`. The reply's note, or the error code and
  message, shows in the status bar. `x` asks for the reason in the app;
  `esc` cancels. Nothing else writes.
- **Opening is never blocking.** `w` opens the worktree and `o` the PR
  with the system opener (`open`, `xdg-open`, `explorer`), spawned and
  reaped in the background; if the opener can't start, the status bar
  shows the `cd` command or the URL instead. `d` runs `git diff
  <base>...<sha>` at the selected round, from the worktree, or from the
  main checkout when the worktree is gone.
- **State and drawing are separate.** `tui::App` holds the state and
  handles keys (`handle_key` returns an `Effect`: none, quit, or open);
  `tui::ui::draw` renders an `&App`. Tests drive `App` without a terminal
  and snapshot ratatui's `TestBackend` buffer as text, with clock times,
  ages, the temp root and commit SHAs redacted.
- **The terminal is always restored.** `ratatui::try_init` enters raw
  mode and the alternate screen and installs a panic hook that restores
  the terminal before the panic prints; `run` restores it on every exit,
  error or not.

## Consequences

- Opening the app on another terminal, or after a restart, shows the same
  state, because there is no state of its own.
- A slow or broken repo can't take the app down; it shows the problem in
  that repo's place.
- Anything the app can do, an agent can do with the CLI, and the journal
  records it the same way.
- Answering questions, snoozing and editing budgets from the app are left
  for later; `hall answer`, `hall inbox` and `hall budget` do them.
