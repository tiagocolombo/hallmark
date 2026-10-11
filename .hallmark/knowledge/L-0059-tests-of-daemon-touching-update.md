---
id: L-0059
kind: rule
scope: ["tests/update.rs", "tests/daemon.rs"]
lens: evidence
source: ["H-0025/F2"]
status: proposed
used: 0
---
# Tests of daemon-touching update behaviour must start a real daemon with the fake runner, not run with no daemon, so a regression in stop/start/pid handling fails a test.

Tests that run with no daemon only exercise the skipped branch of restart_daemon and cannot catch a stop that stops reporting running. The fixed test starts a daemon using the same env as tests/daemon.rs, runs the update, and asserts the step is done and the pid changed. Require a similar test for any change to restart logic.
