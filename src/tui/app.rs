//! The app's state and how keys change it. Nothing here touches the
//! terminal, so tests drive [`App`] directly and render it with
//! ratatui's `TestBackend`.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::data::{self, RepoSlot, RepoStatus};
use crate::cli::{DropArgs, PullBackArgs, ReadyArgs, RetryArgs};
use crate::commands::flow;
use crate::commands::status::{InboxDetail, explain};
use crate::daemon::state::DaemonState;
use crate::error::Result;
use crate::event::{self, Body, Event};
use crate::findings::Ledger;
use crate::git;
use crate::index::InboxEntry;
use crate::item::{Item, ItemId, State};
use crate::output::Response;
use crate::repo::Repo;

/// The board's columns, left to right.
pub const COLUMNS: [&str; 5] = [
    "Development",
    "Awaiting you",
    "Review loop",
    "Delivery",
    "Closed today",
];

/// The column an item is drawn in. A shipped PR stays in Delivery, whatever
/// day it shipped, while the Delivery office watches it (ADR 0030); once it
/// merges or closes, the item is just closed.
pub fn column(item: &Item) -> usize {
    match item.state {
        State::Development => 0,
        State::AwaitingYou | State::Escalated => 1,
        State::Checks | State::Review | State::Fixing | State::Hallmarked => 2,
        State::Closed if crate::delivery::is_watched(item) => 3,
        State::Closed | State::Dropped => 4,
    }
}

/// The most lines a diff view keeps.
const DIFF_LINES: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Inbox, board, the selected item's journal and findings.
    Board,
    /// The selected item's journal, full height, with the replay scrubber.
    Journal,
    /// Every finding of the selected item.
    Findings,
    /// Closed and dropped items.
    History,
    /// The daemon and the tail of `live.log`.
    Live,
    Usage,
    Repos,
    /// `git diff <base>...<sha>` at the selected round.
    Diff,
    /// Your office: every inbox entry, one explained in full, with its
    /// actions.
    Inbox,
    /// The reply of a command run from the app (`hall log`, …).
    Output,
}

impl View {
    pub fn name(self) -> &'static str {
        match self {
            View::Board => "board",
            View::Journal => "journal",
            View::Findings => "findings",
            View::History => "history",
            View::Live => "live",
            View::Usage => "usage",
            View::Repos => "repos",
            View::Diff => "diff",
            View::Inbox => "your office",
            View::Output => "output",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageGroup {
    Room,
    Item,
    Model,
    Day,
}

impl UsageGroup {
    pub fn name(self) -> &'static str {
        match self {
            UsageGroup::Room => "room",
            UsageGroup::Item => "item",
            UsageGroup::Model => "model",
            UsageGroup::Day => "day",
        }
    }
}

/// A line of text being typed at the bottom of the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// `/`: filters items by ID, title and state as you type.
    Filter,
    /// `x`: the reason an item is dropped.
    DropReason(ItemId),
    /// `X` in your office: the reason every item waiting there is dropped.
    DropAll(Vec<ItemId>),
    /// An action with a placeholder (`"<why>"`, `"<answer>"`): the text
    /// that replaces `args[at]` before `args` runs.
    Fill { args: Vec<String>, at: usize },
    /// `g`: the reason a held or escalated item goes on.
    RetryReason(ItemId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub text: String,
    pub error: bool,
}

/// Something the app asks its runner to do outside the terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    Quit,
    /// Open a folder or URL with the system opener; show `fallback` if
    /// that fails.
    Open {
        target: String,
        fallback: String,
    },
    /// Hand the terminal to a program until it exits (`claude --resume`),
    /// then come back.
    Run {
        program: String,
        args: Vec<String>,
        cwd: PathBuf,
    },
}

/// The selected item's journal, folded now and at the replay cursor.
#[derive(Debug, Clone)]
pub struct Selection {
    pub id: ItemId,
    pub events: Vec<Event>,
    pub ledger: Ledger,
    /// The item and its findings folded up to the replay cursor.
    pub at: Option<(Item, Ledger)>,
}

/// A scrollable page of text: a diff, or a command's reply.
#[derive(Debug, Clone)]
pub struct Diff {
    pub title: String,
    pub lines: Vec<String>,
    pub scroll: usize,
}

/// Rendering switches for snapshot tests: fixed placeholders for clock
/// times and ages, and a path prefix shown as `[root]`.
#[derive(Debug, Clone, Default)]
pub struct Redact {
    pub times: bool,
    pub root: Option<String>,
}

/// One board row: an item, and whether it is drawn as a child of the row
/// above.
#[derive(Debug, Clone)]
pub struct BoardRow {
    pub item: Item,
    pub child: bool,
}

pub struct App {
    pub repos: Vec<RepoSlot>,
    pub active: usize,
    pub view: View,
    /// Where `esc` returns from the diff view.
    pub back: View,
    pub help: bool,
    pub input: Option<Input>,
    pub buffer: String,
    pub filter: String,
    pub selected: Option<ItemId>,
    pub selection: Option<Selection>,
    /// The replay position in the selected item's journal; `None` is now.
    pub cursor: Option<usize>,
    pub finding_sel: usize,
    pub finding_open: bool,
    pub history_sel: usize,
    pub repo_sel: usize,
    pub usage_by: UsageGroup,
    pub diff: Option<Diff>,
    /// Lines scrolled up from the bottom of the Live view.
    pub live_scroll: usize,
    /// The selected inbox entry in your office.
    pub inbox_sel: usize,
    /// The selected entry explained (`hall inbox <n>`); `None` until it is
    /// read, or when it can't be.
    pub inbox_detail: Option<InboxDetail>,
    /// Lines scrolled down in the explained entry.
    pub inbox_scroll: usize,
    pub status: Option<Status>,
    pub daemon: Option<DaemonState>,
    pub now: DateTime<Utc>,
    pub redact: Redact,
}

