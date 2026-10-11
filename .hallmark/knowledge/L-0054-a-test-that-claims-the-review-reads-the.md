---
id: L-0054
kind: rule
scope: ["tests/assayer.rs", "tests/refresh.rs", "tests/common/mod.rs"]
lens: evidence
source: ["H-0023/F4"]
status: proposed
used: 0
---
# A test that claims the review reads the whole change after a base merge must go through prompt::prepare, because helpers like c_support::approve skip it.

Tests that approve a conflict resolution through the approve helper append checks and a verdict without building the review prompt, so they cannot show the review's diff range. The review-after-base-merge behaviour needs an assayer test that calls prompt::prepare and asserts the whole-change heading, that the original and fix markers appear in the diff, that the main-only marker does not, and that the new base sha is used. Without it, removing the merge-base ancestor filter in prompt.rs still passes the suite.
