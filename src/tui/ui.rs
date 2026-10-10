//! Drawing. `draw` reads an [`App`] and never changes it, so any state can
//! be rendered into a test buffer.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Clear, Paragraph, Row, Table, TableState};

use super::app::{App, COLUMNS, Input, Tone, UsageGroup, View, tone};
use super::data::{RepoStatus, room_label, room_role};
use crate::budget::{Cap, Spent};
use crate::event::short;
use crate::findings::{Status as FStatus, Tracked};
use crate::index::UsageRow;
use crate::item::{Item, State};
use crate::usage::fmt_tokens;

const BRASS: Color = Color::Yellow;
const OK: Color = Color::Green;
const STOP: Color = Color::Red;
const INFO: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;

fn fg(c: Color) -> Style {
    Style::new().fg(c)
}

fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

fn selected() -> Style {
    Style::new().add_modifier(Modifier::REVERSED)
}

fn pane(title: impl Into<String>) -> Block<'static> {
    Block::bordered()
        .border_style(fg(DIM))
        .title(Span::styled(format!(" {} ", title.into()), bold()))
}

/// Shortens `s` to `max` characters, with an ellipsis.
fn clip(s: &str, max: usize) -> String {
    crate::output::clip(s, max.max(1))
}

pub fn draw(f: &mut Frame, app: &App) {
    let [title, body, status, keys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(f.area());
    title_bar(f, title, app);
    let ready = app
        .repo()
        .is_some_and(|r| r.data.status == RepoStatus::Ready);
    match app.view {
        View::Repos => repos(f, body, app),
        View::Live => live(f, body, app),
        _ if !ready => not_ready(f, body, app),
        View::Board => board_view(f, body, app),
        View::Journal => journal_view(f, body, app),
        View::Findings => findings_view(f, body, app),
        View::History => history(f, body, app),
        View::Usage => usage(f, body, app),
        View::Diff => diff(f, body, app),
    }
    status_line(f, status, app);
    key_bar(f, keys, app);
    if app.help {
        help(f, f.area());
    }
}

// --- chrome ----------------------------------------------------------------

fn title_bar(f: &mut Frame, area: Rect, app: &App) {
    let name = app
        .repo()
        .map(|r| r.name.as_str())
        .unwrap_or("no repository");
    let n = app.repos.len();
    let mut spans = vec![
        Span::styled("hall", bold().fg(BRASS)),
        Span::raw(format!(" · {name} ")),
        Span::styled(
            match n {
                0 => "(no repos)".to_string(),
                1 => "(1 of 1 repo)".to_string(),
                n => format!("({} of {n} repos)", app.active + 1),
            },
            fg(DIM),
        ),
        Span::raw("   daemon "),
    ];
    let d = app.daemon_label();
    let dot = match d {
        "running" => OK,
        "paused" => BRASS,
        _ => STOP,
    };
    spans.push(Span::styled("●", fg(dot)));
    spans.push(Span::raw(format!(" {d}")));
    let waiting = app.inbox().len();
    if waiting > 0 {
        spans.push(Span::styled(format!("   ⚑ {waiting} waiting"), fg(STOP)));
    } else {
        spans.push(Span::styled("   ⚑ 0 waiting", fg(DIM)));
    }
    spans.push(Span::styled(
        format!(
            "   rejected today {} · review tokens {}",
            app.rejected_today(),
            fmt_tokens(app.review_tokens_today())
        ),
        fg(DIM),
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    let view = if app.view == View::Board {
        String::new()
    } else {
        format!("{} · ", app.view.name())
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(view, fg(DIM)),
            Span::raw(app.clock(app.now)),
        ]))
        .right_aligned(),
        area,
    );
}

fn status_line(f: &mut Frame, area: Rect, app: &App) {
    let line = match &app.input {
        Some(Input::Filter) => Line::from(vec![
            Span::styled("/ ", bold().fg(BRASS)),
            Span::raw(app.buffer.clone()),
            Span::styled("▏", fg(BRASS)),
            Span::styled("  enter keep · esc clear", fg(DIM)),
        ]),
        Some(Input::DropReason(id)) => Line::from(vec![
            Span::styled(format!("Drop {id}, reason: "), bold().fg(STOP)),
            Span::raw(app.buffer.clone()),
            Span::styled("▏", fg(BRASS)),
            Span::styled("  enter drop · esc cancel", fg(DIM)),
        ]),
        None => match &app.status {
            Some(s) if s.error => Line::from(Span::styled(s.text.clone(), fg(STOP))),
            Some(s) => Line::from(Span::styled(s.text.clone(), fg(INFO))),
            None if !app.filter.is_empty() => Line::from(Span::styled(
                format!("filter: {} · esc clears", app.filter),
                fg(BRASS),
            )),
            None => Line::default(),
        },
    };
    f.render_widget(Paragraph::new(line), area);
}

