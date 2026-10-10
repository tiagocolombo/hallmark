//! A SQLite index derived from the journal. It is a cache: it catches up by
//! reading each journal from the last indexed byte, and `hall reindex`
//! rebuilds it from nothing.

use std::collections::BTreeSet;
use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use schemars::JsonSchema;
use serde::Serialize;

use crate::error::Result;
use crate::event::{Body, Event};
use crate::findings::PriorStatus;
use crate::item::{Item, ItemId, State};
use crate::journal::{self, Hall};

const SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS files (path TEXT PRIMARY KEY, offset INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS items (
  id TEXT PRIMARY KEY, num INTEGER NOT NULL, state TEXT NOT NULL, branch TEXT, worktree TEXT,
  updated_at TEXT, state_since TEXT, json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS events (
  id INTEGER PRIMARY KEY AUTOINCREMENT, item TEXT, t TEXT NOT NULL, actor TEXT, event TEXT NOT NULL,
  session TEXT, json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_item ON events(item, event);
CREATE TABLE IF NOT EXISTS usage (
  item TEXT, t TEXT NOT NULL, day TEXT NOT NULL, source TEXT, room TEXT, role TEXT, model TEXT,
  input INTEGER, output INTEGER, cache_read INTEGER, cache_write INTEGER, cost_usd REAL,
  requests INTEGER, turns INTEGER, duration_ms INTEGER
);
CREATE TABLE IF NOT EXISTS questions (
  qid TEXT PRIMARY KEY, item TEXT, text TEXT NOT NULL, options TEXT NOT NULL, dflt TEXT,
  wait TEXT, deadline TEXT, asked_at TEXT NOT NULL, asked_by TEXT, session TEXT,
  answer TEXT, answered_at TEXT, answered_by TEXT
);
CREATE TABLE IF NOT EXISTS notifications (
  item TEXT, session TEXT, t TEXT NOT NULL, message TEXT, kind TEXT, resolved_at TEXT
);
CREATE TABLE IF NOT EXISTS findings (
  item TEXT NOT NULL, id TEXT NOT NULL, round INTEGER NOT NULL, t TEXT NOT NULL, sha TEXT,
  lens TEXT NOT NULL, severity TEXT NOT NULL, file TEXT, line INTEGER, title TEXT, scenario TEXT,
  fix TEXT, status TEXT NOT NULL, claimed_by TEXT, dispute TEXT, settled_round INTEGER,
  PRIMARY KEY (item, id)
);
"#;

pub struct Index {
    conn: Connection,
}

/// Something waiting for you.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct InboxEntry {
    /// question, escalated, ready, session_waiting, budget, run_failed,
    /// ship_failed, stuck, daemon_stopped
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qid: Option<String>,
    pub text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub since: DateTime<Utc>,
    /// The command that settles it.
    pub answer_with: Vec<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct QuestionRow {
    pub qid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
    pub text: String,
    pub options: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline: Option<DateTime<Utc>>,
    pub asked_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct UsageRow {
    pub key: String,
    pub records: u64,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub tokens: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UsageBy {
    Item,
    Room,
    Role,
    Model,
    Day,
    Source,
}

/// A finding as indexed, for history and the terminal app.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct FindingRow {
    pub item: ItemId,
    pub id: String,
    pub round: u32,
    pub t: DateTime<Utc>,
    pub lens: String,
    pub severity: String,
    pub file: String,
    pub line: u32,
    pub title: String,
    pub scenario: String,
    pub fix: String,
    /// open, fixed, withdrawn
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claimed_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispute: Option<String>,
}

impl Index {
    /// The connection, for modules that keep their own derived tables (the
    /// Library's search index). Such tables must be rebuildable.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Findings, newest first: optionally one item's, one lens's, or only
    /// those raised since `since`.
    pub fn findings(
        &self,
        item: Option<ItemId>,
        lens: Option<&str>,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<FindingRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT item, id, round, t, lens, severity, file, line, title, scenario, fix, status,
                    claimed_by, dispute
             FROM findings WHERE (?1 IS NULL OR item = ?1) AND (?2 IS NULL OR lens = ?2)
               AND (?3 IS NULL OR t >= ?3)
             ORDER BY t DESC, item, CAST(SUBSTR(id, 2) AS INTEGER)",
        )?;
        let rows = stmt.query_map(
            params![
                item.map(|i| i.to_string()),
                lens,
                since.map(|t| t.to_rfc3339())
            ],
            |r| {
                Ok(FindingRow {
                    item: r.get::<_, String>(0)?.parse().unwrap_or(ItemId(0)),
                    id: r.get(1)?,
                    round: r.get(2)?,
                    t: parse_time(&r.get::<_, String>(3)?),
                    lens: r.get(4)?,
                    severity: r.get(5)?,
                    file: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                    line: r.get::<_, Option<u32>>(7)?.unwrap_or(0),
                    title: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
                    scenario: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
                    fix: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
                    status: r.get(11)?,
                    claimed_by: r.get(12)?,
                    dispute: r.get(13)?,
                })
            },
        )?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn open(hall: &Hall) -> Result<Index> {
        let path = hall.index_path();
        let conn = open_conn(&path)?;
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let conn = if v != SCHEMA_VERSION && v != 0 {
            drop(conn);
            remove_db(&path)?;
            open_conn(&path)?
        } else {
            conn
        };
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
        Ok(Index { conn })
    }

    /// Opens the index and catches up with the journal.
    pub fn synced(hall: &Hall) -> Result<Index> {
        let mut ix = Index::open(hall)?;
        ix.sync(hall)?;
        Ok(ix)
    }

    /// Empties the index and rebuilds it from the journal.
    pub fn rebuild(hall: &Hall) -> Result<Index> {
        let mut ix = Index::open(hall)?;
        ix.clear()?;
        ix.sync(hall)?;
        Ok(ix)
    }

    fn clear(&mut self) -> Result<()> {
        self.conn.execute_batch(
            "DELETE FROM files; DELETE FROM items; DELETE FROM events; DELETE FROM usage;
             DELETE FROM questions; DELETE FROM notifications; DELETE FROM findings;",
        )?;
        Ok(())
    }

    pub fn sync(&mut self, hall: &Hall) -> Result<()> {
        if !hall.exists() {
            return Ok(());
        }
        let files = hall.journal_files()?;
        let tx = self.conn.transaction()?;
        let mut touched: BTreeSet<ItemId> = BTreeSet::new();
        let mut rebuild = false;
        for (item, path) in &files {
            let key = path.to_string_lossy().to_string();
            let offset: u64 = tx
                .query_row("SELECT offset FROM files WHERE path = ?1", [&key], |r| {
                    r.get::<_, i64>(0)
                })
                .optional()?
                .unwrap_or(0) as u64;
            let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            if len < offset {
                rebuild = true;
                break;
            }
            if len == offset {
                continue;
            }
            let text = journal::read_locked(path, offset)?;
            if text.is_empty() {
                continue;
            }
            for e in journal::parse_lines(&text) {
                index_event(&tx, *item, &e)?;
            }
            let new_offset = offset + text.len() as u64;
            tx.execute(
                "INSERT INTO files(path, offset) VALUES (?1, ?2)
                 ON CONFLICT(path) DO UPDATE SET offset = excluded.offset",
                params![key, new_offset as i64],
            )?;
            if let Some(id) = item {
                touched.insert(*id);
            }
        }
        if rebuild {
            // A journal shrank, which only happens if someone edited it.
            drop(tx);
            self.clear()?;
            return self.sync(hall);
        }
        for id in touched {
            if let Ok(item) = hall.load_item(id) {
                upsert_item(&tx, &item)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn items(&self, include_closed: bool) -> Result<Vec<Item>> {
        let mut stmt = self.conn.prepare("SELECT json FROM items ORDER BY num")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            if let Ok(item) = serde_json::from_str::<Item>(&r?)
                && (include_closed || !item.state.is_terminal())
            {
                out.push(item);
            }
        }
        Ok(out)
    }

    pub fn counts(&self) -> Result<Vec<(String, u64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT state, COUNT(*) FROM items GROUP BY state ORDER BY state")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn question(&self, qid: &str) -> Result<Option<QuestionRow>> {
        let qid = qid.trim().to_ascii_uppercase();
        let qid = if qid.starts_with("Q-") {
            qid
        } else {
            format!("Q-{qid}")
        };
        self.conn
            .query_row(
                "SELECT qid, item, text, options, dflt, wait, deadline, asked_at, session, answer, answered_at
                 FROM questions WHERE qid = ?1",
                [qid],
                question_row,
            )
            .optional()
            .map_err(Into::into)
    }

    /// An unanswered question with the same text for the same item.
    pub fn open_question_like(
        &self,
        item: Option<ItemId>,
        text: &str,
    ) -> Result<Option<QuestionRow>> {
        let item = item.map(|i| i.to_string());
        self.conn
            .query_row(
                "SELECT qid, item, text, options, dflt, wait, deadline, asked_at, session, answer, answered_at
                 FROM questions WHERE item IS ?1 AND text = ?2 AND answer IS NULL",
                params![item, text],
                question_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn inbox(&self) -> Result<Vec<InboxEntry>> {
        let mut out = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT qid, item, text, options, dflt, wait, deadline, asked_at, session, answer, answered_at
             FROM questions WHERE answer IS NULL",
        )?;
        for q in stmt.query_map([], question_row)? {
            let q = q?;
            out.push(InboxEntry {
                kind: "question".into(),
                item: q.item,
                answer_with: vec![format!(
                    "hall answer {} {}",
                    q.qid,
                    q.default.clone().unwrap_or_else(|| "<answer>".into())
                )],
                qid: Some(q.qid),
                text: q.text,
                options: q.options,
                default: q.default,
                session: q.session,
                since: q.asked_at,
            });
        }
        for item in self.items(false)? {
            let (kind, text, cmds) = match item.state {
                State::AwaitingYou => (
                    "ready",
                    format!(
                        "{} looks done: {}",
                        item.title,
                        item.summary.clone().unwrap_or_else(|| "no summary".into())
                    ),
                    vec![
                        format!("hall ready {}", item.id),
                        format!("hall pull-back {}", item.id),
                    ],
                ),
                State::Escalated => (
                    "escalated",
                    format!("{}: rejected {} times", item.title, item.rejects),
                    vec![
                        format!("hall log {} --kind verdict", item.id),
                        format!("hall retry {} --reason \"<why>\"", item.id),
                        format!("hall pull-back {}", item.id),
                        format!("hall drop {} --reason \"<why>\"", item.id),
                    ],
                ),
                _ => {
                    // Held by the loop (budget, failed runs) or flagged stuck.
                    if let Some(h) = &item.hold {
                        out.push(InboxEntry {
                            kind: h.kind.as_str().into(),
                            item: Some(item.id),
                            qid: None,
                            text: format!("{}: {}", item.title, h.reason),
                            options: vec![],
                            default: None,
                            session: None,
                            since: h.since,
                            answer_with: vec![
                                format!("hall retry {} --reason \"<why>\"", item.id),
                                format!("hall log {}", item.id),
                                format!("hall pull-back {}", item.id),
                            ],
                        });
                    } else if let Some(since) = item.stuck_since {
                        out.push(InboxEntry {
                            kind: "stuck".into(),
                            item: Some(item.id),
                            qid: None,
                            text: format!(
                                "{}: no progress in {} since {}",
                                item.title,
                                item.state,
                                item.updated_at.format("%H:%M")
                            ),
                            options: vec![],
                            default: None,
                            session: None,
                            since,
                            answer_with: vec![
                                format!("hall status {}", item.id),
                                "hall daemon status".into(),
                            ],
                        });
                    }
                    continue;
                }
            };
            out.push(InboxEntry {
                kind: kind.into(),
                item: Some(item.id),
                qid: None,
                text,
                options: vec![],
                default: None,
                session: None,
                since: item.state_since,
                answer_with: cmds,
            });
        }
        // One entry per waiting session: its latest unresolved notification.
        let mut stmt = self.conn.prepare(
            "SELECT item, session, MAX(t), message FROM notifications
             WHERE resolved_at IS NULL GROUP BY session",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in rows {
            let (item, session, t, message) = row?;
            let item = item.and_then(|s| s.parse().ok());
            out.push(InboxEntry {
                kind: "session_waiting".into(),
                item,
                qid: None,
                text: message.unwrap_or_else(|| "A Claude Code session is waiting for you".into()),
                options: vec![],
                default: None,
                answer_with: vec![match &session {
                    Some(s) => format!("claude --resume {s}"),
                    None => "open the waiting Claude Code session".into(),
                }],
                session,
                since: parse_time(&t),
            });
        }
        // A shipped PR whose CI failed, until a follow-up item takes it on.
        let all = self.items(true)?;
        for item in &all {
            let (Some(ci), Some(url)) = (&item.ci, &item.pr_url) else {
                continue;
            };
            let followed = all
                .iter()
                .any(|f| f.follow_up.as_deref() == Some(url.as_str()) && f.state != State::Dropped);
            if ci.passed || followed {
                continue;
            }
            out.push(InboxEntry {
                kind: "ci_failed".into(),
                item: Some(item.id),
                qid: None,
                text: format!(
                    "{}: CI failed on {url}: {}",
                    item.title,
                    ci.failing.join(", ")
                ),
                options: vec![],
                default: None,
                session: None,
                since: ci.t,
                answer_with: vec![format!("hall new --follow-up {url}")],
            });
        }
        let now = Utc::now();
        if let Some(st) = crate::daemon::state::load()
            && let Some(since) = st.stopped_unexpectedly(now)
        {
            out.push(InboxEntry {
                kind: "daemon_stopped".into(),
                item: None,
                qid: None,
                text: "The Hallmark daemon stopped; the review loop isn't running".into(),
                options: vec![],
                default: None,
                session: None,
                since,
                answer_with: vec!["hall daemon start".into(), "hall daemon status".into()],
            });
        }
        out.sort_by_key(|e| e.since);
        Ok(out)
    }

    pub fn usage(
        &self,
        by: UsageBy,
        since: Option<DateTime<Utc>>,
        item: Option<ItemId>,
    ) -> Result<Vec<UsageRow>> {
        let key = match by {
            UsageBy::Item => "COALESCE(item, '(no item)')",
            UsageBy::Room => "room",
            UsageBy::Role => "role",
            UsageBy::Model => "model",
            UsageBy::Day => "day",
            UsageBy::Source => "source",
        };
        let sql = format!(
            "SELECT {key}, COUNT(*), SUM(requests), SUM(input), SUM(output), SUM(cache_read),
                    SUM(cache_write), SUM(cost_usd)
             FROM usage WHERE (?1 IS NULL OR t >= ?1) AND (?2 IS NULL OR item = ?2)
             GROUP BY 1 ORDER BY SUM(cost_usd) DESC, 1"
        );
        let since = since.map(|t| t.to_rfc3339());
        let item = item.map(|i| i.to_string());
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![since, item], |r| {
            let g = |i: usize| r.get::<_, Option<i64>>(i).map(|v| v.unwrap_or(0) as u64);
            let input = g(3)?;
            let output = g(4)?;
            let cr = g(5)?;
            let cw = g(6)?;
            Ok(UsageRow {
                key: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                records: g(1)?,
                requests: g(2)?,
                input_tokens: input,
                output_tokens: output,
                cache_read_tokens: cr,
                cache_write_tokens: cw,
                tokens: input + output + cr + cw,
                cost_usd: r.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn event_count(&self) -> Result<u64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))?
            as u64)
    }

    pub fn usage_count(&self) -> Result<u64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM usage", [], |r| r.get::<_, i64>(0))? as u64)
    }
}

fn open_conn(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
    Ok(conn)
}

fn remove_db(path: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let p = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn parse_time(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_default()
}

fn question_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<QuestionRow> {
    let opt_time = |i: usize| -> rusqlite::Result<Option<DateTime<Utc>>> {
        Ok(r.get::<_, Option<String>>(i)?.map(|s| parse_time(&s)))
    };
    Ok(QuestionRow {
        qid: r.get(0)?,
        item: r.get::<_, Option<String>>(1)?.and_then(|s| s.parse().ok()),
        text: r.get(2)?,
        options: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
        default: r.get(4)?,
        wait: r.get(5)?,
        deadline: opt_time(6)?,
        asked_at: parse_time(&r.get::<_, String>(7)?),
        session: r.get(8)?,
        answer: r.get(9)?,
        answered_at: opt_time(10)?,
    })
}

fn upsert_item(tx: &rusqlite::Transaction<'_>, item: &Item) -> Result<()> {
    tx.execute(
        "INSERT INTO items(id, num, state, branch, worktree, updated_at, state_since, json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET state = excluded.state, branch = excluded.branch,
           worktree = excluded.worktree, updated_at = excluded.updated_at,
           state_since = excluded.state_since, json = excluded.json",
        params![
            item.id.to_string(),
            item.id.0,
            item.state.as_str(),
            item.branch,
            item.worktree,
            item.updated_at.to_rfc3339(),
            item.state_since.to_rfc3339(),
            serde_json::to_string(item)?,
        ],
    )?;
    Ok(())
}

fn index_event(tx: &rusqlite::Transaction<'_>, file_item: Option<ItemId>, e: &Event) -> Result<()> {
    let item = e.item.or(file_item).map(|i| i.to_string());
    let t = e.t.to_rfc3339();
    tx.execute(
        "INSERT INTO events(item, t, actor, event, session, json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            item,
            t,
            e.actor,
            e.body.name(),
            e.session,
            serde_json::to_string(e)?
        ],
    )?;
    match &e.body {
        Body::Usage(u) => {
            tx.execute(
                "INSERT INTO usage(item, t, day, source, room, role, model, input, output, cache_read,
                   cache_write, cost_usd, requests, turns, duration_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    item,
                    t,
                    e.t.format("%Y-%m-%d").to_string(),
                    u.source,
                    u.room,
                    u.role,
                    u.model,
                    u.input_tokens as i64,
                    u.output_tokens as i64,
                    u.cache_read_tokens as i64,
                    u.cache_write_tokens as i64,
                    u.cost_usd,
                    u.requests,
                    u.turns,
                    u.duration_ms.map(|d| d as i64),
                ],
            )?;
        }
        Body::Question {
            qid,
            text,
            options,
            default,
            wait,
            deadline,
        } => {
            tx.execute(
                "INSERT OR IGNORE INTO questions(qid, item, text, options, dflt, wait, deadline, asked_at,
                   asked_by, session)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    qid,
                    item,
                    text,
                    serde_json::to_string(options)?,
                    default,
                    wait,
                    deadline.map(|d| d.to_rfc3339()),
                    t,
                    e.actor,
                    e.session,
                ],
            )?;
        }
        Body::Answer { qid, answer, .. } => {
            tx.execute(
                "UPDATE questions SET answer = ?2, answered_at = ?3, answered_by = ?4
                 WHERE qid = ?1 AND answer IS NULL",
                params![qid, answer, t, e.actor],
            )?;
        }
        Body::QuestionSession { qid } => {
            tx.execute(
                "UPDATE questions SET session = ?2 WHERE qid = ?1",
                params![qid, e.session],
            )?;
        }
        Body::Notification {
            message,
            notification_type,
        } => {
            tx.execute(
                "INSERT INTO notifications(item, session, t, message, kind) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![item, e.session, t, message, notification_type],
            )?;
        }
        Body::Verdict {
            sha,
            round,
            findings,
            prior,
            ..
        } => {
            for p in prior {
                let status = match p.status {
                    PriorStatus::Fixed => "fixed",
                    PriorStatus::Open => "open",
                    PriorStatus::Withdrawn => "withdrawn",
                };
                tx.execute(
                    "UPDATE findings SET status = ?3,
                       settled_round = CASE WHEN ?3 = 'open' THEN NULL ELSE ?4 END
                     WHERE item = ?1 AND id = ?2",
                    params![item, p.id, status, round],
                )?;
            }
            for f in findings {
                tx.execute(
                    "INSERT OR IGNORE INTO findings(item, id, round, t, sha, lens, severity, file, line,
                       title, scenario, fix, status)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'open')",
                    params![
                        item,
                        f.id,
                        f.round,
                        t,
                        sha,
                        f.lens.as_str(),
                        f.severity.as_str(),
                        f.file,
                        f.line,
                        f.title,
                        f.scenario,
                        f.fix
                    ],
                )?;
            }
        }
        Body::FixSubmitted {
            sha,
            fixes,
            disputes,
        } => {
            for id in fixes {
                tx.execute(
                    "UPDATE findings SET claimed_by = ?3 WHERE item = ?1 AND id = ?2",
                    params![item, id, sha],
                )?;
            }
            for d in disputes {
                tx.execute(
                    "UPDATE findings SET dispute = ?3 WHERE item = ?1 AND id = ?2",
                    params![item, d.id, d.reason],
                )?;
            }
        }
        // You answered the session: whatever it was waiting for is settled.
        Body::Prompt { .. } | Body::SessionStarted { .. } if e.session.is_some() => {
            tx.execute(
                "UPDATE notifications SET resolved_at = ?2 WHERE session = ?1 AND resolved_at IS NULL",
                params![e.session, t],
            )?;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn bundled_sqlite_has_fts5() {
        // The Library's built-in search depends on it.
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE VIRTUAL TABLE t USING fts5(body);
             INSERT INTO t(body) VALUES ('retry webhooks with jitter');",
        )
        .unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM t WHERE t MATCH 'webhook*'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 1);
    }
}
