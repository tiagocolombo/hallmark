# 0047. A CI fix request clears when the PR's checks go green

Status: Accepted, 2026-10-16. Amends 0031.

## Context

- **Nothing read a reopened PR's checks again.** When a shipped PR's CI
  fails, the Delivery office reopens the item to development with
  `ci_fix_requested` (ADR 0031). From then on the CI watch and the merge
  watch skip the item, because both only follow shipped (closed) items. A
  re-run that went green on GitHub never cleared the request.
- **Each retry ran the same useless fix.** The fix run used the failing
  checks recorded at the request. H-0073 failed because the Windows runner
  couldn't download crates, and H-0074 because of a flaky runner test. Both
  went green on a re-run within minutes. Their dev runs found nothing to fix,
  made no commits, and held the items (`ci_failed`). Each `hall retry`
  started another such run, and the items waited for 18 hours on PRs that
  were green.
- **The runs read the wrong log.** `gh run view <run> --job <job>
  --log-failed` answers with the run's latest attempt. After a re-run, that
  is the attempt that passed, not the one that failed. The excerpt also
  took the last 60 lines that mention an error or a failure, and the names
  of passing tests ("an_error_is_reported ... ok") pushed out the test that
  failed.

## Decision

1. **The fix run reads the checks again first.** Before it starts, the
   Delivery office reads the PR's checks (`gh pr checks`):
   - **Green:** the item goes back to its PR without a run, as long as
     nothing moved since the reopen. It records `pr_checks` (passed), then
     `restored {pr_url, sha, reason}`, a new transition from development
     to closed (`Action::Restore`). The item is shipped again: the hold and
     the request clear, and the merge watch follows the PR again.
   - **Still running:** the item is held (`ci_failed`), and the reason says
     so. No run starts.
   - **Red:** the run gets the checks that fail now, with their current
     links. The logs are then the latest attempt's.
   - **Can't tell** (`gh` fails): the checks the request recorded, as
     before.
2. **The CI watch follows held CI fixes.** An item in development with a
   CI fix request and a hold (and no rebase or comments pending) is
   watched, once a minute, for 24 hours from its hold. When its checks pass
   and nothing moved, it goes back to its PR the same way. A green re-run
   clears the inbox entry without a `hall retry`. Red or running changes
   nothing.
3. **"Nothing moved" is checked, never assumed.** The worktree is clean, and
   both the branch and the PR's head (`gh pr view`) are at the commit the
   PR was at on reopening. If not, the item waits for you. The reason
   names what moved and offers `hall ready <id>` (review the new commits)
   or `hall retry <id>` once the branch is back where the PR is.
4. **The log is the failed job's own.** It comes from the API by job id
   (`gh api repos/{owner}/{repo}/actions/jobs/<id>/logs`), falling back to
   `gh run view`. The excerpt strips color codes and leads with the lines
   that say a step failed (`... FAILED`, `panicked at`, `failures:`,
   `test result: FAILED`, `error:`, `##[error]`), from the first, with a
   little context. Without such lines it falls back to the earlier rule:
   lines that mention an error or a failure, from the end.

## Consequences

- A flaky test or a runner outage that a re-run fixes costs nothing: no dev
  run and no review. The item is back on its PR as soon as the watch sees
  green.
- An item reopened for CI can now go from development to closed. Only
  `restored` does that, and only through the Delivery office or the CI
  watch, which both check that nothing moved first.
- The green result recorded with `restored` resets `ci_fixes`, as any green
  result does (ADR 0031). A later failure gets fixes of its own.
- A fix run whose checks are still running waits for you. If the re-run
  fails, `hall retry` fixes it with the checks that fail then. The watch
  doesn't release it, because a person or a re-run started that run and
  knows why.
- The fix run's log can come from a different attempt than the request's:
  the one that fails now. That is the one to fix.