fn key_bar(f: &mut Frame, area: Rect, app: &App) {
    let keys: &[(&str, &str)] = match app.view {
        View::Board | View::Journal => &[
            ("↑↓", "item"),
            ("enter", "journal"),
            ("←→", "step"),
            ("f", "findings"),
            ("d", "diff"),
            ("r", "ready"),
            ("p", "pull back"),
            ("x", "drop"),
            ("/", "filter"),
            ("w", "worktree"),
            ("o", "PR"),
            ("h", "history"),
            ("u", "usage"),
            ("?", "help"),
            ("q", "quit"),
        ],
        View::Findings => &[
            ("↑↓", "finding"),
            ("enter", "scenario + fix"),
            ("←→", "step"),
            ("d", "diff"),
            ("r", "ready"),
            ("p", "pull back"),
            ("esc", "board"),
            ("?", "help"),
            ("q", "quit"),
        ],
        View::History => &[
            ("↑↓", "item"),
            ("enter", "journal"),
            ("/", "filter"),
            ("esc", "board"),
            ("?", "help"),
            ("q", "quit"),
        ],
        View::Usage => &[
            ("b", "by room"),
            ("i", "by item"),
            ("m", "by model"),
            ("d", "by day"),
            ("esc", "board"),
            ("?", "help"),
            ("q", "quit"),
        ],
        View::Live => &[
            ("↑↓", "scroll"),
            ("esc", "board"),
            ("tab", "next repo"),
            ("q", "quit"),
        ],
        View::Repos => &[
            ("↑↓", "repo"),
            ("enter", "switch"),
            ("tab", "next repo"),
            ("esc", "board"),
            ("q", "quit"),
        ],
        View::Diff => &[
            ("↑↓", "scroll"),
            ("pgup pgdn", "page"),
            ("esc", "back"),
            ("q", "quit"),
        ],
    };
    // Two spaces between keys when they all fit, else one. The last two
    // (help, quit) always show; the others while they fit.
    let full: usize = keys
        .iter()
        .map(|(k, w)| k.chars().count() + w.chars().count() + 3)
        .sum();
    let gap = if full <= area.width as usize { 2 } else { 1 };
    let width = |(k, w): &(&str, &str)| k.chars().count() + w.chars().count() + 1 + gap;
    let split = keys.len().saturating_sub(2);
    let (rest, tail) = keys.split_at(split);
    let mut room = (area.width as usize).saturating_sub(tail.iter().map(width).sum());
    let mut shown: Vec<&(&str, &str)> = Vec::new();
    for k in rest {
        let w = width(k);
        if w > room {
            break;
        }
        room -= w;
        shown.push(k);
    }
    shown.extend(tail);
    let mut spans = Vec::new();
    for (k, what) in shown {
        spans.push(Span::styled(*k, bold().fg(BRASS)));
        spans.push(Span::styled(format!(" {what}{}", " ".repeat(gap)), fg(DIM)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn help(f: &mut Frame, area: Rect) {
    let rows: &[(&str, &str)] = &[
        ("↑ ↓", "select an item (or a finding, a row, a repo)"),
        ("enter", "open the item's journal"),
        ("← →", "step through the journal; the board follows"),
        ("home end", "first event · back to now"),
        ("f", "findings for this item"),
        ("d", "diff at the selected round"),
        ("r", "send to review (hall ready)"),
        ("p", "pull back to development"),
        ("x", "drop, with a reason"),
        ("w", "open the item's worktree"),
        ("o", "open the PR"),
        ("b", "board"),
        ("h", "history: closed and dropped"),
        ("u", "usage (then b i m d to group)"),
        ("l", "live: what the daemon is doing"),
        ("R", "repositories"),
        ("tab", "next repository"),
        ("/", "filter by ID, title or state"),
        ("esc", "back · stop replaying · clear filter"),
        ("q", "quit (work carries on)"),
    ];
    let w = 64.min(area.width);
    let h = (rows.len() as u16 + 4).min(area.height);
    let r = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    let mut lines = vec![Line::default()];
    for (k, what) in rows {
        lines.push(Line::from(vec![
            Span::styled(format!("  {k:<10}"), bold().fg(BRASS)),
            Span::raw(*what),
        ]));
    }
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(
            pane("Keys")
                .title_bottom(Span::styled(" any key closes ", fg(DIM)))
                .border_style(fg(BRASS)),
        ),
        r,
    );
}

fn not_ready(f: &mut Frame, area: Rect, app: &App) {
    let Some(repo) = app.repo() else {
        f.render_widget(
            Paragraph::new("No repository here, and none registered. Run `hall init` in one.")
                .block(pane("Board")),
            area,
        );
        return;
    };
    let path = app.show_path(&repo.path.to_string_lossy());
    let text = match &repo.data.status {
        RepoStatus::NotInitialized => {
            format!(
                "{} doesn't use Hallmark yet. Run `hall init` in {path}.",
                repo.name
            )
        }
        RepoStatus::Missing => format!("{} is gone: {path} doesn't exist.", repo.name),
        RepoStatus::Broken(e) => format!("{} can't be read: {e}", repo.name),
        RepoStatus::Ready => String::new(),
    };
    f.render_widget(
        Paragraph::new(vec![Line::default(), Line::from(format!("  {text}"))])
            .block(pane(repo.name.clone())),
        area,
    );
}

// --- board -----------------------------------------------------------------

fn board_view(f: &mut Frame, area: Rect, app: &App) {
    let inbox = app.inbox();
    let inbox_h = if inbox.is_empty() {
        0
    } else {
        inbox.len().min(4) as u16 + 2 + u16::from(inbox.len() > 4)
    };
    let cols = app.board();
    let rows = cols.iter().map(Vec::len).max().unwrap_or(0).max(1) as u16;
    let board_h = 3 + rows.min(8);
    let nf = app
        .shown_ledger()
        .map(|l| l.findings.len())
        .unwrap_or(0)
        .max(1) as u16;
    let findings_h = (nf + 2).min(8);
    let [a_inbox, a_board, a_item, a_findings] = Layout::vertical([
        Constraint::Length(inbox_h),
        Constraint::Length(board_h),
        Constraint::Min(4),
        Constraint::Length(findings_h),
    ])
    .areas(area);
    if inbox_h > 0 {
        inbox_pane(f, a_inbox, app);
    }
    board_pane(f, a_board, app, &cols);
    journal_pane(f, a_item, app, false);
    findings_pane(f, a_findings, app);
}

fn inbox_pane(f: &mut Frame, area: Rect, app: &App) {
    let inbox = app.inbox();
    let mut rows: Vec<Row> = inbox
        .iter()
        .take(4)
        .map(|e| {
            let (label, c) = match e.kind.as_str() {
                "ready" => ("ready?", BRASS),
                "session_waiting" => ("session", INFO),
                "daemon_stopped" => ("daemon", STOP),
                k => (k, STOP),
            };
            let mut text = e.text.clone();
            if !e.options.is_empty() {
                let opts: Vec<String> = e
                    .options
                    .iter()
                    .enumerate()
                    .map(|(i, o)| format!("[{}] {o}", (b'a' + i as u8) as char))
                    .collect();
                text.push_str(&format!("  {}", opts.join("  ")));
            }
            Row::new(vec![
                Span::styled(label.to_string(), fg(c)),
                Span::raw(e.item.map(|i| i.to_string()).unwrap_or_default()),
                Span::raw(text),
                Span::styled(app.age(e.since), fg(DIM)),
            ])
        })
        .collect();
    if inbox.len() > 4 {
        rows.push(Row::new(vec![
            Span::raw(""),
            Span::raw(""),
            Span::styled(format!("… {} more · hall inbox", inbox.len() - 4), fg(DIM)),
        ]));
    }
    let t = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Length(7),
            Constraint::Min(10),
            Constraint::Length(8),
        ],
    )
    .block(
        pane("Your office")
            .title(Span::styled(
                format!("⚑ {} waiting ", inbox.len()),
                fg(STOP),
            ))
            .border_style(fg(STOP)),
    );
    f.render_widget(t, area);
}

/// The short tag after an item on the board.
fn tag(item: &Item) -> (String, Color) {
    if let Some(h) = &item.hold {
        return (format!("held: {}", h.kind.as_str()), STOP);
    }
    if item.stuck_since.is_some() {
        return ("stuck".into(), STOP);
    }
    match item.state {
        State::Development => (String::new(), DIM),
        State::AwaitingYou => ("needs you".into(), BRASS),
        State::Escalated => (format!("escalated ×{}", item.rejects), STOP),
        State::Checks => (format!("checks r{}", item.round), INFO),
        State::Review => (format!("review r{}", item.round), INFO),
        State::Fixing => (format!("fixing r{}", item.round), STOP),
        State::Hallmarked => ("shipping".into(), OK),
        State::Closed => (
            item.pr_url
                .as_deref()
                .and_then(|u| u.rsplit('/').next())
                .filter(|n| n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty())
                .map(|n| format!("#{n}"))
                .unwrap_or_else(|| "closed".into()),
            OK,
        ),
        State::Dropped => ("dropped".into(), DIM),
    }
}

fn board_pane(f: &mut Frame, area: Rect, app: &App, cols: &[Vec<super::app::BoardRow>; 4]) {
    let block = pane("Board");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let areas = Layout::horizontal([Constraint::Ratio(1, 4); 4])
        .spacing(1)
        .split(inner);
    for (i, (name, _)) in COLUMNS.iter().enumerate() {
        let rows = &cols[i];
        let w = areas[i].width as usize;
        let mut lines = vec![Line::from(Span::styled(
            format!("{} ({})", name.to_uppercase(), rows.len()),
            bold().fg(DIM),
        ))];
        if rows.is_empty() {
            lines.push(Line::from(Span::styled("none", fg(DIM))));
        }
        let max_rows = (areas[i].height as usize).saturating_sub(1);
        let sel_at = rows.iter().position(|r| Some(r.item.id) == app.selected);
        // Keep the selected row in view when the column overflows.
        let skip = match sel_at {
            Some(s) if s >= max_rows => s + 1 - max_rows,
            _ => 0,
        };
        for r in rows.iter().skip(skip).take(max_rows) {
            let (t, c) = tag(&r.item);
            let prefix = if r.child { "└ " } else { "" };
            let id = r.item.id.to_string();
            let room = w
                .saturating_sub(prefix.chars().count() + id.len() + 1)
                .saturating_sub(if t.is_empty() {
                    0
                } else {
                    t.chars().count() + 1
                });
            let mut spans = vec![
                Span::styled(prefix, fg(DIM)),
                Span::styled(id, bold()),
                Span::raw(" "),
                Span::raw(clip(&r.item.title, room)),
            ];
            if !t.is_empty() {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(t, fg(c)));
            }
            let mut line = Line::from(spans);
            if Some(r.item.id) == app.selected {
                line = line.style(selected());
            }
            lines.push(line);
        }
        f.render_widget(Paragraph::new(lines), areas[i]);
    }
}

