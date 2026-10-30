# 0066. Records in the repo: the Library and the Gazette, through records items

Status: Accepted, 2026-10-30. Supersedes the parts of
[ADR 0027](0027-the-library-stays-local.md) and
[ADR 0053](0053-the-gazette.md) that say learnings and Gazette pages are
never committed, for repositories that opt in.

## Context

ADR 0027 keeps the Library in a local store next to the journal, and ADR 0053
keeps each Gazette page in the journal. Both are per clone: a teammate, a
second machine or a fresh clone sees none of it, and losing the git dir loses
it. A private repository, or a team that wants its review knowledge and its
change history in one place, would rather keep them in the repository too.

Two constraints shape how:

- Nothing reaches the base without review (ADR 0045: the push gate, the
  shipper and the merger all require the hallmarked head). Files written
  straight to the base, or a commit added on top of a hallmarked head, would
  break that.
- The librarian runs after an item closes, when its PR is already open. So
  an item's learnings and its page can't ride in its own PR.

## Decision

- **Opt in.** `[library] commit = true` (default `false`) keeps the records
  in the repository as well as locally. `[library] dir`
  (`.hallmark/knowledge`) receives learnings, the directory
  `hall library import` already reads from the base.
  `[library] gazette_dir` (`docs/gazette`) receives pages, as
  `<gazette_dir>/<id>.html`.
- **What is committed.**
  - **Accepted learnings only.** A proposed learning is unapproved, and
    committing it would publish what you haven't agreed to. While an update
    waits for you, the accepted version it would replace is committed, as it
    is what briefs reviews.
  - Each learning is written in the canonical format without its usage
    counts (`used: 0`, no `last_used`). Counts change with every review, so
    committing them would mean a records change after every review. They are
    per clone anyway (ADR 0027).
  - A learning the store knows that is no longer accepted (retired,
    rejected) is removed. A file whose ID the store doesn't know is another
    clone's, and is left alone.
  - **Each finished item's Gazette page**: closed or dropped, printed, and
    not itself a records item. A page is copied again when it was reprinted
    from more journal events (its PR merged, CI reported). A page that differs
    only in which hall printed it (a template change) is not.
- **Records items carry them.** An ordinary item titled `Records: …` takes
  the files to the base through the normal flow.
  - The librarian's tick starts one when nothing is left to distil or
    reprint, so the items that finish together share one.
  - It runs `hall new --no-split` as the librarian, writes the files in the
    item's worktree, commits them (`docs: Hallmark records`) and runs
    `hall propose-ready`. From there the item is checked, reviewed, shipped
    and merged like any other work.
  - Its request tells the reviewer what to look for: what must not be
    published in this repository (secrets, credentials, personal data, an
    unfixed security problem), not wording or layout, which the next records
    item would write again.
  - **Until a records item has shipped, each waits for you**
    (`propose-ready --ask`). Turning the setting on by mistake, say in a
    public repository, then publishes nothing until you `hall ready` one.
    A records item you dropped is not a yes, so the next one waits too.
    Once one has shipped, the next go straight to review when `auto_ready`
    allows.
- **One at a time.** While a records item is open, or its PR is, no other
  starts. What changes meanwhile goes in the next one.
- **Cheap when nothing changed.** Each check is remembered in
  `library/records.json` as a fingerprint of the accepted learnings, the
  printed pages (path and events) and the base tip. An unchanged fingerprint
  skips the check with no git reads. A check that started a records item
  records its fingerprint too, so a records item you drop doesn't come
  straight back. It returns once something changes: a new learning, a
  reprint, a new base tip.
- **Records items are not distilled.** Distilling one would print a page for
  the next records item to carry, for ever. Unlike ADR 0021's knowledge
  items, they are not hidden from your inbox: a records item that escalates
  or waits for you is shown.
- **Reviews still read the local store.** A learning committed to the
  repository reaches a review only through the store: the librarian's, or
  one `hall library import` brought in. ADR 0027's safety argument stands.

## Alternatives considered

- **The item's own PR.** Distil before shipping, and add a records-only
  commit on top of the hallmarked head. The page would describe an item
  that hasn't shipped yet. The stamp checks in the push gate, the shipper and
  the merger would all need an exception for that commit.
- **Write to the base directly.** Fast, but it is the one thing Hallmark
  never does: work reaches the base only through review.
- **Commit proposed learnings too.** Rejected: see above. `hall library
  export` remains the way to move everything, any status.

## Consequences

- Each records item runs the repository's gates and one review. That is
  the price of the normal flow. Items that finish together share one.
- A page can describe a finding in detail, including a security one (ADR
  0053). With `commit = true` that description is published with the page.
  The records item's review is the check, and records items wait for
  you until one has shipped.
- To keep a page or learning out, drop the records item. It won't be
  recreated until something else changes, and the next records item
  carries everything still missing, that page included. To stop for good,
  set `commit = false`.
- Pages inline their images, so attachments make the repository larger.
- `[library] dir` is again where learnings live in the repository (ADR
  0021's place for them), but the store stays the source of truth.
