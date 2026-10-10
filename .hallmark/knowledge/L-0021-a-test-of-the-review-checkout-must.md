---
id: L-0021
kind: rule
scope: ["tests/assayer.rs", "src/assayer/review.rs"]
lens: evidence
source: ["H-0009/F6"]
status: proposed
used: 0
---
# A test of the review checkout must assert that the reviewer's cwd HEAD equals the frozen commit sha, not only the directory name and cleanup.

Checking the directory name and removal cannot catch a checkout at the wrong commit or a stale checkout that was not reset. Have the fake runner record git rev-parse HEAD from its cwd and assert it equals the sha. Keep comments in step with what the test checks.
