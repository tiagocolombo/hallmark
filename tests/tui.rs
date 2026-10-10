//! The terminal app, without a terminal: a repo with items in several
//! states is loaded into an `App`, keys are fed to it, and each view is
//! rendered into ratatui's `TestBackend` and snapshotted as text.

mod common;

use std::path::Path;

use chrono::Utc;
use common::{Env, commit_file, git, p};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hallmark::daemon::state::DaemonState;
use hallmark::event::{Body, CheckRun, Event, Usage, VerdictKind};
use hallmark::findings::{Counts, Finding, Lens, PriorRuling, PriorStatus, Severity};
use hallmark::item::{ItemId, State};
use hallmark::journal::Hall;
use hallmark::tui::app::Redact;
use hallmark::tui::{App, Effect, Input, UsageGroup, View, ui};
use insta::assert_snapshot;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

const W: u16 = 120;
const H: u16 = 36;

fn hall_of(env: &Env) -> Hall {
    Hall::new(&env.repo.join(".git"))
}

fn add(hall: &Hall, id: u32, actor: &str, body: Body) {
    let id = ItemId(id);
    hall.append_item(id, &Event::new(actor, Some(id), body))
        .unwrap();
}

fn finding(id: &str, round: u32, lens: Lens, severity: Severity, title: &str) -> Finding {
    Finding {
        id: id.into(),
        round,
        lens,
        severity,
        file: "src/webhooks.rs".into(),
        line: 42,
        title: title.into(),
        scenario: format!("{title}: a retry after a timeout sends the delivery twice"),
        fix: "Key deliveries by event ID and skip ones already sent.".into(),
    }
}

#[allow(clippy::too_many_arguments)]
fn verdict(
    sha: &str,
    round: u32,
    verdict: VerdictKind,
    escalated: bool,
    findings: Vec<Finding>,
    prior: Vec<PriorRuling>,
) -> Body {
    let mut counts = Counts::default();
    for f in &findings {
        counts.add(f.severity);
    }
    Body::Verdict {
        sha: sha.into(),
        round,
        verdict,
        escalated,
        findings,
        prior,
        counts,
        summary: Some("Retries look right; one duplicate-delivery case.".into()),
        report: None,
        reviewer: Some("opus".into()),
        learnings: vec![],
        base: None,
        since: None,
    }
}

fn checks(sha: &str, round: u32, passed: bool) -> Body {
    Body::Checks {
        sha: sha.into(),
        round,
        passed,
        runs: vec![CheckRun {
            command: "cargo test".into(),
            exit_code: if passed { 0 } else { 101 },
            duration_ms: 1200,
            log: None,
        }],
    }
}

fn usage(room: &str, role: &str, model: &str, tokens: u64, cost: f64) -> Body {
    Body::Usage(Usage {
        source: "headless".into(),
        room: room.into(),
        role: role.into(),
        model: model.into(),
        input_tokens: tokens / 10,
        output_tokens: tokens / 10,
        cache_read_tokens: tokens - 2 * (tokens / 10),
        cache_write_tokens: 0,
        cost_usd: cost,
        requests: 3,
        turns: Some(4),
        duration_ms: Some(60_000),
    })
}

