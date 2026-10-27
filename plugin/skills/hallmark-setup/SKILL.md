---
name: hallmark-setup
description: Configure Hallmark for this person and repo with `hall config` - gates and the shell they run in, how worktrees are made and work is shipped, models, budgets, where claude and gh live, notifications. Use when they say "set up Hallmark", "configure hallmark", "change the gates", "use devbox/nix/mise for the checks", "change the ship command", "set a budget", "hall can't find gh", or ask what a Hallmark setting does.
---

# Setting Hallmark up

Every Hallmark setting can be read and changed with `hall config`. You never
edit `.hallmark.toml` or the user config by hand. `hall config` keeps their
comments and checks each value before it writes. Don't run
`hall setup --interactive`: it asks its questions on a terminal, and you
can't answer them. Ask the person yourself, then set what they choose. `hall
init` and the first `hall setup` ask the same questions on a terminal; from
a session they don't (there is no terminal), and `--defaults` says so
explicitly.

## Look first

```sh
hall config list --json          # every key: value, source, kind, help, allowed values
hall config list gates --json    # one key, or every key under a prefix (budget, notify)
```

`source` says where a value comes from:

- `default`: Hallmark's built-in value;
- `repo`: this repo's `.hallmark.toml`;
- `user`: the person's user config;
- `env`: a `HALL_*` variable;
- `path`: a tool found on PATH.

Detect before you ask. Read `package.json`, `Cargo.toml`, `Makefile`,
`justfile`, `flake.nix`, `devbox.json`, `mise.toml` and `.envrc`, then propose
concrete values.

## Ask, in this order, one topic at a time

Propose a value and let the person confirm or change it. Skip anything they
are happy with.

1. **Base branch** (`base`): what reviews compare against.
2. **Gates** (`gates`): the checks every change must pass before review, in
   order. Prefer the repo's own commands (`npm test`, `cargo test`,
   `just lint`). Offer a test-strength gate too, but only when its tool is
   installed and the person accepts that it is slow: mutation testing of the
   changed code (`git diff <base>...HEAD > mutants.diff && cargo mutants
   --in-diff mutants.diff`, PIT's `scmMutationCoverage`), or a coverage floor
   (`cargo llvm-cov --fail-under-lines 90`,
   `pytest --cov=<package> --cov-fail-under=90`). Gates get no base branch or
   diff, and are killed after `gates_timeout` (20 minutes by default), so a
   mutation gate needs its own limit: the table form
   `{ cmd = "<gate>", timeout = "60m" }` in the `gates` list, set with
   `hall config set gates '[{ cmd = "cargo mutants …", timeout = "60m" }, "cargo test"]'`.
3. **The gates' shell.** The daemon runs gates with `sh -c`, outside any shell
   the person entered. If the toolchain comes from `nix develop`, devbox, mise
   or direnv, wrap each gate:
   - `nix develop --command <gate>`
   - `devbox run -- <gate>`
   - `mise exec -- <gate>`
   - `direnv exec . <gate>` (only if the `.envrc` is allowed at every
     worktree path, which direnv requires)

   Offer a wrapper only if its program is installed (`command -v`).

   The person may name another wrapper of their own.
4. **Capture** (`capture.mode`): what happens when you edit code outside an
   item. `gate` denies the edit and explains; `ask` asks the person; `off`
   means only `/hallmark` starts items.
5. **Worktrees and shipping**, only if they use their own tooling:
   - `worktree`, with placeholders `{repo} {id} {branch} {slug} {base}`;
   - `branch`, with `{id} {slug}`;
   - `ship`, with `{branch} {id} {base} {title} {body_file} {review_file} {issue}`
     (`{body_file}` is the What, Why, How, Testing and How to revert description);
   - `update`, with those of `ship`, plus `{pr}` and `{pr_head}`;
   - `review_comment`, which posts each review as a PR comment, with `{pr}
     {review_file} {branch} {id} {base}`; empty for none.
6. **Models and budget**:
   - `budget.review_model`, `budget.fix_model`, `budget.intake_model` and
     `budget.librarian_model`;
   - `budget.daily` and `budget.per_item`: tokens such as `4M` or `800k`, or
     dollars such as `$15`. They default to `4M` and `800k`; `none` turns a
     cap off (unsetting the key brings the default back);
   - `budget.<role>.max_usd`, the cap on one run in dollars (role: review,
     fix, intake, librarian). Unset, it defaults to 6, 2, 0.10 and 0.20;
     `none` lifts it;
   - `max_check_failures`: failed checks in a row before an item is held
     for them (default 3).
7. **Merging** (`[merge]`, ADR 0048): whether shipped PRs merge without
   them. `merge.mode` is `off` (they merge) or `auto` (the merger merges each
   ready PR, one at a time, in dependency order, and brings each PR up to
   date only at its turn, ADR 0050). Ask before `auto`: it is the step from
   a lit loop to a dark one.
   - `merge.command`, with `{pr} {pr_head} {branch} {base} {id} {title}`.
     When the base branch requires an approving review, the PR's author
     can't give it: suggest `--admin` only if they own the repo or may bypass
     the rule.
   - `merge.require_ci`: `false` merges without waiting for CI. With
     `--admin` that includes red CI; say so.
   - `merge.update` (ADR 0051): `files` (default) rebases a PR whenever the
     base moved on a file it touches, so CI tests exactly what lands, one PR
     after another on shared files. `conflicts` rebases only when GitHub says
     it conflicts, and merges clean, green PRs as they are: much faster when
     most PRs share files (docs), and safe only if the base's CI runs on
     every push. Either way the merger stops when the base's tip is red (a
     fix for it is merged by hand), and doesn't merge while the tip's checks
     are still running.
   - `merge.lit`: paths a person merges, as globs (`.github/`, release
     settings). Such a PR waits in the inbox, and `hall retry` lets it merge.
   - `merge.one_approval` and `merge.two_approvals`: paths that need that
     many people's approvals (`hall retry` counts as one, members' approving
     reviews on GitHub the rest) before the merger merges the PR.
8. **The sandbox** (`[sandbox]`, ADR 0053): every headless run is confined.
   Its file tools read only inside the run's directories and never
   `deny_read`. Its commands run in Claude Code's sandbox, with no network
   and no writes outside the worktree, temp and git directories. If the
   gates need more, add only that:
   - `sandbox.allow_write`: caches the toolchain writes (`~/.cargo`,
     `~/.cache/nix`, `~/.npm`). Literal or `~` paths; globs don't match.
   - `sandbox.domains`: hosts it downloads from (`crates.io`,
     `registry.npmjs.org`).
   - `sandbox.unix_sockets`: daemons it talks to. For Nix on macOS,
     `/private/var/run/nix-daemon.socket` (the resolved path).
   - `sandbox.mode`: `auto` (default), `required` (fail where there's no
     sandbox), or `off`. Ask before `off`.

   `claude_args` can't hold flags that loosen a run (`--settings`,
   `--permission-mode`, `--dangerously-skip-permissions`, …): the file is
   refused.
9. **Tools**, user scope: `tools.claude` and `tools.gh` when they aren't on
   the PATH the daemon gets. Use absolute paths.
10. **Notifications**, user scope: `notify.desktop`, `notify.phone`
   (`ntfy://ntfy.sh/<random topic>` or `telegram://<token>@<chat>`),
   `notify.slack` and `notify.quiet_hours` (`22:00-07:30`). Running
   `hall notify test` creates a private ntfy topic if they have none.

