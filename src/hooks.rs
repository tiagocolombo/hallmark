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

use crate::config::CaptureMode;
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
        let text = match &item {
            Some(i) => item_context(i, inbox),
            None => repo_context(&self.repo, &self.items, inbox),
        };
        Ok(Some(context(HookEvent::SessionStart, text)))
    }

    // --- UserPromptSubmit -------------------------------------------------

    fn user_prompt_submit(&mut self) -> Result<Option<Value>> {
        let item = self.item();
        let text = self.input.prompt.clone().unwrap_or_default();
        if !text.trim().is_empty() {
            self.record(item.as_ref().map(|i| i.id), "you", Body::Prompt { text })?;
        }
        let Some(i) = item else { return Ok(None) };
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
            _ => return Ok(None),
        };
        Ok(Some(context(HookEvent::UserPromptSubmit, note)))
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
                let ops = gate::scan(cmd, &self.dir, &self.repo.config.ship_prefixes());
                let mut d = None;
                for op in ops {
                    d = match op.kind {
                        OpKind::Commit => self.check_commit(&op.dir)?,
                        OpKind::Push => self.check_push(&op.dir)?,
                    };
                    if d.is_some() {
                        break;
                    }
                }
                d
            }
            _ => None,
        };
        let Some((kind, err, target)) = denial else {
            return Ok(None);
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
            self.bind(i.id)?;
            return Ok(None);
        }
        if git::is_ignored(&self.repo.toplevel, p) {
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
        if git::hallmark_note(&dir, &head).is_some() {
            return Ok(None);
        }
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
                "{} has no hallmark. Work reaches GitHub only through review: send the item to \
                 review and the shipper pushes it once approved.",
                crate::event::short(&head)
            ),
        )
        .next(next);
        Ok(Some((
            "deny",
            err,
            format!("push {}", crate::event::short(&head)),
        )))
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
            self.bind(id)?;
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
                self.record(
                    Some(item.id),
                    "agent",
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
        if let (Some(tp), Some(sess)) = (self.input.transcript_path.clone(), self.session.clone()) {
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
            self.record(
                item,
                "agent",
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
        State::Development => {
            "Edit files only inside that worktree. When the work looks done, run \
             `hall propose-ready --summary \"<what was done>\"` and ask the human; when they say \
             ready, commit and run `hall ready`. "
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

fn repo_context(repo: &Repo, items: &[Item], inbox: usize) -> String {
    let mut s = format!(
        "Hallmark tracks changes in {} (capture mode: {}). ",
        repo.name,
        repo.config.capture.mode.as_str()
    );
    s.push_str(match repo.config.capture.mode {
        CaptureMode::Gate | CaptureMode::Ask => {
            "Before editing files, start an item with `hall new --title \"<short title>\"` (or \
             `hall adopt` for an existing branch), then work in the worktree it returns. "
        }
        CaptureMode::Off => "Items start with /hallmark or `hall new`. ",
    });
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
