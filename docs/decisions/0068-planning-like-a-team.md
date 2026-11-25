# 0068. Planning like a team: a Product Manager and a Lead plan against the board

Status: Accepted in part, 2026-11-25: step 1 is built (H-0109); the rest is
proposed. Amends 0008 (a new state), 0024 (the Front
Desk), 0036 (the test audit) and 0057 (the PR description).

## Context

The Front Desk (ADR 0024) answers one question about one request: does it
split into parallel parts? It runs on Haiku inside `hall new`, sees the
request and a map of the repository, and nothing else. So:

- **It can't see the sprint.** A request for a change to the module an open
  item is rewriting becomes a second item: a second context load, a second
  Opus review, a second PR and CI run, and a rebase conflict between the two.
- **It can't see a list.** Ten requirements are ten `hall new` calls, each
  planned alone. Two that touch the same files are never grouped, and one
  that needs another's code is started before it.
- **It can't ask.** An ambiguous request, or two that contradict each other,
  are built as written, and the review finds out.
- **Nothing says what done means.** The request is free text. The reviewer
  judges the change against its own reading of it.
- **Every item starts at once.** There is no backlog, no priority and no
  limit on work in progress. An item gets its branch and worktree the moment
  it exists, and a worktree that waits goes stale.

A team splits this work between two people. A **product manager** owns the
what and the why: the stories, what done means for each, their priority, and
the product questions. A **lead developer**, who is also the scrum master,
owns the how: what is already in flight, what to group, what to fold into
open work, how to slice something big, what order builds, and how much fits.
The stakeholder, here the person running Hallmark, settles what neither of
them can.

## Decision

Two planning roles replace the Front Desk. Both only plan: read-only tools
(`Read`, `Grep`, `Glob`, `StructuredOutput`), `HALL_NO_HOOKS=1`, cwd the main
checkout, through the runner (ADR 0013), booked to room `front` as roles `pm`
and `lead`. The person stays the stakeholder: they decide priority when they
want to, answer the questions the roles can't, and can override any plan.

### The Product Manager: what and why

The PM runs first, over all the requests at once. For each one it returns a
**story**:

- a title and a one-paragraph request, in the product's terms, that keeps
  every detail of the original;
- **acceptance criteria**: a short list of checkable statements ("a retry
  stops after 6 attempts", "`hall status --json` lists backlog items with
  `waits_for`"). These are the Definition of Done;
- a **priority** (`now`, `next`, `later`) with a one-line reason;
- `duplicate_of`, when the board or the Library shows it's already done;
- **product questions**, when the request can't be turned into criteria
  without a decision (an ambiguity, two requests that contradict each other,
  a choice with a user-visible trade-off).

The PM sees the requests, the board's titles, requests and criteria, and the
human `decision` learnings and answered questions that a Library search finds.
It doesn't need the code, and it doesn't decide how anything is built.

### The Lead: how, in what order, and how much

The Lead runs on the PM's stories. Its input:

- the stories, numbered, with their criteria and priorities;
- **the board**: every open item's ID, title, state, request and added
  scope, criteria, summary, changed paths with line counts (`git diff
  --numstat` from the merge base), open PR, parent, backlog place and
  dependencies;
- the Library's accepted learnings that a search finds (capped like a
  briefing);
- the repository map, which now names a monorepo's packages (Cargo, npm,
  pnpm, Go and uv workspaces) so the Lead can plan along package lines.

It returns one decision per group of stories:

| Decision | Meaning |
| --- | --- |
| `new` | One new item, optionally split into parallel parts (ADR 0024, unchanged) |
| `merge` | Add the stories to an open item's scope |
| `follow_up` | A new item stacked on an open item's branch or PR |
| `slices` | One large story built as ordered items, each one small enough to review and shippable on its own |
| `spike` | An investigation item first: it writes a design note under `docs/` and changes no code; the stories wait for it |
| `duplicate` | Covered already: cite the item or commit; nothing is created |

Each `new`, `follow_up` or slice may hold several stories (a **group**), lists
`after` (items or other decisions it waits for), and carries an **estimate**
in weighted tokens (ADR 0016). Every decision has a `reason`.

### What a merge, a group or a slice may do

Merging saves a context load, a review, a PR, a CI run and a rebase. It costs
a bigger review, a higher chance of a rejection against the three-round cap,
and a bigger revert. The Lead's prompt states that trade, and **Hallmark
checks the plan, not the model**. A `merge` stands only when:

- the target is in `backlog`, `development` or `awaiting_you`. An item in
  checks, review, fixing or later has a frozen commit: the story becomes a
  `follow_up` instead, and no review work is thrown away;
- the target is not a parallel part, and not a `Records: …` or `Library: …`
  item;
- the target's diff plus the estimate stays under `[plan] max_item_lines`
  (default 400; a backlog target has no diff yet);
- it doesn't mix a `security_review` path with paths outside them.

A group, and each slice, obeys the same size and security rules. A story
estimated above `max_item_lines` must be `slices` or a `spike`, never one
item. Dependencies must name live items or decisions in the same plan, with
no cycle. A decision that fails a check falls back to `new` for its stories,
and the reply says why, as today.

### Acceptance criteria travel with the item

`created` and `scope_added` events carry the criteria.

- The developer's context (SessionStart and the `hall new` reply) lists
  them. `hall propose-ready` refuses with `CRITERIA_OPEN` unless the summary
  accounts for each one: met, with the test or evidence, or deferred, with
  a reason.
- The reviewer's test audit (ADR 0036) checks each criterion: a criterion
  with no test or evidence is an `evidence` finding.
- The PR description gains an **Acceptance criteria** checklist (ADR 0057),
  and so does the Gazette.

### Scope added to an item

A merge appends `scope_added { request, criteria, plan, by }` to the
target's journal. State doesn't change.

- Everything that reads an item's request reads it with its added scope: the
  review prompt, the PR description, the Gazette, the librarian's digest,
  `hall status` and `hall show`.
- **The session building it is told.** The SessionStart and UserPromptSubmit
  context of a session bound to the item says "Scope added (from P-3): …"
  until the item's next commit after it. If that session is idle, the resume
  worker sends it the added scope once, as it sends answers.
- **The caller.** If a live session is bound to the target, the reply says
  the request went to that item and that nothing else is needed. Otherwise
  the reply gives the target's worktree, so the caller builds it.

### Plans

A plan is recorded before anything is created: `P-0003`, journaled
append-only under `<hallmark dir>/plans/` (`stories`, `planned`, `question`,
`answer`, `applied` events), and folded like an item.

- **`hall plan "<req>" ["<req>" …]`**, or `--file <path>` / stdin with one
  request per paragraph, runs the PM and then the Lead once over the whole
  list. `hall new "<request>"` is `hall plan` with one request.
- **Models and budget.** The PM and the Lead are two agents and two runs
  (run roles `pm` and `intake`) that share the Front Desk's budget role:
  `[budget.intake]`, room `front`, a per-run cap of $0.50 and a three-minute
  timeout. Both run on `intake_model`, which now defaults to `sonnet`. The
  fast path, when no item is open at all, runs both on `intake_fast_model`
  (default `haiku`): with no board there is nothing to weigh. A PM run that
  hits Claude's limit, or can't start, stops the planning there, since the
  Lead would stop the same way.
- **Applying.** A plan that only makes new items (with or without parts)
  applies at once, as `hall new` does today. A plan with a merge, a group, a
  slice, a spike, a follow-up or a duplicate is applied at once in a headless
  run (`HALL_HEADLESS`). In a person's session it is returned as `proposed`,
  and its `next` is `hall plan apply P-3`. `--yes` applies it anyway, and
  `--no-merge` asks for new items only. Commands never prompt (ADR 0002).
- Applying creates items in the plan's order, under the repo lock, all in
  `backlog`. The first startable item is then started, within the work in
  progress limit, as `hall new` starts one today.
- `hall plan P-3` shows a plan. `hall plan --open` lists the plans that
  aren't applied.

### Grooming: questions before work

Product questions come from the PM. The Lead can return technical questions
it can't settle from the code, such as a choice between two designs that
both work. Either holds its stories, and the rest of the plan is applied.

- **In a person's session** the reply carries the questions. The session's
  Claude asks them in chat and runs `hall plan answer P-3 <n> "<answer>"`.
- **In a headless run**, each question becomes an inbox question (`hall
  ask`) that belongs to the plan instead of an item: `question { plan }`.
  The inbox shows it like any question.
- **Once every question is answered** (or has taken its default), the next
  `resume` tick plans the held stories again with the answers, and applies
  the result by the headless rules. A story can be held a second time. A
  third time, it is escalated as it stands.