/// A repo with an item in every part of the building:
///
/// - H-0001 in review, round 2, with a fixed claim on F1 and F2 open
/// - H-0002 awaiting you, with a question
/// - H-0003 in development, with a parallel part H-0004 (worktree gone)
/// - H-0005 closed with PR #191, after a blocker was fixed
/// - H-0006 dropped
/// - H-0007 escalated
fn building() -> Env {
    let env = Env::initialized();
    let cfg = env.repo.join(".hallmark.toml");
    let text = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("# daily    = \"4M\"", "daily    = \"4M\"")
        .replace("# per_item = \"800k\"", "per_item = \"800k\"")
        + "\n[budget.review]\ndaily = \"2M\"\n";
    std::fs::write(&cfg, text).unwrap();
    let hall = hall_of(&env);

    // H-0001: two review rounds.
    let (_, wt1) = env.new_item("Retry failed webhook deliveries");
    let sha1 = commit_file(&wt1, "webhooks.rs", "fn retry() {}\n");
    assert_eq!(env.hall(&wt1, &["ready"]).code, 0);
    add(&hall, 1, "assayer", checks(&sha1, 1, true));
    add(
        &hall,
        1,
        "assayer",
        usage("assay", "reviewer", "opus", 400_000, 4.2),
    );
    add(
        &hall,
        1,
        "assayer",
        verdict(
            &sha1,
            1,
            VerdictKind::Changes,
            false,
            vec![
                finding(
                    "F1",
                    1,
                    Lens::Correctness,
                    Severity::Major,
                    "Retries can deliver twice",
                ),
                finding(
                    "F2",
                    1,
                    Lens::Evidence,
                    Severity::Minor,
                    "No test for the backoff cap",
                ),
            ],
            vec![],
        ),
    );
    let sha1b = commit_file(&wt1, "webhooks.rs", "fn retry() { dedupe() }\n");
    add(
        &hall,
        1,
        "fixer",
        Body::FixSubmitted {
            sha: sha1b.clone(),
            fixes: vec!["F1".into()],
            disputes: vec![],
        },
    );
    add(
        &hall,
        1,
        "fixer",
        usage("repair", "fixer", "sonnet", 100_000, 0.9),
    );
    add(&hall, 1, "assayer", checks(&sha1b, 2, true));

    // H-0002: awaiting you, with a question.
    let (_, wt2) = env.new_item("Export tenant usage");
    let r = env.hall(
        &wt2,
        &["propose-ready", "--summary", "CSV export with 14 tests"],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    let r = env.hall(
        &env.repo,
        &[
            "ask",
            "Billed usage or raw events?",
            "--item",
            "H-0002",
            "--options",
            "billed,raw",
            "--default",
            "billed",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);

    // H-0003 and its parallel part H-0004, whose worktree is gone.
    env.new_item("Rate limit the public API");
    add(
        &hall,
        4,
        "hall",
        Body::Created {
            title: "Rate limit: document the headers".into(),
            branch: "hm/H-0004-rate-limit-docs".into(),
            worktree: p(&env.root.join("app-H-0004")),
            base: "main".into(),
            issue: None,
            request: None,
            adopted: false,
            parent: Some(ItemId(3)),
            follow_up: None,
        },
    );
    add(
        &hall,
        3,
        "you",
        usage("workshop", "dev", "opus", 1_200_000, 9.5),
    );

    // H-0005: closed after a blocker was fixed.
    let (_, wt5) = env.new_item("Tenant check in export query");
    let sha5 = commit_file(&wt5, "export.rs", "fn export() {}\n");
    assert_eq!(env.hall(&wt5, &["ready"]).code, 0);
    add(&hall, 5, "assayer", checks(&sha5, 1, true));
    add(
        &hall,
        5,
        "assayer",
        verdict(
            &sha5,
            1,
            VerdictKind::Changes,
            false,
            vec![finding(
                "F1",
                1,
                Lens::SecurityTenancy,
                Severity::Blocker,
                "Export reads other tenants",
            )],
            vec![],
        ),
    );
    let sha5b = commit_file(&wt5, "export.rs", "fn export(tenant: Id) {}\n");
    add(
        &hall,
        5,
        "fixer",
        Body::FixSubmitted {
            sha: sha5b.clone(),
            fixes: vec!["F1".into()],
            disputes: vec![],
        },
    );
    add(&hall, 5, "assayer", checks(&sha5b, 2, true));
    add(
        &hall,
        5,
        "assayer",
        verdict(
            &sha5b,
            2,
            VerdictKind::Approve,
            false,
            vec![],
            vec![PriorRuling {
                id: "F1".into(),
                status: PriorStatus::Fixed,
                note: None,
            }],
        ),
    );
    add(
        &hall,
        5,
        "hall",
        Body::Hallmarked {
            sha: sha5b.clone(),
            round: 2,
        },
    );
    add(
        &hall,
        5,
        "shipper",
        Body::Shipped {
            sha: sha5b,
            pr_url: Some("https://github.com/acme/app/pull/191".into()),
        },
    );

    // H-0006: dropped.
    let (_, wt6) = env.new_item("Old experiment");
    let r = env.hall(&wt6, &["drop", "--reason", "superseded by H-0003"]);
    assert_eq!(r.code, 0, "{}", r.stdout);

    // H-0007: escalated.
    let (_, wt7) = env.new_item("Migrate billing tables");
    let sha7 = commit_file(&wt7, "billing.sql", "ALTER TABLE x;\n");
    assert_eq!(env.hall(&wt7, &["ready"]).code, 0);
    add(&hall, 7, "assayer", checks(&sha7, 1, true));
    add(
        &hall,
        7,
        "assayer",
        verdict(
            &sha7,
            1,
            VerdictKind::Changes,
            true,
            vec![finding(
                "F1",
                1,
                Lens::DataMigrations,
                Severity::Blocker,
                "Migration isn't reversible",
            )],
            vec![],
        ),
    );

    hall.live("assayer: running gates for H-0001 round 2");
    hall.live("assayer: cargo test passed in 1.2s");
    hall.live("assayer: reviewing H-0001 round 2 with opus");
    env
}

/// Adds an uninitialized repo and a missing one to the registry.
fn more_repos(env: &Env) {
    let lib = env.root.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    git(&lib, &["init", "-q", "-b", "main"]);
    let reg = env.home.join("repos.json");
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&reg).unwrap()).unwrap();
    let repos = v["repos"].as_array_mut().unwrap();
    repos.push(serde_json::json!({ "name": "lib", "path": p(&lib) }));
    repos.push(serde_json::json!({ "name": "gone", "path": p(&env.root.join("gone")) }));
    std::fs::write(&reg, v.to_string()).unwrap();
}

