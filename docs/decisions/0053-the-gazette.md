# 0053. The Gazette: a newspaper page for each finished item, for people

Status: Accepted, 2026-10-23.

## Context

An item's record is complete but written for agents. `hall log` is a stream
of events, `hall export` a Markdown dump with every run's steps, and the
findings are scenarios a reviewer wrote for a fixer. Someone who asked for a
change, or a teammate who never saw the session, wants something else: what
changed and why, how the review went, what was caught and fixed, what is
still open, and any mockup or screenshot that shows it.

The name: the journal is already the event log (ADR 0003, ADR 0008). The
page is the **Gazette**.

## Decision

- **One page per item**, `items/<id>/gazette.html` in the journal, next to
  `export.md`. It is a single HTML file laid out like a newspaper's front
  page. The stylesheet is inlined, images are inlined as `data:` URIs, it
  uses system fonts and has light and dark themes. It has no scripts and
  loads nothing from the network, so it opens from disk and inside the web
  app's sandboxed frame.
- **Code sets the facts.** `gazette::page` builds them from the journal:
  - the dateline: outcome, how long it took, rounds;
  - the hallmark stamp;
  - the verdicts round by round, with what was open at each;
  - how often each gate passed, and the PR's CI;
  - the timeline in plain sentences;
  - each finding and how it ended, with those still open as "Corrections";
  - the learnings the item produced;
  - the cost by role;
  - the PR and commits.

  No model counts anything.
- **The librarian writes the words, in the same run.** The learnings schema
  gains a `gazette` object:
  - `kicker`, `headline`, `deck` and `story`;
  - a line per round;
  - a plain headline and text per finding ID;
  - a `diagram` outline (2 to 8 nodes in left-to-right order, labelled
    edges) when the change has a flow worth drawing.

  One Haiku run does both jobs, so the page costs about what distilling
  costs. The prose is saved as `items/<id>/gazette.json`. Without it (an
  older item, a failed run, `hall gazette` before the librarian has run) the
  page uses the item's own words: its title, the dev agent's summary, why and
  how, and each finding's title and scenario.
- **The diagram is laid out by Hallmark.** Nodes take columns by the longest
  path along edges that point forward in the outline's order. An edge that
  points back is drawn dashed, under the boxes. Labels are clipped and
  escaped. Mermaid and other renderers would need scripts or the network.
- **Attachments.** `hall attach <file|url> [--item] [--caption]` copies a
  file into `items/<id>/attachments/` (at most 5 MB) and records an
  `attached` event. Images become plates; other files and links are listed.
  Links in the request are listed too. Attaching the same file or link with
  the same caption again changes nothing.
- **When it is printed.**
  - The librarian prints the page right after it distils an item: closed,
    escalated, or dropped after an escalation. A page that fails to print
    is logged and never fails the distillation.
  - A page records which hall printed it and how many journal events it
    was printed from (two `<meta>` lines). While its item is closed,
    escalated or dropped, a page that differs on either is reprinted from
    code with no model run: by the librarian's tick (a merge, CI reporting
    after the PR opened), and by `hall gazette` when it reads one.
  - A page stays as it was while its item is back in the loop, until the
    item finishes again and is distilled again.
- **Readers.**
  - `hall gazette [<id>]` prints the path, and `--open` opens the page,
    `--html` includes it and `--list` lists every edition.
  - The terminal app opens the selected item's page in the browser on
    <kbd>n</kbd> once it exists.
  - The web app shows it on the item's Gazette tab, in a sandboxed frame
    (no scripts, links open in a new tab) with the page's theme, and
    lists the editions on a Gazette view. It runs `hall gazette <id>
    --html` and `hall gazette --list` like every other call (ADR 0052).
    `--open` and `hall attach` are refused there.

## Consequences

- The page is a local record, like the journal and the Library: never
  committed, per clone. It can describe a security finding in detail, so
  sharing one is a choice.
- The librarian's run now writes more. The Gazette's words are tied to the
  learnings' run. A run that fails loses both, and the page falls back to
  the item's own words until the item is distilled again.
- Distilling also fixed the digest's "human's decisions": a pull-back or
  drop by one of Hallmark's own workers (a rebase that changed the patch) is
  no longer reported as your decision.
- A template change reaches every page: a new hall reprints pages as they
  are read or as the librarian's tick passes them.
