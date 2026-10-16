# 0038. Extensions: skill packs by other authors, per role, with credit

Status: Accepted, 2026-10-12. Amended 2026-10-15: skills load by trigger (addendum).

## Context

Good skills already exist outside Hallmark. Matt Pocock's skills
([github.com/mattpocock/skills](https://github.com/mattpocock/skills), MIT)
cover test-first development, debugging, module design, review and slicing
work into tickets: the jobs Hallmark's roles do. A repo could copy one into
`.hallmark/skills/` (ADR 0037), but every repo would keep its own drifting
copy, nothing would say whose work it is, and the skills don't fit Hallmark's
roles as written: they ask the user questions, publish to an issue tracker
and spawn sub-agents, while the reviewer, the fixer and the Front Desk run
headless and answer through a schema.

## Decision

- **An extension is a skill pack by another author**, embedded in `hall`
  unchanged, pinned to an upstream commit, with its author, source, license
  and license text. Its files live in `extensions/<id>/` with a README that
  credits the author and says how to move the pin. Embedding means it works
  offline, every run of one `hall` version reads the same text, and a
  branch can't change it.
- **Roles.** An extension maps each of its skills to the roles that follow
  it: `planner` (the Front Desk), `developer` (development sessions),
  `reviewer` and `fixer`. Each mapping carries a note on how the skill fits
  that role. That note is where Hallmark adapts it; the author's text is
  never edited. The first extension, `mattpocock`:

  | Role | Skills |
  | --- | --- |
  | planner | `to-tickets` |
  | developer | `tdd`, `diagnosing-bugs`, `codebase-design` |
  | reviewer | `code-review` |
  | fixer | `tdd`, `diagnosing-bugs` |

- **Off until a repo turns it on.** `[extensions]` in `.hallmark.toml` lists
  the roles that follow each extension: `mattpocock = ["reviewer", "fixer"]`.
  `hall extensions enable <id>` turns on every role it serves, or only
  `--role`; `disable` turns them off. The setting is a key like any other,
  so `hall config` and the setup wizard reach it too.
- **How it arrives.** The headless roles get each skill in full in their
  prompt, after Hallmark's method and the repo's own skill, with its credit,
  its note and a rule for headless use: decide where it says to ask; skip
  steps that need its setup skill, an issue tracker, sub-agents or another
  skill; Hallmark's instructions and the repo's rules win, and the output
  format doesn't change. Development sessions get an index instead (each
  skill, when it applies, and `hall extensions show <id> <skill>` to read it),
  so the text costs context only when the work calls for it.
- **Settings are read as for every other key** (ADR 0012): the Front Desk,
  sessions and the fixer read the main checkout's file; the reviewer reads
  the base branch's. `hall extensions enable` and `hall doctor` say when the
  reviewer will follow a change.
- **Credit is visible wherever the text is.** `hall extensions` lists each
  extension's author, source, license and commit; `hall extensions show <id>`
  prints the license in full; every prompt that carries a skill names its
  author and source; the README credits each extension.

## Consequences

- Turning on Matt Pocock's skills is one command, and no repo keeps a copy.
- A newer upstream skill reaches users only through a new `hall` release that
  moves the pin. That is the price of offline, reproducible runs.
- A repo can still extend or replace Hallmark's skills with its own (ADR
  0037); extensions sit beside that, and the repo's text wins where they
  disagree.
- An extension's skills are written for a person in the loop. The headless
  rule keeps the contract, but a skill that is mostly interactive is a poor
  fit for a headless role and shouldn't be mapped to one.
- Adding an extension is a catalog entry in `src/extensions.rs`, its files
  under `extensions/`, and a key in `[extensions]`. Only extensions whose
  license allows redistribution can be embedded.

## Addendum, 2026-10-15: skills load by trigger

Inlining every enabled skill in full cost every headless run its whole text,
whether the run needed it or not: the reviewer about 6.7 KB for
`code-review`, the fixer about 12.3 KB for `tdd` and `diagnosing-bugs`, on
every round.

- **A skill's role mapping can carry a trigger** (`when` in the catalog in
  `src/extensions.rs`). A headless role loads a skill with a trigger only on
  a run that meets it; a skill without one loads on every run. The run's
  context is whether a gate failed and the lenses of the open findings.
- **The fixer**: `diagnosing-bugs` loads only when a gate failed on the
  commit being fixed; `tdd` only when an open finding is under the
  `correctness` or `evidence` lens. A round with neither gets no Extensions
  section.
- **The reviewer and the Front Desk** keep `code-review` and `to-tickets` on
  every run: each is that role's method, not a tool for one kind of problem.
- **Development sessions are unchanged**: they get the index of every skill
  and read one with `hall extensions show` when the work calls for it.

The decision above stands; this changes only when a headless role's skill
text arrives, not which skills a role follows or how they are credited.
