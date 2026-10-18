# Installing Hallmark

Hallmark is one binary, `hall`, plus a Claude Code plugin that the binary
installs for you. You need `git` and [Claude Code](https://claude.com/claude-code)
(`claude`). `gh` is optional; it's used for GitHub issues and pull requests.

1. [Install `hall`](#1-install-hall), using one of the methods below.
2. [Set it up](#2-set-up) once per machine, then once per repo.

## 1. Install `hall`

### From a release

Every merge to `main` gets a version, but only some versions are published
as releases, so the latest release can be behind `main`
([ADR 0040](decisions/0040-a-version-for-every-merge.md)); `hall update`
then brings you up to date. Homebrew publishing is off (ADR 0023), so there
is no `brew` formula.

Each release has archives for macOS (Apple silicon and Intel), Linux (x86_64
and arm64, glibc; x86_64 musl) and Windows (x86_64), each with a `.sha256`
checksum. Each release also has the installer scripts below.

**macOS and Linux:**

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/tiagocolombo/hallmark/releases/latest/download/hallmark-installer.sh | sh
```

**Windows (PowerShell):**

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/tiagocolombo/hallmark/releases/latest/download/hallmark-installer.ps1 | iex"
```

The installers put `hall` in `~/.cargo/bin` (`%USERPROFILE%\.cargo\bin` on
Windows) and add that directory to your PATH. Set `HALLMARK_NO_MODIFY_PATH=1`
to leave PATH alone.

To install by hand, download your platform's archive from the
[releases page](https://github.com/tiagocolombo/hallmark/releases), check it
against its `.sha256`, unpack it and put `hall` on your PATH.

### From source

With Rust 1.88 or newer, and a C compiler for the bundled SQLite:

```sh
# Straight from git:
cargo install --git https://github.com/tiagocolombo/hallmark --locked

# Or from a clone:
git clone https://github.com/tiagocolombo/hallmark.git
cargo install --path hallmark --locked
```

### With Nix

From a clone, with only nixpkgs:

```sh
nix-build nix                     # ./result/bin/hall
nix profile install --file nix
```

The flake (`nix build .#hall`) also builds `hall`, but it takes the dev shell
from a private input (`tiagocolombo/rig`), so evaluating it needs access to that
repository. Use the `nix` directory above instead.

Overlay users can use `overlays.default`, which adds `pkgs.hallmark`.

## 2. Set up

In the repository you want reviewed:

```sh
hall setup
```

That one command does every step and reports each one; it is safe to repeat:

1. **`hall` on your PATH.** Hooks and the daemon use its absolute path, but you
   type `hall`. When its directory isn't on PATH (`cargo install` into
   `~/.cargo/bin` on a machine whose Rust comes from Nix), setup links it into
   `~/.local/bin` if that is on PATH, or tells you the line to add to your
   shell profile. `hall doctor` checks it too.
2. **The Claude Code plugin** (skills, agents, hooks, schemas), written into
   Hallmark's data directory and installed at user level through a local
   marketplace, with hook commands that use the absolute path of this `hall`.
3. **The status line**: `statusLine` in `~/.claude/settings.json` points at
   `hall statusline`. The rest of the file is kept as it was. A status line of
   your own is kept too: that step is skipped and `--force` replaces it.
   `CLAUDE_CONFIG_DIR` is honored.
4. **The repo** (`hall init`): `.hallmark.toml` from the gates it detects, a stub
   `.hallmark/review.md`, a CLAUDE.md section, and the journal. It tells you what
   to commit (`git add .hallmark.toml .hallmark/ CLAUDE.md && git commit -m
   "Adopt Hallmark"`).
5. **The daemon**, started now and registered to start at login, with the
   PATH you ran `hall setup` from. If `claude` or `gh` aren't on that PATH,
   set `tools.claude` and `tools.gh` (`hall config set --user tools.gh
   <path>`). If your gates need a toolchain shell such as `nix develop`, wrap
   them (see [Configuration](configuration.md#gates-that-need-a-toolchain-shell)).
6. **A phone topic**: a private, random ntfy topic, if you have no phone
   channel yet, with how to subscribe.
7. **`hall doctor`**, whose fixes go in `next`.

**`hall setup --interactive`** (or `-i`) asks first. It covers the base
branch, the gates and the shell they run in (`nix develop`, devbox, mise,
direnv), how work is shipped, models and budget caps, where `claude` and `gh`
are, notifications and the daemon. Then it offers every other setting. It
writes nothing until you confirm, and then runs the steps above. Every answer
can also be set later with `hall config`; see
[Configuration](configuration.md#changing-settings).

`--dry-run` shows the plan without doing anything; `--no-statusline`,
`--no-daemon` and `--no-notify` skip a step. Each step also has its own command
(`hall setup claude [--statusline]`, `hall init`, `hall daemon start
--at-login`, `hall notify test`, `hall doctor`).

Restart Claude Code so it loads the plugin.

**In Hallmark's own repository**, `rig install` does all of it: it runs
`cargo install --path . --locked` and then `hall setup` from the installed copy.

## Upgrading

### `hall update`

```sh
hall update            # install the latest main, unless this hall is it
hall update --check    # only say whether an update is available
```

`hall --version` shows the version and the commit this `hall` was built from.
`hall update` asks the repository for the latest commit on `main` with
`git ls-remote`, and when that isn't the commit this `hall` was built from, it
runs `cargo install --git … --rev <that commit> --locked --force` into the
`bin` directory this `hall` runs from. When the commits match, it changes
nothing and says so. Then it has the new `hall` rewrite the Claude Code plugin
(if you installed it) and restarts a running daemon on the new binary. A
daemon in the middle of a run (a review, a fix) finishes that run on the old
binary first: `hall update` says so, and you run `hall daemon start` once
`hall daemon status` shows it has exited. Restart Claude Code afterwards.

Main carries the versions: every merge is tagged `vX.Y.Z` on it, versioned
by semver from conventional commits
([ADR 0035](decisions/0035-semver-from-conventional-commits.md),
[ADR 0040](decisions/0040-a-version-for-every-merge.md)). `hall update`
reads the tags too, and names your version, the latest version and the version
it installed. It remembers the latest version, and when it is newer than your
`hall`, `hall doctor` and the start of each Claude Code session say so. A
session starts a background check at most once a day; `HALL_UPDATE_CHECK=off`
turns that off.

It needs `cargo` (or `tools.cargo` in your user config, or `HALL_CARGO`) and git access to the repository: git and
cargo both use your git credentials, and never prompt for them. `--source`
(or `HALL_UPDATE_SOURCE`) installs from another repository, such as a fork or
an SSH URL, and `--branch` from another branch. A `hall` installed with Nix,
or run from a build directory, is left to the tool that installed it. A `hall`
built without git (a crate tarball, the Nix package) doesn't know its commit,
so `hall update` always installs.

### By hand

Upgrade the binary the same way you installed it: rerun the installer, use
`cargo install … --locked --force`, `nix profile upgrade`,
or `rig install` in Hallmark's repo. Then **rerun `hall setup`**. The plugin is
embedded in the binary, so this refreshes the skills, agents and hooks to
match, re-points the hooks and the status line if `hall` moved, and restarts
the daemon when it runs an older `hall`. Then restart Claude Code.

### Upgrade notes

- **Gates have their own timeout.** They no longer run under
  `[budget.review] timeout`: each stops at `gates_timeout` (default 20m) or its
  own `timeout`. If you raised the review timeout for a slow gate, run
  `hall config set gates_timeout <value>`; `hall doctor` warns when it finds
  that. See [configuration](configuration.md).

## Windows notes

- Use the PowerShell installer or the `x86_64-pc-windows-msvc` zip. `hall`
  needs no bash, Python or Node. Hooks call `hall.exe` directly.
- `git` and `claude` must be on PATH. Git for Windows is fine.
- If `hall` lives under a path with spaces, setup quotes it in the hook and
  status line commands.
- `hall daemon start --at-login` registers the daemon with Task Scheduler.
- Moving `hall.exe` means rerunning `hall setup claude`.
