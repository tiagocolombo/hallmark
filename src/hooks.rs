//! `hall hook <event>`: the Claude Code side of Hallmark. Every hook reads
//! the event JSON on stdin (shapes in tests/fixtures/hooks) and prints
//! nothing, or a `hookSpecificOutput` object.
//!
//! Hooks must never break a session: outside a Hallmark repo they do
//! nothing, and internal errors are logged to `hallmark/hook-errors.log`
//! and swallowed. The gate fails open for the same reason.

use std::io::Read;
use std::path::{Path, PathBuf};

use clap::ValueEnum;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::{CaptureMode, PushGuard};
use crate::error::{ErrorCode, HallError, Result};
use crate::event::{Body, Event};
use crate::gate::{self, OpKind};
use crate::item::{Item, ItemId, State};
use crate::journal::SessionState;
use crate::output::{Response, clip};
use crate::repo::Repo;
use crate::{git, paths, usage};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HookEvent {
    #[value(alias = "SessionStart")]
    SessionStart,
    #[value(alias = "UserPromptSubmit")]
    UserPromptSubmit,
    #[value(alias = "PreToolUse")]
    PreToolUse,
    #[value(alias = "PostToolUse")]
    PostToolUse,
    #[value(alias = "Stop")]
    Stop,
    #[value(alias = "Notification")]
    Notification,
}

impl HookEvent {
    pub fn claude_name(self) -> &'static str {
        match self {
            HookEvent::SessionStart => "SessionStart",
            HookEvent::UserPromptSubmit => "UserPromptSubmit",
            HookEvent::PreToolUse => "PreToolUse",
            HookEvent::PostToolUse => "PostToolUse",
            HookEvent::Stop => "Stop",
            HookEvent::Notification => "Notification",
        }
    }
}

/// The stdin JSON of every hook event we handle (fields vary by event).
#[derive(Debug, Default, Deserialize)]
pub struct HookInput {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<Value>,
    #[serde(default)]
    pub tool_response: Option<Value>,
    #[serde(default)]
    pub last_assistant_message: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub notification_type: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    /// default, acceptEdits, plan, bypassPermissions…
    #[serde(default)]
    pub permission_mode: Option<String>,
}

/// What a hook prints, when it prints anything.
#[derive(Debug, Serialize, JsonSchema)]
pub struct HookOutput {
    #[serde(rename = "hookSpecificOutput")]
    pub hook_specific_output: HookSpecific,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HookSpecific {
    pub hook_event_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
    /// PreToolUse only: deny or ask.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_decision_reason: Option<String>,
}

fn context(event: HookEvent, text: String) -> Value {
    json!({ "hookSpecificOutput": { "hookEventName": event.claude_name(), "additionalContext": text } })
}

fn decision(decision: &str, reason: String) -> Value {
    json!({ "hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": decision,
        "permissionDecisionReason": reason,
    }})
}

pub fn run(event: HookEvent, cwd: &Path) -> Result<Response> {
    // Headless runs that must leave no trace through hooks (the reviewer,
    // intake, the librarian) set this; the runner records them instead.
    if std::env::var_os("HALL_NO_HOOKS").is_some() {
        return Ok(Response::raw(None));
    }
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    let input: HookInput = serde_json::from_str(&buf).unwrap_or_default();
    let dir = input
        .cwd
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| cwd.to_path_buf());
    let repo = match Repo::discover(&dir) {
        Ok(r) if r.initialized() => r,
        _ => return Ok(Response::raw(None)),
    };
    let mut ctx = match Ctx::new(repo, input, dir) {
        Ok(c) => c,
        Err(_) => return Ok(Response::raw(None)),
    };
    let out = match event {
        HookEvent::SessionStart => ctx.session_start(),
        HookEvent::UserPromptSubmit => ctx.user_prompt_submit(),
        HookEvent::PreToolUse => ctx.pre_tool_use(),
        HookEvent::PostToolUse => ctx.post_tool_use(),
        HookEvent::Stop => ctx.stop(),
        HookEvent::Notification => ctx.notification(),
    };
    match out {
        Ok(v) => Ok(Response::raw(v)),
        Err(e) => {
            ctx.repo
                .hall
                .log_hook_error(event.claude_name(), &e.to_string());
            Ok(Response::raw(None))
        }
    }
}

struct Ctx {
    repo: Repo,
    input: HookInput,
    dir: PathBuf,
    /// Live items.
    items: Vec<Item>,
    session: Option<SessionState>,
    /// Set when an edit should be explicitly allowed (see `check_edit`).
    allow: Option<String>,
}

impl Ctx {
    fn new(repo: Repo, input: HookInput, dir: PathBuf) -> Result<Ctx> {
        let items = repo.index()?.items(false)?;
        let session = input.session_id.as_ref().map(|s| repo.hall.session(s));
        Ok(Ctx {
            repo,
            input,
            dir: paths::normalize(&dir),
            items,
            session,
            allow: None,
        })
    }

