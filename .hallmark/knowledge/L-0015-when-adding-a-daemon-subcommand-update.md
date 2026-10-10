---
id: L-0015
kind: rule
scope: ["src/commands/daemon.rs", "src/cli.rs"]
lens: repo_rules
source: ["H-0011/F3"]
status: proposed
used: 0
---
# When adding a daemon subcommand, update the schema text, the README and DESIGN command lists, and the daemon status next hints in the same change.

The `hall schema daemon` action and changed text, the daemon command lists in README.md and docs/DESIGN.md, and the `next` hints printed by `hall daemon status` all name the daemon's subcommands. A new subcommand such as `resume` that leaves any of these stale sends agents and users the wrong way, for example toward `hall log` when a pause could be lifted by hand. H-0011/F3 flagged the missing updates; it was still open when the item closed, so check that these files are touched whenever the daemon CLI surface changes.
