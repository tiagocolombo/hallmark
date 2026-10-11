# Configuration

`.hallmark.toml` is optional. `hall init` writes one with the gates it detects.
`hall` reads it from the main checkout, never from an item's worktree, so an
item can't change its own gate
([ADR 0012](decisions/0012-config-from-the-main-checkout.md)).
`hall schema config` prints the full schema.

## Defaults

```toml
base        = "main"
worktree    = "git worktree add ../{repo}-{id} -b {branch}"
branch      = "hm/{id}-{slug}"
ship        = "git push -u origin {branch} && gh pr create --base {base} --head {branch} --title {title} --body-file {body_file}"
update      = "git push --force-with-lease={branch}:{pr_head} origin {branch} && gh pr edit {pr} --base {base} && gh pr comment {pr} --body-file {body_file}"
gates       = []
reviewer    = "claude -p"
fixer       = "claude -p"   # or another fix engine, run in the worktree
claude_args = []            # extra arguments for every headless claude -p run
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

[budget]                       # no caps unless you set them
review_model    = "opus"
fix_model       = "sonnet"
intake_model    = "haiku"
librarian_model = "haiku"
# daily = "4M"; per_item = "800k" (weighted by price: a cache read counts a tenth) or "$15"
# [budget.review] max_usd = 3.0, daily = "2M", effort = "high", timeout = "20m"

[library]
search         = "builtin"   # or "qmd"
briefing_limit = "6k"
retire_after   = "90d"
# Learnings live in <git-common-dir>/hallmark/library/, never in the repo.
# `hall library import` brings in ones committed by earlier versions.
```

## Placeholders

| Key | Placeholders |
| --- | --- |
| `worktree` | `{repo}` `{id}` `{branch}` `{slug}` `{base}` |
| `branch` | `{id}` `{slug}` |
| `ship` | `{branch}` `{id}` `{base}` `{title}` `{body_file}` (the PR body the shipper writes) `{issue}` |
| `update` | those of `ship`, plus `{pr}` (the PR's URL) and `{pr_head}` (the commit the PR's head was at when the item took it over) |
| `fixer` | `{id}` `{worktree}` `{findings_file}` (JSON from `hall findings`) |

Commands run through `sh -c` on macOS and Linux and `cmd /C` on Windows.

## Your own tooling

A repo with its own tooling overrides only the commands:

```toml
worktree = "rig wt new {branch}"
ship     = "rig submit --pr"
gates    = ["rig test", "rig run build"]
```

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
model   = "opus"
effort  = "high"
max_usd = 3.00      # per run
daily   = "2M"

[budget.fix]
model   = "sonnet"
effort  = "medium"
max_usd = 1.50
```

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
learnings = ["digest"]
```

Anyone who knows a topic's name can read it on the public ntfy.sh, so keep the
topic random, or host ntfy yourself. Notifications carry the item, the question
and its options, never code.