    fn session_id(&self) -> Option<String> {
        self.input.session_id.clone()
    }

    /// The session's item: the worktree it stands in, else the one it
    /// joined earlier, if that is still live.
    fn item(&self) -> Option<Item> {
        if let Some(i) = self.repo.item_at(&self.items, &self.dir) {
            return Some(i);
        }
        let bound = self.session.as_ref()?.item?;
        self.items.iter().find(|i| i.id == bound).cloned()
    }

    fn record(&self, item: Option<ItemId>, actor: &str, body: Body) -> Result<()> {
        let e = Event::new(actor, item, body).with_session(self.session_id());
        match item {
            Some(id) => self.repo.hall.append_item(id, &e),
            None => self.repo.hall.append(&self.repo.hall.repo_events(), &e),
        }
    }

    /// Ties the session to an item; on the first bind, copies the
    /// session's earlier prompts into the item's journal.
    fn bind(&mut self, id: ItemId) -> Result<()> {
        let Some(sess) = self.session.as_mut() else {
            return Ok(());
        };
        if sess.item == Some(id) {
            return Ok(());
        }
        let first = sess.item.is_none();
        sess.item = Some(id);
        let sess = sess.clone();
        self.repo.hall.save_session(&sess)?;
        self.record(Some(id), "hook", Body::SessionBound {})?;
        if first {
            let path = self.repo.hall.repo_events();
            for e in self.repo.hall.read(&path)? {
                if e.session.as_deref() == Some(sess.session.as_str())
                    && matches!(e.body, Body::Prompt { .. })
                {
                    let mut copy = e.clone();
                    copy.item = Some(id);
                    self.repo.hall.append_item(id, &copy)?;
                }
            }
        }
        Ok(())
    }

    /// The item a session working in `i` binds to: a parallel part's parent
    /// (the orchestrator's item), unless the session runs inside the part's
    /// own worktree (ADR 0024). Keeps a session from flapping between parts.
    fn bind_target(&self, i: &Item) -> ItemId {
        match i.parent {
            Some(parent)
                if !paths::is_within(&self.dir, &paths::normalize(Path::new(&i.worktree))) =>
            {
                parent
            }
            _ => i.id,
        }
    }

    /// The parallel parts of `id`, closed ones included.
    fn children(&self, id: ItemId) -> Vec<Item> {
        self.repo
            .index()
            .and_then(|ix| ix.items(true))
            .map(|v| v.into_iter().filter(|i| i.parent == Some(id)).collect())
            .unwrap_or_default()
    }

    /// What waits for the human, with each entry's actions, for Claude to
    /// pass on. With `only_new`, just the entries this session hasn't been
    /// told about yet. Marks them told.
    fn waiting_brief(&mut self, only_new: bool) -> Option<String> {
        if headless_actor().is_some() {
            return None;
        }
        let ix = self.repo.index().ok()?;
        let entries = ix.inbox().ok()?;
        let key = |e: &crate::index::InboxEntry| {
            format!(
                "{}:{}:{}",
                e.kind,
                e.item.map(|i| i.to_string()).unwrap_or_default(),
                e.qid
                    .clone()
                    .unwrap_or_else(|| e.session.clone().unwrap_or_default())
            ) + &format!(":{}", e.since.timestamp_millis())
        };
        let told = self
            .session
            .as_ref()
            .map(|s| s.told.clone())
            .unwrap_or_default();
        let shown: Vec<(usize, crate::index::InboxEntry)> = entries
            .iter()
            .cloned()
            .enumerate()
            .filter(|(_, e)| !only_new || !told.contains(&key(e)))
            .collect();
        if shown.is_empty() {
            // Still forget entries that left, so one that comes back is new.
            if let Some(sess) = self.session.as_mut() {
                let live: Vec<String> = entries.iter().map(key).collect();
                let before = sess.told.len();
                sess.told.retain(|k| live.contains(k));
                if sess.told.len() != before {
                    let sess = sess.clone();
                    let _ = self.repo.hall.save_session(&sess);
                }
            }
            return None;
        }
        let mut s = format!(
            "Hallmark: {} waiting for the human{}. Tell them first, in a line or two each, \
             and offer to run the action they pick (run it for them when they choose):",
            if only_new {
                "new entries are"
            } else {
                "entries are"
            },
            if entries.len() > shown.len() {
                format!(" ({} in all)", entries.len())
            } else {
                String::new()
            }
        );
        for (i, e) in shown.iter().take(5) {
            let detail = crate::commands::status::explain(&self.repo, &ix, e.clone(), i + 1).ok();
            let who = e.item.map(|i| format!(" {i}")).unwrap_or_default();
            s.push_str(&format!(
                "\n{}. {}{who}: {} (waiting {}). Details: `hall inbox {}`.",
                i + 1,
                e.kind,
                clip(&e.text, 140),
                crate::commands::age(e.since),
                i + 1
            ));
            if let Some(d) = detail {
                let acts: Vec<String> = d
                    .actions
                    .iter()
                    .take(4)
                    .map(|a| format!("`{}` ({})", a.command, a.does))
                    .collect();
                s.push_str(&format!(" Actions: {}.", acts.join("; ")));
            }
        }
        if shown.len() > 5 {
            s.push_str("\nThe rest: `hall inbox`.");
        }
        if let Some(sess) = self.session.as_mut() {
            for e in &entries {
                let k = key(e);
                if !sess.told.contains(&k) {
                    sess.told.push(k);
                }
            }
            // Forget entries that left the inbox, so a new one with the same
            // key is announced again.
            let live: Vec<String> = entries.iter().map(key).collect();
            sess.told.retain(|k| live.contains(k));
            let sess = sess.clone();
            let _ = self.repo.hall.save_session(&sess);
        }
        Some(s)
    }

