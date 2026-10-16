# Configuration

`.hallmark.toml` is optional. `hall init` writes one with the gates it detects.
`hall` reads it from the main checkout, never from an item's worktree, so an
item can't change its own gate
([ADR 0012](decisions/0012-config-from-the-main-checkout.md)).
`hall schema config` prints the full schema.

## Changing settings

You never need to edit the TOML by hand
([ADR 0033](decisions/0033-configurable-setup.md)).

- `hall setup --interactive` asks about the settings people change most, then
  offers the rest. It writes nothing until you confirm.
- `hall config` reads and writes any single key. It keeps the file's comments
  and checks each value before writing.

```sh
hall config list                  # every key, its value, and where it comes from
hall config list budget           # only keys under budget
hall config get ship
hall config set budget.daily 4M
hall config set gates "make test" "make lint"   # a list takes several values
hall config set claude_args -- --verbose        # `--` before values that start with -
hall config unset budget.daily    # back to the default
hall config set --user tools.gh /opt/homebrew/bin/gh
```

Repo keys go to the main checkout's `.hallmark.toml`. Commit them like any
other change. `--user` keys go to your user config,
`<user data dir>/hallmark/config.toml`, along with keys only it has, such as
`tools.*`. In Claude Code, ask Claude to "set up Hallmark" and the
`hallmark-setup` skill walks you through the same questions.

## Defaults

```toml
base        = "main"
worktree    = "git worktree add ../{repo}-{id} -b {branch}"
branch      = "hm/{id}-{slug}"
ship        = "git push -u origin {branch} && gh pr create --base {base} --head {branch} --title {title} --body-file {body_file}"
update      = "git push --force-with-lease={branch}:{pr_head} origin {branch} && gh pr edit {pr} --base {base}"
review_comment = "gh pr comment {pr} --body-file {review_file}"   # every review is a PR comment; "" for none
gates       = []
reviewer    = "claude -p"
fixer       = "claude -p"   # or another fix engine, run in the worktree
claude_args = []            # extra arguments for every headless claude -p run
dev_commands = []           # what headless dev runs may run besides the gates
max_rejects = 3             # rejected reviews before the item is escalated to you
auto_ready  = true          # propose-ready goes straight to review when nothing is left to confirm
stuck_after = "30m"
lenses      = "all"
rules       = ["CLAUDE.md", "AGENTS.md"]

[parallel]                     # how many of each run at once, per repo
checks = 2
review = 4
fix    = 4
ship   = 2

[capture]
mode = "gate"   # gate: edits need an item · ask: Claude asks you · off
free = ["docs/", "*.md"]   # paths that need no item

[intake]
split     = "auto"   # or "off"
max_parts = 4

[budget]                       # runs are capped by default; daily caps only if you set them
review_model    = "opus"
fix_model       = "sonnet"
intake_model    = "haiku"
librarian_model = "haiku"
# daily = "4M"; per_item = "800k" (weighted by price: a cache read counts a tenth) or "$15"
# [budget.review] max_usd = 3.0 (or "none"), daily = "2M", effort = "high", timeout = "20m"

[extensions]                # skill packs by other authors: `hall extensions`
# mattpocock = ["planner", "developer", "reviewer", "fixer"]

[library]
search         = "builtin"   # or "qmd"
# collection   = "hallmark-<repo>"   # the qmd collection
briefing_limit = "6k"
auto_accept    = []          # kinds that skip your approval: rule, false-positive, map, decision
retire_after   = "90d"
# Learnings live in <git-common-dir>/hallmark/library/, never in the repo.
# `hall library import` brings in ones committed by earlier versions.
```

## Extensions

`[extensions]` lists, for each extension, the roles that follow its skills:
`planner` (the Front Desk), `developer` (development sessions), `reviewer`
and `fixer`. Empty or absent means off. `hall extensions enable mattpocock`
writes it for you, and `hall extensions` shows what each role gets
([ADR 0038](decisions/0038-extensions.md)).

```toml
[extensions]
mattpocock = ["reviewer", "fixer"]   # Matt Pocock's skills, for these roles only
```