fn item_heading(app: &App) -> (String, String) {
    match app.shown_item() {
        Some(it) => {
            let mut meta = it.state.label().to_lowercase();
            if it.round > 0 {
                meta.push_str(&format!(" · r{}", it.round));
            }
            if let Some(sha) = app.round_sha() {
                meta.push_str(&format!(" · {sha}"));
            }
            (format!("{} · {}", it.id, it.title), meta)
        }
        None => ("No item selected".into(), String::new()),
    }
}

fn journal_pane(f: &mut Frame, area: Rect, app: &App, full: bool) {
    let (title, meta) = item_heading(app);
    let (pos, n) = app.replay_position();
    let mut block = pane(title);
    if !meta.is_empty() {
        block =
            block.title(Line::from(Span::styled(format!(" {meta} "), fg(INFO))).right_aligned());
    }
    if app.cursor.is_some() {
        block = block.title_bottom(
            Line::from(Span::styled(
                format!(" replay {pos}/{n} · → forward · end now "),
                fg(BRASS),
            ))
            .right_aligned(),
        );
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(item) = app.item() else {
        f.render_widget(
            Paragraph::new(Span::styled(
                "No items here yet. Start one with `hall new \"<request>\"`.",
                fg(DIM),
            )),
            inner,
        );
        return;
    };
    let mut lines: Vec<Line> = Vec::new();
    if app.cursor.is_none()
        && let Some((doing, since)) = app.repo().and_then(|r| r.data.now.get(&item.id))
    {
        lines.push(Line::from(vec![
            Span::styled("now ", fg(DIM)),
            Span::styled(
                format!(
                    "{} ({})",
                    doing.replace(&item.worktree, &app.show_path(&item.worktree)),
                    app.age(*since)
                ),
                fg(INFO),
            ),
        ]));
    }
    if full {
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", item.state.room()), fg(DIM)),
            Span::raw(format!(
                "· round {} · rejected {} · branch {}",
                item.round, item.rejects, item.branch
            )),
        ]));
        let mut where_ = format!("worktree {}", app.show_path(&item.worktree));
        if !std::path::Path::new(&item.worktree).is_dir() {
            where_.push_str(" (gone)");
        }
        if let Some(pr) = &item.pr_url {
            where_.push_str(&format!(" · PR {pr}"));
        }
        if let Some(r) = &item.drop_reason {
            where_.push_str(&format!(" · dropped: \"{r}\""));
        }
        lines.push(Line::from(Span::styled(where_, fg(DIM))));
        lines.push(scrubber(app, inner.width as usize));
        lines.push(Line::default());
    }
    let (events, cursor) = app.shown_events();
    let room = (inner.height as usize).saturating_sub(lines.len());
    let hidden = events.len().saturating_sub(room);
    let (skip, more) = if hidden > 0 {
        (hidden + 1, true)
    } else {
        (0, false)
    };
    if more {
        lines.push(Line::from(Span::styled(
            format!("… {} earlier events", skip),
            fg(DIM),
        )));
    }
    for (i, e) in events.iter().enumerate().skip(skip) {
        let c = match tone(&e.body) {
            Tone::Good => OK,
            Tone::Bad => STOP,
            Tone::Decision => BRASS,
            Tone::Dim => DIM,
            Tone::Plain => Color::Reset,
        };
        let mut line = Line::from(vec![
            Span::styled(format!("{}  ", app.clock(e.t)), fg(DIM)),
            Span::styled(format!("{:<9} ", clip(&e.actor, 9)), fg(INFO)),
            Span::styled(e.body.summary(), fg(c)),
        ]);
        if cursor == Some(i) {
            line = line.style(selected());
        }
        lines.push(line);
    }
    if events.is_empty() {
        lines.push(Line::from(Span::styled("(empty journal)", fg(DIM))));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// `━━━━━●──────── 5/12`
fn scrubber(app: &App, width: usize) -> Line<'static> {
    let (pos, n) = app.replay_position();
    let label = format!(" {pos}/{n}");
    let w = width
        .saturating_sub("replay ".len() + label.len() + " now".len())
        .max(4);
    let mark = if n <= 1 {
        w - 1
    } else {
        (pos.saturating_sub(1)) * (w - 1) / (n - 1)
    };
    let bar: String = (0..w)
        .map(|i| match i.cmp(&mark) {
            std::cmp::Ordering::Less => '━',
            std::cmp::Ordering::Equal => '●',
            std::cmp::Ordering::Greater => '─',
        })
        .collect();
    let live = if app.cursor.is_none() { " now" } else { "" };
    Line::from(vec![
        Span::styled("replay ", fg(DIM)),
        Span::styled(bar, fg(BRASS)),
        Span::raw(label),
        Span::styled(live, fg(OK)),
    ])
}