fn load(env: &Env, cwd: &Path) -> App {
    let mut app = App::load(cwd, &env.home.join("repos.json"));
    app.redact = Redact {
        times: true,
        root: Some(p(&env.root)),
    };
    app.daemon = Some(DaemonState {
        pid: 4242,
        started_at: Some(Utc::now()),
        heartbeat: Some(Utc::now()),
        wanted: true,
        paused: None,
        current: Some("reviewing H-0001 round 2".into()),
        ..Default::default()
    });
    app
}

fn render(app: &App, w: u16, h: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| ui::draw(f, app)).unwrap();
    let buf = t.backend().buffer();
    let mut s = String::new();
    for y in 0..h {
        let mut line = String::new();
        for x in 0..w {
            line.push_str(buf[(x, y)].symbol());
        }
        s.push_str(line.trim_end());
        s.push('\n');
    }
    s
}

/// The rendered screen with commit SHAs, which depend on the clock,
/// replaced by `#######`.
fn screen(env: &Env, app: &App, w: u16, h: u16) -> String {
    let mut s = render(app, w, h);
    for sha in git(&env.repo, &["rev-list", "--all"]).lines() {
        s = s.replace(&sha[..7], "#######");
    }
    s
}

fn key(app: &mut App, code: KeyCode) -> Effect {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn chars(app: &mut App, text: &str) {
    for c in text.chars() {
        key(app, KeyCode::Char(c));
    }
}

fn select(app: &mut App, id: u32) {
    for _ in 0..20 {
        if app.selected == Some(ItemId(id)) {
            return;
        }
        key(app, KeyCode::Down);
    }
    for _ in 0..20 {
        if app.selected == Some(ItemId(id)) {
            return;
        }
        key(app, KeyCode::Up);
    }
    panic!("H-{id:04} is not on the board");
}

#[test]
fn renders_every_view() {
    let env = building();
    more_repos(&env);
    let mut app = load(&env, &env.repo);
    assert_eq!(app.repos.len(), 3);
    assert_eq!(app.active, 0);

    select(&mut app, 1);
    assert_snapshot!("board", screen(&env, &app, W, H));

    key(&mut app, KeyCode::Enter);
    assert_eq!(app.view, View::Journal);
    assert_snapshot!("journal", screen(&env, &app, W, H));

    key(&mut app, KeyCode::Char('f'));
    assert_eq!(app.view, View::Findings);
    key(&mut app, KeyCode::Enter);
    assert!(app.finding_open);
    assert_snapshot!("findings", screen(&env, &app, W, H));

    key(&mut app, KeyCode::Char('h'));
    assert_eq!(app.view, View::History);
    assert_snapshot!("history", screen(&env, &app, W, H));

    key(&mut app, KeyCode::Char('l'));
    assert_eq!(app.view, View::Live);
    assert_snapshot!("live", screen(&env, &app, W, H));

    key(&mut app, KeyCode::Char('u'));
    assert_eq!(app.view, View::Usage);
    assert_snapshot!("usage_by_room", screen(&env, &app, W, H));
    key(&mut app, KeyCode::Char('i'));
    assert_eq!(app.usage_by, UsageGroup::Item);
    assert_snapshot!("usage_by_item", screen(&env, &app, W, H));
    key(&mut app, KeyCode::Char('m'));
    assert_eq!(app.usage_by, UsageGroup::Model);
    key(&mut app, KeyCode::Char('d'));
    assert_eq!(app.usage_by, UsageGroup::Day);
    key(&mut app, KeyCode::Char('b'));
    assert_eq!(app.usage_by, UsageGroup::Room);

    key(&mut app, KeyCode::Char('R'));
    assert_eq!(app.view, View::Repos);
    assert_snapshot!("repos", screen(&env, &app, W, 12));

    key(&mut app, KeyCode::Esc);
    assert_eq!(app.view, View::Board);
    key(&mut app, KeyCode::Char('?'));
    assert!(app.help);
    assert_snapshot!("help", screen(&env, &app, W, H));
    key(&mut app, KeyCode::Char('z'));
    assert!(!app.help, "any key closes help");
    assert_eq!(app.view, View::Board);
}

#[test]
fn replay_folds_the_item_at_the_cursor() {
    let env = building();
    let mut app = load(&env, &env.repo);
    select(&mut app, 1);
    let n = app.selection.as_ref().unwrap().events.len();
    assert_eq!(app.replay_position(), (n, n));
    assert_eq!(app.shown_item().unwrap().state, State::Review);

    // Back past the start stops at the first event.
    for _ in 0..(n + 5) {
        key(&mut app, KeyCode::Left);
    }
    assert_eq!(app.cursor, Some(0));
    assert_eq!(app.replay_position(), (1, n));
    assert_eq!(app.shown_item().unwrap().state, State::Development);
    assert!(app.shown_ledger().unwrap().findings.is_empty());

    // Forward to the verdict: the board and findings follow.
    while app.shown_item().unwrap().state != State::Fixing {
        key(&mut app, KeyCode::Right);
    }
    let l = app.shown_ledger().unwrap();
    assert_eq!(l.findings.len(), 2);
    assert!(l.findings[0].claimed_by.is_none(), "not claimed yet");
    assert_snapshot!("board_replay", screen(&env, &app, W, H));

    // Forward past the end returns to now.
    for _ in 0..(n + 5) {
        key(&mut app, KeyCode::Right);
    }
    assert_eq!(app.cursor, None);
    assert_eq!(app.shown_item().unwrap().state, State::Review);
    assert!(app.shown_ledger().unwrap().findings[0].claimed_by.is_some());

    // Esc on the board stops replaying, too.
    key(&mut app, KeyCode::Left);
    assert!(app.cursor.is_some());
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.cursor, None);

    // Changing the selection resets the cursor.
    key(&mut app, KeyCode::Left);
    key(&mut app, KeyCode::Down);
    assert_eq!(app.cursor, None);
}