| Key | Extension |
| --- | --- |
| `mattpocock` | [Matt Pocock's skills](https://github.com/mattpocock/skills), by Matt Pocock (MIT): `to-tickets` for the planner; `tdd`, `diagnosing-bugs` and `codebase-design` for the developer; `code-review` for the reviewer; `tdd` and `diagnosing-bugs` for the fixer |

The reviewer reads this table from the base branch, like its other settings,
so commit the change there. The other roles read the main checkout's file
and follow it at once.

## Placeholders

| Key | Placeholders |
| --- | --- |
| `worktree` | `{repo}` `{id}` `{branch}` `{slug}` `{base}` |
| `branch` | `{id}` `{slug}` |
| `ship` | `{branch}` `{id}` `{base}` `{title}` `{body_file}` (the PR description: What, Why and How) `{review_file}` (the review) `{issue}` |
| `update` | those of `ship`, plus `{pr}` (the PR's URL) and `{pr_head}` (the commit the PR's head was at when the item took it over or last shipped; or the branch's head on `origin` when this item hallmarked that commit, as after an update that pushed and then failed, ADR 0042) |
| `review_comment` | `{pr}` (the PR's URL) `{review_file}` `{branch}` `{id}` `{base}` |
| `fixer` | `{id}` `{worktree}` `{findings_file}` (JSON from `hall findings`) |

Commands run through `sh -c` on macOS and Linux and `cmd /C` on Windows.

## PR descriptions and review comments

The PR the shipper opens has a description with three sections, `## What`,
`## Why` and `## How`, and nothing of the review
([ADR 0039](decisions/0039-pr-description-and-review-comments.md)). The
author gives them with `hall propose-ready --summary … --why … --how …`;
without `--why` the request is used, without `--how` the commit subjects.

Each review is a PR comment: `review_comment` posts `{review_file}` once the
PR is open, and again after every `update` (PR comments addressed, CI fixed,
a rebase), saying why the PR came back. The description isn't touched on an
update. `review_comment = ""` posts nothing; the review stays in
`<git-common-dir>/hallmark/items/<id>/review.md`. On another host, use its
command, such as `glab mr note {pr} --message "$(cat {review_file})"`.

An `update` that `hall init` wrote before this change posted the PR body as
a comment; Hallmark reads it as today's default. If your own `ship` or
`update` comments `{body_file}`, drop that step.

## Your own tooling

A repo with its own tooling overrides only the commands. For example, with a
task runner and a PR tool of your own:

```toml
worktree = "just worktree {branch}"
ship     = "just submit {branch}"
gates    = ["just test", "just lint"]
```

### Gates that need a toolchain shell

The daemon runs gates with `sh -c`, outside any shell you have entered. If
your tools come from `nix develop`, devbox, mise or direnv, run each gate
inside it. `hall setup --interactive` offers this when it sees `flake.nix`,
`devbox.json`, `mise.toml` or `.envrc`.

```toml
gates = [
  "nix develop --command cargo test",       # Nix flake
  # "devbox run -- npm test",               # devbox
  # "mise exec -- pytest",                  # mise
  # "direnv exec . make test",              # direnv
]
```

`hall setup --interactive` preselects a wrapper only when its program is on
PATH, and never preselects direnv: direnv refuses an `.envrc` that wasn't
allowed at the path it runs in, so `direnv allow` it in the main checkout and
in every worktree path (or use direnv's whitelist prefix) before using it.

## Your tools

Hallmark runs `claude`, `gh`, and optionally `qmd` and `cargo`. It finds them
on PATH. When they live elsewhere, say so in your user config. The daemon's
login service starts with a bare PATH, so this is the dependable way to point
it at them:

```toml
# <user data dir>/hallmark/config.toml
[tools]
claude = "/Users/me/.local/bin/claude"
gh     = "/opt/homebrew/bin/gh"
qmd    = "/usr/local/bin/qmd"     # for [library] search = "qmd"
cargo  = "/Users/me/.cargo/bin/cargo"   # for hall update
```

`HALL_CLAUDE`, `HALL_GH`, `HALL_QMD` and `HALL_CARGO` override these. With
neither, `gh` is also looked for in the Nix store.

## Budgets

Budgets apply at four levels: per run (`max_usd`, `timeout`), per role per
day, per item, and for the whole loop per day
([ADR 0016](decisions/0016-budgets-holds-and-pauses.md)). When a cap is hit,
the item (or the loop) waits for you. `hall budget` shows the caps and today's
spend, `hall budget --set per_item=800k` changes one, and `hall budget --grant`
lets the loop go past today's daily cap without changing it.

```toml
[budget]
daily    = "4M"     # whole review loop
per_item = "800k"   # then it goes to you

[budget.review]
model    = "opus"
fallback = "sonnet"  # when model is unavailable or overloaded
effort   = "high"
max_usd  = 3.00      # per run
daily    = "2M"
timeout  = "20m"

[budget.fix]
model   = "sonnet"
effort  = "medium"
max_usd = 1.50
```

Each role has a table with the same keys: `[budget.review]`, `[budget.fix]`,
`[budget.intake]` (the Front Desk) and `[budget.librarian]`. A table's
`model` wins over `<role>_model`, and its `fallback` is used when `model` is
unavailable (a run that fails fast without tokens is retried once with it) or
overloaded.

Every run has a per-run cap, `max_usd`, passed to Claude as
`--max-budget-usd`. When a role's table doesn't set it, the default applies:

| Role | `max_usd` default |
| --- | --- |
| `[budget.review]` | 3.00 |
| `[budget.fix]` | 2.00 |
| `[budget.intake]` | 0.10 |
| `[budget.librarian]` | 0.20 |

A run that needs more than this has already gone wrong: a reviewer that
wanders, or a fixer looping on a flaky gate. A run that reaches its cap stops
and is retried once; a second stop holds the item for you. Set
`max_usd = "none"` to run a role with no per-run cap (only `timeout` bounds
it), or `hall budget --set review.max_usd=none`;
`hall budget --set review.max_usd=default` (or `hall config unset
budget.review.max_usd`) goes back to the default.

## Notifications

Notification channels usually live in your user config,
`<user data dir>/hallmark/config.toml`, so your phone topic stays out of the
repo. `hall setup` creates a private, random [ntfy](https://ntfy.sh) topic if
you have none.

```toml
[notify]
desktop     = true
phone       = "ntfy://ntfy.sh/hallmark-7f3k2q"   # or telegram://<token>@<chat>
slack       = "https://hooks.slack.com/…"
quiet_hours = "22:00-07:30"   # the phone waits; the desktop still shows
remind      = "2h"
digest      = "08:30"

[notify.route]
question  = ["desktop", "phone"]
escalated = ["desktop", "phone"]
stuck     = ["desktop", "phone"]
ready     = ["desktop"]
budget    = ["desktop"]
session_waiting = ["desktop"]   # a Claude session waits for you
learnings = ["digest"]
```

Anyone who knows a topic's name can read it on the public ntfy.sh, so keep the
topic random, or host ntfy yourself. Notifications carry the item, the question
and its options, never code.