fn finding_status(t: &Tracked) -> (String, Color) {
    match t.status {
        FStatus::Open => match &t.claimed_by {
            Some(sha) => (format!("open · fix claimed {}", short(sha)), INFO),
            None if t.dispute.is_some() => ("open · disputed".into(), BRASS),
            None => ("open".into(), STOP),
        },
        FStatus::Fixed => (
            match t.settled_round {
                Some(r) => format!("fixed · verified r{r}"),
                None => "fixed".into(),
            },
            OK,
        ),
        FStatus::Withdrawn => (
            match t.settled_round {
                Some(r) => format!("withdrawn r{r}"),
                None => "withdrawn".into(),
            },
            DIM,
        ),
    }
}

fn sev_color(s: crate::findings::Severity) -> Color {
    if s.blocks() { STOP } else { DIM }
}

fn findings_pane(f: &mut Frame, area: Rect, app: &App) {
    let block = pane("Findings");
    let ledger = app.shown_ledger();
    let rows: Vec<Row> = ledger
        .map(|l| {
            l.findings
                .iter()
                .map(|t| {
                    let (st, c) = finding_status(t);
                    Row::new(vec![
                        Span::raw(t.finding.id.clone()),
                        Span::styled(
                            t.finding.severity.as_str(),
                            fg(sev_color(t.finding.severity)),
                        ),
                        Span::styled(t.finding.lens.as_str(), fg(DIM)),
                        Span::raw(t.finding.title.clone()),
                        Span::styled(where_(t), fg(DIM)),
                        Span::styled(st, fg(c)),
                    ])
                })
                .collect()
        })
        .unwrap_or_default();
    if rows.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled("no findings yet", fg(DIM))).block(block),
            area,
        );
        return;
    }
    let t = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Length(7),
            Constraint::Length(16),
            Constraint::Min(20),
            Constraint::Length(22),
            Constraint::Length(26),
        ],
    )
    .block(block);
    f.render_widget(t, area);
}

