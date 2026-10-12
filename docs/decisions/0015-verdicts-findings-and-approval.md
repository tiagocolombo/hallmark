# 0015. Verdicts, findings, and who decides approval

Status: Accepted, 2026-10-09. Amended by 0039: minors and nits ride along in
the review comment on the PR, not its description.

## Context

The reviewer is a model. Its opinion of the change must not be the
approval: a script decides, from data the schema guarantees. Findings must
be traceable across rounds: raised, claimed fixed, disputed, settled.

## Decision

- **The model returns** `plugin/schemas/verdict.json`: a `summary`, new
  `findings` (lens, severity, file, line, title, scenario, fix), a ruling in
  `prior` on each open finding (fixed, open, withdrawn), and the briefing
  `learnings` it used and whether they held. It has **no approve field**.
- **Hallmark judges it** (`findings::Ledger::judge`):
  - new findings are numbered `F<n>`, unique per item;
  - prior rulings for unknown IDs are ignored, and open findings the
    reviewer leaves out stay open;
  - **approval = 0 open blockers and 0 open majors**, counted after the
    rulings. Minors and nits ride along into the PR body.
- **The `verdict` event** stores the numbered findings, the rulings, the
  open counts, the summary, the report path (`reviews/r<n>.md`), the
  reviewer model, the learnings used, the base commit the rules came from,
  and the last reviewed commit for incremental reviews.
- **Escalation** is decided when the verdict is written: a rejection that
  reaches `max_rejects` is `escalated` (ADR 0008).
- **The fixer** records `fix_submitted` with the findings it claims to fix
  and the ones it disputes (with reasons). Only a later review settles them.
- **The hallmark note** (`stamp::strike`) is JSON: the item, round,
  reviewer model, open counts and time.
- The index keeps a `findings` table for history and the terminal app.

## Consequences

- No prose is ever parsed for a decision.
- A reviewer can't approve by saying so, and can't lose a finding by
  forgetting it.
- The lens and severity lists live in the schema and in `findings.rs`; a
  test keeps them equal.