    fn inbox_count(&self) -> usize {
        self.repo
            .index()
            .and_then(|ix| ix.inbox())
            .map(|v| v.len())
            .unwrap_or(0)
    }

    // --- SessionStart -----------------------------------------------------

    fn session_start(&mut self) -> Result<Option<Value>> {
        if let Some(s) = self.session.as_mut() {
            if s.started_at.is_none() {
                s.started_at = Some(chrono::Utc::now());
            }
            let s = s.clone();
            self.repo.hall.save_session(&s)?;
        }
        let item = self.item();
        if let Some(i) = &item {
            self.bind(i.id)?;
        }
        self.record(
            item.as_ref().map(|i| i.id),
            "hook",
            Body::SessionStarted {
                source: self.input.source.clone(),
            },
        )?;
        let inbox = self.inbox_count();
        let mut text = match &item {
            Some(i) => {
                let mut t = item_context(i, inbox);
                t.push_str(&family_context(i, &self.children(i.id)));
                t
            }
            None => repo_context(&self.repo, &self.items, inbox),
        };
        // The repo's own hallmark skill (ADR 0037), from the base branch.
        let base = item
            .as_ref()
            .map_or(self.repo.config.base.as_str(), |i| &i.base);
        if let Some(skill) = repo_skill_context(&self.repo, base) {
            text.push_str("\n\n");
            text.push_str(&skill);
        }
        // Extensions turned on for development sessions (ADR 0038): which
        // skills, and the command that prints one.
        if let Some(x) = crate::extensions::session(&self.repo.config.extensions) {
            text.push_str("\n\n");
            text.push_str(&x);
        }
        // What the person said since the last run on this item, once.
        if let Some(w) = self.said(item.as_ref()) {
            text.push_str("\n\n");
            text.push_str(&w);
        }
        // The fixer works on an item in the loop on purpose.
        if headless_actor().as_deref() == Some("fixer")
            && let Some(i) = &item
        {
            text = format!(
                "Hallmark: you are the fixer for {} \"{}\" in worktree {}. Fix its failing checks \
                 and open findings there (`hall findings {}`), commit, and record them with \
                 `hall fixed <ids>`. Never push.",
                i.id, i.title, i.worktree, i.id
            );
        }
        // What waits for the human, with the actions, so Claude raises it.
        if let Some(brief) = self.waiting_brief(false) {
            text.push_str("\n\n");
            text.push_str(&brief);
        }
        // The daily budget is spent: the loop waits, this session doesn't.
        if let Some(note) = crate::budget::daily_note(&self.repo) {
            text.push(' ');
            text.push_str(&note);
        }
        // A newer hall is out (as last seen; the lookup runs detached).
        if headless_actor().is_none() {
            if let Some(note) = crate::commands::update::notice() {
                text.push(' ');
                text.push_str(&note);
            }
            crate::commands::update::check_in_background();
        }
        Ok(Some(context(HookEvent::SessionStart, text)))
    }

    // --- UserPromptSubmit -------------------------------------------------

    fn user_prompt_submit(&mut self) -> Result<Option<Value>> {
        let note = self.prompt_note()?;
        // What the person said since the last run, as with SessionStart: a
        // pull-back often goes back to a session that is already open.
        let said = self.said(self.item().as_ref());
        // New inbox entries since this session last heard: Claude passes them on.
        let brief = self.waiting_brief(true);
        let parts: Vec<String> = [note, said, brief].into_iter().flatten().collect();
        Ok((!parts.is_empty()).then(|| context(HookEvent::UserPromptSubmit, parts.join("\n\n"))))
    }

