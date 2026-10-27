# 0060. The last round before escalation asks the reviewer for a diagnosis

Status: Accepted, 2026-10-27. Amends 0018.

## Context

After `max_rejects` rejected reviews an item escalates to a person (ADR
0015). What that person found in the inbox was a count: "rejected 3
times", and a pointer to the verdicts. Reading three reviews to work out
whether the change is nearly there or wrong from the start is the cost
the escalation was meant to save them.

Anthropic's reviewer behaves differently on its third round: instead of
listing more findings, it asks the author to reconsider the approach. Their
numbers say why: with an agent author, rounds that restate findings don't
converge, and the fix is to change what the reviewer says, not how often.

## Decision

- **The reviewer is told when a rejection would escalate.** The prompt
  gains a section, "The last round before escalation", when
  `rejects + 1 >= max_rejects` (read from the base, as the verdict does).
  It says to review as usual and then write the `summary` as the diagnosis
  the person needs: whether the approach itself is wrong and what to do
  instead (a different design, a smaller scope, a split), or whether the
  remaining findings are ordinary and one more round would do. The
  `principal-review` skill says the same.
- **The diagnosis reaches the person.** The item folds the escalating
  verdict's summary into `escalation`; the inbox entry reads "<title>:
  rejected 3 times. The reviewer: <summary>", and the session notification
  carries it too. An approval, or a later verdict that doesn't escalate,
  clears it.
- Nothing else changes: the verdict's shape, the approval rule and
  `max_rejects` stay as they are. The section is only text in the prompt,
  and the summary was always recorded.

## Consequences

- An escalation arrives with a one-paragraph diagnosis instead of a count,
  so the person can decide between `hall retry`, `hall pull-back` and
  `hall drop` without reading the rounds.
- The round itself costs the same; the reviewer writes a different summary,
  not a longer review.
- With `max_rejects = 1` every review is the last round, so the section is
  shown from round 1 ("not rejected yet"), and a repository that escalates
  on the first rejection gets the diagnosis too.
- The section changes what the summary says, not the bar: the reviewer
  still rules on every open finding as it stands.
