# 0035. Versions from conventional commits

Status: Accepted, 2026-10-12

## Context

`Cargo.toml` has said `0.1.0` since the first commit. No tag was ever pushed,
so the release workflow (ADR 0023) never ran. `hall update` compared commits
on `main`, so it could only say "a different commit", never which version you
had or which one was new, and nothing told you an update existed.

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
- **release-plz needs a token.** Both jobs run with the `RELEASE_PLZ_TOKEN`
  secret: a fine-grained token, made by a repository admin, with contents
  and pull-requests write on this repository. The default `GITHUB_TOKEN`
  can't stand in for it: the repository doesn't let GitHub Actions open pull
  requests, a release PR it opened wouldn't run CI (so the required checks
  never pass), the "Protect release tags" ruleset lets only admins create
  tags, and a tag it pushed wouldn't start `release.yml`. Without the
  secret, the jobs stop with an error that names it.

### `hall update` follows main, and names versions

- **It installs the latest commit on `main`**, as before (`--branch` picks
  another branch). Main carries the releases: every release is a `vX.Y.Z`
  tag on it, and the release PR that bumps `Cargo.toml` merges into it, so
  following main never misses one.
- **The reply names versions.** Beside the commits, `installed_version` is
  this hall's version, `latest_version` and `tag` are the highest release
  tagged in the source (read with `git ls-remote --tags`, pre-releases left
  out), and after an update `new_version` is what the new hall reports.
  Tags that can't be read don't stop the update; the version fields are
  then null.
- **A newer release is suggested.** Every release lookup is saved to
  `<user dir>/update.json`. The SessionStart hook and `hall doctor` read it
  without touching the network and, when the release is newer than this
  hall, say "hall X is available (this is Y): run `hall update`". `hall
  doctor` adds `hall update` to `next`. Being behind doesn't count as
  unhealthy. Commits on main without a new release aren't announced; `hall
  update --check` still reports them.
- **Freshness.** When the saved lookup is a day old, the SessionStart hook
  starts a detached `hall update --check` to refresh it, so the next session
  knows. It marks the lookup as checked first, so an offline machine tries
  once a day, not every session. `HALL_UPDATE_CHECK=off` turns this off, and
  the tests set it.

## Consequences

- Version numbers move with every release PR merged, and `hall --version`,
  `hall update` and the GitHub Release agree.
- `hall update` users get every commit on main, released or not. The
  version they see is the last release until the next release PR merges.
- Non-conventional commits already on `main` count as patch-level changes
  in the first release PR's changelog.
- One more secret to keep (`RELEASE_PLZ_TOKEN`), and its expiry: until it
  is set, and whenever it expires, the Release-plz workflow fails on every
  push to `main`.