#[test]
fn selection_walks_the_board_with_children_under_parents() {
    let env = building();
    let mut app = load(&env, &env.repo);
    let order = app.board_order();
    let ids: Vec<String> = order.iter().map(|i| i.to_string()).collect();
    // Development (parent, child), Awaiting you, Review loop, Closed today.
    assert_eq!(
        ids,
        [
            "H-0003", "H-0004", "H-0002", "H-0007", "H-0001", "H-0005", "H-0006"
        ]
    );
    assert_eq!(app.selected, Some(ItemId(3)));
    key(&mut app, KeyCode::Up);
    assert_eq!(app.selected, Some(ItemId(3)), "stays at the top");
    for _ in 0..20 {
        key(&mut app, KeyCode::Down);
    }
    assert_eq!(app.selected, Some(ItemId(6)), "stays at the bottom");
    let cols = app.board();
    assert!(cols[0][1].child, "H-0004 is drawn under H-0003");
}

#[test]
fn filter_narrows_the_board() {
    let env = building();
    let mut app = load(&env, &env.repo);
    key(&mut app, KeyCode::Char('/'));
    assert_eq!(app.input, Some(Input::Filter));
    chars(&mut app, "export");
    assert_eq!(app.board_order(), vec![ItemId(2), ItemId(5)]);
    assert_eq!(app.selected, Some(ItemId(2)), "moves onto a visible item");
    assert_snapshot!("filter", screen(&env, &app, W, H));
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.input, None);
    assert_eq!(app.filter, "export");

    // By state, too.
    key(&mut app, KeyCode::Char('/'));
    key(&mut app, KeyCode::Backspace);
    for _ in 0..10 {
        key(&mut app, KeyCode::Backspace);
    }
    chars(&mut app, "escalated");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.board_order(), vec![ItemId(7)]);

    // History filters by lens and reason.
    app.filter = "security".into();
    let h: Vec<ItemId> = app.history().iter().map(|i| i.id).collect();
    assert_eq!(h, vec![ItemId(5)]);
    app.filter = "superseded".into();
    let h: Vec<ItemId> = app.history().iter().map(|i| i.id).collect();
    assert_eq!(h, vec![ItemId(6)]);

    key(&mut app, KeyCode::Esc);
    assert!(app.filter.is_empty(), "esc clears the filter");
    assert_eq!(app.board_order().len(), 7);
}

