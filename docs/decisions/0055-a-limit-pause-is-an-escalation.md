# 0055. A pause for Claude's limit is an escalation

Status: Accepted, 2026-10-27. Amends 0031.

## Context

- A `claude -p` run that ends with Claude's session or weekly limit ("You've
  hit your session limit · resets 11:10pm") is a rate limit, not a failed
  run. The daemon pauses until the reset time Claude reports (30 minutes when
  it names none) and retries by itself (ADR 0016).
- ADR 0031 says only what the loop gave up on, or what only a person can
  decide, reaches you. A pause the loop lifts by itself is neither.

## Decision

- **A pause is an escalation (`paused`)**, because only a person can top up
  credits and retry before the reset. It reaches Your Office, the desktop and
  phone notifications and every session's hook, with `hall daemon resume` to
  retry at once.
- The entry goes away when the pause lifts or has expired.
- 0031 lists `paused` among its escalations.

## Consequences

- A pause that the person doesn't need to act on still notifies once, when it
  begins.
