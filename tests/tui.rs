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
use hallmark::item::{HoldKind, ItemId, State};
use hallmark::journal::Hall;
use hallmark::trace::{RunEnd, StepKind};
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
            cached: false,
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
        &[
            "propose-ready",
            "--ask",
            "--summary",
            "CSV export with 14 tests",
        ],
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
            pr_url: None,
            pr_head: None,
            own_worktree: Some(true),
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

/// The "now" line asks whether a daemon runs on this machine (its lock under
/// `HALL_HOME`). In-process, that would be the real one: every test here sees
/// a private `HALL_HOME` whose daemon lock this process holds, so a daemon
/// always runs, whatever the machine is doing.
fn daemon_here() {
    static LOCK: std::sync::OnceLock<std::fs::File> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| {
        let home = std::env::temp_dir().join(format!("hall-tui-{}", std::process::id()));
        // SAFETY: set once, before any test reads it, from inside the
        // OnceLock that every test passes through first.
        unsafe { std::env::set_var("HALL_HOME", &home) };
        hallmark::daemon::state::try_instance_lock()
            .unwrap()
            .expect("a private daemon lock")
    });
}

fn load(env: &Env, cwd: &Path) -> App {
    daemon_here();
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
    // Inbox entry keys (`kind:who:<ms>:<hash>`) carry a time and a hash;
    // blank them at the same width so the layout stays.
    let marker = "hall inbox ";
    let mut out = String::new();
    let mut rest = s.as_str();
    while let Some(i) = rest.find(marker) {
        let (head, tail) = rest.split_at(i + marker.len());
        out.push_str(head);
        let end = tail.find([' ', '"', '│']).unwrap_or(tail.len());
        let (token, after) = tail.split_at(end);
        let parts: Vec<&str> = token.split(':').collect();
        if parts.len() == 4 {
            out.push_str(&format!(
                "{}:{}:{}:{}",
                parts[0],
                parts[1],
                "#".repeat(parts[2].len()),
                "#".repeat(parts[3].len())
            ));
        } else {
            out.push_str(token);
        }
        rest = after;
    }
    out.push_str(rest);
    out
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
    // Development (parent, child), Awaiting you, Review loop, Delivery
    // (the shipped PR #191 isn't merged yet), Closed today.
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
fn delivery_holds_shipped_prs_until_they_merge() {
    let env = building();
    let hall = hall_of(&env);
    let lane =
        |app: &App, c: usize| -> Vec<ItemId> { app.board()[c].iter().map(|r| r.item.id).collect() };
    let mut app = load(&env, &env.repo);
    assert_eq!(lane(&app, 3), vec![ItemId(5)], "Delivery watches PR #191");
    assert_eq!(lane(&app, 4), vec![ItemId(6)], "Closed today");

    // Its CI shows next to the PR.
    add(
        &hall,
        5,
        "watchman",
        Body::PrChecks {
            pr_url: "https://github.com/acme/app/pull/191".into(),
            passed: false,
            failing: vec![],
            ended: None,
            not_started: None,
        },
    );
    app.reload_active();
    assert_eq!(
        lane(&app, 3),
        vec![ItemId(5)],
        "red CI doesn't end the watch"
    );
    select(&mut app, 5);
    assert_snapshot!("board_delivery", screen(&env, &app, W, H));

    // Merged: the watch ends, and it's just closed.
    add(
        &hall,
        5,
        "delivery",
        Body::Delivered {
            pr_url: "https://github.com/acme/app/pull/191".into(),
            outcome: "merged".into(),
        },
    );
    app.reload_active();
    assert!(lane(&app, 3).is_empty());
    // Closed today, by when each closed: H-0006 was dropped after H-0005 shipped.
    assert_eq!(lane(&app, 4), vec![ItemId(6), ItemId(5)]);
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

    // An action that doesn't fit the state isn't run: the status line
    // says why, and the journal is unchanged.
    select(&mut app, 5);
    let before = hall.read_item(ItemId(5)).unwrap().len();
    key(&mut app, KeyCode::Char('p'));
    let st = app.status.clone().unwrap();
    assert!(st.error);
    assert_eq!(st.text, "H-0005 is closed, so it can't come back.");
    assert_eq!(hall.read_item(ItemId(5)).unwrap().len(), before);
}

#[test]
fn an_escalated_item_says_why_and_offers_only_what_fits() {
    let env = building();
    let hall = hall_of(&env);
    let mut app = load(&env, &env.repo);
    select(&mut app, 7);
    assert_eq!(app.item().unwrap().state, State::Escalated);
    // The board shows why it waits, the open findings and the keys that
    // fit; the key bar has `g retry` and no `r ready`.
    assert_snapshot!("board_escalated", screen(&env, &app, W, H));

    // `r` doesn't run `hall ready`: it says what does.
    let before = hall.read_item(ItemId(7)).unwrap().len();
    key(&mut app, KeyCode::Char('r'));
    let st = app.status.clone().unwrap();
    assert!(st.error);
    assert_eq!(
        st.text,
        "H-0007 is escalated, so it can't be sent to review: p pulls it back to development \
         · g allows one more fix round · x drops it."
    );
    assert_eq!(hall.read_item(ItemId(7)).unwrap().len(), before);
    assert_eq!(app.input, None);

    // `g` asks for the reason, as before.
    key(&mut app, KeyCode::Char('g'));
    assert_eq!(app.input, Some(Input::RetryReason(ItemId(7))));
    key(&mut app, KeyCode::Esc);

    // `g` on an item with nothing to retry or lift asks for nothing.
    select(&mut app, 3);
    key(&mut app, KeyCode::Char('g'));
    assert_eq!(app.input, None);
    let st = app.status.clone().unwrap();
    assert!(st.error);
    assert_eq!(
        st.text,
        "H-0003 is in development, with nothing to retry or lift: \
         r sends it to review · x drops it."
    );
}

#[test]
fn go_on_lifts_a_budget_hold_without_changing_the_budget() {
    let env = building();
    let hall = hall_of(&env);
    let cfg = std::fs::read_to_string(env.repo.join(".hallmark.toml")).unwrap();
    let mut app = load(&env, &env.repo);

    // Nothing held: nothing to go on with, so `g` asks for nothing.
    select(&mut app, 2);
    let before = hall.read_item(ItemId(2)).unwrap().len();
    key(&mut app, KeyCode::Char('g'));
    assert_eq!(app.input, None);
    assert!(app.status.as_ref().unwrap().error);

    add(
        &hall,
        2,
        "assayer",
        Body::Held {
            kind: HoldKind::Budget,
            reason: "H-0002 reached its 800k token cap".into(),
        },
    );
    app.reload_active();
    let before = before + 1;

    // A reason is required.
    key(&mut app, KeyCode::Char('g'));
    assert_eq!(app.input, Some(Input::RetryReason(ItemId(2))));
    key(&mut app, KeyCode::Enter);
    assert!(app.status.as_ref().unwrap().error);
    assert_eq!(hall.read_item(ItemId(2)).unwrap().len(), before);

    key(&mut app, KeyCode::Char('g'));
    chars(&mut app, "worth finishing");
    key(&mut app, KeyCode::Enter);
    let st = app.status.clone().unwrap();
    assert!(!st.error, "{}", st.text);
    assert!(st.text.contains("800k more tokens"), "{}", st.text);
    assert_eq!(hall.load_item(ItemId(2)).unwrap().hold, None);
    assert_eq!(app.item().unwrap().hold, None, "reloaded");
    let last = hall.read_item(ItemId(2)).unwrap().pop().unwrap();
    assert_eq!(
        last.body,
        Body::Released {
            reason: Some("worth finishing".into()),
            granted_tokens: Some(800_000),
        }
    );
    assert_eq!(
        std::fs::read_to_string(env.repo.join(".hallmark.toml")).unwrap(),
        cfg
    );
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
    daemon_here();
    let app = App::load(&env.repo, &env.root.join("no-such-registry.json"));
    assert_eq!(app.repos.len(), 1);
    assert_eq!(app.repo().unwrap().name, "app");
    // An empty repo renders, and a tiny terminal doesn't panic.
    let mut app = app;
    app.redact.times = true;
    assert_snapshot!("empty", screen(&env, &app, W, 14));
    for v in ['h', 'u', 'l', 'R', 'f', 't', 'b'] {
        key(&mut app, KeyCode::Char(v));
        render(&app, 20, 5);
        render(&app, 1, 1);
    }
    // Outside any repo, with no registry: nothing to show, no crash.
    daemon_here();
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
fn your_office_drops_an_entrys_item_or_every_item_waiting() {
    let env = building();
    let hall = hall_of(&env);
    // A parent and its parallel part both wait for you.
    for id in ["H-0003", "H-0004"] {
        let r = env.hall(&env.repo, &["ask", "Still wanted?", "--item", id]);
        assert_eq!(r.code, 0, "{}", r.stdout);
    }
    let mut app = load(&env, &env.repo);
    // The daemon stopped: an entry that isn't about an item.
    app.daemon.as_mut().unwrap().heartbeat = None;
    app.daemon.as_mut().unwrap().started_at = None;
    key(&mut app, KeyCode::Char('i'));
    assert!(app.inbox().iter().any(|e| e.kind == "daemon_stopped"));

    // x drops the entry's item, not whatever the board had selected.
    while app.inbox_entry().unwrap().item != Some(ItemId(7)) {
        key(&mut app, KeyCode::Down);
    }
    key(&mut app, KeyCode::Char('x'));
    assert_eq!(app.input, Some(Input::DropReason(ItemId(7))));
    key(&mut app, KeyCode::Esc);
    assert_eq!(hall.load_item(ItemId(7)).unwrap().state, State::Escalated);
    while app.inbox_entry().unwrap().kind != "daemon_stopped" {
        key(&mut app, KeyCode::Down);
    }
    key(&mut app, KeyCode::Char('x'));
    assert_eq!(app.input, None);
    assert!(app.status.as_ref().unwrap().error);

    // X drops every item waiting, once each, parts before their parent.
    key(&mut app, KeyCode::Char('X'));
    assert_eq!(
        app.input,
        Some(Input::DropAll(vec![
            ItemId(7),
            ItemId(4),
            ItemId(3),
            ItemId(2)
        ]))
    );
    chars(&mut app, "clearing the desk");
    assert_snapshot!("inbox_drop_all", screen(&env, &app, W, H));
    key(&mut app, KeyCode::Enter);
    let st = app.status.clone().unwrap();
    assert!(!st.error, "{}", st.text);
    assert_eq!(st.text, "Dropped 4 items: H-0007, H-0004, H-0003, H-0002.");
    for id in [2, 3, 4, 7] {
        let item = hall.load_item(ItemId(id)).unwrap();
        assert_eq!(item.state, State::Dropped, "H-{id:04}");
        assert_eq!(item.drop_reason.as_deref(), Some("clearing the desk"));
    }
    assert_eq!(hall.load_item(ItemId(1)).unwrap().state, State::Review);
    let left: Vec<String> = app.inbox().iter().map(|e| e.kind.clone()).collect();
    assert_eq!(left, ["daemon_stopped"]);

    // Nothing left to drop.
    key(&mut app, KeyCode::Char('X'));
    assert_eq!(app.input, None);
    assert!(app.status.as_ref().unwrap().error);
}

#[test]
fn your_office_drop_all_reports_what_it_could_not_drop() {
    let env = building();
    let hall = hall_of(&env);
    // The parent waits for you; its part doesn't, so the parent can't go.
    let r = env.hall(&env.repo, &["ask", "Still wanted?", "--item", "H-0003"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let mut app = load(&env, &env.repo);
    key(&mut app, KeyCode::Char('i'));
    key(&mut app, KeyCode::Char('X'));
    chars(&mut app, "clearing the desk");
    key(&mut app, KeyCode::Enter);
    let st = app.status.clone().unwrap();
    assert!(st.error, "{}", st.text);
    assert!(st.text.starts_with("Dropped 2 of 3."), "{}", st.text);
    assert!(st.text.contains("H-0003:") && st.text.contains("CHILDREN_OPEN"));
    assert_eq!(hall.load_item(ItemId(3)).unwrap().state, State::Development);
    assert_eq!(hall.load_item(ItemId(7)).unwrap().state, State::Dropped);

    // Esc on the reason drops nothing.
    key(&mut app, KeyCode::Char('X'));
    chars(&mut app, "no");
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.input, None);
    assert_eq!(hall.load_item(ItemId(3)).unwrap().state, State::Development);
}

#[test]
fn whats_waiting_reads_in_full() {
    let env = building();
    let summary = "CSV and JSON export of tenant usage, billed by the hour, with 14 tests; \
                   the endpoint streams rows so large tenants don't time out, and the docs \
                   page lists every column with its unit";
    let (_, wt) = env.new_item("Long summary");
    let r = env.hall(&wt, &["propose-ready", "--ask", "--summary", summary]);
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

#[test]
fn long_journal_lines_wrap_to_two_lines_then_clip() {
    let env = Env::initialized();
    let hall = hall_of(&env);
    env.new_item("Wrap the timeline");
    let short = "Cap retries at six attempts with jitter between them";
    add(&hall, 1, "you", Body::Prompt { text: short.into() });
    let long = "word ".repeat(80) + "THE-END";
    add(&hall, 1, "you", Body::Prompt { text: long });
    let mut app = load(&env, &env.repo);
    key(&mut app, KeyCode::Enter); // the journal view
    let s = render(&app, 80, 30);
    // Nothing is cut at a fixed length: the short prompt wraps, whole.
    let words: String = s
        .lines()
        .map(|l| l.trim_matches(['│', ' ']))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(words.contains(short), "{s}");
    // The long one takes two lines, and the second ends clipped.
    let i = s
        .lines()
        .position(|l| l.contains("prompt · \"word word"))
        .expect(&s);
    let next = s.lines().nth(i + 1).unwrap();
    assert!(
        next.trim_start_matches(['│', ' ']).starts_with("word"),
        "{s}"
    );
    assert!(next.contains('…'), "{s}");
    assert!(!s.contains("THE-END"), "{s}");
    let after = s.lines().nth(i + 2).unwrap_or_default();
    assert!(!after.contains("word word"), "only two lines: {s}");
}

/// The stream fixture: a headless run as `claude -p --output-format
/// stream-json` writes it.
fn stream() -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude/stream.jsonl"),
    )
    .unwrap()
}

/// Writes (or replaces) a run's file in the item's `runs/`, as the runner
/// would.
fn write_run(env: &Env, id: u32, name: &str, text: &str) -> std::path::PathBuf {
    let dir = hall_of(env).item_runs(ItemId(id));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

/// The daemon holds the item, since before its runs began: a stream with no
/// end line is going, not interrupted.
fn hold(env: &Env, id: u32) {
    let hall = hall_of(env);
    let lease = hallmark::lease::Lease {
        item: ItemId(id),
        role: "fixer".into(),
        holder: "daemon:1".into(),
        acquired: "2026-10-12T21:00:00Z".parse().unwrap(),
        expires: "2099-01-01T00:00:00Z".parse().unwrap(),
    };
    std::fs::create_dir_all(hall.leases_dir()).unwrap();
    std::fs::write(
        hall.leases_dir().join(format!("{}.json", ItemId(id))),
        serde_json::to_string(&lease).unwrap(),
    )
    .unwrap();
}

fn append(path: &Path, text: &str) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

#[test]
fn t_follows_the_selected_items_latest_run() {
    let env = building();
    let text = stream();
    // An older run that failed, and the latest one, still going: so far
    // only its start and first tool call, and half of the next line.
    let failed = RunEnd::new(600_000, None, Some("timed out after 600s".into())).line();
    write_run(
        &env,
        1,
        "20261012T200000-reviewer.jsonl",
        &format!("{text}{failed}"),
    );
    let cut = text.find("{\"type\":\"user\"").unwrap();
    let half = &text[cut..cut + 40];
    let latest = write_run(
        &env,
        1,
        "20261012T215146-fixer.jsonl",
        &format!("{}{half}", &text[..cut]),
    );
    hold(&env, 1);
    let mut app = load(&env, &env.repo);
    select(&mut app, 1);

    key(&mut app, KeyCode::Char('t'));
    assert_eq!(app.view, View::Trace);
    let t = app.trace.as_ref().unwrap();
    assert_eq!(t.id, ItemId(1));
    assert_eq!(t.run().unwrap().role, "fixer", "the latest run");
    let kinds: Vec<StepKind> = t.steps.iter().map(|s| s.kind).collect();
    assert_eq!(kinds, [StepKind::Start, StepKind::Tool], "whole lines only");
    assert!(!t.info().ended);
    assert_snapshot!("trace_running", screen(&env, &app, W, 16));

    // The run goes on: the next refresh reads only what's new.
    let offset = app.trace.as_ref().unwrap().tail.offset;
    append(&latest, &text[cut + 40..]);
    assert!(app.poll_trace());
    let t = app.trace.as_ref().unwrap();
    assert!(t.tail.offset > offset);
    assert_eq!(t.steps.len(), 7, "{:?}", t.steps);
    assert!(!app.poll_trace(), "nothing new");
    assert!(screen(&env, &app, W, 16).contains("▸ Bash  Count bytes in note.txt"));

    // Scrolled up, the same steps stay in view as more arrive; end follows.
    key(&mut app, KeyCode::Up);
    assert_eq!(app.trace.as_ref().unwrap().scroll, 1);
    append(&latest, &RunEnd::new(4_000, Some(0), None).line());
    // A change under the journal's directory refreshes it too.
    app.reload_active();
    let t = app.trace.as_ref().unwrap();
    assert_eq!(t.scroll, 2);
    assert!(t.info().ended);
    assert_eq!(t.info().failure, None);
    key(&mut app, KeyCode::End);
    assert_eq!(app.trace.as_ref().unwrap().scroll, 0);
    for _ in 0..50 {
        key(&mut app, KeyCode::PageUp);
    }
    assert_eq!(app.trace.as_ref().unwrap().scroll, 7, "stops at the top");
    key(&mut app, KeyCode::PageDown);
    key(&mut app, KeyCode::PageDown);
    assert_eq!(app.trace.as_ref().unwrap().scroll, 0);
    assert_eq!(
        app.selected,
        Some(ItemId(1)),
        "scrolling never moves the board"
    );
    assert_snapshot!("trace_ended", screen(&env, &app, W, 16));

    // [ goes back to the older run, which failed.
    key(&mut app, KeyCode::Char('['));
    let t = app.trace.as_ref().unwrap();
    assert_eq!(t.at, 0);
    assert_eq!(t.run().unwrap().role, "reviewer");
    assert_eq!(t.info().failure.as_deref(), Some("timed out after 600s"));
    assert_snapshot!("trace_failed", screen(&env, &app, W, 16));
    key(&mut app, KeyCode::Char('['));
    assert_eq!(app.trace.as_ref().unwrap().at, 0, "stays at the oldest");

    // An older run stays put when a new one starts; the newest follows.
    write_run(&env, 1, "20261012T220000-reviewer.jsonl", &text[..cut]);
    app.poll_trace();
    assert_eq!(app.trace.as_ref().unwrap().at, 0);
    key(&mut app, KeyCode::Char(']'));
    key(&mut app, KeyCode::Char(']'));
    assert_eq!(app.trace.as_ref().unwrap().at, 2);
    write_run(&env, 1, "20261012T221000-fixer.jsonl", &text[..cut]);
    app.poll_trace();
    let t = app.trace.as_ref().unwrap();
    assert_eq!(
        (t.at, t.runs.len()),
        (3, 4),
        "moves on to the run that started"
    );
    assert_eq!(t.steps.len(), 2);

    // Esc returns, and so does t, to the view it was opened from.
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.view, View::Board);
    assert!(app.trace.is_none());
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Char('t'));
    assert_eq!(app.view, View::Trace);
    key(&mut app, KeyCode::Char('t'));
    assert_eq!(app.view, View::Journal);
}

#[test]
fn t_on_an_item_without_runs_says_so() {
    let env = building();
    let mut app = load(&env, &env.repo);
    select(&mut app, 3);
    key(&mut app, KeyCode::Char('t'));
    assert_eq!(app.view, View::Trace);
    assert!(app.trace.as_ref().unwrap().runs.is_empty());
    assert!(!app.poll_trace());
    assert_snapshot!("trace_empty", screen(&env, &app, W, 12));
    key(&mut app, KeyCode::Char('['));
    key(&mut app, KeyCode::Up);
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.view, View::Board);

    // A run's file that is still empty waits for its first step.
    write_run(&env, 3, "20261012T215146-intake.jsonl", "");
    key(&mut app, KeyCode::Char('t'));
    assert!(app.trace.as_ref().unwrap().steps.is_empty());
    assert!(screen(&env, &app, W, 12).contains("Waiting for the run's first step"));
}

