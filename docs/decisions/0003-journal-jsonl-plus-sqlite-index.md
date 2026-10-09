# 0003. JSONL journal, SQLite index

Status: Accepted, 2026-10-09

## Context

Every step of an item must be recorded so that the human can see what was
rejected, fixed and passed. The record must survive worktree removal, must be
shared by every worktree of a repo, must never be committed (it holds prompts),
and must answer usage and history questions quickly.

## Decision

- **The source of truth** is an append-only JSONL file for each item, at
  `$(git rev-parse --git-common-dir)/hallmark/items/<id>/events.jsonl`.
  - Events that belong to no item go to `hallmark/events.jsonl`. These are
    prompts from sessions without an item, notifications, and gate denials.
- **Every write** takes an exclusive fs4 lock, appends one line, and flushes.
- **A SQLite index**, `hallmark/index.sqlite` (rusqlite, bundled), is derived
  from the files.
  - It records the byte offset it has read for each journal, and catches up on
    every command.
  - `hall reindex` deletes it and rebuilds it.
- **Each event** has a version: `{"v":1,"t":…,"actor":…,"event":…,…}`.

## Consequences

- Nothing is ever edited or deleted, so the history you read is the history that
  happened.
- The index is disposable. Corruption or schema changes cost one `hall reindex`.
- Concurrent writers (hooks in several sessions, the daemon) are serialized
  per file by the lock.
- The git common directory isn't in the working tree, so the journal can't be
  committed by accident. It also isn't backed up by `git push`; `hall export`
  will cover sharing.
