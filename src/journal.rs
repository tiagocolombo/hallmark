//! The append-only journal under `$(git rev-parse --git-common-dir)/hallmark/`.
//! Every write holds a file lock (fs4), so worktrees, hooks and the daemon
//! can write at the same time.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use fs4::FileExt;
use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, HallError, Result};
use crate::event::{self, Body, Event};
use crate::item::{Item, ItemId};

/// The layout of the journal directory.
#[derive(Debug, Clone)]
pub struct Hall {
    pub root: PathBuf,
}

impl Hall {
    pub fn new(common_dir: &Path) -> Hall {
        Hall {
            root: common_dir.join("hallmark"),
        }
    }

    pub fn exists(&self) -> bool {
        self.root.is_dir()
    }

    pub fn items_dir(&self) -> PathBuf {
        self.root.join("items")
    }
    pub fn item_dir(&self, id: ItemId) -> PathBuf {
        self.items_dir().join(id.to_string())
    }
    pub fn item_events(&self, id: ItemId) -> PathBuf {
        self.item_dir(id).join("events.jsonl")
    }
    pub fn item_json(&self, id: ItemId) -> PathBuf {
        self.item_dir(id).join("item.json")
    }
    /// Events that belong to no item: prompts and notifications from
    /// sessions without an item, gate denials, repo-level questions.
    pub fn repo_events(&self) -> PathBuf {
        self.root.join("events.jsonl")
    }
    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }
    pub fn queue_dir(&self) -> PathBuf {
        self.root.join("queue")
    }
    pub fn index_path(&self) -> PathBuf {
        self.root.join("index.sqlite")
    }
    pub fn hook_log(&self) -> PathBuf {
        self.root.join("hook-errors.log")
    }
    /// Full review reports: `reviews/r<round>.md`.
    pub fn item_reviews(&self, id: ItemId) -> PathBuf {
        self.item_dir(id).join("reviews")
    }
    /// Check output: `checks/r<round>.log`.
    pub fn item_checks(&self, id: ItemId) -> PathBuf {
        self.item_dir(id).join("checks")
    }
    /// Raw `claude -p` results and prompts, for debugging.
    pub fn item_runs(&self, id: ItemId) -> PathBuf {
        self.item_dir(id).join("runs")
    }
    /// Work leases (`hall next`).
    pub fn leases_dir(&self) -> PathBuf {
        self.root.join("leases")
    }
    /// What the daemon is doing now, for the terminal app's Live view.
    pub fn live_log(&self) -> PathBuf {
        self.root.join("live.log")
    }

    /// Appends a line to the Live log, trimming it when it grows large.
    pub fn live(&self, line: &str) {
        let path = self.live_log();
        if let Ok(meta) = fs::metadata(&path)
            && meta.len() > 512 * 1024
        {
            let _ = fs::rename(&path, path.with_extension("log.1"));
        }
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(f, "{} {line}", Utc::now().format("%Y-%m-%dT%H:%M:%SZ"));
        }
    }

    /// Queued tickets, oldest first.
    pub fn tickets(&self) -> Result<Vec<Ticket>> {
        let mut out = Vec::new();
        let Ok(rd) = fs::read_dir(self.queue_dir()) else {
            return Ok(out);
        };
        for e in rd.flatten() {
            if e.path().extension().is_some_and(|x| x == "json")
                && let Ok(t) = fs::read_to_string(e.path())
                && let Ok(t) = serde_json::from_str::<Ticket>(&t)
            {
                out.push(t);
            }
        }
        out.sort_by_key(|t| (t.queued_at, t.id));
        Ok(out)
    }

    pub fn ticket(&self, id: ItemId) -> Option<Ticket> {
        let p = self.queue_dir().join(format!("{id}.json"));
        serde_json::from_str(&fs::read_to_string(p).ok()?).ok()
    }

    pub fn create(&self) -> Result<()> {
        for d in [self.items_dir(), self.sessions_dir(), self.queue_dir()] {
            fs::create_dir_all(d)?;
        }
        Ok(())
    }

    /// Every journal file: the repo journal, then each item's.
    pub fn journal_files(&self) -> Result<Vec<(Option<ItemId>, PathBuf)>> {
        let mut out = vec![(None, self.repo_events())];
        for id in self.item_ids()? {
            out.push((Some(id), self.item_events(id)));
        }
        Ok(out)
    }

    pub fn item_ids(&self) -> Result<Vec<ItemId>> {
        let mut ids = Vec::new();
        let dir = self.items_dir();
        if !dir.is_dir() {
            return Ok(ids);
        }
        for e in fs::read_dir(dir)? {
            let e = e?;
            if let Some(id) = e
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<ItemId>().ok())
                && e.path().join("events.jsonl").is_file()
            {
                ids.push(id);
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Runs `f` while holding the repo-wide lock (ID allocation, counters).
    pub fn with_repo_lock<R>(&self, f: impl FnOnce() -> Result<R>) -> Result<R> {
        fs::create_dir_all(&self.root)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.root.join(".lock"))?;
        FileExt::lock(&lock)?;
        let r = f();
        let _ = FileExt::unlock(&lock);
        r
    }

    /// The next free item number. Call under the repo lock.
    pub fn next_item_id(&self) -> Result<ItemId> {
        let max = self.items_dir().read_dir().map_or(0, |rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    e.file_name()
                        .to_str()
                        .and_then(|n| n.parse::<ItemId>().ok())
                })
                .map(|id| id.0)
                .max()
                .unwrap_or(0)
        });
        Ok(ItemId(max + 1))
    }

    /// Allocates `Q-<n>`. Takes the repo lock itself.
    pub fn next_question_id(&self) -> Result<String> {
        self.with_repo_lock(|| {
            let path = self.root.join("counters.json");
            let mut c: Counters = match fs::read_to_string(&path) {
                Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
                Err(_) => Counters::default(),
            };
            c.question += 1;
            fs::write(&path, serde_json::to_string(&c)?)?;
            Ok(format!("Q-{}", c.question))
        })
    }

    pub fn append(&self, path: &Path, event: &Event) -> Result<()> {
        let f = open_journal(path)?;
        FileExt::lock(&f)?;
        let r = write_line(&f, event);
        let _ = FileExt::unlock(&f);
        r
    }

    pub fn append_item(&self, id: ItemId, event: &Event) -> Result<()> {
        self.append(&self.item_events(id), event)?;
        if event.body.action().is_some() {
            self.refresh_item_json(id)?;
        }
        Ok(())
    }

    /// Reads an item's journal, lets `f` decide what to append, and appends
    /// it, all under the journal's lock. `f` sees the folded item.
    pub fn transact<R>(
        &self,
        id: ItemId,
        f: impl FnOnce(&Item, &[Event]) -> Result<(Vec<Event>, R)>,
    ) -> Result<(Item, R)> {
        let path = self.item_events(id);
        if !path.is_file() {
            return Err(not_found(id));
        }
        let mut file = open_journal(&path)?;
        FileExt::lock(&file)?;
        let result = (|| -> Result<(Item, R)> {
            let mut text = String::new();
            file.read_to_string(&mut text)?;
            let events = parse_lines(&text);
            let item = event::fold(id, &events)?;
            let (new, r) = f(&item, &events)?;
            for e in &new {
                write_line(&file, e)?;
            }
            file.flush()?;
            let mut all = events;
            all.extend(new);
            Ok((event::fold(id, &all)?, r))
        })();
        let _ = FileExt::unlock(&file);
        let (item, r) = result?;
        self.write_item_json(&item)?;
        Ok((item, r))
    }

    pub fn read(&self, path: &Path) -> Result<Vec<Event>> {
        Ok(parse_lines(&read_locked(path, 0)?))
    }

    pub fn read_item(&self, id: ItemId) -> Result<Vec<Event>> {
        let p = self.item_events(id);
        if !p.is_file() {
            return Err(not_found(id));
        }
        self.read(&p)
    }

    pub fn load_item(&self, id: ItemId) -> Result<Item> {
        event::fold(id, &self.read_item(id)?)
    }

    pub fn refresh_item_json(&self, id: ItemId) -> Result<Item> {
        let item = self.load_item(id)?;
        self.write_item_json(&item)?;
        Ok(item)
    }

    /// `item.json` is a convenience copy; the journal is the truth.
    pub fn write_item_json(&self, item: &Item) -> Result<()> {
        let path = self.item_json(item.id);
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(item)?)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    // --- sessions ---------------------------------------------------------

    pub fn session(&self, id: &str) -> SessionState {
        let p = self.session_path(id);
        fs::read_to_string(p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| SessionState {
                session: id.to_string(),
                ..Default::default()
            })
    }

    pub fn save_session(&self, s: &SessionState) -> Result<()> {
        fs::create_dir_all(self.sessions_dir())?;
        let p = self.session_path(&s.session);
        let tmp = p.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(s)?)?;
        fs::rename(&tmp, &p)?;
        Ok(())
    }

    fn session_path(&self, id: &str) -> PathBuf {
        let safe: String = id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.sessions_dir().join(format!("{safe}.json"))
    }

    // --- review queue -----------------------------------------------------

    pub fn queue(&self, t: &Ticket) -> Result<()> {
        fs::create_dir_all(self.queue_dir())?;
        let p = self.queue_dir().join(format!("{}.json", t.id));
        fs::write(p, serde_json::to_string_pretty(t)?)?;
        Ok(())
    }

    pub fn unqueue(&self, id: ItemId) -> Result<bool> {
        let p = self.queue_dir().join(format!("{id}.json"));
        match fs::remove_file(p) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    pub fn log_hook_error(&self, event: &str, err: &str) {
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.hook_log())
        {
            let _ = writeln!(f, "{} {event}: {err}", Utc::now().to_rfc3339());
        }
    }
}

