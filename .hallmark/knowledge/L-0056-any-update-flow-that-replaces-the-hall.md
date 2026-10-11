---
id: L-0056
kind: rule
scope: ["src/commands/update.rs"]
lens: security_tenancy
source: ["H-0025/F1"]
status: proposed
used: 0
---
# Any update flow that replaces the hall binary must keep an exe at the absolute path hooks.json calls until the new one is ready, because a missing binary makes gate hooks fail open.

hooks.json calls the hall exe by absolute path, so while the binary is moved aside during a cargo build, every pre-tool-use hook reports 'not found' and Claude Code treats the non-2 exit as allow. On Windows the running exe also cannot be overwritten, so the old one must be renamed aside and restored if the final rename fails. Flag any change that removes or renames the binary before the replacement is confirmed to exist.
