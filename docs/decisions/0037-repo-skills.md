# 0037. Repo skills: a repository extends or replaces Hallmark's skills

Status: Accepted, 2026-10-12

## Context

Hallmark's skills are generic. A repo often needs its own: a Rails app
cares about N+1 queries, a payments service about idempotency keys, a
monorepo about how its tests are run. `.hallmark/review.md` and `rules`
hold notes and conventions, but they can't change the reviewer's method or
the fixer's steps, and nothing reached development sessions.

The headless runs don't load a repo's `.claude/skills/`. The reviewer runs
with `--setting-sources user` (ADR 0018), and anything the checkout holds
belongs to the branch, so the branch could steer its own review.

## Decision

- **Where.** `.hallmark/skills/<name>/SKILL.md`, shaped like a Claude Code
  skill. Three names are used:

  | Skill | Followed by | How it arrives |
  | --- | --- | --- |
  | `principal-review` | the reviewer | the review prompt, after the method |
  | `hallmark-fix` | the fixer | the fixer's prompt, before the closing steps |
  | `hallmark` | development sessions | the session-start hook's context, cut at 8k characters with a `git show` pointer |

- **From the base branch, like the rules** (ADR 0012, ADR 0018). Each run
  reads the file with `git show <base>:<path>`. A branch that edits its own
  skill is judged on that change in the diff; its copy isn't followed until
  merged. `.hallmark/` already needs an item to edit (`Config::is_free`).
- **Extend by default.** The repo's text follows Hallmark's, and where it is
  more specific it wins. `mode: replace` in the frontmatter makes it
  replace Hallmark's skill. Any other value extends: a typo must not drop
  Hallmark's method. A file with only frontmatter or comments is ignored.
- **The contract doesn't move.** A replaced method still answers through
  the verdict schema, and approval is still computed (ADR 0015). The
  agent's position (independent, read-only) is in the agent file, which a
  repo can't change.
- **Visible.** `hall doctor` lists the skills on the base branch and how
  each combines. It fails the check for a name Hallmark doesn't use or a
  skill without text, and notes a skill that isn't merged yet.
- Skill files are cut at 24k characters, like rule files.

## Consequences

- Repo-specific checks reach every review and fix without forking the
  plugin.
- A replaced `principal-review` gives up Hallmark's method, including the
  evidence bar. Extending is the recommended form.
- Intake and the librarian have no repo skill. They can get one later under
  the same rules.