/// What Hallmark remembers about a Claude Code session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionState {
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
    /// Bytes of the transcript already counted for usage.
    #[serde(default)]
    pub transcript_offset: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_request: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    /// Inbox entries this session has already been told about
    /// (`kind:item:qid`), so each is announced once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub told: Vec<String>,
}

/// A ready item waiting for the assayer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub id: ItemId,
    pub sha: String,
    pub round: u32,
    pub queued_at: DateTime<Utc>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Counters {
    #[serde(default)]
    question: u64,
}

fn not_found(id: ItemId) -> HallError {
    HallError::new(
        ErrorCode::ItemNotFound,
        format!("no item {id} in this repository"),
    )
    .next(["hall status --all"])
}

fn open_journal(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)?)
}

/// Reads a journal from byte `offset` under a shared lock. Windows locks are
/// mandatory, so reading without one could fail mid-write. Returns only
/// complete lines; a missing file reads as empty.
pub fn read_locked(path: &Path, offset: u64) -> Result<String> {
    use std::io::{Seek, SeekFrom};
    let mut f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(e.into()),
    };
    FileExt::lock_shared(&f)?;
    let mut buf = Vec::new();
    let r = f
        .seek(SeekFrom::Start(offset))
        .and_then(|_| f.read_to_end(&mut buf));
    let _ = FileExt::unlock(&f);
    r?;
    if let Some(end) = buf.iter().rposition(|b| *b == b'\n') {
        buf.truncate(end + 1);
    } else {
        buf.clear();
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn write_line(mut f: &File, e: &Event) -> Result<()> {
    let mut line = serde_json::to_string(e)?;
    line.push('\n');
    f.write_all(line.as_bytes())?;
    Ok(())
}

/// Parses complete lines. A line that isn't a known event (a newer version,
/// or a torn write) is skipped rather than failing the whole journal.
pub fn parse_lines(text: &str) -> Vec<Event> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Event>(l).ok())
        .collect()
}

