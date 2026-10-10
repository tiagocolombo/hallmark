# 0020. Notifications, resuming on an answer, and what budgets count

Status: Accepted, 2026-10-09

## Context

ADR 0016 set the four budget levels but not what they count, or when a day
starts. A question in the inbox must reach you wherever you are, keep
reaching you until you answer, and stay quiet at night. Once you answer, the
agent that asked must pick up with your answer, without a human copying it
into the session.

## Decision

**Budgets count the loop, by UTC day.**

- Only `headless` usage counts: every `claude -p` run Hallmark starts
  (reviewer, fixer, intake, librarian, resume). Your interactive Workshop
  sessions (`transcript` usage) are measured and shown, never capped.
  `per_item` is what the loop spent on that item, ever.
- "Today" is the UTC date, the `day` column the index writes for each usage
  record. Daily and room caps reset at 00:00 UTC, which is also when a
  paused daemon goes again.
- `budget::check` runs before a run: the daily total first (the daemon
  pauses), then the role's room for today (that role waits; tickets keep
  their order), then the item (held, kind `budget`). A cap is reached when
  spent ≥ cap, so the run that crosses it finishes.
- Resume runs only answer to the daily total. Delivering your answer never
  holds an item.
- A spent daily budget adds a note to the SessionStart context (and to
  UserPromptSubmit for an item in the loop) and to the status line.
- `hall budget --set` edits the main checkout's `.hallmark.toml` with
  toml_edit, keeping comments, and re-parses the whole file before writing.

**Answers reach the asking session.** The `resume` worker, each tick:

1. Records the default for every question whose wait is over (`answer`,
   `defaulted: true`, actor `hall`), once, under the journal's lock.
   Escalations are not questions and never take a default.
2. Resumes one answered question with a session and no `answer_delivered`:
   `claude -p --resume <session> -- "The human answered Q-3 ("billed or
   raw?"): billed"` through `run_metered` (role `resume`, room `workshop`,
   no model, so Claude Code's default applies; `claude_args` still apply).
   It runs in the item's worktree, else the main checkout. Claude Code finds
   a session by ID from any directory (checked live on 2026-10-09).
3. Success records `answer_delivered`. A failure is retried on the next
   tick. The second failure in a row gives up: a `notification` of type
   `resume_failed`, with the session, files it as "session waiting" in the
   inbox (`claude --resume <session>`). An item question's item is also held
   (`run_failed`), as for any run. Questions without a session stay
   answered; there is nothing to resume.

**Notifications come from the inbox.**

- Each inbox entry has a stable key: kind, item, then the question or the
  waiting session. `<hallmark dir>/notify-state.json` keeps, per key, the
  first and last send, the count, and the last send and error per channel.
  It is a cache like the index; the journal records each send as
  `notified { key, kind, channels }`.
- What is due is a pure function of (state, inbox, now, config):
  - a new entry goes to the channels `[notify.route]` gives its kind;
  - an entry still waiting after `remind` (default 2h; `off` disables) is
    sent again on each channel;
  - an entry that left the inbox is forgotten;
  - during `quiet_hours` (local time, may wrap midnight) only the desktop
    shows. The phone and Slack get the entry when quiet hours end.
- **The morning digest** goes out once per local day from `digest` (default
  08:30; empty turns it off). It lists what waits, what closed yesterday,
  and yesterday's usage. It goes to the phone and Slack when configured,
  else the desktop. An empty digest is skipped for the day.
- **Channels.** The desktop uses notify-rust (on macOS, delivered as
  Terminal). `phone` is `ntfy://<host>/<topic>`, which is POSTed with
  `Title`, `Tags` and `Priority` headers, or `telegram://<bot-token>@<chat>`
  (Bot API `sendMessage`). `slack` is an incoming-webhook URL.
- **Content.** Messages carry the repo, the item, what waits, a question's
  options and default, and the command that answers it. They never carry
  code, diffs or file contents. A failed send is recorded and retried on
  the next tick, and never fails the tick.
- **`hall notify test`.** When no phone is set, it writes a private topic
  (`hallmark-` plus 10 random characters from getrandom) to the user config
  and explains how to subscribe. It doesn't send to the new topic unless
  asked with `--channel phone`, because nobody has subscribed yet.
- **`hall statusline`.** Claude Code's `statusLine` command. It reads the
  JSON on stdin and prints one line, such as
  `app · H-0041 development · ⚑ 2 need you`. It adds markers for a paused
  daemon, a stopped daemon, or a spent daily budget. It prints nothing
  outside a Hallmark repo, and it never uses the network or a model.

## Consequences

- A busy Workshop day never stops the review loop, and the loop's caps mean
  the same thing in every timezone.
- You hear about a question within one daemon pass. You hear again every
  two hours, and never from your phone at night.
- Tests need no desktop and no network. `HALL_NOTIFY_LOG` records desktop
  notifications, `HALL_NTFY_URL` and `HALL_TELEGRAM_URL` point the HTTP
  channels at a local listener, and `HALL_NOTIFY_NOW` pins the clock.
- A headless run with hooks on (fixer, resume) is counted twice in
  `hall usage`: once by the runner (`headless`) and once by the Stop hook
  (`transcript`). Budgets are unaffected because they count only
  `headless`. The fix belongs in the Stop hook.
