# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/tiagocolombo/hallmark/compare/v0.1.0...v0.1.1) - 2026-10-13

### Added

- terminal app explains what waits and offers only the actions that fit ([#53](https://github.com/tiagocolombo/hallmark/pull/53))
- PR descriptions: What, Why and How; the review goes in comments ([#52](https://github.com/tiagocolombo/hallmark/pull/52))
- Extensions: Matt Pocock's skills for Hallmark's roles ([#51](https://github.com/tiagocolombo/hallmark/pull/51))

### Fixed

- Auto-release every merge to main ([#54](https://github.com/tiagocolombo/hallmark/pull/54))

## [0.1.0](https://github.com/tiagocolombo/hallmark/releases/tag/v0.1.0) - 2026-10-12

### Added

- Semver releases from conventional commits; hall update shows versions ([#46](https://github.com/tiagocolombo/hallmark/pull/46))
- a Delivery lane on the TUI board for PRs the Delivery office watches ([#31](https://github.com/tiagocolombo/hallmark/pull/31))
- hall budget --grant lets the loop go past the daily cap today ([#17](https://github.com/tiagocolombo/hallmark/pull/17))
- hall setup puts hall on PATH, and hall doctor checks it
- hall setup does every step in one command
- hall hud, one call for the Claude Code heads-up display
- model fallback per role, and gh on the ship command's PATH
- Hallmark's icon on desktop notifications
- hall retry releases a held item; intake passes StructuredOutput in allowed tools
- budgets, resume on answer, notifications and the status line
- *(library)* learnings, base-branch store, FTS5 and qmd search, briefings, librarian and hall library
- the shipper, hall export and hall history
- Front Desk intake splits requests into parallel parts, hall merge, follow-up items
- *(assayer)* checks, the principal engineer's review and hall verdict
- *(tui)* the terminal app: board, journal replay, findings, history, live, usage and repos
- hall setup claude --statusline, full plugin extraction and next steps
- the daemon loop, start/stop/status, pauses and the watchman
- hall fixed and the fixer worker (Repair Shop)
- runner::request and budget Stop::progress for every worker
- Stage 2 foundation: runner, findings, leases, typed config and command stubs
- hall binary, agent contract, journal, hooks and plugin skeleton

### Fixed

- the Delivery office's conflict run can finish the rebase ([#30](https://github.com/tiagocolombo/hallmark/pull/30))
- a hallmarked update whose PR merged with it closes instead of waiting for you ([#28](https://github.com/tiagocolombo/hallmark/pull/28))
- a completed run that mentions rate limits doesn't pause the daemon ([#7](https://github.com/tiagocolombo/hallmark/pull/7))
- *(doctor)* check that the daemon runs
- pass shell commands to cmd.exe verbatim on Windows
- polish from the live end-to-end run
- isolate every headless run and count its usage once
- *(library)* let --json-schema runs work live: no $schema key, StructuredOutput in the librarian's tools
- *(assayer)* hall findings gives check logs relative to the item dir
- *(tui)* ring the bell only after a redraw
- make the intake plan work with real claude -p runs
- the fixer finds hall on PATH and can fill its schema
- *(assayer)* structured output for the principal engineer, found live
- *(test)* use real directories in the gate path test so it passes on Windows
- let auto-accepting sessions edit their item's worktree

### Other

- test-first review, repo skills and the name on the site and README ([#50](https://github.com/tiagocolombo/hallmark/pull/50))
- Test-first, principal-level review skills ([#44](https://github.com/tiagocolombo/hallmark/pull/44))
- Repo skill overrides from .hallmark/skills ([#45](https://github.com/tiagocolombo/hallmark/pull/45))
- Index heals item rows an older hall folded without every event ([#43](https://github.com/tiagocolombo/hallmark/pull/43))
- serve the landing page at hallmark.colombo.dev ([#42](https://github.com/tiagocolombo/hallmark/pull/42))
- Configurable setup: hall config and hall setup --interactive ([#39](https://github.com/tiagocolombo/hallmark/pull/39))
- Delivery watches PRs closely ([#38](https://github.com/tiagocolombo/hallmark/pull/38))
- Fresh base for new items; only escalations reach your office ([#29](https://github.com/tiagocolombo/hallmark/pull/29))
- Professional public README, contributing, security and docs ([#33](https://github.com/tiagocolombo/hallmark/pull/33))
- Old hall processes never clobber the index; the terminal app restarts on update ([#32](https://github.com/tiagocolombo/hallmark/pull/32))
- Your office: drop an entry's item, or every item waiting ([#26](https://github.com/tiagocolombo/hallmark/pull/26))
- Delivery office, rebase before PR, scoped Hallmark rules, inbox clean ([#25](https://github.com/tiagocolombo/hallmark/pull/25))
- Adopt open PRs: reopen shipped items for rebases and conflict fixes ([#20](https://github.com/tiagocolombo/hallmark/pull/20))
- hall update: install the latest hall from main ([#23](https://github.com/tiagocolombo/hallmark/pull/23))
- Your office: read and act on every entry; the Library never waits for you ([#19](https://github.com/tiagocolombo/hallmark/pull/19))
- Keep the Library local: learnings live with the journal, never in the repo ([#15](https://github.com/tiagocolombo/hallmark/pull/15))
- Override a budget hold without changing the budget ([#14](https://github.com/tiagocolombo/hallmark/pull/14))
- Budgets weigh tokens by price; hall continue moves an item on ([#13](https://github.com/tiagocolombo/hallmark/pull/13))
- Run each phase in parallel, with configurable slots ([#9](https://github.com/tiagocolombo/hallmark/pull/9))
- what each item is doing now, and what waits for you ([#11](https://github.com/tiagocolombo/hallmark/pull/11))
- Watch shipped PRs' CI; stack follow-ups on open PRs ([#6](https://github.com/tiagocolombo/hallmark/pull/6))
- Each repo runs on its own schedule in the daemon ([#5](https://github.com/tiagocolombo/hallmark/pull/5))
- Migrate from dev to rig
- README, DESIGN.md and the Stage 2 end-to-end run
- workstream E, the Library
- workstream H, intake, parallel parts and follow-ups
- workstream D, budgets, resume and notifications
- *(tui)* drive the app without a terminal and snapshot every view; ADR 0022
- *(library)* search, briefing, librarian, review and retirement; librarian prompt and schema
- *(assayer)* checks, reviews, verdicts and failures with the fake runner
- daemon passes, pauses, watchman and start/stop/status; ADR 0017
- ADR 0024 and the skill's parallel parts flow
- configure dist releases for macOS, Linux and Windows
- add the interactive design page and explain how Hallmark works
- README, condensed design, ADRs and contributor commands
- fmt, clippy and a test matrix on macOS, Linux and Windows
- drive hall against temporary git repos, hooks with captured inputs, insta snapshots
- record Step 0 spikes with real claude -p and hook samples
- set up the Nix dev shell for Rust
