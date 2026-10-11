# 0027. The Library stays local: learnings live next to the journal

Status: Accepted, 2026-10-10. Supersedes the parts of
[ADR 0021](0021-the-library.md) about where learnings live, knowledge items,
reading from the base branch, review and retirement.

## Context

ADR 0021 committed learnings to `.hallmark/knowledge/` and changed them only
through knowledge items, reviewed and shipped like code. That keeps a branch
from softening its own review, but it puts the Library in the repository. For
a public repository that makes no sense: what reviews of your changes taught
is your working knowledge, not part of the project, and every learning would
cost a pull request.

## Decision

- **The store.** Learnings live in `<git-common-dir>/hallmark/library/`,
  next to the journal:
  - `learnings/L-0017-<slug>.md`: the current version of each learning, in
    the same Markdown-with-frontmatter format (ADR 0021), one file per ID,
    whatever its status;
  - `history/L-0017/<stamp>-replaced.md` and `…-rejected.md`: every version a
    change replaced and every proposal you rejected, so nothing is lost;
  - `.lock`: one writer at a time (the daemon runs workers in parallel).
    Files are written through a temporary file and a rename.

  The store is never committed, is shared by every worktree, and outlives
  them. `[library] dir` no longer says where learnings live; it is only
  where `hall library import` looks for committed ones.
- **No knowledge items.** The librarian writes straight into the store:
  kinds in `auto_accept` as `accepted`, the rest as `proposed`. It still
  updates rather than duplicates (an `updates` ID, or a matching title), and
  the source item still gets `distilled { learnings }`, now without `into`.
  It creates no item, makes no commit and runs no `hall propose-ready`.
  Items titled `Library: …` left by ADR 0021 are still not distilled.
- **Approval stays with you, locally.** `hall library review` lists the
  proposed learnings (`new` or `update`).
  - `--accept` makes a proposal `accepted`. A retired learning given to
    `--accept` is reinstated from its last accepted version.
  - `--reject` moves the proposal to `history/` with your `--reason`, and
    restores the version it replaced, or removes a new learning. Rejecting
    again changes nothing; rejecting an accepted learning is `USAGE`
    (retire it instead).
  - A rejected ID is never reused: IDs are allocated above every ID in
    `learnings/` and `history/`.
  - Every change takes effect at once.
- **The inbox.** Proposed learnings are one inbox entry of kind `learnings`
  ("3 proposed learnings to review", `hall library review`). Counting them
  reads only each file's frontmatter. Notifications route the kind to the
  morning digest by default (`[notify.route] learnings`).
- **Readers use the store.** Briefings and the default search use accepted
  learnings. While an update of an accepted learning waits for you, the
  accepted version it would replace (from `history/`) is what briefs, so a
  proposal never takes a learning away from reviews. `--proposed` search
  adds the current proposals. The built-in FTS5 index and the qmd export are
  keyed by the documents they were built from, as before; the qmd export is
  now flat (`<collection>/L-0017-….md`).
- **Retirement is local and immediate.** `hall library retire <id> --reason`
  and the librarian (a verdict with `held: false` after the file last
  changed, nothing using it for `retire_after` since its last use or change,
  or the model's `retire` list) set `status: retired`. The previous version
  goes to `history/`. "Last changed" is the file's modification time;
  `retirements.json` and the `git log` dates are gone.
- **Portability.**
  - `hall library import [<source>]` adds learnings the store doesn't know,
    keeping their IDs, file names and text. With no source it reads
    `[library] dir` on the base branch (`git ls-tree` and one
    `git cat-file --batch`); a source can be a directory of learning files
    or an export file. Files that aren't learnings and IDs the store already
    knows are skipped and reported. After importing from the base, `next`
    suggests `git rm -r -q .hallmark/knowledge && git commit -m "Move the
    Library out of the repo"`.
  - `hall library export [--out <file>]` writes every current learning, any
    status, as `{"format": "hallmark-library", "version": 1, "learnings":
    [{path, content}]}`. Without `--out` the reply carries the same
    `learnings`, so it imports too. History is not exported.

## Consequences

- **Still safe.** ADR 0021 read reviews' learnings from the base branch so
  that a branch could not soften its own review. No branch can write to a
  directory inside the git dir, outside every worktree, so this is stricter:
  a learning committed anywhere, even on the base, never reaches a review
  until someone imports it. What a review is told depends only on what you
  (or `auto_accept`) accepted.
- The Library is per clone. Two clones of one repository learn separately;
  `export` and `import` move learnings between them. Losing the git dir
  loses the Library, as it loses the journal.
- Approving a learning is a command, not a pull request. Nobody else sees or
  reviews it; that is the point for public repositories, and a team that
  wants shared, reviewed learnings can commit an export and import it.
- Usage counts (`used`, `last_used`) are still written into files the
  Library touches, from the journals, and never lowered.
- Two ADR 0021 consequences no longer hold: there are no knowledge items to
  recognize by title, and no merge conflicts between them.
