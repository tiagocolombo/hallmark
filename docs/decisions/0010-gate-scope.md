# 0010. Where the gates apply

Status: Accepted, 2026-10-09

## Context

The plugin's hooks are installed at user level, so they fire in every repo and
every session. Gating repos that never opted in would break unrelated work. The
gates must also hold up against shell commands written in many shapes.

## Decision

- **Scope.** The gates apply only in repos where `hall init` has created
  `<git-common-dir>/hallmark/`. `.hallmark.toml` alone doesn't opt in, because
  the file is optional. Everywhere else, the hooks do nothing.
- **The edit gate.**
  - It covers `Edit`, `Write`, `MultiEdit` and `NotebookEdit` on a path inside
    the repo (any of its worktrees), and a Bash `git commit`.
  - Such a call is allowed only inside an active item's worktree. Paths outside
    the repo are never gated.
- **Capture modes**, set by `[capture] mode`:
  - `gate` (default): deny, with the error JSON naming `hall new` or
    `hall adopt`.
  - `ask`: return `permissionDecision: "ask"`.
  - `off`: allow; items start only through `/hallmark`.
- **The push gate.**
  - It applies whatever the capture mode is.
  - It denies `git push` (including `git -C dir push`), `gh pr create`, and the
    configured ship command, unless `HEAD` has a hallmark.
  - Compound commands are split on `&&`, `||`, `;`, `|` and newlines, and each
    part is tokenized with shell-word rules. Leading `cd` steps and `VAR=value`
    assignments are skipped.

## Consequences

- Installing the plugin is harmless in repos that don't use Hallmark.
- The gate covers commands that agents actually write. It is not a sandbox: a
  determined `bash -c "$(…)"` can evade the parser. It exists to keep honest
  agents on the path, and GitHub branch protection remains the final control.

## Addendum: allowing edits in the item's worktree

Item worktrees are usually siblings of the main checkout, so they sit outside
the session's working directory, and Claude Code refuses writes there
(docs/spikes.md, end-to-end check). When the session's `permission_mode` is
`acceptEdits` or `bypassPermissions`, the PreToolUse hook answers `allow` for
Edit/Write inside a live item's worktree. That extends an edit permission the
human already gave to the item's worktree. In `default` and `plan` modes, Hallmark
stays silent and Claude Code asks as usual.

