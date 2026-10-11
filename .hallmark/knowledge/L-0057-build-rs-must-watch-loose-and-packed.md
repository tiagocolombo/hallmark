---
id: L-0057
kind: rule
scope: ["build.rs"]
lens: correctness
source: ["H-0025/F3"]
status: proposed
used: 0
---
# build.rs must watch loose and packed refs for the current branch, including branch names containing slashes, or HALL_COMMIT goes stale after a `git gc` packs refs.

If the watched ref file is missing because refs were packed, cargo never reruns build.rs when the next commit creates the loose ref, so `hall --version` keeps the old commit. Filtering out non-existent paths drops the file that matters. Git pack-refs --prune also removes empty parent directories of slashed branch names, so the parent path must be watched too. This was still open at merge, so check whether it was fixed before relying on it.