impl App {
    /// Loads every repo in `registry` (`repos.json`) plus the one at `cwd`.
    /// The repo at `cwd` starts active. Never fails: unreadable repos show
    /// their problem instead.
    pub fn load(cwd: &Path, registry: &Path) -> App {
        let now = Utc::now();
        let (list, here) = data::discover(cwd, registry);
        let repos = list
            .into_iter()
            .map(|(name, path)| RepoSlot::open(name, path, now))
            .collect();
        let mut app = App {
            repos,
            active: here.unwrap_or(0),
            view: View::Board,
            back: View::Board,
            help: false,
            input: None,
            buffer: String::new(),
            filter: String::new(),
            selected: None,
            selection: None,
            cursor: None,
            finding_sel: 0,
            finding_open: false,
            history_sel: 0,
            repo_sel: 0,
            usage_by: UsageGroup::Room,
            diff: None,
            live_scroll: 0,
            inbox_sel: 0,
            inbox_detail: None,
            inbox_scroll: 0,
            status: None,
            daemon: None,
            now,
            redact: Redact::default(),
        };
        app.repo_sel = app.active;
        app.fix_selection();
        app
    }

    pub fn repo(&self) -> Option<&RepoSlot> {
        self.repos.get(self.active)
    }

    /// The selected item as it is now.
    pub fn item(&self) -> Option<&Item> {
        let id = self.selected?;
        self.repo()?.data.item(id)
    }

    /// The selected item at the replay cursor (or now).
    pub fn shown_item(&self) -> Option<&Item> {
        if let Some(s) = &self.selection
            && let Some((it, _)) = &s.at
            && Some(s.id) == self.selected
        {
            return Some(it);
        }
        self.item()
    }

    /// The selected item's findings at the replay cursor (or now).
    pub fn shown_ledger(&self) -> Option<&Ledger> {
        let s = self.selection.as_ref()?;
        Some(s.at.as_ref().map(|(_, l)| l).unwrap_or(&s.ledger))
    }

    /// Events up to the replay cursor, and the cursor's index.
    pub fn shown_events(&self) -> (&[Event], Option<usize>) {
        match &self.selection {
            Some(s) => match self.cursor {
                Some(c) if c < s.events.len() => (&s.events[..=c], Some(c)),
                _ => (&s.events[..], None),
            },
            None => (&[], None),
        }
    }

    /// The local date of `t`.
    fn local_day(t: DateTime<Utc>) -> chrono::NaiveDate {
        t.with_timezone(&Local).date_naive()
    }

    pub fn matches(&self, item: &Item) -> bool {
        if self.filter.is_empty() {
            return true;
        }
        let f = self.filter.to_lowercase();
        item.id.to_string().to_lowercase().contains(&f)
            || item.title.to_lowercase().contains(&f)
            || item.state.as_str().contains(&f)
            || item.state.label().to_lowercase().contains(&f)
    }

    /// The board's columns, with the selected item shown at the replay
    /// cursor and parallel parts grouped under their parent.
    pub fn board(&self) -> [Vec<BoardRow>; 5] {
        let mut cols: [Vec<BoardRow>; 5] = Default::default();
        let Some(repo) = self.repo() else {
            return cols;
        };
        let today = Self::local_day(self.now);
        let shown = self.shown_item().cloned();
        for it in &repo.data.items {
            let it = match &shown {
                Some(s) if s.id == it.id => s,
                _ => it,
            };
            if !self.matches(it) {
                continue;
            }
            let c = column(it);
            if c == COLUMNS.len() - 1 && Self::local_day(it.state_since) != today {
                continue;
            }
            cols[c].push(BoardRow {
                item: it.clone(),
                child: false,
            });
        }
        for col in &mut cols {
            let present: Vec<ItemId> = col.iter().map(|r| r.item.id).collect();
            for r in col.iter_mut() {
                r.child = r.item.parent.is_some_and(|p| present.contains(&p));
            }
            // Parents first, each followed by its children.
            col.sort_by_key(|r| {
                let group = if r.child {
                    r.item.parent.map(|p| p.0).unwrap_or(r.item.id.0)
                } else {
                    r.item.id.0
                };
                (group, r.child, r.item.id.0)
            });
        }
        // Closed today: newest first, each part still under its parent.
        let closed = &mut cols[COLUMNS.len() - 1];
        let when: std::collections::HashMap<ItemId, chrono::DateTime<chrono::Utc>> = closed
            .iter()
            .map(|r| (r.item.id, r.item.state_since))
            .collect();
        closed.sort_by_key(|r| {
            let group = match (r.child, r.item.parent) {
                (true, Some(p)) => when.get(&p).copied().unwrap_or(r.item.state_since),
                _ => r.item.state_since,
            };
            let parent = if r.child {
                r.item.parent
            } else {
                Some(r.item.id)
            };
            (
                std::cmp::Reverse(group),
                parent.map(|p| p.0),
                r.child,
                std::cmp::Reverse(r.item.state_since),
            )
        });
        cols
    }

    /// Board items in the order ↑↓ walks them.
    pub fn board_order(&self) -> Vec<ItemId> {
        self.board()
            .into_iter()
            .flat_map(|c| c.into_iter().map(|r| r.item.id))
            .collect()
    }