#[test]
fn ready_and_pull_back_go_through_the_journal() {
    let env = building();
    let hall = hall_of(&env);
    let mut app = load(&env, &env.repo);

    select(&mut app, 2);
    key(&mut app, KeyCode::Char('r'));
    let st = app.status.clone().unwrap();
    assert!(!st.error, "{}", st.text);
    assert!(st.text.starts_with("H-0002: Round 1 queued"), "{}", st.text);
    assert_eq!(hall.load_item(ItemId(2)).unwrap().state, State::Checks);
    assert_eq!(app.item().unwrap().state, State::Checks, "reloaded");

    key(&mut app, KeyCode::Char('p'));
    assert_eq!(hall.load_item(ItemId(2)).unwrap().state, State::Development);
    assert_eq!(app.item().unwrap().state, State::Development);

    // An action that isn't allowed shows the error and changes nothing.
    select(&mut app, 5);
    key(&mut app, KeyCode::Char('p'));
    let st = app.status.clone().unwrap();
    assert!(st.error);
    assert!(st.text.contains("WRONG_STATE"), "{}", st.text);
    assert_eq!(hall.load_item(ItemId(5)).unwrap().state, State::Closed);
}

#[test]
fn drop_asks_for_a_reason() {
    let env = building();
    let hall = hall_of(&env);
    let mut app = load(&env, &env.repo);
    select(&mut app, 3);

    key(&mut app, KeyCode::Char('x'));
    assert_eq!(app.input, Some(Input::DropReason(ItemId(3))));
    chars(&mut app, "not now");
    assert_snapshot!("drop_input", screen(&env, &app, W, H));
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.input, None);
    assert_eq!(hall.load_item(ItemId(3)).unwrap().state, State::Development);

    // 'q' while typing is text, not quit.
    key(&mut app, KeyCode::Char('x'));
    assert_eq!(key(&mut app, KeyCode::Char('q')), Effect::None);
    key(&mut app, KeyCode::Backspace);
    key(&mut app, KeyCode::Enter);
    assert!(app.status.as_ref().unwrap().error, "a reason is required");
    assert_eq!(hall.load_item(ItemId(3)).unwrap().state, State::Development);

    // A parent with an open parallel part can't be dropped (CHILDREN_OPEN)…
    key(&mut app, KeyCode::Char('x'));
    chars(&mut app, "split differently");
    key(&mut app, KeyCode::Enter);
    let st = app.status.clone().unwrap();
    assert!(st.error && st.text.contains("CHILDREN_OPEN"), "{}", st.text);
    assert_eq!(hall.load_item(ItemId(3)).unwrap().state, State::Development);

    // …until the part is dropped first.
    select(&mut app, 4);
    key(&mut app, KeyCode::Char('x'));
    chars(&mut app, "folded into the parent");
    key(&mut app, KeyCode::Enter);
    assert_eq!(hall.load_item(ItemId(4)).unwrap().state, State::Dropped);
    select(&mut app, 3);
    key(&mut app, KeyCode::Char('x'));
    chars(&mut app, "split differently");
    key(&mut app, KeyCode::Enter);
    let item = hall.load_item(ItemId(3)).unwrap();
    assert_eq!(item.state, State::Dropped);
    assert_eq!(item.drop_reason.as_deref(), Some("split differently"));
}

