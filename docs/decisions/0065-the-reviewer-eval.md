# 0065. The reviewer eval: replay today's reviewer on the changes the loop got wrong

Status: Accepted, 2026-10-27. Amends 0056 and 0061.

## Context

Every change to the reviewer (its skill, its lenses, its model, what the
prompt carries, the verifier, the security pass) shipped on judgement.
`hall stats` (ADR 0056) says how the loop is doing, but not whether a
change to the reviewer would have caught the bugs that got through.
Anthropic keeps an eval set of past PRs that caused incidents or reverts,
confirmed by engineers, runs the reviewer against it daily, and keeps a
change to the reviewer only when the score goes up. ADR 0061 gives
Hallmark the set: items whose merged PR was reverted, with the commit the
reviewer first saw and the commit that undid the change.

## Decision

- **`hall eval`** replays today's reviewer on each case. A case is an item
  with `reverted` set (`--since` narrows by when; `--limit`, default 10;
  or the items named). For each, the reviewer runs on the commit its first
  review saw, as a round-1 review: the item as it was then (nothing
  reviewed, no findings), the journal up to that review, today's skill,
  rules from the base, today's model, the same agent, schema and
  isolation (`review::request`), in a review checkout removed afterwards.
- **It is scored against the revert.** A verdict is *rejected* when it has
  a blocker or major, and *caught* when one of them sits in a file the
  revert commit touched (`git diff-tree` on the revert; with no known
  files, any blocking finding counts). The verdict recorded at the time is
  scored the same way, so each case reads "then: missed, now: caught".
  Totals give caught then and now, rejected now, and the cost.
- **It spends usage on purpose.** The run's role is `eval`, under the
  review budget; it is never run by the daemon. `--dry-run` lists the
  cases and the then-scores without a model. Runs are booked to the
  repository, not the item: no verdict, `run_failed` or hold is recorded
  on a closed item, and a failed replay is said in the case.
- **The score is coarse by design.** "A blocking finding in a reverted
  file" is what the journal can check without a person; it says whether
  the reviewer would have stopped the change, not whether it named the
  exact bug. A person reading the case's titles judges that.

## Consequences

- A change to the reviewer can be tried before it ships: run the eval on
  the branch, compare `now_caught` and `now_rejected` with the last run,
  and keep the change when the score goes up and `hall stats` noise
  doesn't.
- The set grows only with reverts; a repository with none has nothing to
  replay, and the command says so.
- Each case costs a full review at the reviewer's effort for that change.
  `--limit` and `--since` bound it.
