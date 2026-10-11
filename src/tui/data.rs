//! What the app reads: the repo registry, and each repo's index, journal
//! and live log. Nothing here writes, except the index catching up with the
//! journal (it is a cache, ADR 0003).

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use rusqlite::params;

use crate::commands::init::Registry;
use crate::config::{Config, Role};
use crate::index::{FindingRow, InboxEntry, Index, UsageBy, UsageRow};
use crate::item::{Item, ItemId, State};
use crate::journal::Hall;
use crate::paths;
use crate::repo::Repo;

/// How many lines of `live.log` the Live view keeps.
const LIVE_LINES: usize = 300;
/// How many days the usage view's "by day" grouping shows.
const USAGE_DAYS: i64 = 14;

/// One repository the app knows about.
pub struct RepoSlot {
    pub name: String,
    /// The main checkout.
    pub path: PathBuf,
    pub data: RepoData,
    index: Option<Index>,
    hall: Option<Hall>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RepoStatus {
    Ready,
    /// A git repository without `<git-common-dir>/hallmark`.
    NotInitialized,
    /// The registered path is gone.
    Missing,
    /// Something failed while reading it; the message says what.
    Broken(String),
}

/// Everything the views draw for one repo, refreshed when its journal
/// changes.
#[derive(Debug, Clone)]
pub struct RepoData {
    pub status: RepoStatus,
    pub config: Config,
    /// Every item, closed and dropped included, by number.
    pub items: Vec<Item>,
    /// What waits for you (the daemon entry is added by the app, which
    /// owns the daemon state).
    pub inbox: Vec<InboxEntry>,
    /// Every finding of every item, newest first.
    pub findings: Vec<FindingRow>,
    pub usage: UsageData,
    /// The tail of `live.log`, oldest first.
    pub live: Vec<String>,
    /// Reviews that asked for changes today (UTC).
    pub rejected_today: u64,
    /// The `now` line of each open item: what it is doing, and for how long
    /// (the same line `hall status` and the HUD show).
    pub now: BTreeMap<ItemId, (String, DateTime<Utc>)>,
}

/// The `now` lines of the open items, read from the journal.
fn now_lines(
    hall: &Hall,
    path: &Path,
    config: &Config,
    items: &[Item],
) -> BTreeMap<ItemId, (String, DateTime<Utc>)> {
    let repo = Repo {
        cwd: path.to_path_buf(),
        toplevel: path.to_path_buf(),
        common_dir: hall.root.parent().unwrap_or(path).to_path_buf(),
        main_root: path.to_path_buf(),
        name: String::new(),
        hall: hall.clone(),
        config: config.clone(),
        config_path: None,
    };
    let live: Vec<Item> = items
        .iter()
        .filter(|i| !i.state.is_terminal())
        .cloned()
        .collect();
    live.iter()
        .filter_map(|i| {
            let events = hall.read_item(i.id).ok()?;
            let n = crate::now::now_cheap(&repo, i, &live, &events);
            Some((i.id, (n.doing, n.since)))
        })
        .collect()
}

impl RepoData {
    fn empty(status: RepoStatus) -> RepoData {
        RepoData {
            status,
            config: Config::default(),
            items: Vec::new(),
            inbox: Vec::new(),
            findings: Vec::new(),
            usage: UsageData::default(),
            live: Vec::new(),
            rejected_today: 0,
            now: BTreeMap::new(),
        }
    }

    pub fn item(&self, id: ItemId) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    /// Items that are neither closed nor dropped.
    pub fn open_count(&self) -> usize {
        self.items.iter().filter(|i| !i.state.is_terminal()).count()
    }
}

/// Usage grouped the ways the Usage view shows it.
#[derive(Debug, Clone, Default)]
pub struct UsageData {
    /// Today (UTC), by room.
    pub by_room: Vec<UsageRow>,
    /// Today (UTC), by model.
    pub by_model: Vec<UsageRow>,
    /// All time, by item: the per-item cap counts an item's whole life.
    pub by_item: Vec<UsageRow>,
    /// The last [`USAGE_DAYS`] days, newest first.
    pub by_day: Vec<DayRow>,
    /// What the review loop (every room but the Workshop) spent today:
    /// weighted tokens and dollars.
    pub loop_today: crate::budget::Spent,
    /// Tokens `hall retry` granted each item on top of `per_item`.
    pub granted: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default)]
