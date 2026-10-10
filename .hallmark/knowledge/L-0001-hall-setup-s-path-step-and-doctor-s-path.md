---
id: L-0001
kind: map
scope: ["src/commands/setup.rs", "src/commands/doctor.rs"]
lens: correctness
source: ["H-0001"]
status: proposed
used: 0
---
# hall setup's path_step and doctor's path check together decide whether the installed hall is reachable from the shell.

path_step in setup.rs compares the hall that the shell finds on PATH with the running binary. When they differ, it symlinks the binary into ~/.local/bin if that directory is on PATH, and otherwise prints a shell profile line. doctor.rs has a matching `path` check that reports the failure and suggests `hall setup` as the fix. Reviews should check that the link-or-warn logic, the doctor check, and the tests in tests/setup.rs still agree on which directory is reached, and on what counts as shadowed.