#[test]
fn open_and_diff() {
    let env = building();
    let mut app = load(&env, &env.repo);

    select(&mut app, 1);
    match key(&mut app, KeyCode::Char('w')) {
        Effect::Open { target, fallback } => {
            assert!(target.ends_with("app-H-0001"), "{target}");
            assert_eq!(fallback, "cd [root]/app-H-0001");
        }
        e => panic!("{e:?}"),
    }
    // No PR yet.
    assert_eq!(key(&mut app, KeyCode::Char('o')), Effect::None);
    assert!(app.status.as_ref().unwrap().error);

    key(&mut app, KeyCode::Char('d'));
    assert_eq!(app.view, View::Diff);
    let d = app.diff.as_ref().unwrap();
    assert!(d.title.starts_with("H-0001 · diff main..."), "{}", d.title);
    assert!(d.lines.iter().any(|l| l == "+fn retry() { dedupe() }"));
    key(&mut app, KeyCode::PageDown);
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.view, View::Board);
    assert!(app.diff.is_none());

    // A missing worktree is reported, never opened.
    select(&mut app, 4);
    assert_eq!(key(&mut app, KeyCode::Char('w')), Effect::None);
    assert!(
        app.status
            .as_ref()
            .unwrap()
            .text
            .contains("worktree is gone"),
        "{:?}",
        app.status
    );

    select(&mut app, 5);
    match key(&mut app, KeyCode::Char('o')) {
        Effect::Open { target, .. } => assert_eq!(target, "https://github.com/acme/app/pull/191"),
        e => panic!("{e:?}"),
    }
    assert_eq!(key(&mut app, KeyCode::Char('q')), Effect::Quit);
}

