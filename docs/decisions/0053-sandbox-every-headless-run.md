# 0053. Sandbox every headless run: reads stay in, commands run in Claude Code's sandbox

Status: Accepted, 2026-10-22. Amends 0012, 0013 and 0019.

## Context

Every headless run (ADR 0013) ran on the host as the person, with their
environment, network and files. Each run was steered (its own agent, no MCP
servers, read-only roles without the checkout's settings), but nothing kept
it in. Live probes on 2026-10-22 (Claude Code 2.1.308, macOS, `claude -p`,
a fake canary file) showed what that allowed:

- A bare `Read` rule, which every role had, read a file outside the run's
  directory. A bare `Write` rule with `acceptEdits`, which the fixer and the
  Delivery office's dev runs had, wrote one.
- `Bash(git diff:*)`, the reviewer's, ran `git diff --output=<any path>`.
- A gate command such as `cargo test` runs code the agent wrote. Run that way,
  a script wrote to `~` and `~/.zshrc`, read `~/.ssh`, the gh token and
  `~/.claude.json`, and reached the network. An allowlist matches a command
  line, not what the command does. Such code could also rewrite the main
  checkout's `.hallmark.toml`, which ADR 0012 keeps out of an item's reach.
- `claude_args` was appended to every run unchecked, so
  `--dangerously-skip-permissions` would pass through.

Claude Code's own sandbox closes these, once it is set up right. The probes
measured that too:

- `permissions.blockReadsOutsideWorkingDirectories` stops the file tools
  reading outside the run's directories, and `--add-dir` directories count
  as inside.
- With `sandbox.enabled`, commands write only the working directory, the
  temp dir and the worktree's git directory (Claude Code adds that one, and
  keeps its `hooks/` and `config` out). Writes to `~` and the main
  checkout's files fail. The network is closed but for allowed domains.
- Credentials stay readable to sandboxed commands until they are listed in
  `sandbox.filesystem.denyRead`.
- With the default `autoAllowBashIfSandboxed`, a sandboxed command that no
  `--allowedTools` rule matches runs anyway. It must be `false` to keep the
  allowlist.
- Globs in `allowWrite` (`hallmark-*`) don't match. Literal paths and `~`
  paths do.
- This repo's gates (`nix develop --command cargo test --all-targets`) pass
  in the sandbox with `~/.cache/nix` and `~/.cargo` writable and the
  nix-daemon socket allowed by its resolved path
  (`/private/var/run/nix-daemon.socket`).

## Decision

**Every run gets `--settings`** with:

- `permissions.blockReadsOutsideWorkingDirectories: true`;
- a `Read(<path>)` and `Read(<path>/**)` deny rule per `[sandbox] deny_read`
  path (an absolute path gets the `//` a rule needs);
- unless the sandbox is off, `sandbox`:
  - `enabled: true` and `failIfUnavailable: true`, so a run where the sandbox
    can't start (Linux or WSL2 without bubblewrap and socat) fails instead of
    running commands unconfined; `mode = "off"` is the explicit opt-out;
  - `autoAllowBashIfSandboxed: false` and `allowUnsandboxedCommands: false`;
  - `filesystem.allowWrite` and `denyRead` from `allow_write` and
    `deny_read`;
  - `network.allowedDomains`, `allowUnixSockets` and `allowLocalBinding`
    from `domains`, `unix_sockets` and `local_binding`.

The settings go inline, not in a file a run could edit before the next one.

**Every run also** passes `--disallowedTools WebFetch,WebSearch` (not
`--tools`: each run's `--allowedTools` already names what it may use without
asking, and `--tools` would also hide tools the agent definitions rely on, so
removing the two network tools is the restriction that matters) and doesn't
inherit `remove_env` (by default `SSH_AUTH_SOCK`, the GitHub tokens and the
AWS keys).

**`[sandbox] mode`:**

| Mode | What it does |
| --- | --- |
| `auto` (default) | The sandbox wherever Claude Code has one, leaving it out on native Windows. A run fails where it can't start. |
| `required` | Asks for it everywhere, native Windows included, and fails the run where it can't start. |
| `off` | Leaves it out. The permissions part, the web tools and `remove_env` stay. |

**Defaults.** `domains` is empty: no network for commands unless the repo
names hosts. `local_binding` is `true`, for tests that start a server.
`deny_read` lists the usual credential paths (`~/.ssh`, `~/.aws`, cloud CLIs,
`~/.config/gh`, `~/.docker/config.json`, `~/.gnupg`, `~/.kube`, `~/.netrc`,
`~/.npmrc`, Claude's own credentials).

**Edits stay in the worktree.** The fixer and the Delivery office lose their
bare `Edit` and `Write` rules. `acceptEdits` already allows edits inside the
run's directory, and only there (amends ADR 0019).

**`claude_args` can't loosen a run.**

- `.hallmark.toml` is refused (exit 2) when `claude_args` holds a flag that
  would loosen a run or bring in code outside the sandbox:
  - `--dangerously-skip-permissions` and `--allow-dangerously-skip-permissions`;
  - `--permission-mode` and `--permission-prompt-tool`;
  - `--settings` and `--setting-sources`;
  - `--add-dir`;
  - `--allowedTools`, `--allowed-tools` and `--tools`;
  - `--mcp-config` and `--plugin-dir` (their hooks and servers run outside
    the sandbox; Hallmark's own plugin goes through the runner).
- When the file comes from a linked worktree (the fallback ADR 0012 keeps for
  a main checkout without one), runs ignore its `claude_args` and `[sandbox]`
  and use the defaults (amends ADR 0012).

`runner::confinement` builds all of this, so `runner::request` and the
resume run share it (amends ADR 0013).

## Consequences

- A misled agent can no longer read the person's credentials with its tools
  or its commands. On macOS, Linux and WSL2 its commands can't write outside
  the worktree, temp and git directories, and can't reach the network beyond
  `domains`.
- A repo whose toolchain writes home caches, talks to a daemon or downloads
  packages names them in `[sandbox]`. This repo does (docs/configuration.md,
  "A toolchain in the sandbox").
- In `acceptEdits` runs, Claude Code also approves simple file commands inside
  the run's directory (`echo x > file`) without an allow rule. They run in
  the sandbox like any other command (checked live with the runner's own
  settings, 2026-10-22).
- A command blocked by the sandbox fails with "Operation not permitted". The
  fixer is told so and reports it rather than working around it.
- Signed commits need the signing agent's socket in `unix_sockets` and its
  variable kept out of `remove_env`, or no signing in item worktrees.
- Native Windows gets the permissions part only, unless `required`.
- Still outside the sandbox:
  - The gates the Assay Office runs itself, and a `fixer` engine that isn't
    `claude -p`. They run the branch's code as the person.
  - The worktree's git directory is writable from the sandbox, so a run can
    still write refs and the journal there. The push gate's check of the
    note against the journal (ADR 0045) is only as strong as that journal.
  - Hooks and the `claude` process itself.

  Running agents and gates in a container or VM, with only the worktree
  inside and pushing left to the host, would close these.
- Interactive sessions are the person's own. `.claude/settings.local.json`
  can turn the same sandbox on for them, and work in a sibling worktree then
  needs `/add-dir <worktree>`.
