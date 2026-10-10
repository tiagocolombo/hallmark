---
id: L-0029
kind: rule
scope: ["src/**", "src/tui/**", "src/commands/hud.rs", "src/hooks.rs"]
lens: first_principles
source: ["H-0008/F4"]
status: proposed
used: 0
---
# When a request says several surfaces show the same view, check each surface calls the shared function, not only the one the author implemented first.

In H-0008 the HUD gained the now field but the terminal app did not call the now module at all, and the gap only surfaced in review. For any change that adds a derived field, grep for every renderer, the HUD, the TUI and the hooks, and confirm each reads it.