#[test]
fn switches_repos_and_survives_odd_ones() {
    let env = building();
    more_repos(&env);
    let mut app = load(&env, &env.repo);
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.active, 1);
    assert_eq!(app.repo().unwrap().name, "lib");
    assert_snapshot!("not_initialized", screen(&env, &app, W, H));
    // Actions explain instead of failing.
    key(&mut app, KeyCode::Char('r'));
    assert!(app.status.as_ref().unwrap().error);
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.repo().unwrap().name, "gone");
    render(&app, W, H);
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.active, 0);
    assert!(app.selected.is_some());

    // Repos view: enter switches.
    key(&mut app, KeyCode::Char('R'));
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.active, 1);
    assert_eq!(app.view, View::Board);
}

#[test]
fn falls_back_to_the_current_repo_without_a_registry() {
    let env = Env::initialized();
    let app = App::load(&env.repo, &env.root.join("no-such-registry.json"));
    assert_eq!(app.repos.len(), 1);
    assert_eq!(app.repo().unwrap().name, "app");
    // An empty repo renders, and a tiny terminal doesn't panic.
    let mut app = app;
    app.redact.times = true;
    assert_snapshot!("empty", screen(&env, &app, W, 14));
    for v in ['h', 'u', 'l', 'R', 'f', 'b'] {
        key(&mut app, KeyCode::Char(v));
        render(&app, 20, 5);
        render(&app, 1, 1);
    }
    // Outside any repo, with no registry: nothing to show, no crash.
    let app = App::load(&env.root, &env.root.join("no-such-registry.json"));
    assert!(app.repos.is_empty());
    render(&app, W, 10);
}

#[test]
fn reload_picks_up_new_events() {
    let env = building();
    let hall = hall_of(&env);
    let mut app = load(&env, &env.repo);
    select(&mut app, 1);
    let before = app.selection.as_ref().unwrap().events.len();
    add(
        &hall,
        1,
        "assayer",
        verdict("abc", 2, VerdictKind::Approve, false, vec![], vec![]),
    );
    app.reload_active();
    assert_eq!(app.selection.as_ref().unwrap().events.len(), before + 1);
    assert_eq!(app.item().unwrap().state, State::Hallmarked);
}

#[test]
fn your_office_explains_each_entry_and_runs_its_actions() {
    let env = building();
    let hall = hall_of(&env);
    let mut app = load(&env, &env.repo);

    key(&mut app, KeyCode::Char('i'));
    assert_eq!(app.view, View::Inbox);
    assert_eq!(app.inbox().len(), 3);
    assert_eq!(app.selected, Some(ItemId(2)), "follows the entry's item");
    assert_snapshot!("inbox_ready", screen(&env, &app, W, H));

    // The question: each option is an action.
    key(&mut app, KeyCode::Down);
    assert_eq!(app.inbox_entry().unwrap().kind, "question");
    let actions = app.inbox_actions();
    assert_eq!(actions[0].0, "hall answer Q-1 a");
    assert_eq!(actions[1].0, "hall answer Q-1 b");
    assert_snapshot!("inbox_question", screen(&env, &app, W, H));
    assert_eq!(key(&mut app, KeyCode::Char('2')), Effect::None);
    let st = app.status.clone().unwrap();
    assert!(!st.error, "{}", st.text);
    assert!(st.text.starts_with("hall answer Q-1 b"), "{}", st.text);
    assert_eq!(app.inbox().len(), 2, "answered");
    assert!(app.inbox().iter().all(|e| e.kind != "question"));

    // No such action.
    key(&mut app, KeyCode::Char('9'));
    assert!(app.status.as_ref().unwrap().error);

    // The escalation: a reader opens its reply in full…
    key(&mut app, KeyCode::Down);
    let e = app.inbox_entry().unwrap();
    assert_eq!(e.kind, "escalated");
    assert_eq!(app.selected, Some(ItemId(7)));
    let actions = app.inbox_actions();
    assert_eq!(actions[0].0, "hall findings H-0007");
    key(&mut app, KeyCode::Char('1'));
    assert_eq!(app.view, View::Output);
    let d = app.diff.as_ref().unwrap();
    assert_eq!(d.title, "hall findings H-0007");
    assert!(
        d.lines
            .iter()
            .any(|l| l.contains("Migration isn't reversible")),
        "{:?}",
        d.lines
    );
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.view, View::Inbox, "esc goes back to your office");

    // …and an action with a placeholder asks for it first.
    let at = actions
        .iter()
        .position(|(c, _)| c.starts_with("hall retry"))
        .unwrap();
    key(&mut app, KeyCode::Char((b'1' + at as u8) as char));
    assert!(
        matches!(app.input, Some(Input::Fill { .. })),
        "{:?}",
        app.input
    );
    chars(&mut app, "the reviewer misread the migration");
    assert_snapshot!("inbox_fill", screen(&env, &app, W, H));
    key(&mut app, KeyCode::Enter);
    let st = app.status.clone().unwrap();
    assert!(!st.error, "{}", st.text);
    let item = hall.load_item(ItemId(7)).unwrap();
    assert_ne!(item.state, State::Escalated);
    assert_eq!(app.inbox().len(), 1, "only the ready? entry is left");

    // Esc on the placeholder runs nothing.
    key(&mut app, KeyCode::Up);
    let at = app
        .inbox_actions()
        .iter()
        .position(|(c, _)| c.contains("<why>"))
        .unwrap();
    key(&mut app, KeyCode::Char((b'1' + at as u8) as char));
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.input, None);
    assert_eq!(hall.load_item(ItemId(2)).unwrap().state, State::AwaitingYou);

    // Enter opens the entry's item; esc returns to the board.
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.view, View::Journal);
    assert_eq!(app.selected, Some(ItemId(2)));
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.view, View::Board);
}

