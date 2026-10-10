---
id: L-0028
kind: rule
scope: ["src/commands/watch.rs", "src/commands/status.rs", "src/commands/meta.rs", "tests/snapshots/**", "tests/foundation.rs"]
lens: repo_rules
source: ["H-0008/F5"]
status: proposed
used: 0
---
# Every JSON line a command streams, such as hall watch progress lines, needs a JsonSchema type served through hall schema, and every new reply shape needs an insta snapshot.

Deriving only Serialize leaves hall schema describing only the last line an agent receives. New snapshots should pin the human-facing text of actions, such as the does field, so that wording changes are visible in review. foundation.rs checks that the schema names resolve.
