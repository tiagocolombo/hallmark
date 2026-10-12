# 0035. Versions from conventional commits

Status: Accepted, 2026-10-12

## Context

`Cargo.toml` has said `0.1.0` since the first commit. No tag was ever pushed,
so the release workflow (ADR 0023) never ran. `hall update` compared commits
on `main`, so it could only say "a different commit", never which version you
had or which one was new.

The repository asks for conventional commits (CLAUDE.md, CONTRIBUTING.md),
but PRs are squash-merged, and the shipper titled PRs with the item's title
("Configurable setup: hall config and hall setup --interactive"). Most of
`main` isn't conventional.

## Decision

### Conventional commits reach `main`

- **The shipper types PR titles.** An item's title is written for people.
  When the item's commits since its base are conventional, the shipper puts
  the most significant type in front of the title (`feat` > `fix` > `perf` >
  `refactor` > `revert` > `build` > `test` > `ci` > `docs` > `style` >
  `chore`), with `!` when any commit is breaking (`!` or a `BREAKING CHANGE:`
  footer). A title that is already conventional is kept, and so is the title
  of a branch whose commits aren't conventional, so other repositories see no
  change. This is `src/conventional.rs`, and it fills `{title}` in the ship
  command.
- **CI checks the PR title** (`.github/workflows/pr-title.yml`). GitHub uses
  the PR title for a squash merge of more than one commit, and the commit's
  subject for a squash merge of one, so the title is what `main` gets.

### release-plz makes the versions

- `release-plz.toml` and `.github/workflows/release-plz.yml`. On every push
  to `main`, release-plz opens or refreshes a **release PR** that bumps
  `Cargo.toml` by semver and writes `CHANGELOG.md`:
  - `feat` bumps the minor version;
  - `fix` and everything else bump the patch version;
  - a breaking change bumps the major version, or the minor while the
    version is `0.x`.
- **Merging the release PR tags `vX.Y.Z`**, and the tag runs `release.yml`
  as before: dist builds the binaries and creates the GitHub Release.
  release-plz creates no GitHub Release itself (`git_release_enable =
  false`) and publishes nothing to crates.io (`publish = false`).
- **The tag needs a token.** A tag pushed with the default `GITHUB_TOKEN`
  doesn't start other workflows. The jobs use the `RELEASE_PLZ_TOKEN` secret
  when it exists: a fine-grained token with contents and pull-requests write
  on this repository. Without it, the tag is still made, but `release.yml`
  doesn't run until the tag is pushed again by someone else. Repository
  settings must also allow GitHub Actions to create pull requests.

### `hall update` follows releases

- **By default it installs the latest release:** the highest `vX.Y.Z` tag of
  the source, read with `git ls-remote --tags`, with pre-releases left out.
  It runs `cargo install --git <source> --tag vX.Y.Z`. The installed hall is
  current when its version is that release's or newer: a build of `main`
  after `v0.3.0` says `0.3.0` until the next release PR merges.
- **`--branch <b>`** follows a branch's latest commit, as `hall update` did
  before. A source with no release yet falls back to `main`.
- **The reply names versions.** `installed_version` and `latest_version`,
  plus `tag`, beside the commits. `branch` is null when following releases.
- **A newer release is suggested.** Every release lookup is saved to
  `<user dir>/update.json`. The SessionStart hook and `hall doctor` read it
  without touching the network, and say "hall X is available (this is Y):
  run `hall update`". `hall doctor` adds `hall update` to `next`. Being
  behind doesn't count as unhealthy.
- **Freshness.** When the saved lookup is a day old, the SessionStart hook
  starts a detached `hall update --check` to refresh it, so the next session
  knows. It marks the lookup as checked first, so an offline machine tries
  once a day, not every session. `HALL_UPDATE_CHECK=off` turns this off, and
  the tests set it.

## Consequences

- Version numbers move with every release PR merged, and `hall --version`,
  `hall update` and the GitHub Release agree.
- Following releases is slower than following `main`: work reaches `hall
  update` users only when the release PR merges. `--branch main` is still
  there for people who want every commit.
- Non-conventional commits already on `main` count as patch-level changes
  in the first release PR's changelog.
- One more secret to keep (`RELEASE_PLZ_TOKEN`), and its expiry, if releases
  should build on their own.
