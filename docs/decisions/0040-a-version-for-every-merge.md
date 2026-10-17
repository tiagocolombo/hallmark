# 0040. A version for every merge; releases on demand

Status: Accepted, 2026-10-13. Amends 0023 and 0035.

## Context

ADR 0035 made release-plz keep one release PR open: merging it bumped the
version, tagged `vX.Y.Z`, and the tag ran `release.yml`, which builds the
binaries and creates the GitHub Release. The version only moved when someone
merged that PR, so `hall --version` named the same version across many
merges, and every bump was also a published release.

It also never bumped at all after `v0.1.0`. release-plz looks for the last
version in the cargo registry, and hallmark isn't on crates.io
(`publish = false` only skips the upload). Finding nothing, it took
`Cargo.toml`'s `0.1.0` for a first release that was already tagged, said
"the repository is already up-to-date" and opened no release PR, though
`feat` PRs had merged since.

We want the version to move with every merge to `main`, and a GitHub Release
only when we choose to publish one.

## Decision

- **Versions come from tags.** `git_only = true` in `release-plz.toml`:
  the last version is the highest `vX.Y.Z` tag, not the registry. In
  git-only mode release-plz never runs `cargo publish`, so `publish = false`
  is gone.
- **Every merge gets a version.** On a push to `main`, release-plz opens the
  release PR (the next version by semver from the conventional commits, and
  its `CHANGELOG.md` entry), and the workflow merges it at once with
  `gh pr merge --squash --admin`. That merge is another push to `main`,
  where release-plz tags `vX.Y.Z`. The "Protect main" ruleset wants an
  approving review, and nobody can approve a PR their own token opened, so
  the release PR is merged with the admin bypass of the `RELEASE_PLZ_TOKEN`
  owner. It changes only `Cargo.toml`, `Cargo.lock` and `CHANGELOG.md`, and
  main's CI runs on it after the merge.
- **How far a version moves.** While the version is `0.x`, a breaking
  change bumps the minor and everything else, `feat` included, bumps the
  patch: release-plz's `features_always_increment_minor` is off, as Cargo's
  semver rules want (`0.1.3` became `0.1.4` on the `feat` PR #55). ADR 0035
  said `feat` bumps the minor; that holds only from `1.0`. A new minor or
  major is set by hand: a PR sets `version` in `Cargo.toml`. With the
  version above the highest tag, release-plz takes it as already bumped
  ("local version > registry version. Only changelog will be updated" in
  `release_plz_core`'s updater), so the release PR keeps it, adds only its
  `CHANGELOG.md` entry, and its merge tags it.
- **The tag comes first.** The release-PR job runs after the tagging job,
  so on the push of a merged release PR the new tag exists before
  release-plz looks for changes since the last version, and nothing is
  bumped twice.
- **A release is published by hand.** `release.yml` no longer runs on tag
  pushes. It's a `workflow_dispatch` run on a tag:
  `gh workflow run release.yml --ref vX.Y.Z`, or Actions → Release → Run
  workflow with the tag picked. `github.ref_name` is then the tag, as it was
  on a push, so the dist steps are unchanged. The first step fails unless
  the ref is a `vX.Y.Z` tag. The workflow comes from the tag's commit, so
  tags made before this change can't be published this way.
- **Words.** A *version* is a `vX.Y.Z` tag on `main`, one per merge. A
  *release* is a GitHub Release, with binaries and installers, for the
  versions we publish. `hall update` reads tags, so it names versions: "The
  latest version is X". The SessionStart hook and `hall doctor` say a newer
  version exists, and `hall update` installs it (main always carries it).

## Consequences

- `hall --version`, `hall update` and the tags move with every merge.
  `CHANGELOG.md` gets an entry per merge.
- `main` gets two commits per merge: the change, then
  `chore: release vX.Y.Z`. CI runs on both.
- The installers (`releases/latest`) and the releases page can lag the
  latest version; they move when a release is published.
- Two PRs merged close together can share one version: a bump covers the
  commits release-plz saw when it ran.
- `RELEASE_PLZ_TOKEN` must belong to a repository admin, since its bypass
  merges the release PRs. Without the token, or when it expires, the
  Release-plz workflow fails on every push to `main` and versions stop
  moving.
