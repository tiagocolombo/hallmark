# Installing Hallmark

Hallmark is one binary, `hall`, plus a Claude Code plugin that the binary
installs for you. You need `git` and [Claude Code](https://claude.com/claude-code)
(`claude`). `gh` is optional; it's used for GitHub issues and pull requests.

1. [Install `hall`](#1-install-hall), using one of the methods below.
2. [Set it up](#2-set-up) once per machine, then once per repo.

> **The repository is private.** Release downloads and `cargo install --git`
> need GitHub access to `tiagocolombo/hallmark`. Each method below says how.
> See [ADR 0023](decisions/0023-packaging-and-releases.md).

## 1. Install `hall`

### From a release

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

**Homebrew:**

```sh
brew install tiagocolombo/tap/hall
```

The installers put `hall` in `~/.cargo/bin` (`%USERPROFILE%\.cargo\bin` on
Windows) and add that directory to your PATH. Set `HALLMARK_NO_MODIFY_PATH=1`
to leave PATH alone.

**While the repository is private,** the URLs above return 404 unless you're
signed in. The Homebrew tap isn't published yet. Download through `gh`
instead, which uses your login:

```sh
mkdir hall-release && cd hall-release
gh release download --repo tiagocolombo/hallmark \
  --pattern 'hallmark-installer.sh' --pattern 'hallmark-*.tar.xz*'
HALLMARK_DOWNLOAD_URL="file://$PWD" sh hallmark-installer.sh
```

Alternatively, download your platform's archive
(`gh release download --repo tiagocolombo/hallmark --pattern 'hallmark-aarch64-apple-darwin.tar.xz'`),
check it against its `.sha256`, unpack it and put `hall` on your PATH.

### From source

With Rust 1.88 or newer, and a C compiler for the bundled SQLite:

```sh
# Clone, then build (uses your existing git access):
git clone git@github.com:tiagocolombo/hallmark.git
cargo install --path hallmark --locked

# Or straight from git, over SSH:
CARGO_NET_GIT_FETCH_WITH_CLI=true \
  cargo install --git ssh://git@github.com/tiagocolombo/hallmark.git --locked
```

`CARGO_NET_GIT_FETCH_WITH_CLI=true` makes cargo use your `git` and SSH key. If
you use HTTPS, use a token instead:
`cargo install --git https://<token>@github.com/tiagocolombo/hallmark.git --locked`,
with a fine-grained token that has read access to the repository's contents.

### With Nix

```sh
nix profile install git+ssh://git@github.com/tiagocolombo/hallmark   # installs hall
nix run git+ssh://git@github.com/tiagocolombo/hallmark -- --version  # runs it once
nix build .#hall                                                     # from a clone: ./result/bin/hall
```

The flake depends on a private input (`tiagocolombo/rig`, for the dev shell),
so evaluating it needs SSH access to that repo too. Without that access, build
from a clone with only nixpkgs:

```sh
nix-build nix                     # ./result/bin/hall
nix profile install --file nix
```

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
   PATH you ran `hall setup` from. Run it from the shell your project builds in
   (inside `nix develop` for a Nix project), so the gates and `gh` are found.
6. **A phone topic**: a private, random ntfy topic, if you have no phone
   channel yet, with how to subscribe.
7. **`hall doctor`**, whose fixes go in `next`.

`--dry-run` shows the plan without doing anything; `--no-statusline`,
`--no-daemon` and `--no-notify` skip a step. Each step also has its own command
(`hall setup claude [--statusline]`, `hall init`, `hall daemon start
--at-login`, `hall notify test`, `hall doctor`).

Restart Claude Code so it loads the plugin.

**In Hallmark's own repository**, `rig install` does all of it: it runs
`cargo install --path . --locked` and then `hall setup` from the installed copy.

## Upgrading

Upgrade the binary the same way you installed it: rerun the installer, use
`brew upgrade hall`, `cargo install … --locked --force`, `nix profile upgrade`,
or `rig install` in Hallmark's repo. Then **rerun `hall setup`**. The plugin is
embedded in the binary, so this refreshes the skills, agents and hooks to
match, re-points the hooks and the status line if `hall` moved, and restarts
the daemon when it runs an older `hall`. Then restart Claude Code.

## Windows notes

- Use the PowerShell installer or the `x86_64-pc-windows-msvc` zip. `hall`
  needs no bash, Python or Node. Hooks call `hall.exe` directly.
- `git` and `claude` must be on PATH. Git for Windows is fine.
- If `hall` lives under a path with spaces, setup quotes it in the hook and
  status line commands.
- `hall daemon start --at-login` registers the daemon with Task Scheduler.
- Moving `hall.exe` means rerunning `hall setup claude`.