    /// What the person said since the last run on the session's item
    /// (`hall pull-back --reason`, `hall retry`), for a development session:
    /// only while the item is with it, and once (ADR 0044). Recorded as
    /// given when handed out; a journal that can't be written never breaks
    /// the session, it only means the words may come again.
    fn said(&self, item: Option<&Item>) -> Option<String> {
        if headless_actor().is_some() {
            return None;
        }
        let i = item?;
        if !matches!(i.state, State::Development | State::AwaitingYou) {
            return None;
        }
        // The Delivery office's run is next: it takes the words.
        if i.rebase.is_some() || i.feedback.is_some() || i.ci_fix.is_some() {
            return None;
        }
        let events = self.repo.hall.read_item(i.id).ok()?;
        let w = crate::words::pending(&events, "session")?;
        let _ = crate::words::delivered(&self.repo, i.id, "session", Some(&w), &events);
        Some(w.section.trim_end().to_string())
    }

    /// The note about this session's item, if it needs one.
    fn prompt_note(&mut self) -> Result<Option<String>> {
        let item = self.item();
        let text = self.input.prompt.clone().unwrap_or_default();
        let headless = headless_actor();
        if !text.trim().is_empty() {
            let actor = headless.as_deref().unwrap_or("you");
            self.record(item.as_ref().map(|i| i.id), actor, Body::Prompt { text })?;
        }
        let Some(i) = item else { return Ok(None) };
        // The fixer is meant to edit an item in the loop; don't tell it not to.
        if headless.as_deref() == Some("fixer") {
            return Ok(None);
        }
        let note = match i.state {
            s if s.in_loop() => format!(
                "Hallmark: {} is in the review loop ({}, round {}). Don't edit it. If the human wants \
                 changes, offer `hall pull-back {}`.",
                i.id, i.state, i.round, i.id
            ),
            State::AwaitingYou => format!(
                "Hallmark: {} is waiting for the human's call. If they say it's ready, run `hall ready {}`; \
                 if not, run `hall pull-back {}` and keep working.",
                i.id, i.id, i.id
            ),
            State::Escalated => format!(
                "Hallmark: {} was escalated after {} rejected reviews. See `hall log {} --kind verdict`.",
                i.id, i.rejects, i.id
            ),
            State::Hallmarked => format!(
                "Hallmark: {} is approved and hallmarked; it ships as is. New edits need `hall pull-back {}`.",
                i.id, i.id
            ),
            State::Development => {
                let kids = self.children(i.id);
                if !kids.iter().any(|c| !c.state.is_terminal()) {
                    return Ok(None);
                }
                format!("Hallmark: {}", family_context(&i, &kids).trim_start())
            }
            _ => return Ok(None),
        };
        // An item in the loop won't move while the daily budget is spent.
        let note = match crate::budget::daily_note(&self.repo) {
            Some(b) if i.state.in_loop() => format!("{note} {b}"),
            _ => note,
        };
        Ok(Some(note))
    }

    // --- PreToolUse: the gate --------------------------------------------

