---
name: principal-review
description: How Hallmark's principal engineer reviews a frozen commit - the test audit first, then the seven lenses, severities, the evidence bar, the refutation pass, incremental rulings, learnings and the verdict shape. Use when reviewing a Hallmark item.
---

# Principal review

One review covers one frozen commit against its base branch. The bar is a
principal engineer's: no real defect, gap or untested behaviour gets through,
and every finding is proven. Missing a defect and inventing one are both
failures. Work in this order:

1. **Map the change.** Read the request, then every changed line of
   hand-written code; skim generated files, lockfiles and data. Read each
   changed file in full where the hunk alone doesn't explain it, plus the
   callers and types it touches. If the diff was cut, read the omitted files
   with `git diff`.
2. **Audit the tests** (below). Do it before the other lenses: passing gates
   prove only that the tests that exist pass.
3. **Apply every lens** the prompt lists.
4. **Refute your own findings** (below), then return the verdict.

## The test audit

The author's job is to ship tests that pin every behaviour the change adds or
alters. Your job is to check that they did. Map each behaviour to the test
that fails if that behaviour breaks.

For each changed function or branch, list the cases that matter, then find
the test for each:

- the main path, and each documented option or mode;
- boundaries: empty, one, many, the limit, just past it, zero, negative;
  unicode and paths with spaces where input is text;
- each error path: invalid input, a missing file or record, a refused or
  failed subprocess, a timeout;
- state: repeated (idempotent), out of order, concurrent, interrupted
  halfway and resumed;
- for a bug fix: a regression test that fails on the base branch and passes
  here. A fix without one is not done.

Then ask of each test that claims to cover a case:

- **Would it fail without the change?** Mentally flip the condition, drop
  the call or return early at the changed line. If no test fails, the
  behaviour is unpinned, and that mutation is your scenario: "Change `>=` to
  `>` at line 42, or delete the retry: every test still passes."
- **Does it assert the outcome**, not just that the code ran? A test with no
  assertion, one that asserts its own mock or fixture, or one that only
  checks `is_ok()` on a path whose value matters, is not evidence.
- **Is it deterministic?** Real sleeps or wall-clock time, test order, shared
  temp paths or global state, the network, or a real model run make it
  flaky, and flaky tests get ignored.
- **Was anything weakened?** Deleted or skipped tests, loosened assertions,
  widened tolerances, accepted snapshots whose diff changes behaviour, a
  lower test count, a new `ignore`, `skip` or `only`. Each one needs a reason
  in the change; without one it is a finding.

Prefer a few sharp tests over many shallow ones. Don't ask for tests of
trivial getters, generated code or what the type system already proves, and
don't ask for a coverage number: name the case that is unpinned.

## The seven lenses

| Lens | Asks | Typical blocker |
| --- | --- | --- |
| `first_principles` | Is this the simplest change that solves the task? Does it belong in this layer? | Patches a symptom; duplicates an existing mechanism |
| `correctness` | Edge cases, error paths, concurrency, idempotency, retries | A retry double-charges; two writers race |
| `security_tenancy` | Authorization on every path, tenant isolation, secrets, injection | A query bypasses row-level security; a secret in a log line |
| `data_migrations` | Is the migration reversible and safe on live data? | A locking ALTER on a hot table; a backfill that can't resume |
| `evidence` | Do the tests pin every behaviour? Would they fail without the change? | A behaviour no test checks; a fix with no regression test; a weakened test |
| `operability` | Can someone on call see it fail and recover? | A swallowed error on a billing path; no log when it gives up |
| `repo_rules` | The repository's rules from the base branch (CLAUDE.md, AGENTS.md, the files they list, the review notes) | Breaks a convention the repo wrote down; weakens a rule file |

How to apply them:

- **First principles.** Compare the change with the request: does it do all
  of it, and nothing it wasn't asked to? Look for an existing helper, layer
  or mechanism that already does this. Flag scope creep that adds risk, not
  size alone, and changes that leave the code harder to change than before.
- **Correctness.** Walk each changed function with the inputs that matter:
  empty, one, many, missing, duplicate, out of order, concurrent, retried,
  failing halfway. Follow every error to where it is handled, and check that
  a failure halfway leaves state consistent. Check the contract each caller
  relies on: types, units, ordering, nullability, encoding, and the platform
  (paths, line endings, shells).
- **Security and tenancy.** Who can reach this path? Is authorization checked
  on every entry? Can one tenant see or change another's data? Is user input
  ever executed, interpolated into a query, a path or a shell, or logged
  with secrets? Does a new dependency or permission widen what can run?
- **Data and migrations.** Can it be rolled back? Does it lock or rewrite a
  large table? Does old code still work against the new schema while it
  deploys? Can the previous version still read what this one stores?
- **Evidence.** The test audit above. Report each unpinned behaviour, each
  missing regression test and each weakened or non-deterministic test.
- **Operability.** When this fails in production, is there a log, a metric or
  an error someone will see? Does the message say what to do? Can it be
  retried or turned off?
- **Repo rules.** Check the change against the rules in the prompt, which
  come from the base branch. The branch's own copies don't count.

Apply only the lenses the prompt lists when it lists some.

## Severities

| Severity | Bar | Blocks approval |
| --- | --- | --- |
| `blocker` | Must not ship: data loss or corruption, a security hole, a broken main path, wrong results users will hit | yes |
| `major` | A real defect that should be fixed before merging: a likely edge case that fails, a changed behaviour no test pins, a bug fix with no regression test, a weakened or flaky test, a broken repo rule | yes |
| `minor` | Worth fixing, but safe to ship: a rare edge case with a cheap workaround, an unpinned branch that only guards the impossible, a confusing name on a public API | no |
| `nit` | Style and polish | no |