#[test]
fn now_block_actions_for_runs_open_in_the_app() {
    let env = building();
    let text = stream();
    write_run(&env, 1, "20261012T215146-reviewer.jsonl", &text);
    let mut app = load(&env, &env.repo);
    select(&mut app, 2);
    key(&mut app, KeyCode::Char('i'));

    // Following a run opens the Trace view on its item, never blocks.
    assert_eq!(app.run_command("hall trace H-0001 --follow"), Effect::None);
    assert_eq!(app.view, View::Trace);
    assert_eq!(app.selected, Some(ItemId(1)));
    let t = app.trace.as_ref().unwrap();
    assert_eq!((t.id, t.steps.len()), (ItemId(1), 7));
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.view, View::Inbox, "back where it was run from");
    // Reading a run (the failed-run action) opens the same view; `hall
    // trace` doesn't run inside the app.
    assert_eq!(app.run_command("hall trace H-0001"), Effect::None);
    assert_eq!(app.view, View::Trace);
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.view, View::Inbox);
    assert_eq!(app.run_command("hall trace H-0099 --follow"), Effect::None);
    assert!(app.status.as_ref().unwrap().error);
    assert_eq!(app.view, View::Inbox);

    // Resuming a failed run starts Claude Code where the run ran: here a
    // folder that isn't the item's worktree, with a space in its name.
    let dir = env.root.join("review H-0001");
    std::fs::create_dir_all(&dir).unwrap();
    let cmd = format!("cd \"{}\" && claude --resume 91aedf44", dir.display());
    match app.run_command(&cmd) {
        Effect::Run { program, args, cwd } => {
            assert_eq!(program, "claude");
            assert_eq!(args, ["--resume", "91aedf44"]);
            assert_eq!(cwd, dir);
        }
        e => panic!("{e:?}"),
    }
    // A folder that is gone is shown for you to do.
    let gone = format!(
        "cd {} && claude --resume 91aedf44",
        p(&env.root.join("gone"))
    );
    assert_eq!(app.run_command(&gone), Effect::None);
    assert!(
        app.status
            .as_ref()
            .unwrap()
            .text
            .starts_with("Do this yourself"),
        "{:?}",
        app.status
    );
}
