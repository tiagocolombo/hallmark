---
id: L-0014
kind: rule
scope: ["src/runner.rs"]
lens: correctness
source: ["H-0011"]
status: proposed
used: 0
---
# Rate-limit detection in runner::classify runs only when a run failed, and must read the result text rather than the raw stdout JSON.

A successful run's result text can discuss rate limits (H-0007's completed review did), so a completed run must never be classified as a rate-limit pause. A run counts as failed when is_error is set, the subtype starts with "error", terminal_reason is not "completed", or the output did not parse. For failed runs with no result field, scanning the whole stdout JSON is unsafe, because permission_denials[].tool_input can contain rate-limit words from a denied Bash command; H-0011/F1 flagged this and it was still open when the item closed. To check a change, confirm that a completed run with rate-limit wording in its result is not paused, and that a failed run without result does not match on denied tool inputs.