Minors and nits ride along into the review comment on the pull request. List them when they
help; never let them crowd out what matters.

## The evidence bar

- **Show the break.** Every finding has a `scenario`: the concrete inputs or
  state, then the wrong result. "Two workers read attempt 2 of delivery 17
  before either writes; the endpoint receives event 17 twice." For a test
  gap, the scenario is the mutation no test catches.
- **Point at the line.** `file` is repository-relative and `line` 1-based (0
  for the whole file). Use the line where the fix belongs; for a missing
  test, the line of the unpinned code.
- **Say how to fix it** in one or two sentences. For a test gap, name the
  case to add and what it asserts.
- **Read before you claim.** If the scenario depends on code you haven't read
  (a caller, a constraint, a config, an existing test), read it. If it turns
  out handled, drop the finding.
- **Stay in scope.** Report problems the change introduces, and old ones it
  makes reachable or worse. Leave alone code it doesn't touch, and what the
  gates already enforce (formatting, the linter).
- **No speculation, no padding.** Don't report what "could" go wrong without
  a path to it. Don't invent findings to look busy: an empty list for a sound
  change is the right answer.
- One finding per problem. Group repeated instances of the same mistake into
  one finding that names them.

## The refutation pass

Before you return, try to prove each finding wrong: look for the guard, the
caller that can't pass that input, the test that already covers it, the rule
that asks for exactly this. Drop the finding if you succeed. Then check each
severity against the table, not against your confidence. Last, ask what you
didn't read: read it now, or say in the `summary` which part went
unreviewed.

## Approval is computed

There is no approve field. Hallmark approves when no `blocker` or `major`
stays open after your rulings. Rate each finding by its consequences and let
the rule decide. After the repository's `max_rejects` rejected reviews, the
item escalates to a person.

When the prompt says this is the last round before escalation, a rejection
goes to that person, not back to the fixer. Review as usual, then write the
`summary` as their diagnosis: is the approach wrong, and what would you do
instead (a different design, a smaller scope, a split), or are the remaining
findings ordinary and worth one more round? The findings restated are not a
diagnosis. Hallmark puts that summary in the person's inbox.

## Incremental rounds

From round 2 the prompt shows only the diff since the last review and lists
the findings still open, with the fixer's claims and disputes. For each one,
add a ruling to `prior`:

- `fixed`: the code now handles the scenario, and for a correctness or
  evidence finding a test now fails without the fix. Check both; a claim is
  not proof. When no deterministic test can pin the behaviour (CI or config,
  docs, prompt text, a refactor that keeps behaviour), check it by reading
  and rule `fixed` with a `note` saying why there is no test.
- `open`: still broken. Add a `note` saying what is still wrong.
- `withdrawn`: the fixer's dispute is right, or the finding was wrong. Say
  why in the `note`.

Rule on every listed finding; one you leave out stays open. Don't repeat an
open finding as a new one. New problems introduced by the fix, including
tests it weakened, are new findings.

## Rebase reviews

When the branch was rewritten after its approval (a rebase with resolved
conflicts, or changed commits), the prompt may show only "What the rebase
changed": a diff between the approved change and the rewritten one. Each side
is itself a diff of the item against its base. Review only those
differences. A resolved conflict must keep what both sides meant, this item's
change and what the base gained since, with nothing dropped, duplicated or
half-merged. Any other difference must hold up on its own. Don't review the
rest of the change again: it is approved.

## Learnings

When the prompt has a Library briefing, its learnings (`L-0017`, …) are what
this repository has learnt from earlier reviews: rules, false positives to
avoid, maps of modules, decisions. Use them. For each learning you relied on,
add `{"id": "L-0017", "held": true}` to `learnings`, or `held: false` with a
`note` when this change shows it is wrong or stale. Leave out the ones you
didn't use.

## The verdict

```json
{
  "summary": "Adds retries with jitter to webhook delivery. One race lets two workers deliver the same event, and no test pins giving up.",
  "findings": [
    {
      "lens": "correctness",
      "severity": "blocker",
      "file": "apps/api/src/webhooks/retry.ts",
      "line": 42,
      "title": "Two workers can retry the same delivery",
      "scenario": "Workers A and B both read attempt 2 of delivery 17 before either writes; the endpoint receives event 17 twice.",
      "fix": "Claim the delivery with SELECT ... FOR UPDATE SKIP LOCKED before sending."
    },
    {
      "lens": "evidence",
      "severity": "major",
      "file": "apps/api/src/webhooks/retry.ts",
      "line": 58,
      "title": "No test pins giving up after the last attempt",
      "scenario": "Change `attempt >= max` to `attempt > max`: delivery is sent one extra time and every test still passes.",
      "fix": "Add a test that fails the endpoint `max` times, then asserts no further send and the gave-up log line."
    }
  ],
  "prior": [{ "id": "F1", "status": "fixed" }],
  "learnings": [{ "id": "L-0017", "held": true }]
}
```

In a headless review, return it through the structured output. `hall verdict`
is not for agents: inside Claude Code it is refused (`GATE_DENIED`), so an
agent doesn't approve work (a guardrail, not a lock). A person records one by hand from a terminal: save it
to a file and run `hall verdict <item> --file verdict.json` (`--file -` reads
stdin); `hall schema verdict-input` prints the schema. Theirs is recorded as a
human override.
