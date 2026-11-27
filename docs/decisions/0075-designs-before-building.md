# 0075. Designs before building: a mockup or a diagram the person approves

Status: Proposed, 2026-11-27. Amends 0068 (grooming and the plan).

## Context

Grooming (ADR 0068) settles a story through words. The PM writes
acceptance criteria, and the PM and the Lead ask questions when a decision
is missing. Words are a poor way to agree on two kinds of work:

- **Anything people look at**, such as a page, a mobile screen, a dialog, a
  component or an email. "A settings page with the budgets" can be built in
  a dozen layouts. The person sees which one they got only in the PR, after
  a development session, a review and CI have been paid for. Changing the
  layout then means another round, or another item.
- **Anything with moving parts**, such as a new service, a worker, an
  endpoint, a schema change or a new state. The criteria say what it must
  do, not how the parts talk. A wrong boundary, a missing queue or an
  ownership problem in the data shows up in review, when it is expensive
  to change, or after the merge.

A team settles both before building. A designer brings a mockup, a tech
lead brings a diagram, and the stakeholder says yes or "move this".

## Decision

Grooming gains a design step. A story that builds something visual gets a
**prototype**, and a story that changes how the system is put together
gets **diagrams**. The person reviews them and approves them before the
story's item starts. The approved design then travels with the item as
part of what done means.

### Which stories need a design

The PM marks each story with `design`:

| `design` | When | What is made |
| --- | --- | --- |
| `visual` | It adds or changes something a person sees: a page, a screen (web, mobile, desktop), a dialog, a component, an email or notification, a TUI view | A prototype |
| `system` | It adds or changes a component, a service, a worker, an API or endpoint, a schema, a queue, a state machine, or how existing parts talk | Diagrams |
| `both` | Both kinds, such as a new page backed by a new endpoint | Both |
| `none` | A fix, a refactor, a copy change, a test, docs, a small tweak inside an existing screen or flow | Nothing |

The Lead can raise `none` to a design kind when the code shows a bigger
change than the request suggested, and it says why. It can't lower one.
The person can add or drop a design on any story (`hall plan design P-3
<n> visual|system|both|none`).

### The Designer

A third planning role, `design`, runs after the Lead on the stories that
need a design. It has the same limits as the PM and the Lead
(`HALL_NO_HOOKS=1`, cwd the main checkout, through the runner), with one
difference: it can `Write`, and only inside the plan's design directory,
`<hallmark dir>/plans/P-0003/design/<story>/v<n>/`.

Its input is the story with its criteria, the Lead's decision for it, the
repository map, and the Library's learnings. It reads the code it needs,
such as the existing pages, styles and components, or the modules,
schemas and routes the story touches.

**A prototype (`visual`)** is a set of self-contained HTML files, one per
screen, with an `index.html` that links them:

- It is built from the app's own look. It uses the design tokens, styles
  and components the Designer finds in the repository, not a generic kit,
  so the person judges the real thing.
- It shows the states that matter: empty, loading, error, a long list, a
  narrow window. A mobile screen sits in a device-sized frame.
- It is clickable where the story is a flow, such as a form, a wizard or a
  navigation between screens. The data is fake and is labelled as fake.
- It has no network access and no external scripts or fonts, so it opens
  from disk and shows the same thing to everyone.

**Diagrams (`system`)** are Mermaid sources, each with a one-paragraph
caption, chosen for the change:

- a **component diagram** of the parts involved, with the new and changed
  parts marked;
- a **sequence diagram** of the main flow, and of the failure path when
  the story has one;
- a **data diagram** (an ER diagram) when a schema or a stored format
  changes;
- a **state diagram** when states or transitions change.

Each diagram shows only what the story touches and its direct neighbours,
not the whole system. A `design.md` lists the diagrams and the decisions
they record, such as "the worker owns the retry, not the endpoint".

The Designer also writes `notes.md`. It names the open choices it made,
and the alternatives it rejected, so the person can say "take the other
one".

### Review and approval

A story with a design is held in grooming until the person approves it.
The rest of the plan is applied as usual, and stories that wait for a held
story wait with it.

- **It always waits for a person**, headless or not. A plan question can
  take its default, but a design never does, because approving it is the
  point of the step.
