---
id: L-0019
kind: rule
scope: ["src/daemon/run.rs", "src/daemon/mod.rs"]
lens: operability
source: ["H-0009/F4"]
status: proposed
used: 0
---
# A pause raised by one job must be journaled in every registered repo, not only the repo that triggered it, and unpaused must go to the same journals.

ADR 0017 requires pauses to appear in every repository's journal so that hall log explains why other repos' workers stopped. The daemon opens each target's Hall when the job begins, stores those journals, and settle writes unpaused to them. Check this whenever pause or unpause handling changes.
