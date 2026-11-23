# 0067. aarch64 macOS releases build on macos-15

Status: Accepted, 2026-11-23. Supersedes the `aarch64-apple-darwin` runner in 0023.

## Context

dist 0.31 builds `aarch64-apple-darwin` on `macos-14`. GitHub has retired
that image: the v0.3.20 release run's job for the target was cancelled
within a second with "The configuration 'macos-14-arm64-us-default' is not
supported", so the host job was skipped and no GitHub Release was made.

## Decision

`[dist.github-custom-runners]` in `dist-workspace.toml` sets
`aarch64-apple-darwin = "macos-15"`. The release workflow reads its build
matrix from `dist plan` at run time, so `release.yml` is unchanged.

## Consequences

- The release workflow builds the target again.
- The binary is still built for the same target. A macos-15 runner links
  against a newer SDK, but the deployment target dist sets is unchanged.
- The release job reads the config at the tag it runs on, so a tag made
  before this change can't be rebuilt; the next release carries it.