fn where_(t: &Tracked) -> String {
    if t.finding.line > 0 {
        format!("{}:{}", t.finding.file, t.finding.line)
    } else {
        t.finding.file.clone()
    }
}

// --- journal and findings views ------------------------------------------

fn journal_view(f: &mut Frame, area: Rect, app: &App) {
    journal_pane(f, area, app, true);
}

fn findings_view(f: &mut Frame, area: Rect, app: &App) {
    let (title, meta) = item_heading(app);
    let ledger = app.shown_ledger();
    let list: Vec<&Tracked> = ledger
        .map(|l| l.findings.iter().collect())
        .unwrap_or_default();
    let detail_h = if app.finding_open && !list.is_empty() {
        (area.height / 2).max(6)
    } else {
        0
    };
    let [a_list, a_detail] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(detail_h)]).areas(area);
    let mut block = pane(format!("Findings · {title}"))
        .title(Line::from(Span::styled(format!(" {meta} "), fg(INFO))).right_aligned());
    if app.cursor.is_some() {
        let (pos, n) = app.replay_position();
        block = block.title_bottom(
            Line::from(Span::styled(format!(" as of event {pos}/{n} "), fg(BRASS))).right_aligned(),
        );
    }
    if list.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled("no findings yet", fg(DIM))).block(block),
            a_list,
        );
        return;
    }
    let rows: Vec<Row> = list
        .iter()
        .map(|t| {
            let (st, c) = match t.status {
                FStatus::Open if t.dispute.is_some() => ("open · disputed", BRASS),
                FStatus::Open => ("open", STOP),
                FStatus::Fixed => ("fixed", OK),
                FStatus::Withdrawn => ("withdrawn", DIM),
            };
            Row::new(vec![
                Span::raw(t.finding.id.clone()),
                Span::styled(
                    t.finding.severity.as_str(),
                    fg(sev_color(t.finding.severity)),
                ),
                Span::styled(t.finding.lens.as_str(), fg(DIM)),
                Span::styled(where_(t), fg(DIM)),
                Span::raw(t.finding.title.clone()),
                Span::styled(st, fg(c)),
                Span::raw(
                    t.claimed_by
                        .as_deref()
                        .map(short)
                        .unwrap_or("—")
                        .to_string(),
                ),
                Span::raw(
                    t.settled_round
                        .map(|r| format!("review r{r}"))
                        .unwrap_or_else(|| "—".into()),
                ),
            ])
        })
        .collect();
    let header = Row::new(vec![
        "id", "sev", "lens", "where", "title", "status", "fix", "settled",
    ])
    .style(bold().fg(DIM));
    let t = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Length(7),
            Constraint::Length(16),
            Constraint::Length(20),
            Constraint::Min(16),
            Constraint::Length(15),
            Constraint::Length(8),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .row_highlight_style(selected())
    .block(block);
    let mut state = TableState::default().with_selected(Some(app.finding_sel.min(list.len() - 1)));
    f.render_stateful_widget(t, a_list, &mut state);
    if detail_h > 0 {
        let t = list[app.finding_sel.min(list.len() - 1)];
        let mut lines = vec![
            Line::from(vec![
                Span::styled("raised    ", bold().fg(DIM)),
                Span::raw(format!("round {}", t.finding.round)),
            ]),
            Line::from(vec![
                Span::styled("scenario  ", bold().fg(DIM)),
                Span::raw(t.finding.scenario.clone()),
            ]),
            Line::from(vec![
                Span::styled("fix       ", bold().fg(DIM)),
                Span::raw(t.finding.fix.clone()),
            ]),
        ];
        if let Some(d) = &t.dispute {
            lines.push(Line::from(vec![
                Span::styled("disputed  ", bold().fg(BRASS)),
                Span::raw(d.clone()),
            ]));
        }
        if let Some(n) = &t.note {
            lines.push(Line::from(vec![
                Span::styled("reviewer  ", bold().fg(DIM)),
                Span::raw(n.clone()),
            ]));
        }
        f.render_widget(
            Paragraph::new(lines)
                .wrap(ratatui::widgets::Wrap { trim: false })
                .block(pane(format!("{} · {}", t.finding.id, t.finding.title))),
            a_detail,
        );
    }
}

