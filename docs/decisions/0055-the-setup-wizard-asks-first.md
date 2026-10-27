# 0055. The setup wizard asks first, about every setting, or is skipped

Status: Accepted, 2026-10-26. Amends 0033 and, through it, 0002.

## Context

- ADR 0033 gave every setting a command (`hall config`) and a wizard that
  runs only when asked: `hall setup --interactive`. People setting Hallmark up
  for the first time ran `hall setup`, got a `.hallmark.toml` with detected
  gates and the defaults, and found out about the rest (the shell their gates
  need, budgets, merging, notifications) when something didn't work.
- The wizard asked about the settings people change most, then offered the
  rest behind one yes/no question, so most of the file was never shown.
- The user config, `<user data dir>/hallmark/config.toml`, was created only
  when a key was first written, so there was nothing to tell a first run from
  a later one.

## Decision

### The wizard runs the first time, on a terminal

- `hall init`, in a repo without `.hallmark.toml`, asks about the repo's
  settings before it writes anything.
- `hall setup`, when there is no user config yet or the repo has no
  `.hallmark.toml`, asks about everything: the repo's settings, your tools,
  notifications, and the daemon.
- `hall setup --interactive` asks the same way whenever asked, as before.
- It asks only when stdin and stderr are terminals, stdout is a terminal or
  `--json` isn't given, and the run isn't one of Hallmark's own headless runs
  (`HALL_HEADLESS`). Everywhere else (an agent's shell, a script, CI) the
  defaults are written without a question, as before.
- `--defaults`, on both commands, never asks.

### The first question is whether to go through the settings, or skip

- "Yes" walks every key of the files the command covers, section by section,
  in the order of the configuration reference: branches and gates (and the
  shell they run in), worktrees and shipping, the loop, capture, merging, the
  Front Desk, models and budget (the four role tables included), extensions,
  the Library, the sandbox; then, for `hall setup`, your tools and
  notifications, and whether the daemon starts at login. Each question shows
  its default (or what `hall init` detected): Enter keeps it, `-` goes back to
  the default, `?` explains it. A key the sections don't name is still asked,
  under "Other settings", so a new setting is offered the day it exists.
- "Skip for now" asks nothing more. The commands write their basic files:
  `.hallmark.toml` as `hall init` always has, with the detected gates and the
  defaults, and a user config with the defaults and the keys people set as
  comments. `hall setup --interactive` asks again later; `hall config` sets
  any key.
- Nothing is written until the list of changes is confirmed. Cancelling, or
  running out of input, writes nothing: `hall init` leaves the repo as it was.
- The answers are applied after the files exist, through the same code as
  `hall config set`, so the files keep their comments.

### `hall setup` creates the user config

`hall setup` writes `<user data dir>/hallmark/config.toml` with the defaults
when it doesn't exist, on a terminal or not, and reports it as a step. Its
absence is what makes a `hall setup` the first one.

### Amending ADR 0033 and 0002

"Never prompts, except `hall setup --interactive` on a terminal" becomes
"never prompts, except the setup wizard on a terminal: `hall init` and
`hall setup` the first time, `hall setup --interactive` when asked;
`--defaults` writes the defaults instead". `hall help --json` states the
exception.

## Consequences

- A person's first `hall setup` or `hall init` on a terminal sees every
  setting once, with its default, or skips in one keystroke. Agents, hooks and
  scripts see no change: off a terminal the commands behave as before.
- The walk is long (about 90 questions for `hall setup`, most of them Enter).
  That is the point of asking for every setting; the skip is one keystroke
  away, and `hall config` reaches any single key.
- `hall setup` in Hallmark's own `rig install` runs on a terminal and so asks
  the first time; "skip for now" keeps it to one question.
- The wizard's sections are unit-tested against the settings schema: every
  key of both files is offered, and every key a section names exists.