pub struct DayRow {
    pub day: String,
    pub records: u64,
    pub tokens: u64,
    /// Weighted tokens outside the Workshop, which a token daily cap counts.
    pub loop_tokens: u64,
    /// Dollars outside the Workshop, which a dollar daily cap counts.
    pub loop_usd: f64,
    pub cost_usd: f64,
}

impl RepoSlot {
    pub fn open(name: String, path: PathBuf, now: DateTime<Utc>) -> RepoSlot {
        let mut s = RepoSlot {
            name,
            path,
            data: RepoData::empty(RepoStatus::Missing),
            index: None,
            hall: None,
        };
        s.reload(now);
        s
    }

    /// The journal directory, when the repo is set up.
    pub fn hall(&self) -> Option<&Hall> {
        self.hall.as_ref()
    }

    /// Re-reads the repo. A failure never escapes: it becomes the repo's
    /// status, so one broken repo can't take the app down.
    pub fn reload(&mut self, now: DateTime<Utc>) {
        if !self.path.is_dir() {
            self.data = RepoData::empty(RepoStatus::Missing);
            self.index = None;
            self.hall = None;
            return;
        }
        if self.hall.is_none() {
            match Repo::discover(&self.path) {
                Ok(repo) => {
                    if repo.initialized() {
                        self.hall = Some(repo.hall.clone());
                    } else {
                        let mut d = RepoData::empty(RepoStatus::NotInitialized);
                        d.config = repo.config;
                        self.data = d;
                        return;
                    }
                }
                Err(e) => {
                    self.data = RepoData::empty(RepoStatus::Broken(e.message));
                    return;
                }
            }
        }
        let hall = self.hall.clone().expect("set above");
        if !hall.exists() {
            self.hall = None;
            self.index = None;
            self.data = RepoData::empty(RepoStatus::NotInitialized);
            return;
        }
        match self.read(&hall, now) {
            Ok(d) => self.data = d,
            Err(e) => {
                // A broken cache: drop the connection and try again next time.
                self.index = None;
                self.data = RepoData::empty(RepoStatus::Broken(e.message));
            }
        }
    }

    fn read(&mut self, hall: &Hall, now: DateTime<Utc>) -> crate::error::Result<RepoData> {
        if self.index.is_none() {
            self.index = Some(Index::open(hall)?);
        }
        let ix = self.index.as_mut().expect("set above");
        ix.sync(hall)?;
        let (config, _) = Config::load(&[&self.path]).unwrap_or_default();
        let items = ix.items(true)?;
        // The app reads the daemon state itself, so it can be injected.
        let inbox = ix
            .inbox()?
            .into_iter()
            .filter(|e| e.kind != "daemon_stopped")
            .collect();
        let findings = ix.findings(None, None, None)?;
        let usage = UsageData::load(ix, now)?;
        let rejected_today = ix.conn().query_row(
            "SELECT COUNT(*) FROM events WHERE event = 'verdict' AND t >= ?1
               AND json LIKE '%\"verdict\":\"changes\"%'",
            params![start_of_day(now).to_rfc3339()],
            |r| r.get::<_, i64>(0),
        )? as u64;
        let now_map = now_lines(hall, &self.path, &config, &items);
        Ok(RepoData {
            status: RepoStatus::Ready,
            now: now_map,
            config,
            items,
            inbox,
            findings,
            usage,
            live: tail(&hall.live_log(), LIVE_LINES),
            rejected_today,
        })
    }

    /// Re-reads only `live.log`.
    pub fn reload_live(&mut self) {
        if let Some(h) = &self.hall {
            self.data.live = tail(&h.live_log(), LIVE_LINES);
        }
    }
}

