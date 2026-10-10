---
id: L-0002
kind: map
scope: ["src/daemon/run.rs", "src/daemon/mod.rs"]
lens: first_principles
source: ["H-0007"]
status: proposed
used: 0
---
# The daemon runs one thread per registered repo, each on its own schedule, and the main loop collects their outcomes over a channel.

Each repo's next pass starts one poll interval after its own last pass ended, so a slow repo does not hold other repos' queues. The main loop re-reads the registry on its own timer, starts threads for new repos, and drains a channel of Outcomes into state.json (repo summaries, the per-repo pass count, and paused journals). Stop, heartbeats and pauses are handled in that loop, so changes to scheduling should be checked against all three.
