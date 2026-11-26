# 0069. Agents consult the Lead: answers between agents, kept in the Library

Status: Proposed, 2026-11-25. Amends 0027 (a new kind) and 0031 (what may
reach the inbox).

## Context

Agents can only ask a person. `hall ask` puts a question in the inbox, and
the answer reaches the asking session through the resume worker (ADR 0020).
Most of what an agent needs, though, is not a decision. It's a fact about
the code: where migrations run, which error code a case uses, what the
parent item's API looks like. Those questions either go to the person,
filling the office ADR 0031 keeps for escalations, or aren't asked, and the
agent guesses. The headless fixer can't ask at all.

The same questions come back. Parallel parts ask about the same contract.
Item after item explores the same module. The Library keeps what reviews
taught (ADR 0021, 0027), but only reviewers read it, and it holds no
questions and answers.

## Decision

### `hall consult`

`hall consult "<question>" [--paths <glob>…] [--item <id>]` answers an
agent's question about the code **within the call**. A headless run never
waits on anyone.

1. **Search first.** The Library's accepted learnings and answers are
   searched (the built-in or qmd search, ADR 0021), weighted toward entries
   whose scope matches `--paths` or the item's changed paths. A hit above
   `[consult] reuse_score` is returned as `source: "library"`, with no model
   run. A stale answer is never reused this way.
2. **Otherwise the Lead answers** (ADR 0068): a short read-only run
   (`Read`, `Grep`, `Glob`, `StructuredOutput`) on `consult_model` (default
   `sonnet`), with the question, the asking item's request and the board, the
   search hits and the repository. The `[consult]` timeout defaults to three
   minutes, and the run is booked to the asking item as role `consult`. It
   returns one outcome:
   - `answered`: an answer with evidence (file and line), and the paths it
     covers;
   - `product`: a question about what the product should do, not about the
     code. It goes to the Product Manager (ADR 0068). The PM answers only
     from the item's acceptance criteria, its request and added scope, or a
     recorded human decision, and cites the one it used. When none covers
     the question, Hallmark files it with `hall ask` for the asking item and
     session, and the reply is the question's (`status: waiting`);
   - `unknown`: the code doesn't say. The reply tells the agent to go ahead
     on a stated assumption and record it in its summary.
3. **Kept.** An `answered` outcome is saved to the Library and journaled on
   the asking item as `consulted { question, answer, entry }`.

Failures fail open, as intake does: no `claude`, a budget stop or a failed
run reply `unknown` with the reason.

### Who may consult

- The developer's session, and the subagents building parallel parts. The
  `hallmark` skill says to consult before guessing, and to ask the person
  only for decisions.
- The fixer and the Delivery office's dev runs: `Bash(hall consult:*)` joins
  their allowed tools.
- **Not the reviewer or the verifier.** A review must stay independent of
  what the author's side was told. A reviewer that asks the Lead "is this
  fine?" is reviewed by the Lead.

### Answers in the Library

`answer` is a fifth kind, next to rule, false-positive, map and decision. An
answer entry is `A-0012-<slug>.md` in the same store (ADR 0027). Its
frontmatter holds the question, `scope` (the paths it covers), `source` (the
asking item), `asked_by`, `verified_at` (the base commit it was answered
against) and `status`. The body is the answer with its evidence.

- **Accepted when written.** An answer is a cited fact, not a ruling. It
  never reaches the inbox's `learnings` entry.
- **Stale when its code changes.** An answer is stale once a file in its
  scope changed on the base after `verified_at`. Search returns stale
  answers last, marked stale, and step 1 doesn't reuse them: the Lead
  answers again and the entry is updated.
- **Disputed when wrong.** `hall consult --dispute A-0012 "<why>"` records
  the dispute, and the next consult on it goes to the Lead with the dispute
  attached. That's the downvote.
- **Never in a review's briefing.** Answers come from the author's side, so
  they can't soften a review (ADR 0021's rule). Only a learning can reach a
  reviewer.
- **Curated by the librarian.** Its tick, with no model run, retires answers
  stale for `retire_after` and nobody has asked about again. When it
  distils an item, the digest lists the item's consults. An answer asked for
  again and again, or one that explains a module, may become a `map` or
  `rule` learning, and those go through `auto_accept` or `hall library
  review` as any learning does. The answers it replaces are retired.
- `hall library search` returns answers with `--kind answer` or `--kind
  all`. `hall library show A-0012` shows one.

### The person's questions stay theirs

`hall ask` runs the same search, with no model run, over human `decision`
learnings and answered questions. When one matches well, the reply carries
it as `prior`, and its `next` suggests reusing it. The question is still
filed. Neither the Lead nor the PM makes a new decision for the person: the
PM only applies one already recorded, and says which.

## Consequences

- Questions about the code are answered in seconds, with evidence. Product
  questions that a criterion or an earlier decision already settles are
  answered by the PM, and the inbox keeps only new decisions.
- A question asked once is answered for every later agent, until the code
  it describes changes.
- Each new answer costs one short Sonnet run on the asking item's budget. A
  reused one costs a search.
- Reviews are untouched: they read learnings, never answers.
- The web app and the TUI's Library views gain the `answer` kind, and
  `docs/commands.md` and `docs/configuration.md` gain `hall consult` and
  `[consult]`.