impl UsageData {
    fn load(ix: &Index, now: DateTime<Utc>) -> crate::error::Result<UsageData> {
        let today = start_of_day(now);
        let by_room = ix.usage(UsageBy::Room, Some(today), None)?;
        let loop_today = by_room.iter().filter(|r| r.key != "workshop").fold(
            crate::budget::Spent::default(),
            |mut s, r| {
                s.tokens += r.weighted_tokens();
                s.usd += r.cost_usd;
                s
            },
        );
        let first_day = (now - Duration::days(USAGE_DAYS - 1))
            .format("%Y-%m-%d")
            .to_string();
        let mut stmt = ix.conn().prepare(
            "SELECT day, COUNT(*),
                    COALESCE(SUM(COALESCE(input,0) + COALESCE(output,0) + COALESCE(cache_read,0)
                                 + COALESCE(cache_write,0)), 0),
                    COALESCE(SUM(CASE WHEN COALESCE(room,'') != 'workshop'
                        THEN COALESCE(input,0) + COALESCE(output,0)
                             + COALESCE(cache_read,0) * 0.1
                             + COALESCE(cache_write,0) * 1.25 ELSE 0 END), 0),
                    COALESCE(SUM(cost_usd), 0.0),
                    COALESCE(SUM(CASE WHEN COALESCE(room,'') != 'workshop'
                        THEN COALESCE(cost_usd,0) ELSE 0 END), 0.0)
             FROM usage WHERE day >= ?1 GROUP BY day ORDER BY day DESC",
        )?;
        let by_day = stmt
            .query_map(params![first_day], |r| {
                Ok(DayRow {
                    day: r.get(0)?,
                    records: r.get::<_, i64>(1)? as u64,
                    tokens: r.get::<_, i64>(2)? as u64,
                    loop_tokens: r.get::<_, f64>(3)?.max(0.0).round() as u64,
                    cost_usd: r.get(4)?,
                    loop_usd: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut granted = BTreeMap::new();
        for it in ix.items(true)? {
            let g = crate::budget::item_granted(ix, it.id);
            if g > 0 {
                granted.insert(it.id.to_string(), g);
            }
        }
        Ok(UsageData {
            granted,
            by_model: ix.usage(UsageBy::Model, Some(today), None)?,
            by_item: ix.usage(UsageBy::Item, None, None)?,
            by_room,
            by_day,
            loop_today,
        })
    }
}

/// Midnight UTC: budgets reset then (ADR 0016).
pub fn start_of_day(now: DateTime<Utc>) -> DateTime<Utc> {
    now.date_naive()
        .and_hms_opt(0, 0, 0)
        .map(|t| t.and_utc())
        .unwrap_or(now)
}

/// The rooms usage is booked to, with their names in the building.
pub fn room_label(key: &str) -> &str {
    match key {
        "workshop" => "Workshop",
        "assay" => "Assay Office",
        "repair" => "Repair Shop",
        "front" => "Front Desk",
        "library" => "Library",
        "" => "(none)",
        k => k,
    }
}

/// The role whose `[budget.<role>] daily` caps a room.
pub fn room_role(key: &str) -> Option<Role> {
    match key {
        "assay" => Some(Role::Reviewer),
        "repair" => Some(Role::Fixer),
        "front" => Some(Role::Intake),
        "library" => Some(Role::Librarian),
        _ => None,
    }
}

/// The last `n` lines of a file, reading at most its last 128 KiB.
pub fn tail(path: &Path, n: usize) -> Vec<String> {
    let Ok(mut f) = fs::File::open(path) else {
        return Vec::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(128 * 1024);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        // The first line is probably cut.
        lines.remove(0);
    }
    let skip = lines.len().saturating_sub(n);
    lines[skip..].iter().map(|l| l.to_string()).collect()
}

/// The repositories to show: the registry, plus the one you're standing
/// in when it isn't registered (or there is no registry). The second value
/// is the index of the repo containing `cwd`, if any.
pub fn discover(cwd: &Path, registry: &Path) -> (Vec<(String, PathBuf)>, Option<usize>) {
    let reg: Registry = fs::read_to_string(registry)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for r in reg.repos {
        let p = PathBuf::from(&r.path);
        let key = paths::display(&paths::normalize(&p));
        if seen.contains_key(&key) {
            continue;
        }
        seen.insert(key, out.len());
        out.push((r.name, p));
    }
    let here = Repo::discover(cwd).ok().map(|repo| {
        let key = paths::display(&paths::normalize(&repo.main_root));
        match seen.get(&key) {
            Some(i) => *i,
            None => {
                out.insert(0, (repo.name.clone(), repo.main_root.clone()));
                0
            }
        }
    });
    // Inserting at 0 shifts the registry's indices, but `here` is then 0.
    (out, here)
}

/// Items in the order the history view lists them: newest end first.
pub fn history(items: &[Item]) -> Vec<&Item> {
    let mut v: Vec<&Item> = items
        .iter()
        .filter(|i| matches!(i.state, State::Closed | State::Dropped))
        .collect();
    v.sort_by(|a, b| b.state_since.cmp(&a.state_since).then(b.id.cmp(&a.id)));
    v
}
