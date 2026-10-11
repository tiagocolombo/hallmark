# 0023. Packaging and releases

Status: Accepted, 2026-10-09

> **Note, 2026-10-10:** the `tiagocolombo/dev` tool and flake input are now
> `tiagocolombo/rig` (the input is `rig`, the spec is `rig.toml`). The decision
> below is unchanged; read `dev` as `rig` wherever it names the input.

> **Note, 2026-10-11:** the repository is now public. The actions in
> `release.yml` are pinned by commit through `github-action-commits` in
> `dist-workspace.toml`, and Dependabot skips the file (`exclude-paths`):
> its edits put the workflow out of step with the config, and `dist plan`
> then fails the release. Bump those pins in the config and run `dist
> generate`. The workflow is still never edited by hand.

## Context

ADR 0001 promised a single cargo-dist pipeline for all three operating
systems. Stage 3 needs `hall` to be installable without a Rust toolchain. It
also needs a Nix package, and a `hall setup claude` that installs everything
the plugin now contains, including the status line.

Two constraints shape this:

- `tiagocolombo/hallmark` is a **private** repository, and it stays private.
- The flake's only input, `tiagocolombo/dev`, is also private. It provides the
  dev shell everyone works in, so `nix develop` must keep working unchanged.

## Decision

### Releases: dist (cargo-dist) 0.31.0

The config is in `dist-workspace.toml`, plus `[profile.dist]` (release with thin
LTO) in `Cargo.toml`. `.github/workflows/release.yml` is generated from that
config by `dist generate`. Don't edit the workflow by hand.

| Target | Runner | Archive |
| --- | --- | --- |
| `aarch64-apple-darwin` | macos-14 | `.tar.xz` |
| `x86_64-apple-darwin` | macos-15-intel | `.tar.xz` |
| `aarch64-unknown-linux-gnu` | ubuntu-22.04-arm | `.tar.xz` |
| `x86_64-unknown-linux-gnu` | ubuntu-22.04 | `.tar.xz` |
| `x86_64-unknown-linux-musl` | ubuntu-22.04 (musl-tools) | `.tar.xz` |
| `x86_64-pc-windows-msvc` | windows-2022 | `.zip` |

- **Installers:**
  - `hallmark-installer.sh` and `hallmark-installer.ps1`, which install into
    `CARGO_HOME/bin` with no updater;
  - `hall.rb`, a Homebrew formula written for the tap
    `tiagocolombo/homebrew-tap`;
  - a sha256 file per archive, plus `sha256.sum`.
- **Trigger:** the workflow runs only when a version tag such as `v0.2.0` is
  pushed. `pr-run-mode = "skip"` keeps it off pull requests. CI (`ci.yml`) is
  unchanged.
- **Homebrew publishing is off.** The tap is configured, so the formula is
  built and attached to the release, but no job pushes it to the tap.
- **Dry-run only, so far.**
  - `dist plan` lists the artifacts above.
  - `dist build --artifacts=local --target=aarch64-apple-darwin` built
    `hallmark-aarch64-apple-darwin.tar.xz` and its `.sha256`.
  - `dist build --artifacts=global` built both installers and the formula.
  - The shell installer, pointed at those local artifacts with
    `HALLMARK_DOWNLOAD_URL=file://…`, installed a working `hall`.
  - Nothing has been tagged, released or published.

### The private repository

A release on a private repository is visible only to people with access. The
installer and formula URLs point at
`github.com/tiagocolombo/hallmark/releases/download/…`, which returns 404
without auth. These are the options, and none of them is chosen yet:

1. **Stay private, install through `gh`.** `gh release download` uses the
   person's login. Run the installer with `HALLMARK_DOWNLOAD_URL=file://<dir>`,
   or unpack the archive by hand. This works today, and docs/install.md
   documents it. The installers also send `HALLMARK_GITHUB_TOKEN` as a bearer
   token, but GitHub's browser download URLs for private repositories may not
   accept it, so don't rely on that.
2. **A public release mirror.** Publish only the binaries and installers to a
   public repository, such as `tiagocolombo/hallmark-releases`. Set dist's
   `hosting`/download URL to it, and push from the workflow with a token. The
   source stays private, and the curl, PowerShell and Homebrew one-liners work
   for everyone.
