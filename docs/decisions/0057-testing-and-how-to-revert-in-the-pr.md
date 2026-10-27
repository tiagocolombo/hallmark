# 0057. The PR description says how the change was tested and how to revert it

Status: Accepted, 2026-10-27. Amends 0039. Amended by 0063: a sixth section, `## Risk`.

## Context

ADR 0039 made the PR description What, Why and How, and moved the review
into comments. Two things a reader of a PR wants were missing: what was done
to show the change works, beyond "the review passed", and what undoing it
takes once it has merged. Anthropic's PR template asks for both (Testing and
How to revert), has the agent draft them and the person who asked for the
change sign off, and says the second field is what makes mistakes fast to
undo.

In Hallmark the facts for both exist at ship time. The gates that passed on
the hallmarked commit are in the journal (`checks`), the author's own
testing is best known to the author, and a revert's steps are the author's
to state (the reviewer's data and migrations lens checks that the change
is reversible, but doesn't write the steps).

## Decision

- **Two more sections.** `items/<id>/pr-body.md` gains `## Testing` and
  `## How to revert` after `## How`, before `Closes #<n>` and the footer.
- **Testing** is `propose-ready --tested` when the author gave one, then
  always one line naming the gates that passed on the commit, from the
  last passing `checks` event: "The repository's gates passed on this
  commit: `cargo test`, `cargo clippy`." A repository with no gates says
  so. The gates are the repository's checks, not the review, so naming them
  doesn't break 0039's rule that the description holds nothing of the
  review.
- **How to revert** is the author's `propose-ready --revert` steps when
  given, then always "Also `git revert` the commit this PR lands as on
  `<base>`" for the base the PR targets. Without `--revert` the line reads
  "`git revert` the commit this PR lands as on `<base>`; the author listed
  no other steps." The line says "Also", not "Then", so it sets no order;
  authors say whether each of their steps comes before or after the revert.
- **The author is asked, not required.** The `hallmark` skill and the
  session hook name both flags and say when to use them: `--tested` for
  what was run or tried beyond the gates, `--revert` for what a revert
  needs beyond `git revert`. The skill says never to claim a test that
  wasn't run.
- The `proposed_ready` event carries `tested` and `revert`, optional, so
  old journals read as before.

## Consequences

- A PR says what proved it and how to undo it, in the author's words where
  they add something and from the record where they don't.
- The default revert line is a plain statement of what the author said,
  not a promise that a revert is safe: a change that needs more should say
  so in `--revert`, and a reviewer who sees a migration or a flag with no
  revert steps has a finding under operability.
- Risk, the third field in Anthropic's template, waits for the merger's
  risk tiers: the tier is a fact about the change that the loop should
  compute, not a line the author writes.
