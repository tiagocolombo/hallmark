# 0046. Condense command output, never evidence

Status: Accepted, 2026-10-15.

## Context

- **Raw output crowds out the work.** A failing `cargo test` in a large
  crate prints thousands of lines, almost all of them `... ok`. An agent
  that reads it spends its context on progress and passing tests, and the
  four lines that matter are easy to miss.
- **Some output is the point.** A diff, a commit or a log is evidence: the
  reviewer and the fixer must see it exactly as git printed it.
- **Truncating blindly loses failures.** Keeping the last N lines drops the
  first failure's message; keeping the first N drops the summary.

## Decision

A library, `src/condense/`, condenses command output for an agent's
context. Later commands (`hall run`, the gates) call it; this item adds no
command and no setting.

- **Lossless on disk, lossy in context.** The caller keeps the full output
  in a log; the condensed text ends with a footer that names the log and how
  to show a part of it (`hall run --show <path> [--grep <re>] [--around <re>
  -n 40] [--tail 200]`).
- **Evidence is never condensed.** `git diff`, `git show` and `git log`, with
  any global options and anywhere in a shell line, pass through untouched.
  The list is hard-wired; it may grow, and nothing may remove from it.
- **Small output is untouched.** Output of at most 2 KB and 60 lines passes
  verbatim, ANSI and all. A result that isn't smaller than its input is
  replaced by the input.
- **One filter per tool family**, picked from the command line: `cargo
  test`; `cargo build`, `check` and `clippy`; `cargo fmt --check`; pytest;
  jest and vitest (also `npm|pnpm|yarn test`); `go test`; and a generic one.
  Each strips ANSI and `\r` redraws (CRLF-safe), then keeps the exit code,
  every failing test or error with its file:line, the first message block of
  each failure, the tool's own summary line, and distinct warnings once with
  a count. A family filter that doesn't recognise its output falls back to
  the generic one: the first 20 and last 40 lines, repeats as `[xN]`, the
  gap as `... N lines omitted`.
- **Deterministic and tested.** A filter is a pure function over text. Each
  family is pinned by a fixture log in `tests/fixtures/output/` (cargo logs
  from real runs, the others in the tools' real formats) and a reviewed
  snapshot; a test checks every failure survives.
- **Measured honestly.** The result counts the bytes and lines kept and
  dropped. Tokens saved are bytes/4 and are always labelled an estimate.

The shape of the filters and the idea of measuring the gain come from rtk
(rtk-ai/rtk). Nothing of rtk is embedded or required.

## Consequences

- Agents read a failing run in tens of lines instead of thousands: the
  3,166-line `cargo test` fixture condenses to 47.
- A filter can be wrong about what matters. The full log stays on disk and
  the footer says how to read it, so a dropped line is one command away.
- A new tool family is a new filter with a fixture and a snapshot; until
  then its output gets the generic head and tail.
- Limits are defaults in code for now; the `[condense]` settings come with
  the command that uses them.