3. **Make the repository public.** This is the simplest, but it isn't our call
   to make here.

Homebrew needs option 2 or 3. A formula can't fetch private assets without a
custom download strategy. Private repositories also pay for Actions minutes:
macOS runners bill at 10x. The `ubuntu-22.04-arm` runner may not be available
to private repositories on every plan. If it isn't, build that target on a
larger runner or cross-compile it.

### Nix

`flake.nix` merges new outputs into `dev.lib.mkProject ./dev.toml`. The merge
uses `lib.recursiveUpdate`, so the dev shell, services and apps stay exactly as
they were. The new outputs are:

- `packages.<system>.{hall,default}` and `apps.<system>.{hall,default}`;
- `overlays.default`, which adds `pkgs.hallmark`.

The package is `nix/package.nix`, built with `rustPlatform.buildRustPackage`
and `cargoLock.lockFile = ../Cargo.lock`. It uses the nixpkgs `dev` already
pins (`dev.inputs.nixpkgs`), so the lock still holds one nixpkgs. Its source is
everything except build output, docs, CI and the Nix files.

The package runs no tests (`doCheck = false`). The suite drives real git repos
and reads fixtures, and CI already runs it on all three OSes. An install check
runs `hall --version`. Bundled SQLite compiles with the stdenv's C compiler.
`nix build .#default` builds it, and `./result/bin/hall --version` prints
`hall 0.1.0`.

**Limitation:** evaluating any output of the flake fetches the private `dev`
input, so someone without access to it can't use the flake, even just for the
package. `nix/default.nix` is the fallback. It needs only nixpkgs
(`nix-build nix`, `nix profile install --file nix`).

If the package ever needs a public flake, there are two ways to get one:

- a second flake in `nix/flake.nix` with only a `nixpkgs` input, used as
  `github:tiagocolombo/hallmark?dir=nix`;
- making `dev` public.

We haven't built either, because the repository is private anyway.

### `hall setup claude`

ADR 0009's flow stays: extract, `marketplace add`, `install`. The changes:

- The order is now `marketplace add`, `marketplace update`, `install`,
  `update`. The updates refresh an existing install and are allowed to fail.
- Setup deletes the old plugin directory before extracting, so files a newer
  plugin dropped don't linger.
- The reply lists every extracted file in `plugin_files`: skills, agents,
  hooks and schemas.
- `next` names the rest of the machine setup: `hall init`,
  `hall daemon start --at-login`, `hall notify test` and `hall doctor`.
- `--statusline` sets
  `"statusLine": {"type": "command", "command": "<abs path to hall> statusline"}`
  in Claude Code's user settings. The settings file is `HALL_CLAUDE_SETTINGS`,
  else `$CLAUDE_CONFIG_DIR/settings.json`, else `~/.claude/settings.json`.
  - Only that member's text is edited, so the rest of the file keeps its bytes
    and key order. The result is checked by parsing it back before it's
    written, through a temp file and a rename.
  - Setup refuses (exit 2, `USAGE`) to replace a status line that isn't
    Hallmark's unless `--force` is given. It always replaces a `hall statusline`
    from another path.
  - `--dry-run` reports the same plan or refusal and writes nothing.
  - The status line is checked before anything else runs, so a refusal changes
    nothing.

## Consequences

- **A real release** is `git tag v0.x.y && git push --tags`, once these are
  settled:
  - the private-repo option above;
  - `HOMEBREW_TAP_TOKEN` and `publish-jobs = ["homebrew"]`, if Homebrew is
    wanted, with the tap repository created first;
  - `homepage` in `[package]`, which dist warns about for the formula.
- **Changing targets or installers** means editing `dist-workspace.toml`, then
  running `dist generate` and `dist plan`. dist isn't in the dev shell. Run it
  with `nix shell nixpkgs#cargo-dist` inside `nix develop`.
- **Nix users** rebuild `hall` from source. There's no binary cache.
- **Adding a plugin file** needs no change to setup. `plugin::FILES` drives
  extraction and the reply, and `tests/setup.rs` checks that every file under
  `plugin/` is extracted.
