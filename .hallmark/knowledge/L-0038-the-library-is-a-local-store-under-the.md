---
id: L-0038
kind: map
scope: ["src/library/**", "src/commands/library.rs"]
lens: first_principles
source: ["H-0012"]
status: proposed
used: 0
---
# The Library is a local store under the git common dir, not committed knowledge; approval is a human step through hall library review.

Learnings are Markdown files in <git-common-dir>/hallmark/library/, shared by every worktree and never committed. The librarian writes proposals there, and hall library review --accept/--reject is the only approval path, with an inbox entry for proposed learnings. Briefings, built-in and qmd search, and retirement all read this store. hall library import moves a committed .hallmark/knowledge/ into the store, and export/import (portable.rs) move it between machines. ADR 0027 supersedes ADR 0021's commit-and-approve model.