    /// History rows (closed and dropped), filtered.
    pub fn history(&self) -> Vec<&Item> {
        let Some(repo) = self.repo() else {
            return vec![];
        };
        let f = self.filter.to_lowercase();
        data::history(&repo.data.items)
            .into_iter()
            .filter(|it| {
                if f.is_empty() || self.matches(it) {
                    return true;
                }
                // Also by lens, finding title and drop reason.
                it.drop_reason
                    .as_deref()
                    .is_some_and(|r| r.to_lowercase().contains(&f))
                    || repo.data.findings.iter().any(|x| {
                        x.item == it.id
                            && (x.lens.contains(&f)
                                || x.title.to_lowercase().contains(&f)
                                || x.severity == f)
                    })
            })
            .collect()
    }

    /// The inbox of the active repo, plus the daemon if it stopped.
    pub fn inbox(&self) -> Vec<InboxEntry> {
        let mut out: Vec<InboxEntry> = self
            .repo()
            .map(|r| r.data.inbox.clone())
            .unwrap_or_default();
        if let Some(st) = &self.daemon
            && let Some(since) = st.stopped_unexpectedly(self.now)
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
                answer_with: vec!["hall daemon start".into()],
                notice: false,
            });
        }
        out
    }

    /// running, paused or stopped.
    pub fn daemon_label(&self) -> &'static str {
        match &self.daemon {
            Some(d) if d.alive(self.now) && d.paused.is_some() => "paused",
            Some(d) if d.alive(self.now) => "running",
            _ => "stopped",
        }
    }

    // --- refresh ----------------------------------------------------------

    /// Re-reads one repo (after its journal changed).
    pub fn reload_repo(&mut self, i: usize) {
        let now = self.now;
        if let Some(r) = self.repos.get_mut(i) {
            r.reload(now);
        }
        if i == self.active {
            self.fix_selection();
            self.reload_selection();
            if self.view == View::Inbox {
                self.load_inbox_detail();
            }
        }
    }

    pub fn reload_active(&mut self) {
        self.reload_repo(self.active);
    }

    /// Keeps the selection on an item that exists, preferring the board.
    fn fix_selection(&mut self) {
        let exists = self
            .selected
            .is_some_and(|id| self.repo().is_some_and(|r| r.data.item(id).is_some()));
        if !exists {
            let first = self.board_order().first().copied().or_else(|| {
                self.repo()
                    .and_then(|r| r.data.items.iter().rev().find(|i| !i.state.is_terminal()))
                    .map(|i| i.id)
            });
            self.select(first);
        } else if self.selection.as_ref().map(|s| s.id) != self.selected {
            self.reload_selection();
        }
    }

    fn select(&mut self, id: Option<ItemId>) {
        if self.selected != id {
            self.cursor = None;
            self.finding_sel = 0;
            self.finding_open = false;
        }
        self.selected = id;
        self.reload_selection();
    }

    fn reload_selection(&mut self) {
        let Some(id) = self.selected else {
            self.selection = None;
            return;
        };
        let events = self
            .repo()
            .and_then(|r| r.hall())
            .and_then(|h| h.read_item(id).ok())
            .unwrap_or_default();
        if let Some(c) = self.cursor
            && c + 1 >= events.len()
        {
            self.cursor = None;
        }
        let ledger = Ledger::from_events(&events);
        self.selection = Some(Selection {
            id,
            events,
            ledger,
            at: None,
        });
        self.refold();
    }

    /// Folds the selected item up to the replay cursor.
    fn refold(&mut self) {
        let cursor = self.cursor;
        if let Some(s) = &mut self.selection {
            s.at = cursor.and_then(|c| {
                let evs = &s.events[..=c.min(s.events.len().saturating_sub(1))];
                let item = event::fold(s.id, evs).ok()?;
                Some((item, Ledger::from_events(evs)))
            });
        }
    }

    // --- keys -------------------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        if key.kind == KeyEventKind::Release {
            return Effect::None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Effect::Quit;
        }
        if self.input.is_some() {
            self.input_key(key);
            return Effect::None;
        }
        if self.help {
            self.help = false;
            return match key.code {
                KeyCode::Char('q') => Effect::Quit,
                _ => Effect::None,
            };
        }
        match key.code {
            KeyCode::Char('q') => return Effect::Quit,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Tab => self.switch_repo(1),
            KeyCode::BackTab => self.switch_repo(self.repos.len().saturating_sub(1)),
            KeyCode::Esc => self.escape(),
            KeyCode::Char('/') => {
                self.input = Some(Input::Filter);
                self.buffer = self.filter.clone();
            }
            _ => return self.view_key(key),
        }
        Effect::None
    }

    fn escape(&mut self) {
        match self.view {
            View::Diff | View::Output => {
                self.diff = None;
                self.view = self.back;
                if self.view == View::Inbox {
                    self.load_inbox_detail();
                }
            }
            View::Board if self.cursor.is_some() => {
                self.cursor = None;
                self.refold();
            }
            View::Board if !self.filter.is_empty() => self.filter.clear(),
            View::Findings if self.finding_open => self.finding_open = false,
            _ => self.view = View::Board,
        }
        self.status = None;
    }

    fn view_key(&mut self, key: KeyEvent) -> Effect {
        let v = self.view;
        match (v, key.code) {
            // Usage groupings come before the board's letters.
            (View::Usage, KeyCode::Char('b')) => self.usage_by = UsageGroup::Room,
            (View::Usage, KeyCode::Char('i')) => self.usage_by = UsageGroup::Item,
            (View::Usage, KeyCode::Char('m')) => self.usage_by = UsageGroup::Model,
            (View::Usage, KeyCode::Char('d')) => self.usage_by = UsageGroup::Day,

            (View::Diff | View::Output, k) => self.diff_key(k),
            (View::Inbox, KeyCode::Up) => self.move_inbox(-1),
            (View::Inbox, KeyCode::Down) => self.move_inbox(1),
            (View::Inbox, KeyCode::PageDown) => self.inbox_scroll += 5,
            (View::Inbox, KeyCode::PageUp) => {
                self.inbox_scroll = self.inbox_scroll.saturating_sub(5)
            }
            (View::Inbox, KeyCode::Enter) => match self.inbox_entry().and_then(|e| e.item) {
                Some(id) if self.repo().is_some_and(|r| r.data.item(id).is_some()) => {
                    self.select(Some(id));
                    self.view = View::Journal;
                }
                _ => self.err("This entry isn't about an item."),
            },
            (View::Inbox, KeyCode::Char(c @ '1'..='9')) => {
                return self.run_action(c as usize - '1' as usize);
            }
            // Dismiss the entry, or clear the whole inbox: hidden until
            // each one changes.
            (View::Inbox, KeyCode::Char('D')) => match self.inbox_actions().last() {
                Some((cmd, _)) if cmd.ends_with("--dismiss") => {
                    let cmd = cmd.clone();
                    return self.run_command(&cmd);
                }
                _ => self.err("This entry can't be dismissed."),
            },
            (View::Inbox, KeyCode::Char('C')) => {
                if self.inbox().is_empty() {
                    self.info("Nothing needs you.");
                } else {
                    return self.run_command("hall inbox --clear");
                }
            }
            (View::Inbox, KeyCode::Char('x')) => match self.entry_item() {
                Some(id) => {
                    self.input = Some(Input::DropReason(id));
                    self.buffer.clear();
                }
                None => self.err("This entry isn't about an item that can be dropped."),
            },
            (View::Inbox, KeyCode::Char('X')) => {
                let ids = self.office_items();
                if ids.is_empty() {
                    self.err("Nothing in your office is an item that can be dropped.");
                } else {
                    self.input = Some(Input::DropAll(ids));
                    self.buffer.clear();
                }
            }
            (View::Repos, KeyCode::Up) => self.repo_sel = self.repo_sel.saturating_sub(1),
            (View::Repos, KeyCode::Down) => {
                self.repo_sel = (self.repo_sel + 1).min(self.repos.len().saturating_sub(1))
            }
            (View::Repos, KeyCode::Enter) => {
                let to = self.repo_sel;
                self.activate(to);
                self.view = View::Board;
            }
            (View::History, KeyCode::Up) => self.history_sel = self.history_sel.saturating_sub(1),
            (View::History, KeyCode::Down) => {
                self.history_sel =
                    (self.history_sel + 1).min(self.history().len().saturating_sub(1))
            }
            (View::History, KeyCode::Enter) => {
                if let Some(id) = self.history().get(self.history_sel).map(|i| i.id) {
                    self.select(Some(id));
                    self.view = View::Journal;
                }
            }
            (View::Live, KeyCode::Up) => self.live_scroll += 1,
            (View::Live, KeyCode::Down) => self.live_scroll = self.live_scroll.saturating_sub(1),
            (View::Live, KeyCode::PageUp) => self.live_scroll += 10,
            (View::Live, KeyCode::PageDown) => {
                self.live_scroll = self.live_scroll.saturating_sub(10)
            }
            (View::Findings, KeyCode::Up) => self.finding_sel = self.finding_sel.saturating_sub(1),
            (View::Findings, KeyCode::Down) => {
                let n = self.shown_ledger().map(|l| l.findings.len()).unwrap_or(0);
                self.finding_sel = (self.finding_sel + 1).min(n.saturating_sub(1));
            }
            (View::Findings, KeyCode::Enter) => self.finding_open = !self.finding_open,

            (_, KeyCode::Up) => self.move_selection(-1),
            (_, KeyCode::Down) => self.move_selection(1),
            (_, KeyCode::Left) => self.step(-1),
            (_, KeyCode::Right) => self.step(1),
            (_, KeyCode::Home) if self.selection.as_ref().is_some_and(|s| s.events.len() > 1) => {
                self.cursor = Some(0);
                self.refold();
            }
            (_, KeyCode::End) => {
                self.cursor = None;
                self.refold();
            }
            (_, KeyCode::Enter) => self.view = View::Journal,
            (_, KeyCode::Char('f')) => {
                self.view = View::Findings;
                self.finding_sel = 0;
            }
            (_, KeyCode::Char('h')) => {
                self.view = View::History;
                self.history_sel = 0;
            }
            (_, KeyCode::Char('i')) => self.open_inbox(),
            (_, KeyCode::Char('u')) => self.view = View::Usage,
            (_, KeyCode::Char('l')) => {
                self.view = View::Live;
                self.live_scroll = 0;
            }
            (_, KeyCode::Char('R')) => {
                self.view = View::Repos;
                self.repo_sel = self.active;
            }
            (_, KeyCode::Char('b')) => self.view = View::Board,
            (_, KeyCode::Char('d')) => self.open_diff(),
            (_, KeyCode::Char('r')) => self.ready(),
            (_, KeyCode::Char('p')) => self.pull_back(),
            (_, KeyCode::Char('g')) => match self.selected {
                Some(id) => {
                    self.input = Some(Input::RetryReason(id));
                    self.buffer.clear();
                }
                None => self.err("No item selected."),
            },
            (_, KeyCode::Char('x')) => match self.selected {
                Some(id) => {
                    self.input = Some(Input::DropReason(id));
                    self.buffer.clear();
                }
                None => self.err("No item selected."),
            },
            (_, KeyCode::Char('w')) => return self.open_worktree(),
            (_, KeyCode::Char('o')) => return self.open_pr(),
            _ => {}
        }
        Effect::None
    }

    fn diff_key(&mut self, k: KeyCode) {
        let Some(d) = &mut self.diff else { return };
        let max = d.lines.len().saturating_sub(1);
        d.scroll = match k {
            KeyCode::Up | KeyCode::Char('k') => d.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => (d.scroll + 1).min(max),
            KeyCode::PageUp => d.scroll.saturating_sub(20),
            KeyCode::PageDown | KeyCode::Char(' ') => (d.scroll + 20).min(max),
            KeyCode::Home => 0,
            KeyCode::End => max,
            _ => d.scroll,
        };
    }

    fn input_key(&mut self, key: KeyEvent) {
        let Some(input) = self.input.clone() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                match input {
                    Input::Filter => {
                        self.filter.clear();
                        self.fix_board_selection();
                    }
                    Input::DropReason(_) | Input::DropAll(_) => self.info("Drop cancelled."),
                    Input::RetryReason(_) => self.info("Cancelled."),
                    Input::Fill { .. } => self.info("Not run."),
                }
                self.input = None;
                self.buffer.clear();
            }
            KeyCode::Enter => {
                self.input = None;
                let text = std::mem::take(&mut self.buffer);
                match input {
                    Input::Filter => {
                        self.filter = text.trim().to_string();
                        self.fix_board_selection();
                    }
                    Input::DropReason(id) => self.drop_item(id, text.trim()),
                    Input::DropAll(ids) => self.drop_all(&ids, text.trim()),
                    Input::Fill { mut args, at } => {
                        if text.trim().is_empty() {
                            self.err("Not run: the text is required.");
                        } else {
                            args[at] = text.trim().to_string();
                            self.exec(args);
                        }
                    }
                    Input::RetryReason(id) => self.retry(id, text.trim()),
                }
            }
            KeyCode::Backspace => {
                self.buffer.pop();
                if input == Input::Filter {
                    self.filter = self.buffer.clone();
                    self.fix_board_selection();
                }
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.buffer.push(c);
                if input == Input::Filter {
                    self.filter = self.buffer.clone();
                    self.fix_board_selection();
                }
            }
            _ => {}
        }
    }

    /// After the filter changes: keep the selection on a visible item.
    fn fix_board_selection(&mut self) {
        let order = self.board_order();
        if self.selected.is_some_and(|id| order.contains(&id)) {
            return;
        }
        if let Some(first) = order.first().copied() {
            self.select(Some(first));
        }
    }

    fn move_selection(&mut self, by: i32) {
        let order = self.board_order();
        if order.is_empty() {
            return;
        }
        let at = self
            .selected
            .and_then(|id| order.iter().position(|x| *x == id));
        let to = match at {
            Some(i) if by < 0 => i.saturating_sub(1),
            Some(i) => (i + 1).min(order.len() - 1),
            None => 0,
        };
        self.select(Some(order[to]));
    }

    /// Steps the replay cursor; past the last event it returns to now.
    pub fn step(&mut self, by: i32) {
        let n = self.selection.as_ref().map(|s| s.events.len()).unwrap_or(0);
        if n == 0 {
            return;
        }
        let cur = self.cursor.unwrap_or(n - 1);
        self.cursor = if by < 0 {
            Some(cur.saturating_sub(1))
        } else if cur + 1 >= n - 1 {
            None
        } else {
            Some(cur + 1)
        };
        if self.cursor == Some(n - 1) {
            self.cursor = None;
        }
        self.refold();
    }

    /// The replay position, 1-based, and the number of events.
    pub fn replay_position(&self) -> (usize, usize) {
        let n = self.selection.as_ref().map(|s| s.events.len()).unwrap_or(0);
        (self.cursor.map(|c| c + 1).unwrap_or(n), n)
    }

    fn switch_repo(&mut self, by: usize) {
        if self.repos.is_empty() {
            return;
        }
        let to = (self.active + by) % self.repos.len();
        self.activate(to);
    }

    fn activate(&mut self, i: usize) {
        if i >= self.repos.len() {
            return;
        }
        if i != self.active {
            self.active = i;
            self.selected = None;
            self.selection = None;
            self.cursor = None;
            self.diff = None;
            self.history_sel = 0;
            if self.view == View::Diff {
                self.view = View::Board;
            }
            self.reload_active();
        }
        self.repo_sel = i;
        let name = self.repos[i].name.clone();
        self.info(&format!("{name} ({} of {} repos)", i + 1, self.repos.len()));
    }

    // --- actions ------------------------------------------------------------

    fn info(&mut self, text: &str) {
        self.status = Some(Status {
            text: text.into(),
            error: false,
        });
    }

    fn err(&mut self, text: &str) {
        self.status = Some(Status {
            text: text.into(),
            error: true,
        });
    }

    /// The selected item and the directory commands run from.
    fn target(&mut self) -> Option<(ItemId, PathBuf)> {
        let Some(repo) = self.repo() else {
            self.err("No repository.");
            return None;
        };
        if repo.data.status != RepoStatus::Ready {
            let name = repo.name.clone();
            self.err(&format!(
                "{name} doesn't use Hallmark yet: run `hall init` there."
            ));
            return None;
        }
        let path = repo.path.clone();
        match self.selected {
            Some(id) => Some((id, path)),
            None => {
                self.err("No item selected.");
                None
            }
        }
    }

    fn report(&mut self, id: ItemId, r: Result<Response>) {
        match r {
            Ok(resp) => self.info(&format!("{id}: {}", note(&resp))),
            Err(e) => self.err(&format!("{id}: {} ({})", e.message, e.code.as_str())),
        }
        self.reload_active();
    }

    /// `r`: freeze the commit and send the item to review (`hall ready`).
    pub fn ready(&mut self) {
        if let Some((id, cwd)) = self.target() {
            let r = flow::ready(
                &cwd,
                ReadyArgs {
                    id: Some(id.to_string()),
                    note: None,
                },
            );
            self.report(id, r);
        }
    }

    /// `p`: back to development (`hall pull-back`).
    pub fn pull_back(&mut self) {
        if let Some((id, cwd)) = self.target() {
            let r = flow::pull_back(
                &cwd,
                PullBackArgs {
                    id: Some(id.to_string()),
                    reason: Some("pulled back from the terminal app".into()),
                },
            );
            self.report(id, r);
        }
    }

    /// `g`: go on (`hall retry`): one more fix round for an escalated item,
    /// or lift a hold. A budget hold also grants the item another
    /// `per_item`; the budget itself is unchanged.
    pub fn retry(&mut self, id: ItemId, reason: &str) {
        if reason.is_empty() {
            self.err("Not retried: a reason is required.");
            return;
        }
        if let Some((_, cwd)) = self.target() {
            let r = flow::retry(
                &cwd,
                RetryArgs {
                    id: Some(id.to_string()),
                    reason: reason.into(),
                    tokens: None,
                },
            );
            self.report(id, r);
        }
    }

    fn drop_item(&mut self, id: ItemId, reason: &str) {
        if reason.is_empty() {
            self.err("Not dropped: a reason is required.");
            return;
        }
        let Some((_, cwd)) = self.target() else {
            return;
        };
        let r = flow::drop(
            &cwd,
            DropArgs {
                id: Some(id.to_string()),
                reason: reason.into(),
                keep_worktree: false,
            },
        );
        self.report(id, r);
    }

    /// `X` in your office: drops every item waiting there, each as
    /// `hall drop` would, with the same reason. One that can't be dropped
    /// (a parent whose parts are still open) is reported and the rest go on.
    fn drop_all(&mut self, ids: &[ItemId], reason: &str) {
        if reason.is_empty() {
            self.err("Not dropped: a reason is required.");
            return;
        }
        let Some(repo) = self.repo() else {
            self.err("No repository.");
            return;
        };
        if repo.data.status != RepoStatus::Ready {
            let name = repo.name.clone();
            self.err(&format!(
                "{name} doesn't use Hallmark yet: run `hall init` there."
            ));
            return;
        }
        let cwd = repo.path.clone();
        let mut dropped = vec![];
        let mut failed = vec![];
        for id in ids {
            let r = flow::drop(
                &cwd,
                DropArgs {
                    id: Some(id.to_string()),
                    reason: reason.into(),
                    keep_worktree: false,
                },
            );
            match r {
                Ok(_) => dropped.push(id.to_string()),
                Err(e) => failed.push(format!("{id}: {} ({})", e.message, e.code.as_str())),
            }
        }
        if failed.is_empty() {
            self.info(&format!(
                "Dropped {} item{}: {}.",
                dropped.len(),
                if dropped.len() == 1 { "" } else { "s" },
                dropped.join(", ")
            ));
        } else {
            self.err(&format!(
                "Dropped {} of {}. {}",
                dropped.len(),
                ids.len(),
                failed.join(" · ")
            ));
        }
        self.reload_active();
    }

    /// `d`: the diff at the selected round, from the worktree if it is
    /// still there, else from the main checkout.
    fn open_diff(&mut self) {
        let Some(item) = self.shown_item().cloned() else {
            self.err("No item selected.");
            return;
        };
        let Some(repo) = self.repo() else { return };
        let rev = item
            .frozen_sha
            .clone()
            .unwrap_or_else(|| item.branch.clone());
        let wt = PathBuf::from(&item.worktree);
        let dir = if wt.is_dir() { wt } else { repo.path.clone() };
        let range = format!("{}...{}", item.base, rev);
        match git::run(&dir, &["diff", "--no-color", "--no-ext-diff", &range]) {
            Ok(out) => {
                let mut lines: Vec<String> = out.lines().map(str::to_string).collect();
                if lines.len() > DIFF_LINES {
                    let more = lines.len() - DIFF_LINES;
                    lines.truncate(DIFF_LINES);
                    lines.push(format!("… {more} more lines"));
                }
                if lines.is_empty() {
                    lines.push("(no changes)".into());
                }
                let round = if item.round > 0 {
                    format!(" · round {}", item.round)
                } else {
                    String::new()
                };
                self.diff = Some(Diff {
                    title: format!(
                        "{} · diff {}...{}{round}",
                        item.id,
                        item.base,
                        event::short(&rev)
                    ),
                    lines,
                    scroll: 0,
                });
                if self.view != View::Diff {
                    self.back = self.view;
                }
                self.view = View::Diff;
            }
            Err(e) => self.err(&format!("{}: {}", item.id, e.message)),
        }
    }

    /// `w`: open the worktree folder; if the opener fails, the runner shows
    /// the `cd` command instead.
    fn open_worktree(&mut self) -> Effect {
        let Some(item) = self.item().cloned() else {
            self.err("No item selected.");
            return Effect::None;
        };
        let wt = Path::new(&item.worktree);
        if !wt.is_dir() {
            self.err(&format!(
                "{}'s worktree is gone: {}",
                item.id,
                self.show_path(&item.worktree)
            ));
            return Effect::None;
        }
        let cd = format!("cd {}", self.show_path(&item.worktree));
        self.info(&format!("Opening {}'s worktree · {cd}", item.id));
        Effect::Open {
            target: item.worktree.clone(),
            fallback: cd,
        }
    }

    /// `o`: open the PR in the browser.
    fn open_pr(&mut self) -> Effect {
        let Some(item) = self.item().cloned() else {
            self.err("No item selected.");
            return Effect::None;
        };
        match &item.pr_url {
            Some(url) => {
                self.info(&format!("Opening {url}"));
                Effect::Open {
                    target: url.clone(),
                    fallback: format!("PR: {url}"),
                }
            }
            None => {
                self.err(&format!("{} has no PR yet.", item.id));
                Effect::None
            }
        }
    }

    /// Called by the runner when an [`Effect::Open`] failed.
    pub fn open_failed(&mut self, fallback: &str) {
        self.info(fallback);
    }

    // --- your office ---------------------------------------------------------

    /// `i`: your office, on the entry last selected.
    fn open_inbox(&mut self) {
        self.view = View::Inbox;
        self.inbox_scroll = 0;
        self.load_inbox_detail();
        self.follow_entry();
    }

    fn move_inbox(&mut self, by: i32) {
        let n = self.inbox().len();
        if n == 0 {
            return;
        }
        self.inbox_sel = if by < 0 {
            self.inbox_sel.saturating_sub(1)
        } else {
            (self.inbox_sel + 1).min(n - 1)
        };
        self.inbox_scroll = 0;
        self.load_inbox_detail();
        self.follow_entry();
    }

    /// Selects the entry's item, so the item keys (`r`, `p`, `d`, …) and
    /// `enter` act on it.
    fn follow_entry(&mut self) {
        if let Some(id) = self.inbox_entry().and_then(|e| e.item)
            && self.repo().is_some_and(|r| r.data.item(id).is_some())
        {
            self.select(Some(id));
        }
    }

    /// The selected inbox entry.
    pub fn inbox_entry(&self) -> Option<InboxEntry> {
        self.inbox().get(self.inbox_sel).cloned()
    }

    /// The selected entry's item, if it is one that can be dropped.
    fn entry_item(&self) -> Option<ItemId> {
        let id = self.inbox_entry()?.item?;
        let it = self.repo()?.data.item(id)?;
        (!it.state.is_terminal()).then_some(id)
    }

    /// Every item waiting in your office that can be dropped, once each,
    /// newest first: a parallel part is always newer than its parent, so
    /// parts are dropped before the parent that waits on them.
    pub fn office_items(&self) -> Vec<ItemId> {
        let Some(repo) = self.repo() else {
            return vec![];
        };
        let mut ids: Vec<ItemId> = self
            .inbox()
            .iter()
            .filter_map(|e| e.item)
            .filter(|id| {
                repo.data
                    .item(*id)
                    .is_some_and(|it| !it.state.is_terminal())
            })
            .collect();
        ids.sort_by_key(|id| std::cmp::Reverse(id.0));
        ids.dedup();
        ids
    }

    /// Explains the selected entry, as `hall inbox <n>` does.
    pub fn load_inbox_detail(&mut self) {
        let n = self.inbox().len();
        self.inbox_sel = self.inbox_sel.min(n.saturating_sub(1));
        let number = self.inbox_sel + 1;
        self.inbox_detail = self.inbox_entry().and_then(|e| {
            let repo = Repo::discover(&self.repo()?.path).ok()?;
            let ix = repo.index().ok()?;
            explain(&repo, &ix, e, number).ok()
        });
    }

    /// The selected entry's actions: the command, and what it does.
    pub fn inbox_actions(&self) -> Vec<(String, String)> {
        match (&self.inbox_detail, self.inbox_entry()) {
            (Some(d), Some(e)) if d.entry.kind == e.kind && d.entry.item == e.item => d
                .actions
                .iter()
                .map(|a| (a.command.clone(), a.does.clone()))
                .collect(),
            (_, Some(e)) => e
                .answer_with
                .iter()
                .map(|c| (c.clone(), String::new()))
                .collect(),
            _ => vec![],
        }
    }

    /// `1`–`9`: runs the selected entry's action.
    fn run_action(&mut self, n: usize) -> Effect {
        match self.inbox_actions().get(n) {
            Some((cmd, _)) => {
                let cmd = cmd.clone();
                self.run_command(&cmd)
            }
            None => {
                self.err(&format!("There is no action {}.", n + 1));
                Effect::None
            }
        }
    }

    /// Runs an action's command: `hall …` in-process (asking first for any
    /// `<placeholder>`), `claude …` with the terminal handed over, and
    /// anything else is shown for you to do.
    pub fn run_command(&mut self, cmd: &str) -> Effect {
        let Some(args) = shlex::split(cmd).filter(|a| !a.is_empty()) else {
            self.err(&format!("Can't read the command: {cmd}"));
            return Effect::None;
        };
        match args[0].as_str() {
            "hall" if args.get(1).is_some_and(|a| a == "watch") => {
                self.info(&format!(
                    "`{cmd}` follows the item live: run it in a terminal."
                ));
            }
            "hall" => match args.iter().position(|a| placeholder(a).is_some()) {
                Some(at) => {
                    self.input = Some(Input::Fill { args, at });
                    self.buffer.clear();
                }
                None => self.exec(args),
            },
            "claude" => return self.hand_over(args),
            _ => self.info(&format!("Do this yourself: {cmd}")),
        }
        Effect::None
    }

    /// Runs `hall <args>` in-process. Readers (`hall log`, …) open their
    /// reply in the Output view; the rest show their note.
    fn exec(&mut self, args: Vec<String>) {
        let Some(repo) = self.repo() else {
            self.err("No repository.");
            return;
        };
        if repo.data.status != RepoStatus::Ready {
            let name = repo.name.clone();
            self.err(&format!(
                "{name} doesn't use Hallmark yet: run `hall init` there."
            ));
            return;
        }
        let cwd = repo.path.clone();
        let shown = shlex::try_join(args.iter().map(String::as_str)).unwrap_or(args.join(" "));
        match crate::cli::run_in_app(&args[1..], &cwd) {
            Ok(resp) if reads_only(&args[1..]) => {
                let mut lines: Vec<String> = resp.text.lines().map(str::to_string).collect();
                if lines.is_empty() {
                    lines.push("(nothing)".into());
                }
                if !matches!(self.view, View::Diff | View::Output) {
                    self.back = self.view;
                }
                self.diff = Some(Diff {
                    title: shown,
                    lines,
                    scroll: 0,
                });
                self.view = View::Output;
                self.status = None;
            }
            Ok(resp) => self.info(&format!("{shown}: {}", note(&resp))),
            Err(e) => self.err(&format!("{shown}: {} ({})", e.message, e.code.as_str())),
        }
        self.reload_active();
    }

    /// `claude --resume <session>`: the runner hands it the terminal, in the
    /// item's worktree (where the session started), and comes back after.
    fn hand_over(&mut self, args: Vec<String>) -> Effect {
        let cwd = self
            .item()
            .map(|i| PathBuf::from(&i.worktree))
            .filter(|p| p.is_dir())
            .or_else(|| self.repo().map(|r| r.path.clone()));
        let Some(cwd) = cwd else {
            self.err("No repository.");
            return Effect::None;
        };
        Effect::Run {
            program: args[0].clone(),
            args: args[1..].to_vec(),
            cwd,
        }
    }

    /// Called by the runner when an [`Effect::Run`] is over.
    pub fn ran(&mut self, program: &str, r: std::io::Result<std::process::ExitStatus>) {
        match r {
            Ok(_) => self.info(&format!("Back from {program}.")),
            Err(e) => self.err(&format!("Could not run {program}: {e}")),
        }
        self.reload_active();
    }

    /// Text for display, with the redacted prefix replaced wherever it is.
    pub fn show_text(&self, s: &str) -> String {
        match &self.redact.root {
            Some(root) => s
                .replace(root.as_str(), "[root]")
                .replace(&root.replace('/', "\\"), "[root]"),
            None => s.to_string(),
        }
    }

    // --- formatting helpers shared with the views ---------------------------

    /// A path for display, with the redacted prefix replaced.
    pub fn show_path(&self, p: &str) -> String {
        let p = p.replace('\\', "/");
        match &self.redact.root {
            Some(root) if p.starts_with(root.as_str()) => format!("[root]{}", &p[root.len()..]),
            _ => p,
        }
    }

    /// A clock time, local.
    pub fn clock(&self, t: DateTime<Utc>) -> String {
        if self.redact.times {
            "hh:mm".into()
        } else {
            t.with_timezone(&Local).format("%H:%M").to_string()
        }
    }

    /// A date, local.
    pub fn date(&self, t: DateTime<Utc>) -> String {
        if self.redact.times {
            "yyyy-mm-dd".into()
        } else {
            t.with_timezone(&Local).format("%Y-%m-%d").to_string()
        }
    }

    /// "now", "12m", "3h 05m", "2d".
    pub fn age(&self, t: DateTime<Utc>) -> String {
        if self.redact.times {
            return "–".into();
        }
        let s = (self.now - t).num_seconds().max(0);
        match s {
            s if s < 60 => "now".into(),
            s if s < 3600 => format!("{}m", s / 60),
            s if s < 86_400 => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
            s => format!("{}d", s / 86_400),
        }
    }

    /// The SHA a verdict or fix refers to at the cursor, for titles.
    pub fn round_sha(&self) -> Option<String> {
        self.shown_item()
            .and_then(|i| i.frozen_sha.as_deref())
            .map(|s| event::short(s).to_string())
    }

    /// Tokens the selected repo's reviews used today (title bar).
    pub fn review_tokens_today(&self) -> u64 {
        self.repo()
            .map(|r| {
                r.data
                    .usage
                    .by_room
                    .iter()
                    .filter(|u| u.key == "assay")
                    .map(|u| u.tokens)
                    .sum()
            })
            .unwrap_or(0)
    }

    /// Rejected reviews today in the active repo (title bar).
    pub fn rejected_today(&self) -> u64 {
        self.repo().map(|r| r.data.rejected_today).unwrap_or(0)
    }
}

