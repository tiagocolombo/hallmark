---
id: L-0055
kind: map
scope: ["src/commands/update.rs", "build.rs"]
source: ["H-0025"]
status: proposed
used: 0
---
# `hall update` checks main with `git ls-remote`, builds the new hall with `cargo install --git --rev` into a staging root, swaps the binary into the bin dir hall runs from, refreshes the plugin, then restarts the daemon.

build.rs records the build commit in HALL_COMMIT so the command can tell whether the installed hall is already the latest and skip with a message. The install goes to a staging root (`<root>/.hall-update`) and swap_in moves the built exe into place. Plugin refresh and daemon restart run after the swap, so reviewers should check each step's ordering and failure handling together.
