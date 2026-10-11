---
id: L-0058
kind: rule
scope: ["src/commands/update.rs"]
lens: correctness
source: ["H-0025/F4"]
status: proposed
used: 0
---
# When cargo installs into a staging root, the .crates.toml and .crates2.json metadata must be carried to the real cargo root, or `cargo install --list` keeps reporting the old rev.

cargo writes install metadata into the --root it was given, so deleting the staging root after the swap discards the record that hallmark moved to a new rev. Tools such as cargo-update read that metadata and will keep offering updates. Check any change that uses a temporary cargo root for whether it copies or merges that metadata back. This was still open at merge.
