# 0036. Test-first review: an unpinned behaviour blocks approval

Status: Accepted, 2026-10-12

## Context

The gates prove that the tests which exist pass. They don't prove that the
change is tested. The `evidence` lens asked "do the tests prove the
behaviour?", but it was one lens among seven, applied after the others, and
"missing evidence" was a major only for the main behaviour. A change could
pass review with an error path, a boundary or a bug fix that no test pins.

Established practice points the same way:

- Google's reviewers read every line, and check that tests "will fail when
  the code is broken" ([eng-practices](https://google.github.io/eng-practices/review/reviewer/looking-for.html)).
  Its mutation testing shows reviewers the surviving mutants in a change:
  a line that can be broken without any test failing
  ([Petrović et al., ICSE 2023](https://homes.cs.washington.edu/~rjust/publ/mutation_testing_resolution_icse_2023.pdf)).
- SQLite: a bug isn't fixed until a test that reproduces it is in the suite
  ([testing](https://www.sqlite.org/testing.html)).
- Meta's TestGen-LLM keeps a generated test only if it builds, passes
  reliably over repeated runs and adds coverage
  ([arXiv 2402.09171](https://arxiv.org/abs/2402.09171)).
- Claude Code's code-review plugin re-validates each issue before reporting
  it, which keeps false positives out.

## Decision

- **The reviewer audits the tests first** (`principal-review`, "The test
  audit"), before the other lenses. For each changed behaviour it lists the
  cases (main path, boundaries, error paths, repeated, concurrent or
  interrupted runs) and finds the test that fails if the behaviour breaks.
  It can't run tests, so it reasons like a mutation tool: flip the condition
  at the changed line; if no test would fail, the behaviour is unpinned, and
  that mutation is the finding's scenario.
- **Severities.** A changed behaviour no test pins, a bug fix with no
  regression test, and a weakened, skipped or non-deterministic test are
  `major`, so they block approval. An unpinned branch that only guards the
  impossible is `minor`. The lens list and the schema don't change.
- **A refutation pass.** Before returning, the reviewer tries to prove each
  finding wrong and drops the ones it can, then names in the summary any
  part of the change it didn't review.
- **The author is the test engineer.** The `hallmark` skill has a "Tests
  before review" step: list the cases, write a test per case, watch each new
  test fail without the change, keep tests deterministic, never weaken one.
  Parallel parts carry the same duty.
- **The fixer writes the test first** for a correctness or evidence finding,
  and the reviewer rules `fixed` only when that test exists.
- **No coverage number.** The reviewer names the case that is unpinned, not a
  percentage. A repo that wants a measured floor adds a test-strength gate
  (mutation testing of the changed code, or a coverage floor); the
  `hallmark-setup` skill offers one when its tool is installed.

## Consequences

- More rounds on changes that arrive under-tested; fewer regressions after
  merge. `max_rejects` still bounds the loop.
- Reviews read more code (the tests too), so they cost a little more.
- Findings stay provable: a test gap's scenario is a concrete mutation, so
  the fixer can check it and dispute it like any other finding.
