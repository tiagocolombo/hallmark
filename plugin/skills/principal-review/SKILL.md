---
name: principal-review
description: How Hallmark's principal engineer reviews a frozen commit - the seven lenses, severities, the evidence bar, incremental rulings, learnings and the verdict shape. Use when reviewing a Hallmark item or recording a verdict with `hall verdict`.
---

# Principal review

One review covers one frozen commit against its base branch. Read the diff,
then the code around it, and apply every lens the repository uses. Report only
what you can show breaks.

## The seven lenses

| Lens | Asks | Typical blocker |
| --- | --- | --- |
| `first_principles` | Is this the simplest change that solves the task? Does it belong in this layer? | Patches a symptom; duplicates an existing mechanism |
| `correctness` | Edge cases, error paths, concurrency, idempotency, retries | A retry double-charges; two writers race |
| `security_tenancy` | Authorization on every path, tenant isolation, secrets, injection | A query bypasses row-level security; a secret in a log line |
| `data_migrations` | Is the migration reversible and safe on live data? | A locking ALTER on a hot table; a backfill that can't resume |
| `evidence` | Do the tests prove the behaviour? Did the test count drop? | A test asserts its own mock; a suite stopped running |
| `operability` | Can someone on call see it fail and recover? | A swallowed error on a billing path; no log when it gives up |
| `repo_rules` | The repository's rules from the base branch (CLAUDE.md, AGENTS.md, the files they list, the review notes) | Breaks a convention the repo wrote down; weakens a rule file |

How to apply them:

- **First principles.** Compare the change with the request. Look for an
  existing helper, layer or mechanism that already does this. Flag scope
  creep that adds risk, not size alone.
- **Correctness.** Walk each changed function with the inputs that matter:
  empty, one, many, missing, duplicate, out of order, concurrent, retried,
  failing halfway. Follow errors to where they are handled.
- **Security and tenancy.** Who can reach this path? Is authorization checked
  on every entry? Can one tenant see or change another's data? Is user input
  ever executed, interpolated into a query or a shell, or logged with
  secrets?
- **Data and migrations.** Can it be rolled back? Does it lock or rewrite a
  large table? Does old code still work against the new schema while it
  deploys?
- **Evidence.** Find the tests for each behaviour the change claims. Would
  they fail without the change? Were tests deleted, skipped or weakened?
- **Operability.** When this fails in production, is there a log, a metric or
  an error someone will see? Can it be retried or turned off?
- **Repo rules.** Check the change against the rules in the prompt, which
  come from the base branch. The branch's own copies don't count.

Apply only the lenses the prompt lists when it lists some.

## Severities

| Severity | Bar | Blocks approval |
| --- | --- | --- |
| `blocker` | Must not ship: data loss or corruption, a security hole, a broken main path, wrong results users will hit | yes |
| `major` | A real defect that should be fixed before merging: a likely edge case that fails, missing evidence for the main behaviour, a broken repo rule | yes |
| `minor` | Worth fixing, but safe to ship: a rare edge case with a cheap workaround, a confusing name on a public API | no |
| `nit` | Style and polish | no |

Minors and nits ride along into the pull request body. List them when they
help; never let them crowd out what matters.

## The evidence bar

- **Show the break.** Every finding has a `scenario`: the concrete inputs or
  state, then the wrong result. "Two workers read attempt 2 of delivery 17
  before either writes; the endpoint receives event 17 twice."
- **Point at the line.** `file` is repository-relative and `line` 1-based (0
  for the whole file). Use the line where the fix belongs.
- **Say how to fix it** in one or two sentences.
- **Read before you claim.** If the scenario depends on code you haven't read
  (a caller, a constraint, a config), read it. If it turns out handled, drop
  the finding.
- **No speculation, no padding.** Don't report what "could" go wrong without
  a path to it. Don't invent findings to look busy: an empty list for a sound
  change is the right answer.
- One finding per problem. Group repeated instances of the same mistake into
  one finding that names them.

## Approval is computed

There is no approve field. Hallmark approves when no `blocker` or `major`
stays open after your rulings. Rate each finding by its consequences and let
the rule decide. After the repository's `max_rejects` rejected reviews, the
item escalates to a person.

## Incremental rounds

From round 2 the prompt shows only the diff since the last review and lists
the findings still open, with the fixer's claims and disputes. For each one,
add a ruling to `prior`:

- `fixed`: the code now handles the scenario. Check it; a claim is not proof.
- `open`: still broken. Add a `note` saying what is still wrong.
- `withdrawn`: the fixer's dispute is right, or the finding was wrong. Say
  why in the `note`.

Rule on every listed finding; one you leave out stays open. Don't repeat an
open finding as a new one. New problems introduced by the fix are new
findings.

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
  "summary": "Adds retries with jitter to webhook delivery. One race lets two workers deliver the same event.",
  "findings": [
    {
      "lens": "correctness",
      "severity": "blocker",
      "file": "apps/api/src/webhooks/retry.ts",
      "line": 42,
      "title": "Two workers can retry the same delivery",
      "scenario": "Workers A and B both read attempt 2 of delivery 17 before either writes; the endpoint receives event 17 twice.",
      "fix": "Claim the delivery with SELECT ... FOR UPDATE SKIP LOCKED before sending."
    }
  ],
  "prior": [{ "id": "F1", "status": "fixed" }],
  "learnings": [{ "id": "L-0017", "held": true }]
}
```

In a headless review, return it through the structured output. To record one
by hand, save it to a file and run `hall verdict <item> --file verdict.json`
(`--file -` reads stdin); `hall schema verdict-input` prints the schema.
