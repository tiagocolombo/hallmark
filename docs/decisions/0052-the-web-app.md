# 0052. The web app: `hall web` runs hall's own commands for a page on this machine

Status: Accepted, 2026-10-18.

## Context

The terminal app (ADR 0022) shows the whole building, but only in a
terminal, one key at a time, and it can't show a long journal, a diff and a
run side by side. People asked for the same thing in a browser: every
repo's items, the office, the merge queue, usage, and the settings, with
buttons instead of commands to remember.

A web app could read the journal and the index itself, as the terminal app
does, and change things through its own code. Then it would be a second
implementation of every rule: which actions fit which state, what a
`retry` means for a budget hold, how a question is answered. ADR 0002 makes
the CLI the API, and every reply already says what makes sense next
(`next`, a now block's `actions`, an inbox entry's `answer_with`).

## Decision

`hall web` starts a small HTTP server on 127.0.0.1 (port 4747, or a free
one when that's taken; `--port` picks one and fails when it's taken) and
opens the browser on it (`--no-open` doesn't). It runs until stopped.

- **The page is embedded.** One HTML file, a stylesheet, a script and the
  icon, in the binary (`src/web/assets/`). No build step, no framework, no
  fonts or scripts from the network, so it works offline and on Windows.
- **One screen, three columns.** A top bar (the repo, the daemon, the views,
  an update), a sub-bar (what the view shows, how often it reads again, and
  a light, dark or system theme), then the open items on the left, the view
  in the middle and the daemon's activity on the right. Either side column
  folds away. The floor is a strip of figures with 14-day sparklines, then a
  card per item: where it is (stage n of 7), its open findings as squares,
  its round, rejections, time in state and cost, and a band that says what
  it is doing now. The theme, the refresh and the folded columns are kept
  in the browser, per viewer.
- **Every call is a hall command.** `POST /api/run {"repo", "args"}` runs
  `hall <args> --json` in a child process, in that repo, and returns its
  exit code, its reply (the last line) and the lines before it (`hall
  trace`'s steps). The page draws the replies and turns the commands they
  offer into buttons; a placeholder such as `"<why>"` is asked for first.
  So what each state allows is decided by hall, as it is for an agent.
  `GET /api/diff` is the one exception: the item's `git diff
  <base>...<commit>`, as the terminal app shows it.
- **Only what a person does from a page.** `cli::allowed_on_web` allows the
  item flow, answers, the readers, settings, budgets, the Library, the
  daemon, `doctor` and `update`. It refuses the agents' and hooks'
  commands (`next`, `verdict`, `fixed`, `propose-ready`, `ask`, `hook`,
  `statusline`), anything that needs a terminal (`watch`, `trace --follow`,
  `new --open`, `setup`, `init`, `daemon run`), `adopt` without `--pr`, and
  `update` from another source or branch. The commands run with the
  session variables removed (`CLAUDECODE`, `HALL_ACTOR`, `HALL_HEADLESS`…),
  so what they record is yours.
- **Only the repos it shows.** The registry's, plus the one `hall web`
  started in (the terminal app's list). A call naming any other directory
  is `NOT_A_REPO`.
- **Only this machine, and only with the token.** The address it prints has
  a random token in its fragment, which browsers never send. The page keeps
  it for the tab and sends it as `X-Hall-Token` with every API call. Another
  site's page can't send that header without a CORS preflight, which is
  never answered, so it can't drive the API. A request whose `Host` isn't
  127.0.0.1, localhost or [::1] on this port is refused, so a page that
  rebinds its own name to 127.0.0.1 gets nothing either. The page itself
  and `GET /api/ping` need no token: they hold nothing secret.
- **It updates itself.** The top bar shows the daemon and, from `hall update
  --check` (on load and hourly), whether a newer hall is out. *Update and
  restart* runs `hall update`, which builds it, refreshes the plugin and
  restarts the daemon, then `POST /api/restart`: the server starts the new
  binary in its place on the same port with the same token
  (`HALL_WEB_TOKEN`). On Unix it replaces the process; on Windows the old
  one waits for the new one. The open page reconnects and reloads.
- **It redraws, gently.** The open view is read again every few seconds (a
  running run every two), never while a dialog is open, a command runs or a
  field has focus.

## Consequences

- A new command, flag or reply field reaches the page without new server
  code; the page only learns how to draw it.
- Each call costs a process start, tens of milliseconds. The page reads a
  handful of commands every few seconds, which is fine on one machine.
- Anyone who can read the address can act as you while `hall web` runs, as
  anyone at your terminal can. The token lasts until the server stops, and
  an update keeps it.
- The page shows only what the commands say. A view that needs data no
  command gives (the diff) needs an API of its own, and should become a
  command first where it can.
- The token never goes in a process's arguments, which other users on the
  machine can read. `hall web` opens a page written 0600 under the user
  directory (`web-open.html`; on Linux in a private, visible
  `~/hallmark-web`, which snap browsers can read) that redirects to the tokenized address, and
  removes `HALL_WEB_TOKEN` from its own environment at start, so no child
  (the daemon, a model run) inherits it. Only a restart hands it on.