/// Convenience for commands: an event for `item`, by `actor`.
pub fn ev(actor: &str, item: Option<ItemId>, body: Body) -> Event {
    Event::new(actor, item, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let hall = Hall::new(dir.path());
        hall.create().unwrap();
        let e = ev("you", None, Body::Prompt { text: "hi".into() });
        hall.append(&hall.repo_events(), &e).unwrap();
        hall.append(&hall.repo_events(), &e).unwrap();
        assert_eq!(hall.read(&hall.repo_events()).unwrap(), vec![e.clone(), e]);
    }

    #[test]
    fn skips_unknown_and_torn_lines() {
        let good = serde_json::to_string(&ev(
            "you",
            None,
            Body::Turn {
                summary: "x".into(),
            },
        ))
        .unwrap();
        let text =
            format!("{good}\n{{\"v\":9,\"event\":\"from_the_future\"}}\n{{\"v\":1,\"t\n{good}\n");
        assert_eq!(parse_lines(&text).len(), 2);
    }

    #[test]
    fn concurrent_appends_never_interleave() {
        let dir = tempfile::tempdir().unwrap();
        let hall = Hall::new(dir.path());
        hall.create().unwrap();
        let path = hall.repo_events();
        std::thread::scope(|s| {
            for t in 0..8 {
                let (hall, path) = (&hall, &path);
                s.spawn(move || {
                    for i in 0..50 {
                        let text = format!("{t}-{i}-{}", "x".repeat(2000));
                        hall.append(path, &ev("you", None, Body::Prompt { text }))
                            .unwrap();
                    }
                });
            }
        });
        let raw = fs::read_to_string(&path).unwrap();
        assert_eq!(raw.lines().count(), 400);
        assert_eq!(parse_lines(&raw).len(), 400);
    }

    #[test]
    fn question_ids_increase() {
        let dir = tempfile::tempdir().unwrap();
        let hall = Hall::new(dir.path());
        hall.create().unwrap();
        assert_eq!(hall.next_question_id().unwrap(), "Q-1");
        assert_eq!(hall.next_question_id().unwrap(), "Q-2");
    }
}
