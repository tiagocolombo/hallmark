//! The terminal app (Stage 3, ADR 0022): `hall` with no arguments on a
//! terminal.
//!
//! It only reads: the journal, the index, `live.log`, the registry and the
//! daemon state. A file watcher on each repo's `<git-common-dir>/hallmark/`
//! marks the repo stale, and the app re-reads it once the writes settle.
//! The three actions it offers (`r`, `p`, `x`) call the same command
//! functions as `hall ready`, `hall pull-back` and `hall drop`. Closing it
//! never stops work.
//!
//! [`App`] holds the state and handles keys; [`ui::draw`] renders it.
//! Neither needs a terminal, so tests drive them with ratatui's
//! `TestBackend`.

pub mod app;
pub mod data;
pub mod ui;

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use chrono::Utc;
use crossterm::event::{self, Event as TermEvent};

pub use app::{App, Effect, Input, UsageGroup, View};

use crate::error::{HallError, Result};
use crate::output::Response;
use crate::repo::user_dir;

/// Quiet time after the last file change before re-reading a repo.
const SETTLE: Duration = Duration::from_millis(250);
/// The longest a busy repo waits for a re-read while it keeps changing.
const MAX_WAIT: Duration = Duration::from_secs(1);
/// The slow tick: clock, daemon state, and a full re-read when the watcher
/// isn't available.
const TICK: Duration = Duration::from_secs(2);

/// Opens the app on the repo at `cwd` (or the first registered one).
pub fn run(cwd: &Path) -> Result<Response> {
    let mut app = App::load(cwd, &user_dir().join("repos.json"));
    app.daemon = crate::daemon::state::load();
    let (tx, rx) = channel::<PathBuf>();
    let watcher = watch(&app, tx);
    // try_init enters raw mode and the alternate screen, and installs a
    // panic hook that restores the terminal before the panic prints.
    let mut terminal = ratatui::try_init().map_err(|e| {
        HallError::internal(format!("could not open the terminal app: {e}"))
            .next(["hall status", "hall inbox"])
    })?;
    let r = event_loop(&mut terminal, &mut app, &rx, watcher.is_some());
    ratatui::restore();
    drop(watcher);
    r?;
    Ok(Response::raw(None))
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    rx: &Receiver<PathBuf>,
    watching: bool,
) -> Result<()> {
    let mut stale: BTreeSet<usize> = BTreeSet::new();
    let mut first_change: Option<Instant> = None;
    let mut last_change = Instant::now();
    let mut last_tick = Instant::now();
    let mut redraw = true;
    let mut waiting = app.inbox().len();
    loop {
        if redraw {
            terminal.draw(|f| ui::draw(f, app))?;
            redraw = false;
        }
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                TermEvent::Key(k) => {
                    match app.handle_key(k) {
                        Effect::Quit => return Ok(()),
                        Effect::Open { target, fallback } => {
                            if open(&target).is_err() {
                                app.open_failed(&fallback);
                            }
                        }
                        Effect::None => {}
                    }
                    redraw = true;
                }
                TermEvent::Resize(..) => {
                    terminal.autoresize()?;
                    redraw = true;
                }
                _ => {}
            }
        }
        while let Ok(p) = rx.try_recv() {
            if let Some(i) = repo_for(app, &p) {
                stale.insert(i);
                last_change = Instant::now();
                first_change.get_or_insert(last_change);
            }
        }
        if let Some(first) = first_change
            && (last_change.elapsed() >= SETTLE || first.elapsed() >= MAX_WAIT)
        {
            app.now = Utc::now();
            for i in std::mem::take(&mut stale) {
                app.reload_repo(i);
            }
            first_change = None;
            redraw = true;
        }
        if last_tick.elapsed() >= TICK {
            last_tick = Instant::now();
            app.now = Utc::now();
            app.daemon = crate::daemon::state::load();
            if !watching {
                for i in 0..app.repos.len() {
                    app.reload_repo(i);
                }
            }
            redraw = true;
        }
        // The bell rings when something new lands in your office.
        let now_waiting = app.inbox().len();
        if now_waiting > waiting {
            let _ = std::io::stdout().write_all(b"\x07");
            let _ = std::io::stdout().flush();
        }
        waiting = now_waiting;
    }
}

/// Which repo a changed path belongs to. Writes the app itself causes (the
/// index catching up) and temporary files are ignored.
fn repo_for(app: &App, p: &Path) -> Option<usize> {
    let name = p.file_name()?.to_string_lossy();
    if name.starts_with("index.sqlite") || name.ends_with(".tmp") || name == ".lock" {
        return None;
    }
    let p = crate::paths::normalize(p);
    app.repos.iter().position(|r| {
        r.hall()
            .is_some_and(|h| crate::paths::is_within(&p, &crate::paths::normalize(&h.root)))
    })
}

/// Watches every initialized repo's journal directory. `None` when the
/// platform watcher can't start; the slow tick then re-reads instead.
fn watch(app: &App, tx: std::sync::mpsc::Sender<PathBuf>) -> Option<::notify::RecommendedWatcher> {
    use ::notify::{RecursiveMode, Watcher};
    let mut w = ::notify::recommended_watcher(move |res: ::notify::Result<::notify::Event>| {
        if let Ok(ev) = res {
            for p in ev.paths {
                let _ = tx.send(p);
            }
        }
    })
    .ok()?;
    let mut any = false;
    for r in &app.repos {
        if let Some(h) = r.hall()
            && w.watch(&h.root, RecursiveMode::Recursive).is_ok()
        {
            any = true;
        }
    }
    any.then_some(w)
}

/// Opens a folder or URL with the system opener, without waiting for it.
fn open(target: &str) -> std::io::Result<()> {
    let mut c = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(windows) {
        Command::new("explorer")
    } else {
        Command::new("xdg-open")
    };
    let mut child = c
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Reap it in the background so it never blocks the app.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