- **The inbox** gets a `design` entry for each story: "Design to review:
  P-3, story 2, the budgets page (v1)". Its actions are `hall plan design
  P-3 2 --open`, `hall plan approve P-3 2`, and `hall plan revise P-3 2
  "<what to change>"`.
- **`--open`** opens the design in `hall web`, which shows a prototype in a
  frame with a screen picker and renders diagrams with an embedded Mermaid
  (no network). `hall web` gets a Designs page for every design waiting for
  review, with the same actions as buttons.
- **In a person's session** the `hall plan` reply lists the designs and
  their paths. The session's Claude shows them to the person and runs
  `approve` or `revise` with their answer.
- **`revise`** runs the Designer again with the person's notes and the
  previous version, and writes `v2`. Every version is kept. After five
  revisions the story is escalated as it stands, with its designs, because
  a design that won't converge is a request to talk.
- **Rejecting** a design is `hall plan design P-3 2 none --reason
  "<why>"`, which builds the story without one, or `hall drop` on the
  story.

The plan journal records `design_requested`, `design_made { story,
version, kind, files, sha256 }`, `design_revised { notes, by }` and
`design_approved { story, version, by }`.

### The approved design travels with the item

When the story's item is created, the approved version is copied into the
item's directory (`design/`), and `created` carries `design { kind,
version, sha256, plan }`. From then on it is frozen. A change to it is a
new design and a new approval, not an edit.

- **Criteria.** The PM's criteria gain one line: "matches the approved
  design (P-3, story 2, v2)". It is checked like any other criterion
  (ADR 0068): the summary must account for it.
- **The developer** sees the design's paths in the SessionStart context
  and in the `hall new` or `hall start` reply. The skill tells it to build
  to the design and to write down any departure from it, with the reason.
- **The reviewer** gets the design in its prompt. A departure that isn't
  written down is a `scope` finding. A visual departure is judged on what
  the code renders: layout, content, states. Pixels and exact colours are
  not judged.
- **The PR description** gets a Design section. Diagrams are pasted in as
  Mermaid blocks, which GitHub renders. A prototype is named by its path
  and version, with the Designer's notes. Screenshots can come later.
- **The Gazette and the librarian** read the approval and the revision
  notes. The person's revision notes are kept as `decision` learnings
  ("prefers tables over cards for lists of items"), so the next design
  starts closer.

### Settings and cost

- `[plan] design = "auto" | "visual" | "system" | "off"`. The default,
  `auto`, makes designs for every kind the PM marks. `visual` and `system`
  limit designs to that kind, and `off` turns the step off.
- `[budget] design_model` defaults to `sonnet`. Designer runs are booked to
  room `front` as role `design`, with a per-run cap of $1.50 and a
  ten-minute timeout. A run that fails or hits a limit holds the story
  with the reason, as a question would. It doesn't skip the design.
- The design is a document, not code. It is never committed by the step
  itself, so the repository stays clean. A repo that wants designs kept in
  the tree says so with `[plan] design_dir = "docs/design"`, and the item
  then commits its approved design there as its first commit.

### The order to build it

This rests on 0068's plans (step 3) and grooming (step 4), and is built
after them:

1. The PM's `design` mark, the Designer for `system` stories (Mermaid
   only), approval and revision through the CLI and the inbox, the design
   on the item, in the developer's context, the review and the PR.
2. Prototypes for `visual` stories, and the Designs page in `hall web`
   with Mermaid rendering.
3. Revision notes as `decision` learnings, and `design_dir`.

## Consequences

- The person sees the shape of a visual or structural change before any
  code is written. A layout or a boundary is changed for the cost of a
  Designer run, not a review round.
- Planning a story with a design costs one more Sonnet run, plus one per
  revision, and it waits for the person. Work that needs no design (fixes,
  refactors, small changes) skips the step and doesn't wait.
- A headless loop can't build visual or structural work without a person
  any more. This is on purpose, and `[plan] design = "off"` restores the
  old flow.
- The reviewer gets one more thing to check against, and its findings on
  departures are more grounded than "this isn't what was asked".
- A prototype is only as faithful as the styles the Designer finds. A repo
  with no clear design system gets plausible mockups, not exact ones, and
  the notes say so.
- Later, each with its own ADR: screenshots of the built pages compared
  with the approved prototype, and a design that changes after approval
  (a `design_changed` event that asks again).