- An answered product question is a human decision. The librarian files it
  as a `decision` learning, so the PM finds it next time.

### The backlog: priority, work in progress, starting

`backlog` is a new state (ADR 0008, through `state::next`): an item that
exists, with its story, criteria, priority, estimate and dependencies, but
has no branch or worktree yet.

- `created { backlog: true, … }` makes one, and `hall drop` drops one.
- **Priority** is the PM's, and the person's to change: `hall plan
  prioritize <id> now|next|later` records `prioritized { by }`. A person's
  call is never changed by a later plan.
- An item is **startable** when everything in its `after` list is merged,
  dropped, or shipped with an open PR it can stack on (`stackable_base`).
- **Work in progress is capped.** `[plan] wip` (default 3) is the most
  items in `development` or `awaiting_you` at once, parallel parts not
  counted. `hall start <id>` moves a backlog item to `development` and makes
  its branch and worktree then, from the latest base (ADR 0031) or stacked
  on the open PR it waits for. It refuses an item that isn't startable
  (`WAITING`, exit 4, naming what it waits for), or one over the limit
  (`WIP_FULL`, exit 4, naming the items in progress). `--force` passes the
  limit.
- `hall next --start` starts the first startable item by priority, then
  plan order. `hall status` lists the backlog with what each item waits for.
  The daemon never starts backlog items: development stays with a session.
- **Capacity.** `hall plan` reports the plan's estimate against what's left
  of the daily budget. It doesn't refuse. The person decides.

### Re-planning when things change

When an item is escalated, dropped or reverted (ADR 0061), the backlog items
that wait for it are stranded. The next `resume` tick re-plans them: the
Lead, with the event and its reason on the board, keeps, re-points, re-orders
or merges them. A stranded product question goes to the PM, and to the inbox
if the PM can't settle it. The plan journal records it as `replanned
{ cause }`.

### The board in every session

Without a model run, the hooks warn about collisions. When a session edits
a file that another open item has changed (from that item's diff against
its merge base, cached in the index), the next context line names it:
"H-0112 also changes src/config.rs (in review)."

### Monorepos, not multi-repo

Planning works inside one repository. In a monorepo the repository map and
the board group paths by package, so items, groups and slices follow package
lines. Work that spans repositories is out of scope: Hallmark plans and
tracks one repository at a time.

### The order to build it

Each step is one item:

1. The PM and the Lead, with the board in their input, for a single request:
   stories with criteria, and `new`, `merge`, `follow_up`, `duplicate`, with
   Hallmark's checks. `scope_added`, and the request read with it by the
   review, the fixer, the PR description and the librarian. Criteria kept
   on the item and in the `hall new` reply. Until plans exist (step 3),
   `hall new` applies the Lead's decision at once, and its `next` offers
   `--no-merge` to build the request on its own instead.
2. Criteria downstream: the developer's context, `propose-ready`'s
   `CRITERIA_OPEN`, the test audit, the PR's checklist and the Gazette. The
   added scope delivered to the session building the item.
3. `hall plan` for lists: groups, slices, spikes, estimates, plan journals.
4. Grooming: plan questions, headless re-planning on answers.
5. The backlog: the state, priority, the work in progress limit, `hall
   start`, `hall next --start`.
6. Re-planning on escalation, drop and revert.
7. Collision warnings in the hooks.

## Consequences

- A small change to code that is already being worked on rides with that
  work: one review, one PR. A list of requirements becomes the fewest items
  that can be reviewed safely, sized and ordered so each one builds.
- Every item says what done means, and the review checks it.
- Planning costs two Sonnet runs when something is in flight or the list is
  long, and two Haiku runs on the fast path, instead of one Haiku run.
- A merge never discards review work, and it never grows an item past a size
  the reviewer handles well. A wrong merge is undone by a person: `hall
  pull-back`, and a new item for the scope.
- Questions reach the person only when neither role can plan without them,
  and each answer is kept as a decision.
- `backlog` is a new state: the TUI, the web app, `hall status`, stats and
  `docs/how-it-works.md` all have to show it.
- Later: `hall stats` feeding the Lead its own record (estimates against
  actual usage, rejections by item size and by merged scope), so the limits
  come from evidence; and a headless builder that pulls startable items
  within the limit. Each needs its own ADR.