/// A command's note, or the first line of its reply.
fn note(resp: &Response) -> String {
    resp.value
        .get("note")
        .and_then(|n| n.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| resp.text.lines().next().unwrap_or("Done.").to_string())
}

/// `<why>` → `why`: a word in an action that you fill in.
pub fn placeholder(arg: &str) -> Option<&str> {
    arg.strip_prefix('<')
        .and_then(|a| a.strip_suffix('>'))
        .filter(|a| !a.is_empty())
}

/// Commands whose reply is the point: the app shows it in full.
fn reads_only(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some("status" | "log" | "inbox" | "findings" | "history" | "export" | "usage") => true,
        Some("daemon") => args.get(1).is_some_and(|a| a == "status"),
        _ => false,
    }
}

/// Whether an event's summary is good news, bad news or a decision; used
/// to colour journal lines.
pub fn tone(body: &Body) -> Tone {
    match body {
        Body::Checks { passed: false, .. }
        | Body::Verdict {
            verdict: event::VerdictKind::Changes,
            ..
        }
        | Body::RunFailed { .. }
        | Body::Held { .. }
        | Body::Stuck { .. }
        | Body::Dropped { .. } => Tone::Bad,
        Body::Checks { passed: true, .. }
        | Body::Verdict { .. }
        | Body::Hallmarked { .. }
        | Body::Shipped { .. }
        | Body::Merged { .. } => Tone::Good,
        Body::ProposedReady { .. } | Body::Ready { .. } | Body::Question { .. } => Tone::Decision,
        Body::Usage(_) | Body::Notified { .. } | Body::SessionStarted { .. } => Tone::Dim,
        _ => Tone::Plain,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Good,
    Bad,
    Decision,
    Dim,
    Plain,
}
