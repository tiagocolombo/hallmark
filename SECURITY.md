# Security policy

## Reporting a vulnerability

Please don't report security issues in public issues, discussions or pull
requests.

Report them privately through GitHub instead: open the repository's
**Security** tab and choose
[**Report a vulnerability**](https://github.com/tiagocolombo/hallmark/security/advisories/new).
Include what you found, how to reproduce it, and the impact you expect.

You should get an acknowledgement within a few days. We'll keep you updated
while we work on a fix, and credit you in the advisory unless you'd rather not
be named.

## Supported versions

Hallmark is pre-1.0. Security fixes go into the latest release only.

## Scope

Hallmark runs locally and acts on your repositories, so these areas matter
most:

- **The gates and hooks**: anything that lets an item push, ship or bypass the
  edit and push gates without a hallmark.
- **Configured commands**: `.hallmark.toml` runs shell commands. It is read
  from the main checkout so that an item can't change its own gate. Anything
  that breaks that guarantee is in scope.
- **Headless runs**: every `claude -p` run is confined (ADR 0053). Anything
  that lets a run read a `[sandbox] deny_read` path, write outside its
  worktree, temp and git directories, reach a host the repo didn't allow, or
  loosen its confinement through `.hallmark.toml` is in scope. Gates the
  Assay Office runs itself are not sandboxed.
- **The journal**: it contains your prompts and stays in your git directory.
  Anything that leaks it into a commit, a pull request or a notification is in
  scope.
- **Notifications**: they should carry only an item's ID and title, a question
  and its options, never code.

Issues in Claude Code itself should be reported to
[Anthropic](https://www.anthropic.com/responsible-disclosure-policy).