// --- history ---------------------------------------------------------------

fn history(f: &mut Frame, area: Rect, app: &App) {
    let Some(repo) = app.repo() else { return };
    let items = app.history();
    let rows: Vec<Row> = items
        .iter()
        .map(|it| {
            let mut reasons: Vec<Line> = repo
                .data
                .findings
                .iter()
                .filter(|x| x.item == it.id && (x.severity == "blocker" || x.severity == "major"))
                .take(3)
                .map(|x| {
                    Line::from(Span::styled(
                        format!("✗ {} {} · {} · {}", x.id, x.severity, x.lens, x.title),
                        fg(STOP),
                    ))
                })
                .collect();
            if let Some(r) = &it.drop_reason {
                reasons.push(Line::from(Span::styled(
                    format!("dropped: \"{r}\""),
                    fg(DIM),
                )));
            }
            let h = 1 + reasons.len() as u16;
            let mut title = vec![Line::from(it.title.clone())];
            title.extend(reasons);
            let (outcome, c) = match it.state {
                State::Closed => ("closed", OK),
                _ => ("dropped", DIM),
            };
            Row::new(vec![
                Text::from(it.id.to_string()),
                Text::from(Span::styled(outcome, fg(c))),
                Text::from(format!("r{}", it.round)),
                Text::from(Span::styled(
                    format!("rejected {}×", it.rejects),
                    fg(if it.rejects > 0 { STOP } else { DIM }),
                )),
                Text::from(app.date(it.state_since)),
                Text::from(title),
            ])
            .height(h)
        })
        .collect();
    let title = if app.filter.is_empty() {
        format!("History · closed and dropped ({})", items.len())
    } else {
        format!(
            "History · closed and dropped ({}) · filter \"{}\"",
            items.len(),
            app.filter
        )
    };
    if rows.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled("nothing closed or dropped yet", fg(DIM)))
                .block(pane(title)),
            area,
        );
        return;
    }
    let t = Table::new(
        rows,
        [
            Constraint::Length(7),
            Constraint::Length(8),
            Constraint::Length(5),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new(vec![
            "item",
            "outcome",
            "round",
            "rejected",
            "ended",
            "title · why rejected",
        ])
        .style(bold().fg(DIM)),
    )
    .row_highlight_style(selected())
    .block(pane(title));
    let mut state = TableState::default().with_selected(Some(app.history_sel.min(items.len() - 1)));
    f.render_stateful_widget(t, area, &mut state);
}

// --- live --------------------------------------------------------------------

