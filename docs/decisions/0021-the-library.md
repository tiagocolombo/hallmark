# 0021. The Library: learnings as reviewed files, read from the base

Status: Accepted, 2026-10-09

## Context

Each review starts with a fresh context on purpose, so it needs a short
briefing of what earlier reviews of the repository taught. That knowledge
changes how every later review judges code. A bad learning would quietly
weaken all of them, and a branch must never be able to soften its own
review. The Library also has to work on every install, with no extra
runtime, and stay cheap: the librarian runs after every item.

## Decision

- **A learning is a Markdown file** in `[library] dir`
  (`.hallmark/knowledge/L-0017-<slug>.md`). Its frontmatter holds `id`,
  `kind` (rule, false-positive, map, decision), `scope` (globs), `lens`,
  `source`, `status` (proposed, accepted, retired), `used` and `last_used`.
  A `# title` and a body follow.
  - The parser is tolerant: comments, bare or quoted list items, block
    lists, a title written as the first paragraph, and unknown keys, which
    are kept.
  - The writer is canonical.
  - No YAML crate.
- **Learnings change only through knowledge items.** These are ordinary
  items titled `Library: …`.
  - The Library creates one by running `hall new --title … --no-split` as a
    subprocess, because the CLI is the API. It reuses the oldest open one
    that is still in development or awaiting you.
  - It commits the files in that item's worktree, journals the commit, and
    runs `hall propose-ready`, so the item shows in the inbox as "ready?".
  - From there it is reviewed and shipped like code.
  - A Library lock (`hallmark/library/.lock`, not the repo lock that
    `hall new` takes) serializes writers.
- **Reviews read accepted learnings from the base branch only**, with
  `git ls-tree` and one `git cat-file --batch`. Search reads the same set by
  default; `--proposed` adds what open knowledge items propose. Retired
  learnings are never returned.
- **IDs** are allocated above the highest ID found in any of these places:
  - the base branch;
  - the worktree of every open knowledge item;
  - the branch tip of every knowledge item that was not dropped, because a
    shipped item may not be merged yet.
- **Search.**
  - **Built in:** SQLite FTS5 (`porter unicode61`) in
    `<hallmark dir>/library.sqlite`. It is a cache keyed by the paths and
    contents it was built from, so it rebuilds itself and `hall reindex`
    doesn't need to know about it.
    - Queries are reduced to words, and each word is quoted as a prefix
      term joined with OR, so FTS5 syntax never gets through.
    - Results are ranked by bm25, with the ID and title weighed above the
      body. The score is `x/(1+x)` of the negated bm25.
  - **qmd** (`search = "qmd"`):
    - The set being searched is exported to
      `<hallmark dir>/library/qmd/<collection>/{base|H-…}/`. It is
      registered once with
      `qmd collection add <dir> --name <c> --mask "**/*.md"` and refreshed
      with `qmd update` when its key changes.
    - Searches run `qmd search --json -n <n> -c <c> <words>`, so Hallmark
      queries one collection at a time. Results are mapped back to
      learnings by file name.
    - `HALL_QMD` overrides the binary.
    - qmd wasn't installed when this was written. The parser follows qmd's
      documented JSON (an array of `{docid, score, file, title, snippet}`)
      and also accepts `path`, `filepath` and `{"results": […]}`.
    - Any qmd failure falls back to the built-in search, with a `note`.
- **Briefing** (`library::briefing`):
  1. Accepted learnings whose scope globs match a changed path, the most
     matches first (globs use `literal_separator`, so `*` stays within a
     directory).
  2. Then a search with terms from the diff: words on `+` and `-` lines,
     and path segments weighed ×3, minus stop words.
  - Duplicates are dropped. Learnings that don't fit the
    `briefing_limit` cap (estimated as characters / 4) are skipped in favour
    of smaller ones.
  - The text asks the reviewer to report `learnings: [{id, held}]`.
  - A base that can't be read gives an empty briefing, not an error.
- **The librarian** (Haiku, `agents/librarian.md`, `schemas/learnings.json`):
  - **Which items:** closed or escalated items, and items dropped after an
    escalation (that is your ruling). An item counts as distilled when a
    `distilled` event follows its last state change, so an item that is
    escalated, retried and later closed is distilled again. This makes
    `hall library distill` idempotent without a `--force`.
  - **The prompt** is a digest of the journal: the request, changed paths,
    rounds, each finding with its dispute and outcome, your decisions, the
    questions you answered, and the learnings the reviews used. It also
    lists the existing learnings, base and pending, one line each.
  - **No duplicates:** Hallmark keeps one file per ID. When `updates` names
    an existing ID, or a "new" learning has the same normalized title as an
    existing one, that learning is updated.
  - **Status:** kinds in `auto_accept` are written `accepted`; the rest are
    written `proposed`, and that includes updates.
  - The source item gets `distilled { into, learnings }`. The runner
    records usage.
- **Retirement.** An accepted learning on the base is proposed as `retired`
  in a knowledge item when either of these holds:
  - a verdict reported `held: false` after the file last changed on the
    base;
  - nothing used it within `retire_after`, counted from the later of its
    last use and its creation date in `git log`.

  The model's `retire` list is added to these. Learnings already pending
  in an open knowledge item are skipped. So are learnings proposed in the
  last `retire_after` by a knowledge item that wasn't dropped
  (`hallmark/library/retirements.json`). With nothing to distil, a tick
  checks retirements at most once a day, and that check needs no model run.
  `used` and `last_used` come from verdict events in every journal. They
  are written into each file the Library touches, and never lowered.
- **`hall library review`**:
  - With no flags, it lists proposed learnings and pending retirements.
  - `--accept` sets a proposal to `accepted`.
  - `--reject` restores the base version, or deletes a new file.
  - Each call makes one commit per knowledge item. A knowledge item already
    in the review loop is `WRONG_STATE`.
  - When nothing proposed remains, `next` is `hall ready <knowledge item>`.

## Consequences

- A learning reaches reviews only after it is approved and merged, the same
  way as code. A branch that adds a permissive learning gets no benefit from
  it.
- Two `claude -p` behaviours were found in live runs, on Claude Code
  2.1.296:
  - `--json-schema` rejects a `"$schema": ".../draft/2020-12/schema"` key.
  - An agent's `tools:` list hides the `StructuredOutput` tool, and then no
    `structured_output` comes back.

  The librarian's schema has no `$schema` key, and its `tools:` and
  `--allowedTools` both name `StructuredOutput`.
- Knowledge items are recognized by their title prefix. Renaming one takes
  it out of the Library's view.
- A knowledge item edits a copy of the base learnings made when its branch
  was created. Two knowledge items changing the same learning conflict at
  merge, as code does.
