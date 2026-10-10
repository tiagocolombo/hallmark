---
id: L-0034
kind: rule
scope: ["src/tui/app.rs", "src/tui/ui.rs"]
lens: first_principles
source: ["H-0015/F3"]
status: proposed
used: 0
---
# Terminal-app keys that retry or override an escalated item must require a typed reason, because the librarian's digest records a retried item as the human's ruling on the escalation.

A one-key action in the TUI that calls flow::retry (or any path that writes a retried event on an escalated item) must open the same reason prompt as the other input-based actions and refuse an empty reason before the command runs. A canned reason such as 'retried from the terminal app' is recorded as the human's decision and cannot be corrected, since the journal is append-only. tests/tui.rs should cover the prompt and the refusal.