fn live(f: &mut Frame, area: Rect, app: &App) {
    let [a_daemon, a_log] =
        Layout::vertical([Constraint::Length(5), Constraint::Min(3)]).areas(area);
    let mut lines = Vec::new();
    match &app.daemon {
        Some(d) => {
            let label = app.daemon_label();
            let c = match label {
                "running" => OK,
                "paused" => BRASS,
                _ => STOP,
            };
            let mut first = vec![
                Span::styled("● ", fg(c)),
                Span::styled(label, bold()),
                Span::raw(format!(" · pid {}", d.pid)),
            ];
            if let Some(s) = d.started_at {
                first.push(Span::raw(format!(" · started {}", app.clock(s))));
            }
            if let Some(h) = d.heartbeat {
                first.push(Span::styled(
                    format!(" · heartbeat {}", app.age(h)),
                    fg(DIM),
                ));
            }
            lines.push(Line::from(first));
            if let Some(p) = &d.paused {
                let until = p
                    .until
                    .map(|u| format!(" until {}", app.clock(u)))
                    .unwrap_or_default();
                lines.push(Line::from(Span::styled(
                    format!("paused{until}: {}", p.reason),
                    fg(BRASS),
                )));
            }
            if let Some(c) = &d.current {
                lines.push(Line::from(format!("now: {c}")));
            }
            if label == "stopped" {
                lines.push(Line::from(Span::styled(
                    "The review loop isn't running: `hall daemon start`.",
                    fg(DIM),
                )));
            }
        }
        None => {
            lines.push(Line::from(vec![
                Span::styled("● ", fg(STOP)),
                Span::styled("stopped", bold()),
            ]));
            lines.push(Line::from(Span::styled(
                "The daemon has never run here. `hall daemon start` runs the review loop.",
                fg(DIM),
            )));
        }
    }
    f.render_widget(Paragraph::new(lines).block(pane("Daemon")), a_daemon);
    let log = app.repo().map(|r| r.data.live.as_slice()).unwrap_or(&[]);
    let block = pane("Live · live.log");
    let inner = block.inner(a_log);
    f.render_widget(block, a_log);
    if log.is_empty() {
        f.render_widget(Paragraph::new(Span::styled("nothing yet", fg(DIM))), inner);
        return;
    }
    let h = inner.height as usize;
    let end = log
        .len()
        .saturating_sub(app.live_scroll.min(log.len().saturating_sub(1)));
    let start = end.saturating_sub(h);
    let lines: Vec<Line> = log[start..end]
        .iter()
        .map(|l| {
            // "2026-10-09T12:00:00Z text": show the time dim.
            match l.split_once(' ') {
                Some((t, rest)) if t.ends_with('Z') && t.len() == 20 => Line::from(vec![
                    Span::styled(
                        if app.redact.times {
                            "hh:mm:ss".to_string()
                        } else {
                            t[11..19].to_string()
                        },
                        fg(DIM),
                    ),
                    Span::raw(format!(" {rest}")),
                ]),
                _ => Line::from(l.clone()),
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

// --- usage -------------------------------------------------------------------

/// `██████░░░░  61%`
fn bar(spent: Spent, cap: Cap) -> Line<'static> {
    let frac = cap.fraction(&spent);
    let filled = ((frac * 10.0).round() as usize).min(10);
    let c = if frac >= 0.9 {
        STOP
    } else if frac >= 0.5 {
        BRASS
    } else {
        OK
    };
    Line::from(vec![
        Span::styled("█".repeat(filled), fg(c)),
        Span::styled("░".repeat(10 - filled), fg(DIM)),
        Span::raw(format!(" {:>3.0}% of {}", frac * 100.0, cap.limit_label())),
    ])
}

fn usage_cells(key: Line<'static>, r: &UsageRow, budget: Line<'static>) -> Row<'static> {
    Row::new(vec![
        key,
        Line::from(r.records.to_string()),
        Line::from(fmt_tokens(r.tokens)),
        Line::from(format!("{:.2}", r.cost_usd)),
        budget,
    ])
}

fn usage(f: &mut Frame, area: Rect, app: &App) {
    let Some(repo) = app.repo() else { return };
    let u = &repo.data.usage;
    let budget = &repo.data.config.budget;
    let daily = budget.daily.as_deref().and_then(Cap::parse);
    let per_item = budget.per_item.as_deref().and_then(Cap::parse);
    let widths = [
        Constraint::Length(30),
        Constraint::Length(6),
        Constraint::Length(9),
        Constraint::Length(9),
        Constraint::Min(20),
    ];
    let header = |k: &str| {
        Row::new(vec![
            k.to_string(),
            "runs".into(),
            "tokens".into(),
            "≈ $".into(),
            "budget".into(),
        ])
        .style(bold().fg(DIM))
    };
    let dim = |s: &str| Line::from(Span::styled(s.to_string(), fg(DIM)));
    let (title, rows): (String, Vec<Row>) = match app.usage_by {
        UsageGroup::Room => {
            let mut rows: Vec<Row> = u
                .by_room
                .iter()
                .map(|r| {
                    let cap = room_role(&r.key)
                        .and_then(|role| budget.role(role))
                        .and_then(|b| b.daily.as_deref())
                        .and_then(Cap::parse);
                    let b = match (r.key.as_str(), cap) {
                        ("workshop", _) => dim("not capped"),
                        (_, Some(c)) => bar(row_spent(r), c),
                        (_, None) => dim("—"),
                    };
                    usage_cells(Line::from(room_label(&r.key).to_string()), r, b)
                })
                .collect();
            let total = sum(&u.by_room);
            let loop_line = match daily {
                Some(c) => {
                    let mut l = bar(u.loop_today, c);
                    l.spans.insert(0, Span::raw("loop "));
                    l
                }
                None => dim(&format!(
                    "loop {} · no daily cap",
                    fmt_tokens(u.loop_today.tokens)
                )),
            };
            rows.push(usage_cells(Line::from("total"), &total, loop_line).style(bold()));
            ("Usage · by room · today (UTC)".into(), rows)
        }
        UsageGroup::Model => {
            let mut rows: Vec<Row> = u
                .by_model
                .iter()
                .map(|r| usage_cells(Line::from(r.key.clone()), r, dim("")))
                .collect();
            rows.push(usage_cells(Line::from("total"), &sum(&u.by_model), dim("")).style(bold()));
            ("Usage · by model · today (UTC)".into(), rows)
        }
        UsageGroup::Item => (
            "Usage · by item · all time".into(),
            item_rows(app, &u.by_item, per_item, usize::MAX),
        ),
        UsageGroup::Day => {
            let rows = u
                .by_day
                .iter()
                .map(|d| {
                    let r = UsageRow {
                        key: d.day.clone(),
                        records: d.records,
                        tokens: d.tokens,
                        cost_usd: d.cost_usd,
                        ..Default::default()
                    };
                    let b = match daily {
                        Some(c) => {
                            let spent = Spent {
                                tokens: d.loop_tokens,
                                usd: d.loop_usd,
                            };
                            let mut l = bar(spent, c);
                            l.spans.insert(0, Span::raw("loop "));
                            l
                        }
                        None => dim(&format!("loop {}", fmt_tokens(d.loop_tokens))),
                    };
                    let key = if app.redact.times {
                        "yyyy-mm-dd".to_string()
                    } else {
                        d.day.clone()
                    };
                    usage_cells(Line::from(key), &r, b)
                })
                .collect();
            ("Usage · by day · last 14 days (UTC)".into(), rows)
        }
    };
    let key_name = app.usage_by.name();
    let show_items = app.usage_by != UsageGroup::Item;
    let n_items = u.by_item.len().min(5) as u16;
    let [a_main, a_items] = Layout::vertical([
        Constraint::Min(4),
        Constraint::Length(if show_items { n_items.max(1) + 2 } else { 0 }),
    ])
    .areas(area);
    if rows.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled("no usage recorded yet", fg(DIM))).block(pane(title)),
            a_main,
        );
    } else {
        f.render_widget(
            Table::new(rows, widths)
                .header(header(key_name))
                .block(pane(title)),
            a_main,
        );
    }
    if show_items {
        let rows = item_rows(app, &u.by_item, per_item, 5);
        if rows.is_empty() {
            f.render_widget(
                Paragraph::new(Span::styled("none", fg(DIM))).block(pane("Most expensive items")),
                a_items,
            );
        } else {
            f.render_widget(
                Table::new(rows, widths).block(pane("Most expensive items")),
                a_items,
            );
        }
    }
}

