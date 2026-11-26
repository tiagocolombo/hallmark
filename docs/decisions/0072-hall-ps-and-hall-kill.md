# 0072. `hall ps` and `hall kill`: the halls that keep running

Status: Accepted, 2026-11-26.

## Context

Some halls run until they're stopped: the web app, the daemon, `watch`,
`trace --follow` and the terminal app. A `hall web` left open in a
forgotten terminal, or one you want running without a terminal at all,
couldn't be found or stopped from hall. Only the daemon had a
`start`/`stop` of its own (0017).

## Decision

- **A registry of run files.** Each hall that keeps running (web, `watch`,
  `trace --follow`, the terminal app) registers itself under
  `<user dir>/run/` for its whole life. It creates and locks `<pid>.lock`
  first, then writes `<pid>.json` with its kind, start time, directory,
  arguments, and for the web app its port and address. The OS releases the
  lock however the process ends, so a lister that can take the lock knows
  the process is gone, even after a crash or a kill, and removes its files.
  A recycled pid can't look alive. The directory is per user and readable
  only by you, since a web entry carries its token. Registration never fails
  a command: a hall that can't register just isn't listed.
- **The daemon is read from its own lock.** The loop already holds
  `daemon.lock` (0017), so `hall ps` lists it from there. `hall kill` stops
  it the way `hall daemon stop` does: `wanted` is cleared, so the inbox
  doesn't flag it as stopped unexpectedly.
- **`hall ps [--kind]`** lists them, and `hall -l` is the same. **`hall kill
  <pid>… | --kind <kind> | --all`** stops them: SIGTERM on Unix,
  `taskkill /T /F` on Windows. It waits up to 5s for each lock to come
  free. It only stops processes that `hall ps` lists, never an arbitrary
  pid. An unknown pid is a note, not an error, so repeating a kill is safe.
  Commands never prompt (0002), so there's no "kill this?" menu: the reply's
  `next` names the kill commands.
- **`hall web -d`** starts `hall web --no-open --json` detached: a new
  process group on Unix, `DETACHED_PROCESS` on Windows, like the daemon. Its
  output goes to a private log in the run directory. `-d` waits for the
  server's start line, opens the browser and returns the address, the port,
  the pid and the log. The log is removed once the server is gone. **`hall
  web -l`** is `hall ps --kind web`, and **`hall web --kill [<pid>]`** stops
  one web server, or every one when no pid is given.
- None of these run from the web app or the terminal app: a page could stop
  the server serving it.

## Consequences

- Short commands aren't listed: they're gone before anyone could look.
- On Windows a restarted `hall web` (after an update) drops its entry before
  it starts the new server, so only the new one is listed.
- A terminal app ended with SIGTERM can't restore the terminal. Prefer `q`;
  `hall kill` is for the one you can't reach.
