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

The flake depends on a private input (`tiagocolombo/dev`, for the dev shell),
so evaluating it needs SSH access to that repo too. Without that access, build
from a clone with only nixpkgs:

```sh
nix-build nix                     # ./result/bin/hall
nix profile install --file nix
```

Overlay users can use `overlays.default`, which adds `pkgs.hallmark`.

## 2. Set up

Once per machine:

```sh
hall setup claude --statusline   # the Claude Code plugin, plus Hallmark's status line
hall daemon start --at-login     # the review loop, now and at every login
hall notify test                 # check your notification channels
```

`hall setup claude` writes the plugin (skills, agents, hooks, schemas) into
Hallmark's data directory and installs it at user level through a local
marketplace. Hook commands use the absolute path of the `hall` that ran setup.
To see the plan without doing anything, add `--dry-run`.

`--statusline` sets `statusLine` in `~/.claude/settings.json`. It keeps the
rest of the file as it was. If you already have a status line of your own, it
stops and changes nothing. Add `--force` to replace it.
`CLAUDE_CONFIG_DIR` is honored.

Then, once per repo:

```sh
cd your-repo
hall init
git add .hallmark.toml .hallmark/ CLAUDE.md && git commit -m "Adopt Hallmark"
hall doctor                      # checks everything and names the fix for each problem
```

Restart Claude Code so it loads the plugin.

## Upgrading

Upgrade the binary the same way you installed it: rerun the installer, use
`brew upgrade hall`, `cargo install … --locked --force`, or
`nix profile upgrade`. Then **rerun `hall setup claude`**. The plugin is
embedded in the binary, so this refreshes the skills, agents and hooks to
match. It also re-points the hooks and the status line if `hall` moved.
Restart the daemon (`hall daemon stop && hall daemon start --at-login`) and
Claude Code. `hall doctor` reports hooks that point at a stale `hall`.

## Windows notes

- Use the PowerShell installer or the `x86_64-pc-windows-msvc` zip. `hall`
  needs no bash, Python or Node. Hooks call `hall.exe` directly.
- `git` and `claude` must be on PATH. Git for Windows is fine.
- If `hall` lives under a path with spaces, setup quotes it in the hook and
  status line commands.
- `hall daemon start --at-login` registers the daemon with Task Scheduler.
- Moving `hall.exe` means rerunning `hall setup claude`.