fn row_spent(r: &UsageRow) -> Spent {
    Spent {
        tokens: r.weighted_tokens(),
        usd: r.cost_usd,
    }
}

fn item_rows(app: &App, rows: &[UsageRow], cap: Option<Cap>, n: usize) -> Vec<Row<'static>> {
    let Some(repo) = app.repo() else {
        return vec![];
    };
    rows.iter()
        .take(n)
        .map(|r| {
            let item = r.key.parse().ok().and_then(|id| repo.data.item(id));
            let key = match item {
                Some(it) => clip(&format!("{} {}", it.id, it.title), 30),
                None => r.key.clone(),
            };
            let b = match (item, cap) {
                (Some(it), _)
                    if it
                        .hold
                        .as_ref()
                        .is_some_and(|h| h.kind == crate::item::HoldKind::Budget) =>
                {
                    Line::from(Span::styled("over item cap · with you", fg(STOP)))
                }
                (Some(it), Some(c)) if !it.state.is_terminal() => bar(row_spent(r), c),
                (Some(it), _) => {
                    let (t, c) = tag(it);
                    let label = if t.is_empty() {
                        it.state.label().to_lowercase()
                    } else {
                        format!("{} · {t}", it.state.label().to_lowercase())
                    };
                    Line::from(Span::styled(label, fg(c)))
                }
                (None, _) => Line::default(),
            };
            usage_cells(Line::from(key), r, b)
        })
        .collect()
}

fn sum(rows: &[UsageRow]) -> UsageRow {
    let mut t = UsageRow::default();
    for r in rows {
        t.records += r.records;
        t.tokens += r.tokens;
        t.cost_usd += r.cost_usd;
    }
    t
}

// --- repos -------------------------------------------------------------------

fn repos(f: &mut Frame, area: Rect, app: &App) {
    let rows: Vec<Row> = app
        .repos
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let (status, c) = match &r.data.status {
                RepoStatus::Ready => ("ok".to_string(), OK),
                RepoStatus::NotInitialized => ("not initialized".to_string(), BRASS),
                RepoStatus::Missing => ("missing".to_string(), STOP),
                RepoStatus::Broken(e) => (format!("error: {e}"), STOP),
            };
            let waiting = r.data.inbox.len();
            Row::new(vec![
                Line::from(if i == app.active { "●" } else { "" }),
                Line::from(Span::styled(r.name.clone(), bold())),
                Line::from(format!("{} open", r.data.open_count())),
                Line::from(Span::styled(
                    format!("⚑ {waiting} need you"),
                    fg(if waiting > 0 { STOP } else { DIM }),
                )),
                Line::from(Span::styled(status, fg(c))),
                Line::from(Span::styled(
                    app.show_path(&r.path.to_string_lossy()),
                    fg(DIM),
                )),
            ])
        })
        .collect();
    let n = rows.len();
    let t = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Length(20),
            Constraint::Length(8),
            Constraint::Length(13),
            Constraint::Length(16),
            Constraint::Min(20),
        ],
    )
    .row_highlight_style(selected())
    .block(pane(format!("Repositories ({n})")));
    let mut state = TableState::default().with_selected((n > 0).then(|| app.repo_sel.min(n - 1)));
    f.render_stateful_widget(t, area, &mut state);
}

// --- diff --------------------------------------------------------------------

fn diff(f: &mut Frame, area: Rect, app: &App) {
    let Some(d) = &app.diff else {
        f.render_widget(Paragraph::new("").block(pane("Diff")), area);
        return;
    };
    let block = pane(d.title.clone()).title_bottom(
        Line::from(Span::styled(
            format!(" line {}/{} ", d.scroll + 1, d.lines.len()),
            fg(DIM),
        ))
        .right_aligned(),
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines: Vec<Line> = d
        .lines
        .iter()
        .skip(d.scroll)
        .take(inner.height as usize)
        .map(|l| {
            let style = if l.starts_with("+++") || l.starts_with("---") || l.starts_with("diff ") {
                bold()
            } else if l.starts_with('+') {
                fg(OK)
            } else if l.starts_with('-') {
                fg(STOP)
            } else if l.starts_with("@@") {
                fg(INFO)
            } else {
                Style::new()
            };
            Line::from(Span::styled(l.clone(), style))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}