## Set what they chose

```sh
hall config set base trunk
hall config set gates "nix develop --command cargo test" "nix develop --command cargo clippy --all-targets -- -D warnings"
hall config set capture.mode ask
hall config set budget.daily 4M
hall config set claude_args -- --verbose          # `--` before values that start with -
hall config set sandbox.allow_write "~/.cache/nix" "~/.cargo"
hall config set --user tools.gh /opt/homebrew/bin/gh
hall config set --user notify.quiet_hours 22:00-07:30
hall config unset budget.per_item                 # back to the default (800k)
hall config set budget.daily none                 # no daily cap
```

- A list takes several values, or one TOML array (`'["a", "b"]'`).
- A `USAGE` error names the problem and the allowed values. Fix the value,
  never the file.
- Repo settings are written to the main checkout's `.hallmark.toml`, which is
  development work. Tell the person to commit it. The reviewer reads review
  settings from the base branch, so those apply once the change is merged.
- `hall config set` refuses inside Hallmark's headless runs, such as the fixer
  and the reviewer.

## Repo skills

When the repo needs its own review or fix rules (N+1 queries, a migration
policy, how its tests are run), write a skill in the repo. Hallmark reads it
from the base branch, so it applies once merged:

| File | Followed by |
| --- | --- |
| `.hallmark/skills/principal-review/SKILL.md` | the reviewer |
| `.hallmark/skills/hallmark-fix/SKILL.md` | the fixer |
| `.hallmark/skills/hallmark/SKILL.md` | development sessions |

It adds to Hallmark's skill. To replace it instead, put `mode: replace` in
the frontmatter. Keep additions concrete, with the severity each problem
gets: "An N+1 query on a list endpoint is a major." It's development work:
make it an item. `hall doctor` lists what the base branch has.

## Extensions

Skills by other authors that Hallmark's roles can follow, embedded in `hall`
and credited: `hall extensions --json` lists them, what each role gets and
what's on. When the person wants one, such as Matt Pocock's skills:

- `hall extensions enable mattpocock` turns it on for every role it serves;
  `--role reviewer --role fixer` for only some.
- `hall extensions disable mattpocock [--role …]` turns roles off.
- Like other repo settings, it's written to the main checkout's
  `.hallmark.toml`: tell the person to commit it. The reply's `note` says
  when the reviewer will follow it.
- When you mention an extension, name its author: the credit is in the
  reply.

## Finish

Run `hall doctor --json`, and follow its `next` for anything still missing. If
the plugin or the daemon isn't set up yet, the person runs `hall setup` in a
terminal, or `hall setup --interactive` to answer the same questions there.