#[test]
fn whats_waiting_reads_in_full() {
    let env = building();
    let summary = "CSV and JSON export of tenant usage, billed by the hour, with 14 tests; \
                   the endpoint streams rows so large tenants don't time out, and the docs \
                   page lists every column with its unit";
    let (_, wt) = env.new_item("Long summary");
    let r = env.hall(&wt, &["propose-ready", "--summary", summary]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let mut app = load(&env, &env.repo);
    select(&mut app, 8);
    // On the board, the item shows what waits for you, wrapped, not cut.
    let s = screen(&env, &app, 80, H);
    let flat = s
        .lines()
        .map(|l| l.trim_matches(|c| c == '│' || c == ' '))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(flat.contains("its unit"), "{s}");
    assert!(!flat.contains("its unit…"), "{s}");
    // So does your office.
    key(&mut app, KeyCode::Char('i'));
    while app.inbox_entry().unwrap().item != Some(ItemId(8)) {
        key(&mut app, KeyCode::Down);
    }
    assert_snapshot!("inbox_long", screen(&env, &app, 80, H));
}

#[test]
fn actions_that_need_the_terminal_hand_it_over() {
    let env = building();
    let mut app = load(&env, &env.repo);
    select(&mut app, 2);
    match app.run_command("claude --resume 8dc2096f") {
        Effect::Run { program, args, cwd } => {
            assert_eq!(program, "claude");
            assert_eq!(args, ["--resume", "8dc2096f"]);
            assert!(cwd.ends_with("app-H-0002"), "{}", cwd.display());
        }
        e => panic!("{e:?}"),
    }
    // Following live and doing it by hand are shown, not run.
    assert_eq!(app.run_command("hall watch H-0002"), Effect::None);
    assert!(
        app.status
            .as_ref()
            .unwrap()
            .text
            .contains("run it in a terminal")
    );
    assert_eq!(
        app.run_command("open the waiting Claude Code session"),
        Effect::None
    );
    assert!(
        app.status
            .as_ref()
            .unwrap()
            .text
            .starts_with("Do this yourself")
    );
    // Commands that don't belong in the app are refused.
    assert_eq!(app.run_command("hall daemon run --once"), Effect::None);
    let st = app.status.clone().unwrap();
    assert!(st.error && st.text.contains("USAGE"), "{}", st.text);
}