    fn pre_tool_use(&mut self) -> Result<Option<Value>> {
        let tool = self.input.tool_name.clone().unwrap_or_default();
        let ti = self.input.tool_input.clone().unwrap_or(Value::Null);
        let denial = match tool.as_str() {
            "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
                let p = ti
                    .get("file_path")
                    .or_else(|| ti.get("notebook_path"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if p.is_empty() {
                    None
                } else {
                    self.check_edit(&paths::resolve(&self.dir, Path::new(p)))?
                }
            }
            "Bash" => {
                let cmd = ti
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let ship = self.repo.config.ship_prefixes();
                let allow = self.repo.config.capture.push_allow_prefixes();
                let alias = |dir: &Path, name: &str| git::alias(dir, name);
                let ctx = gate::Context {
                    ship: &ship,
                    allow: &allow,
                    alias: &alias,
                };
                let scan = gate::inspect(cmd, &self.dir, &ctx);
                let mut d = None;
                for op in &scan.ops {
                    d = match op.kind {
                        OpKind::Commit => self.check_commit(&op.dir)?,
                        OpKind::Push => self.check_push(&op.dir)?,
                        OpKind::NoteWrite => self.check_note_write(&op.dir),
                    };
                    if d.is_some() {
                        break;
                    }
                }
                if d.is_none() && self.repo.config.capture.push_guard == PushGuard::Strict {
                    d = scan.suspects.iter().find_map(|s| self.check_guard(s));
                }
                d
            }
            _ => None,
        };
        let Some((kind, err, target)) = denial else {
            return Ok(self.allow.take().map(|r| decision("allow", r)));
        };
        let item = self.item().map(|i| i.id);
        self.record(
            item,
            "hall",
            Body::GateDenied {
                tool: tool.clone(),
                code: err.code.as_str().into(),
                target,
            },
        )?;
        let reason = serde_json::to_string(&err.envelope())?;
        Ok(Some(decision(kind, reason)))
    }

    /// Edits inside a live item's worktree are fine; elsewhere in the repo
    /// they need an item.
    fn check_edit(&mut self, p: &Path) -> Result<Option<(&'static str, HallError, String)>> {
        if !self.repo.contains(p) {
            return Ok(None);
        }
        if let Some(i) = self.repo.item_at(&self.items, p) {
            self.bind(self.bind_target(&i))?;
            // An item's worktree is usually a sibling directory, outside the
            // session's working directory. When the session already accepts
            // edits without asking, extend that to the item's worktree.
            // Other modes keep Claude Code's own prompt.
            let auto = matches!(
                self.input.permission_mode.as_deref(),
                Some("acceptEdits" | "bypassPermissions")
            );
            // Never in a headless run: its edits stay in its own worktree,
            // under Claude Code's own scope (ADR 0053).
            if auto && headless_actor().is_none() && !paths::is_within(p, &self.dir) {
                self.allow = Some(format!(
                    "Hallmark: {} is inside {}'s worktree, and this session accepts edits.",
                    paths::display(p),
                    i.id
                ));
            }
            return Ok(None);
        }
        if git::is_ignored(&self.repo.toplevel, p) {
            return Ok(None);
        }
        // Docs, designs and plans aren't development work (`[capture] free`).
        if self
            .repo
            .relative(p)
            .is_some_and(|r| self.repo.config.is_free(&r))
        {
            return Ok(None);
        }
        Ok(self.capture_denial(&format!("edit {}", paths::display(p))))
    }

    fn check_commit(&mut self, dir: &Path) -> Result<Option<(&'static str, HallError, String)>> {
        let dir = paths::normalize(dir);
        if !self.repo.contains(&dir) {
            return Ok(None);
        }
        if self.repo.item_at(&self.items, &dir).is_some() {
            return Ok(None);
        }
        // A commit of free paths only (docs, Markdown) needs no item.
        if only_free_changes(&self.repo, &dir) {
            return Ok(None);
        }
        Ok(self.capture_denial(&format!("commit in {}", paths::display(&dir))))
    }

    fn capture_denial(&self, what: &str) -> Option<(&'static str, HallError, String)> {
        let mode = self.repo.config.capture.mode;
        if mode == CaptureMode::Off {
            return None;
        }
        let err = match self.item() {
            Some(i) => HallError::new(
                ErrorCode::NoItem,
                format!(
                    "This session works on {} in {}. Make changes there, not in {}.",
                    i.id,
                    i.worktree,
                    paths::display(&self.dir)
                ),
            )
            .next([format!("cd {}", crate::commands::new::shell_quote(&i.worktree))]),
            None => HallError::new(
                ErrorCode::NoItem,
                format!(
                    "Changes in {} need a Hallmark item. Start one, then work in the worktree it returns.",
                    self.repo.name
                ),
            )
            .next(["hall new --title \"<short title>\"", "hall adopt"]),
        };
        let kind = if mode == CaptureMode::Ask {
            "ask"
        } else {
            "deny"
        };
        Some((kind, err, what.to_string()))
    }

    /// Pushes and PRs need a hallmark on HEAD, whatever the capture mode.
    fn check_push(&self, dir: &Path) -> Result<Option<(&'static str, HallError, String)>> {
        let dir = paths::normalize(dir);
        if !self.repo.contains(&dir) {
            return Ok(None);
        }
        let Ok(head) = git::head(&dir) else {
            return Ok(None);
        };
        let unverified = match crate::stamp::verify(&self.repo, &dir, &head) {
            Ok(_) => return Ok(None),
            Err(u) => u,
        };
        let item = self.repo.item_at(&self.items, &dir).or_else(|| self.item());
        let next: Vec<String> = match &item {
            Some(i) => vec![
                format!("hall ready {}", i.id),
                format!("hall status {}", i.id),
            ],
            None => vec![
                "hall new --title \"<short title>\"".into(),
                "hall status".into(),
            ],
        };
        let err = HallError::new(
            ErrorCode::GateDenied,
            format!(
                "{}. Work reaches GitHub only through review: send the item to review and the \
                 shipper pushes it once approved.",
                unverified.reason(&head)
            ),
        )
        .next(next);
        Ok(Some((
            "deny",
            err,
            format!("push {}", crate::event::short(&head)),
        )))
    }

    /// What to run instead of pushing from `dir`: send its item to review.
    fn review_next(&self, dir: &Path) -> Vec<String> {
        match self.repo.item_at(&self.items, dir).or_else(|| self.item()) {
            Some(i) => vec![
                format!("hall ready {}", i.id),
                format!("hall status {}", i.id),
            ],
            None => vec!["hall status".into()],
        }
    }

    /// The hallmark is a git note, so only the daemon's assayer and shipper
    /// (`HALL_ACTOR`) write notes, whatever the capture mode.
    fn check_note_write(&self, dir: &Path) -> Option<(&'static str, HallError, String)> {
        let dir = paths::normalize(dir);
        if !self.repo.contains(&dir) {
            return None;
        }
        if matches!(crate::repo::actor().as_str(), "assayer" | "shipper") {
            return None;
        }
        let err = HallError::new(
            ErrorCode::GateDenied,
            "Only Hallmark's assayer and shipper write git notes here: the hallmark is a note, \
             and a note written in a session could stand in for an approval. Send the item to \
             review instead.",
        )
        .next(self.review_next(&dir));
        Some(("deny", err, format!("note in {}", paths::display(&dir))))
    }

    /// `[capture] push_guard = "strict"`: inside an item's worktree, a
    /// command the gate can't read that may push is denied too.
    fn check_guard(&self, s: &gate::Suspect) -> Option<(&'static str, HallError, String)> {
        let dir = paths::normalize(&s.dir);
        if !self.repo.contains(&dir) {
            return None;
        }
        self.repo
            .item_at(&self.items, &dir)
            .or_else(|| self.repo.item_at(&self.items, &self.dir))?;
        let text = clip(&s.text, 200);
        let err = HallError::new(
            ErrorCode::GateDenied,
            format!(
                "`{text}` may push without a hallmark: {}. In an item's worktree the gate denies \
                 what it can't read, and work reaches GitHub only through review. If the repo \
                 needs this command, list it in `[capture] push_allow`.",
                s.why
            ),
        )
        .next(self.review_next(&dir));
        Some(("deny", err, format!("run {text}")))
    }

    // --- PostToolUse ------------------------------------------------------

    fn post_tool_use(&mut self) -> Result<Option<Value>> {
        if self.input.tool_name.as_deref() != Some("Bash") {
            return Ok(None);
        }
        let cmd = self
            .input
            .tool_input
            .as_ref()
            .and_then(|t| t.get("command"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let stdout = self
            .input
            .tool_response
            .as_ref()
            .and_then(|r| r.get("stdout"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let reply = last_json(&stdout);

        // `hall new` / `hall adopt` → this session now works on that item.
        if mentions(&cmd, &["new", "adopt"])
            && let Some(id) = reply
                .as_ref()
                .and_then(|v| v.get("id"))
                .and_then(Value::as_str)
            && let Ok(id) = id.parse::<ItemId>()
        {
            self.items = self.repo.index()?.items(false)?;
            let target = self
                .items
                .iter()
                .find(|i| i.id == id)
                .map(|i| self.bind_target(i))
                .unwrap_or(id);
            self.bind(target)?;
        }
        // `hall ask` → remember which session to resume with the answer.
        if mentions(&cmd, &["ask"])
            && let Some(qid) = reply
                .as_ref()
                .and_then(|v| v.get("qid"))
                .and_then(Value::as_str)
        {
            let item = reply
                .as_ref()
                .and_then(|v| v.get("item"))
                .and_then(Value::as_str)
                .and_then(|s| s.parse().ok());
            if self.session_id().is_some() {
                self.record(
                    item,
                    "hook",
                    Body::QuestionSession {
                        qid: qid.to_string(),
                    },
                )?;
            }
        }
        // Commits made in an item's worktree go in its journal.
        for op in gate::scan(&cmd, &self.dir, &[]) {
            if op.kind != OpKind::Commit {
                continue;
            }
            let dir = paths::normalize(&op.dir);
            let Some(item) = self.repo.item_at(&self.items, &dir) else {
                continue;
            };
            let Ok(sha) = git::head(&dir) else { continue };
            let seen = self
                .repo
                .hall
                .read_item(item.id)?
                .iter()
                .any(|e| matches!(&e.body, Body::Commit { sha: s, .. } if *s == sha));
            if !seen {
                let subject = git::subject(&dir, &sha);
                let actor = headless_actor().unwrap_or_else(|| "agent".into());
                self.record(
                    Some(item.id),
                    &actor,
                    Body::Commit {
                        sha,
                        subject,
                        fixes: vec![],
                    },
                )?;
            }
        }
        Ok(None)
    }

    // --- Stop -------------------------------------------------------------

    fn stop(&mut self) -> Result<Option<Value>> {
        let item = self.item().map(|i| i.id);
        // A headless run's usage is recorded by the runner from its result;
        // counting the transcript too would double it.
        let transcript = self
            .input
            .transcript_path
            .clone()
            .filter(|_| headless_actor().is_none());
        if let (Some(tp), Some(sess)) = (transcript, self.session.clone()) {
            let path = PathBuf::from(&tp);
            if path.is_file() {
                let u = usage::transcript_usage(
                    &path,
                    sess.transcript_offset,
                    sess.last_request.as_deref(),
                )?;
                for rec in u.records {
                    self.record(item, "hook", Body::Usage(rec))?;
                }
                let mut s = sess;
                s.transcript_offset = u.consumed;
                s.last_request = u.last_request;
                self.repo.hall.save_session(&s)?;
                self.session = Some(s);
            }
        }
        if let Some(m) = self
            .input
            .last_assistant_message
            .clone()
            .filter(|m| !m.trim().is_empty())
        {
            let actor = headless_actor().unwrap_or_else(|| "agent".into());
            self.record(
                item,
                &actor,
                Body::Turn {
                    summary: clip(&m, 600),
                },
            )?;
        }
        Ok(None)
    }

    // --- Notification -----------------------------------------------------

    fn notification(&mut self) -> Result<Option<Value>> {
        let item = self.item().map(|i| i.id);
        let message = self
            .input
            .message
            .clone()
            .unwrap_or_else(|| "Claude Code needs you".into());
        self.record(
            item,
            "hook",
            Body::Notification {
                message,
                notification_type: self.input.notification_type.clone(),
            },
        )?;
        Ok(None)
    }
}

/// The role of a headless `claude -p` run started by Hallmark's runner
/// (`HALL_HEADLESS=1`, `HALL_ACTOR=fixer`…), or None in a person's session.
fn headless_actor() -> Option<String> {
    std::env::var_os("HALL_HEADLESS")?;
    Some(
        std::env::var("HALL_ACTOR")
            .ok()
            .filter(|a| !a.trim().is_empty())
            .unwrap_or_else(|| "agent".into()),
    )
}

/// True when the command runs `hall <one of subs>`.
fn mentions(cmd: &str, subs: &[&str]) -> bool {
    gate::split_segments(cmd).iter().any(|seg| {
        let w = gate::words(seg);
        let Some(pos) = w.iter().position(|x| {
            let base = x.rsplit(['/', '\\']).next().unwrap_or(x);
            base == "hall" || base == "hall.exe"
        }) else {
            return false;
        };
        w.get(pos + 1).is_some_and(|s| subs.contains(&s.as_str()))
    })
}

/// The last JSON object in a command's output.
fn last_json(out: &str) -> Option<Value> {
    out.lines().rev().find_map(|l| {
        let l = l.trim();
        if l.starts_with('{') {
            serde_json::from_str(l).ok()
        } else {
            None
        }
    })
}

fn item_context(i: &Item, inbox: usize) -> String {
    let mut s = format!(
        "Hallmark: this session works on {} \"{}\" (state: {}, {}) in worktree {} on branch {}. ",
        i.id,
        i.title,
        i.state,
        i.state.room(),
        i.worktree,
        i.branch
    );
    s.push_str(match i.state {
        State::Development if i.parent.is_some() => {
            "Edit files only inside that worktree, with the Edit and Write tools, and run git \
             there with `git -C <worktree>`. "
        }
        State::Development => {
            "Edit files only inside that worktree, with the Edit and Write tools, and run git \
             there with `git -C <worktree>`. When the work looks done, commit and run \
             `hall propose-ready --summary \"<what was done>\" --why \"<why it's needed>\" \
             --how \"<the approach>\"` (the PR's What, Why and How): with nothing left to \
             confirm it goes straight to review; add `--ask` when the human must decide \
             something. "
        }
        State::AwaitingYou => "It is waiting for the human's call: `hall ready` or `hall pull-back`. ",
        s if s.in_loop() => "It is in the review loop; don't edit it. Offer `hall pull-back` if the human wants changes. ",
        State::Escalated => "It was escalated after repeated rejections; see `hall log --kind verdict`. ",
        _ => "",
    });
    s.push_str("Never push or open PRs: only hallmarked commits leave, through the shipper. ");
    if inbox > 0 {
        s.push_str(&format!(
            "{inbox} entr{} wait in the human's inbox (`hall inbox`). ",
            if inbox == 1 { "y" } else { "ies" }
        ));
    }
    s.push_str("`hall help --json` lists every command.");
    s
}

/// For a parent: its parts and the orchestration flow. For a part: that it
/// merges into its parent. Empty for other items.
fn family_context(i: &Item, children: &[Item]) -> String {
    if let Some(parent) = i.parent {
        return format!(
            " {} is a parallel part of {parent}: build only this part, commit in its worktree, \
             and don't run `hall ready` on it; when it is done, `hall merge {}` merges it into \
             {parent}, which is reviewed once as a whole.",
            i.id, i.id
        );
    }
    if children.is_empty() {
        return String::new();
    }
    let list: Vec<String> = children
        .iter()
        .map(|c| {
            format!(
                "{} {} ({}, {})",
                c.id,
                clip(&c.title, 40),
                c.state,
                c.worktree
            )
        })
        .collect();
    let open: Vec<String> = children
        .iter()
        .filter(|c| !c.state.is_terminal())
        .map(|c| c.id.to_string())
        .collect();
    let mut s = format!(
        " {} is split into parallel parts: {}. ",
        i.id,
        list.join("; ")
    );
    if open.is_empty() {
        s.push_str(&format!(
            "Every part is merged or dropped: run the gates in {}'s worktree, summarise, \
             `hall propose-ready {} --summary … --why … --how …`: it goes to review on its own \
             unless something is left \
             to confirm (then `--ask`, and `hall ready {}` when the human says so).",
            i.id, i.id, i.id
        ));
    } else {
        s.push_str(&format!(
            "You orchestrate: run one subagent per open part ({}) in parallel with the Agent \
             tool, each confined to its part's worktree; `hall merge <part>` as each finishes; \
             `hall ready {}` only after every part is merged or dropped.",
            open.join(", "),
            i.id
        ));
    }
    s
}

/// Every change a commit in `dir` could take (staged, and tracked files a
/// `commit -a` would add) is under `[capture] free`, and there is one.
fn only_free_changes(repo: &Repo, dir: &Path) -> bool {
    let mut files: Vec<String> = Vec::new();
    for args in [
        &["diff", "--cached", "--name-only"][..],
        &["diff", "--name-only"][..],
        // A `git add -A && git commit` also takes untracked files.
        &[
            "ls-files",
            "--others",
            "--exclude-standard",
            "--full-name",
            ":/",
        ][..],
    ] {
        match git::run(dir, args) {
            Ok(out) => files.extend(out.lines().map(str::to_string)),
            Err(_) => return false,
        }
    }
    files.retain(|f| !f.trim().is_empty());
    !files.is_empty() && files.iter().all(|f| repo.config.is_free(f))
}

/// A session's context carries at most this much of a repo skill.
const SKILL_CONTEXT_MAX: usize = 8_000;

/// The repo's `hallmark` skill on `base`, as session context, cut to
/// [`SKILL_CONTEXT_MAX`] with a pointer to the whole file.
fn repo_skill_context(repo: &Repo, base: &str) -> Option<String> {
    let base = crate::assayer::base::Base::resolve(repo, base).ok()?;
    let skill = crate::skills::read(&repo.main_root, &base.sha, "hallmark")?;
    let origin = format!("{} at {}", base.name, crate::event::short(&base.sha));
    let text = crate::skills::section(&skill, &origin);
    if text.len() <= SKILL_CONTEXT_MAX {
        return Some(text);
    }
    Some(format!(
        "{}\nRead the whole skill with `git show {}:{}`.",
        crate::assayer::base::cut(&text, SKILL_CONTEXT_MAX),
        base.name,
        skill.path
    ))
}

fn repo_context(repo: &Repo, items: &[Item], inbox: usize) -> String {
    let mut s = format!(
        "Hallmark tracks changes in {} (capture mode: {}). ",
        repo.name,
        repo.config.capture.mode.as_str()
    );
    s.push_str(match repo.config.capture.mode {
        CaptureMode::Gate | CaptureMode::Ask => {
            "Before changing the app's code, start an item with `hall new --title \"<short title>\"` (or \
             `hall adopt` for an existing branch), then work in the worktree it returns: \
             edit there with the Edit and Write tools and run git with `git -C <worktree>`. \
             The worktree may be outside this session's directory; if Claude Code asks, the \
             human can approve it or run `/add-dir <worktree>`. "
        }
        CaptureMode::Off => "Items start with /hallmark or `hall new`. ",
    });
    s.push_str(
        "Only a clear, concise requirement to build or change something in the app becomes an \
         item. Questions, documents, architecture, requirements and plans don't: answer or \
         write them directly. ",
    );
    if !repo.config.capture.free.is_empty() {
        s.push_str(&format!(
            "Edits under {} need no item. ",
            repo.config.capture.free.join(", ")
        ));
    }
    s.push_str("Never push or open PRs: only hallmarked commits leave. ");
    if !items.is_empty() {
        let list: Vec<String> = items
            .iter()
            .take(8)
            .map(|i| format!("{} {} ({})", i.id, clip(&i.title, 40), i.state))
            .collect();
        s.push_str(&format!("Open items: {}. ", list.join("; ")));
    }
    if inbox > 0 {
        s.push_str(&format!(
            "{inbox} entr{} wait in the human's inbox (`hall inbox`). ",
            if inbox == 1 { "y" } else { "ies" }
        ));
    }
    s.push_str("`hall help --json` lists every command.");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_hook_inputs() {
        let dir = format!("{}/tests/fixtures/hooks", env!("CARGO_MANIFEST_DIR"));
        for f in std::fs::read_dir(dir).unwrap() {
            let text = std::fs::read_to_string(f.unwrap().path()).unwrap();
            let i: HookInput = serde_json::from_str(&text).unwrap();
            assert!(i.session_id.is_some());
            assert!(i.cwd.is_some());
        }
    }

    #[test]
    fn spots_hall_subcommands() {
        assert!(mentions("hall new --title x", &["new"]));
        assert!(mentions(
            "cd .. && /usr/local/bin/hall adopt",
            &["new", "adopt"]
        ));
        assert!(!mentions("hall status", &["new"]));
    }

    #[test]
    fn finds_the_reply_json() {
        let out = "warning: x\n{\"id\":\"H-0001\",\"next\":[]}\n";
        assert_eq!(last_json(out).unwrap()["id"], "H-0001");
        assert!(last_json("plain").is_none());
    }
}
