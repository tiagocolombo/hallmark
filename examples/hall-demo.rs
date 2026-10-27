//! Seeds fictional repos for recording the `hall` terminal app, then plays
//! the daemon's part so the board moves while it's recorded.
//!
//!   hall-demo seed <root>       repos, items and journals (HALL_HOME must be set)
//!   hall-demo animate <root>    appends events over time, keeps the daemon "alive"

#![allow(clippy::too_many_arguments)]

use hallmark::daemon::nap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration as Std;

use chrono::{DateTime, Duration, Utc};
use hallmark::daemon::state::{self, DaemonState};
use hallmark::event::{Body, CheckRun, Event, Usage, VerdictKind};
use hallmark::findings::{Counts, Dispute, Finding, Lens, PriorRuling, PriorStatus, Severity};
use hallmark::item::{HoldKind, ItemId};
use hallmark::journal::{Hall, Ticket};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn hall_cli(dir: &Path, args: &[&str]) {
    let out = Command::new("hall")
        .current_dir(dir)
        .args(args)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "hall {args:?}: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

fn commit(dir: &Path, file: &str, body: &str, msg: &str) -> String {
    let p = dir.join(file);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", msg]);
    git(dir, &["rev-parse", "HEAD"])
}

fn slug(title: &str) -> String {
    let s: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    s.split('-')
        .filter(|w| !w.is_empty())
        .take(5)
        .collect::<Vec<_>>()
        .join("-")
}

struct Repo {
    hall: Hall,
    path: PathBuf,
    name: String,
}

impl Repo {
    fn open(path: &Path) -> Repo {
        Repo {
            hall: Hall::new(&path.join(".git")),
            path: path.to_path_buf(),
            name: path.file_name().unwrap().to_string_lossy().into(),
        }
    }

    fn at(&self, id: u32, actor: &str, t: DateTime<Utc>, body: Body) {
        let mut e = Event::new(actor, Some(ItemId(id)), body);
        e.t = t;
        self.hall.append_item(ItemId(id), &e).unwrap();
    }

    fn now(&self, id: u32, actor: &str, body: Body) {
        self.at(id, actor, Utc::now(), body);
    }

    fn wt(&self, id: u32) -> PathBuf {
        self.path
            .parent()
            .unwrap()
            .join(format!("{}-H-{id:04}", self.name))
    }

    /// Creates the item's branch and worktree, and its `created` event.
    fn create(
        &self,
        id: u32,
        title: &str,
        t: DateTime<Utc>,
        request: Option<&str>,
        parent: Option<u32>,
    ) -> PathBuf {
        let branch = format!("hm/H-{id:04}-{}", slug(title));
        let wt = self.wt(id);
        git(
            &self.path,
            &[
                "worktree",
                "add",
                "-q",
                &wt.to_string_lossy(),
                "-b",
                &branch,
            ],
        );
        self.at(
            id,
            "you",
            t,
            Body::Created {
                title: title.into(),
                branch,
                worktree: wt.to_string_lossy().into(),
                base: "main".into(),
                issue: None,
                request: request.map(Into::into),
                adopted: false,
                own_worktree: Some(true),
                parent: parent.map(ItemId),
                follow_up: None,
                pr_url: None,
                pr_head: None,
            },
        );
        wt
    }

    fn talk(&self, id: u32, t: DateTime<Utc>, prompt: &str, turn: &str) {
        self.at(
            id,
            "you",
            t,
            Body::Prompt {
                text: prompt.into(),
            },
        );
        self.at(
            id,
            "agent",
            t + Duration::minutes(4),
            Body::Turn {
                summary: turn.into(),
            },
        );
    }

    fn work(&self, id: u32, t: DateTime<Utc>, file: &str, body: &str, msg: &str) -> String {
        let sha = commit(&self.wt(id), file, body, msg);
        self.at(
            id,
            "agent",
            t,
            Body::Commit {
                sha: sha.clone(),
                subject: msg.into(),
                fixes: vec![],
            },
        );
        sha
    }

    fn usage(
        &self,
        id: u32,
        t: DateTime<Utc>,
        room: &str,
        role: &str,
        model: &str,
        tokens: u64,
        cost: f64,
    ) {
        self.at(
            id,
            if role == "dev" { "you" } else { role },
            t,
            Body::Usage(Usage {
                source: if role == "dev" {
                    "transcript"
                } else {
                    "headless"
                }
                .into(),
                room: room.into(),
                role: role.into(),
                model: model.into(),
                input_tokens: tokens / 12,
                output_tokens: tokens / 14,
                cache_read_tokens: tokens - tokens / 12 - tokens / 14 - tokens / 20,
                cache_write_tokens: tokens / 20,
                cost_usd: cost,
                requests: 6,
                turns: Some(9),
                duration_ms: Some(240_000),
            }),
        );
    }

    fn ready(&self, id: u32, t: DateTime<Utc>, sha: &str, round: u32) {
        self.at(
            id,
            "you",
            t,
            Body::Ready {
                sha: sha.into(),
                round,
                note: None,
            },
        );
    }

    fn checks(&self, id: u32, t: DateTime<Utc>, sha: &str, round: u32, passed: bool) {
        let mut runs = vec![
            run("cargo fmt --check", 0, 900),
            run("cargo clippy --all-targets -- -D warnings", 0, 41_000),
        ];
        runs.push(run(
            "cargo test --all-targets",
            if passed { 0 } else { 101 },
            63_000,
        ));
        self.at(
            id,
            "assayer",
            t,
            Body::Checks {
                sha: sha.into(),
                round,
                passed,
                runs,
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn verdict(
        &self,
        id: u32,
        t: DateTime<Utc>,
        sha: &str,
        round: u32,
        escalated: bool,
        findings: Vec<Finding>,
        prior: Vec<PriorRuling>,
        summary: &str,
    ) {
        let mut counts = Counts::default();
        for f in &findings {
            counts.add(f.severity);
        }
        // In this demo, a finding still open from an earlier round is a blocker.
        for p in &prior {
            if p.status == PriorStatus::Open {
                counts.add(Severity::Blocker);
            }
        }
        let verdict = if counts.blocker + counts.major > 0 {
            VerdictKind::Changes
        } else {
            VerdictKind::Approve
        };
        self.usage(
            id,
            t - Duration::minutes(1),
            "assay",
            "reviewer",
            "opus",
            180_000 + 40_000 * round as u64,
            2.4 + 0.3 * round as f64,
        );
        self.at(
            id,
            "assayer",
            t,
            Body::Verdict {
                sha: sha.into(),
                round,
                verdict,
                escalated,
                findings,
                prior,
                counts,
                summary: Some(summary.into()),
                report: None,
                reviewer: Some("opus".into()),
                learnings: vec![],
                base: None,
                since: None,
            },
        );
    }

    fn fixed(
        &self,
        id: u32,
        t: DateTime<Utc>,
        file: &str,
        body: &str,
        fixes: &[&str],
        disputes: Vec<Dispute>,
    ) -> String {
        let sha = commit(
            &self.wt(id),
            file,
            body,
            &format!("fix: address {}", fixes.join(", ")),
        );
        self.usage(
            id,
            t - Duration::minutes(1),
            "repair",
            "fixer",
            "sonnet",
            90_000,
            0.8,
        );
        self.at(
            id,
            "fixer",
            t,
            Body::FixSubmitted {
                sha: sha.clone(),
                fixes: fixes.iter().map(|s| s.to_string()).collect(),
                disputes,
            },
        );
        sha
    }

    fn ship(&self, id: u32, t: DateTime<Utc>, sha: &str, round: u32, pr: u32) {
        self.at(
            id,
            "assayer",
            t,
            Body::Hallmarked {
                sha: sha.into(),
                round,
            },
        );
        self.at(
            id,
            "shipper",
            t + Duration::minutes(1),
            Body::Shipped {
                sha: sha.into(),
                pr_url: Some(format!("https://github.com/acme/{}/pull/{pr}", self.name)),
            },
        );
    }

    fn queue(&self, id: u32, sha: &str, round: u32, t: DateTime<Utc>) {
        self.hall
            .queue(&Ticket {
                id: ItemId(id),
                sha: sha.into(),
                round,
                queued_at: t,
            })
            .unwrap();
    }
}

fn run(cmd: &str, code: i32, ms: u64) -> CheckRun {
    CheckRun {
        command: cmd.into(),
        exit_code: code,
        duration_ms: ms,
        log: None,
        cached: false,
    }
}

fn finding(
    id: &str,
    round: u32,
    lens: Lens,
    severity: Severity,
    file: &str,
    line: u32,
    title: &str,
    scenario: &str,
    fix: &str,
) -> Finding {
    Finding {
        id: id.into(),
        round,
        lens,
        severity,
        file: file.into(),
        line,
        title: title.into(),
        scenario: scenario.into(),
        fix: fix.into(),
    }
}

fn prior(id: &str, status: PriorStatus) -> PriorRuling {
    PriorRuling {
        id: id.into(),
        status,
        note: None,
    }
}

fn new_repo(root: &Path, name: &str) -> Repo {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    git(&path, &["init", "-q", "-b", "main"]);
    std::fs::write(path.join("README.md"), format!("# {name}\n")).unwrap();
    std::fs::write(
        path.join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"),
    )
    .unwrap();
    std::fs::create_dir_all(path.join("src")).unwrap();
    std::fs::write(path.join("src/lib.rs"), "pub mod orders;\n").unwrap();
    git(&path, &["add", "."]);
    git(&path, &["commit", "-q", "-m", "Initial commit"]);
    hall_cli(&path, &["init"]);
    let cfg = path.join(".hallmark.toml");
    let text = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("# daily    = \"4M\"", "daily    = \"4M\"")
        .replace("# per_item = \"800k\"", "per_item = \"800k\"");
    std::fs::write(&cfg, text).unwrap();
    git(&path, &["add", "."]);
    git(&path, &["commit", "-q", "-m", "Adopt Hallmark"]);
    Repo::open(&path)
}

fn ago(d: i64, h: i64, m: i64) -> DateTime<Utc> {
    Utc::now() - Duration::days(d) - Duration::hours(h) - Duration::minutes(m)
}

fn seed(root: &Path) {
    let r = new_repo(root, "storefront");

    // H-0001: a clean first-round approval, merged.
    let t = ago(3, 6, 0);
    r.create(
        1,
        "Paginate the admin order list",
        t,
        Some("The admin order list loads 20k rows. Paginate it, 50 per page."),
        None,
    );
    r.talk(
        1,
        t + Duration::minutes(2),
        "Paginate the admin order list, 50 per page, keyset on created_at",
        "Added keyset pagination with a cursor and 9 tests.",
    );
    let s = r.work(
        1,
        t + Duration::minutes(40),
        "src/admin/orders.rs",
        "pub fn page(cursor: Option<Cursor>) -> Page { todo!() }\n",
        "feat: keyset pagination for admin orders",
    );
    r.usage(
        1,
        t + Duration::minutes(41),
        "workshop",
        "dev",
        "opus",
        640_000,
        5.1,
    );
    r.ready(1, t + Duration::minutes(45), &s, 1);
    r.checks(1, t + Duration::minutes(48), &s, 1, true);
    r.verdict(
        1,
        t + Duration::minutes(60),
        &s,
        1,
        false,
        vec![],
        vec![],
        "Keyset pagination is correct and tested at the page edges.",
    );
    r.ship(1, t + Duration::minutes(61), &s, 1, 184);
    r.at(
        1,
        "shipper",
        t + Duration::minutes(70),
        Body::PrChecks {
            pr_url: "https://github.com/acme/storefront/pull/184".into(),
            passed: true,
            failing: vec![],
            ended: None,
            not_started: None,
        },
    );
    r.at(
        1,
        "shipper",
        t + Duration::hours(3),
        Body::Delivered {
            pr_url: "https://github.com/acme/storefront/pull/184".into(),
            outcome: "merged".into(),
        },
    );
    r.at(
        1,
        "librarian",
        t + Duration::hours(3),
        Body::Distilled {
            into: None,
            learnings: vec!["L-3".into(), "L-4".into()],
        },
    );

    // H-0002: checks failed first, so no review was spent; merged.
    let t = ago(2, 4, 0);
    r.create(2, "Upgrade the Postgres driver", t, None, None);
    r.talk(
        2,
        t + Duration::minutes(1),
        "Upgrade tokio-postgres to 0.8 and fix the breaking changes",
        "Upgraded and migrated the row decoding.",
    );
    let s = r.work(
        2,
        t + Duration::minutes(30),
        "src/db.rs",
        "use tokio_postgres::Row;\n",
        "chore: upgrade tokio-postgres",
    );
    r.usage(
        2,
        t + Duration::minutes(31),
        "workshop",
        "dev",
        "opus",
        420_000,
        3.3,
    );
    r.ready(2, t + Duration::minutes(33), &s, 1);
    r.checks(2, t + Duration::minutes(36), &s, 1, false);
    let s = r.fixed(
        2,
        t + Duration::minutes(50),
        "src/db.rs",
        "use tokio_postgres::{Row, types::Json};\n",
        &["checks"],
        vec![],
    );
    r.checks(2, t + Duration::minutes(53), &s, 2, true);
    r.verdict(
        2,
        t + Duration::minutes(66),
        &s,
        2,
        false,
        vec![],
        vec![],
        "Driver upgrade is mechanical; decoding is covered by the existing tests.",
    );
    r.ship(2, t + Duration::minutes(67), &s, 2, 186);
    r.at(
        2,
        "shipper",
        t + Duration::hours(5),
        Body::Delivered {
            pr_url: "https://github.com/acme/storefront/pull/186".into(),
            outcome: "merged".into(),
        },
    );

    // H-0003: a tenancy blocker, fixed, shipped, PR still open.
    let t = ago(1, 3, 0);
    r.create(3, "Tenant check in the invoice export", t, None, None);
    r.talk(
        3,
        t + Duration::minutes(1),
        "Add a CSV export of invoices for the billing page",
        "Export endpoint with streaming CSV and 6 tests.",
    );
    let s = r.work(
        3,
        t + Duration::minutes(25),
        "src/billing/export.rs",
        "pub fn export(q: Query) -> Csv { todo!() }\n",
        "feat: invoice CSV export",
    );
    r.usage(
        3,
        t + Duration::minutes(26),
        "workshop",
        "dev",
        "opus",
        380_000,
        3.0,
    );
    r.ready(3, t + Duration::minutes(28), &s, 1);
    r.checks(3, t + Duration::minutes(31), &s, 1, true);
    r.verdict(3, t + Duration::minutes(45), &s, 1, false, vec![finding("F1", 1, Lens::SecurityTenancy, Severity::Blocker, "src/billing/export.rs", 18, "The export reads other tenants' invoices", "A user of tenant A passes tenant B's account ID and receives B's invoices: the query filters by account only.", "Scope the query by the session's tenant and add a cross-tenant test.")], vec![], "One blocker: the export query isn't scoped to the tenant.");
    let s = r.fixed(
        3,
        t + Duration::minutes(60),
        "src/billing/export.rs",
        "pub fn export(tenant: TenantId, q: Query) -> Csv { todo!() }\n",
        &["F1"],
        vec![],
    );
    r.checks(3, t + Duration::minutes(63), &s, 2, true);
    r.verdict(
        3,
        t + Duration::minutes(72),
        &s,
        2,
        false,
        vec![],
        vec![prior("F1", PriorStatus::Fixed)],
        "F1 is fixed and covered by a cross-tenant test.",
    );
    r.ship(3, t + Duration::minutes(73), &s, 2, 189);
    r.at(
        3,
        "shipper",
        t + Duration::minutes(85),
        Body::PrChecks {
            pr_url: "https://github.com/acme/storefront/pull/189".into(),
            passed: true,
            failing: vec![],
            ended: None,
            not_started: None,
        },
    );

    // H-0004: dropped.
    let t = ago(1, 1, 0);
    r.create(4, "Legacy cart experiment", t, None, None);
    r.at(
        4,
        "you",
        t + Duration::minutes(20),
        Body::Dropped {
            reason: "superseded by H-0009".into(),
            worktree_removed: false,
        },
    );

    // H-0005: in review, round 2 (the animation approves and ships it).
    let t = ago(0, 2, 30);
    r.create(
        5,
        "Retry failed webhook deliveries",
        t,
        Some("Webhook deliveries that fail are lost. Retry them with backoff."),
        None,
    );
    r.talk(
        5,
        t + Duration::minutes(2),
        "Retry failed webhook deliveries with exponential backoff and jitter",
        "Added a retry queue with backoff, capped at 6 attempts, and 14 tests.",
    );
    let s = r.work(
        5,
        t + Duration::minutes(50),
        "src/webhooks/retry.rs",
        include_str!("hall-demo-files/retry_v1.rs"),
        "feat: retry failed webhook deliveries",
    );
    r.usage(
        5,
        t + Duration::minutes(51),
        "workshop",
        "dev",
        "opus",
        910_000,
        7.4,
    );
    r.ready(5, t + Duration::minutes(55), &s, 1);
    r.checks(5, t + Duration::minutes(58), &s, 1, true);
    r.verdict(5, t + Duration::minutes(75), &s, 1, false, vec![
        finding("F1", 1, Lens::Correctness, Severity::Major, "src/webhooks/retry.rs", 42, "Retries can deliver an event twice", "A delivery times out after the receiver got it; the retry sends it again and the receiver applies it twice.", "Send an idempotency key with each delivery and keep it across retries."),
        finding("F2", 1, Lens::Evidence, Severity::Minor, "tests/webhooks.rs", 88, "No test for the backoff cap", "Nothing shows that attempt 7 is never scheduled.", "Add a test that drives 7 failures and asserts the delivery is parked."),
    ], vec![], "Backoff is right; a timeout can still deliver twice.");
    let s = r.fixed(
        5,
        t + Duration::minutes(100),
        "src/webhooks/retry.rs",
        include_str!("hall-demo-files/retry_v2.rs"),
        &["F1", "F2"],
        vec![],
    );
    r.checks(5, t + Duration::minutes(104), &s, 2, true);

    // H-0006: awaiting you, with a question.
    let t = ago(0, 1, 40);
    r.create(6, "CSV export for orders", t, None, None);
    r.talk(
        6,
        t + Duration::minutes(1),
        "Add a CSV export to the orders page",
        "Streaming CSV export with 11 tests.",
    );
    r.work(
        6,
        t + Duration::minutes(30),
        "src/orders/export.rs",
        "pub fn orders_csv() {}\n",
        "feat: orders CSV export",
    );
    r.usage(
        6,
        t + Duration::minutes(31),
        "workshop",
        "dev",
        "sonnet",
        350_000,
        1.9,
    );
    r.at(
        6,
        "agent",
        t + Duration::minutes(33),
        Body::ProposedReady {
            summary: "Streaming CSV export, 11 tests. One open choice: refunds.".into(),
            why: None,
            how: None,
            tested: None,
            revert: None,
        },
    );
    let qid = r.hall.next_question_id().unwrap();
    r.at(
        6,
        "agent",
        t + Duration::minutes(34),
        Body::Question {
            qid,
            text: "Include refunded orders in the export?".into(),
            options: vec!["include".into(), "exclude".into()],
            default: Some("exclude".into()),
            wait: Some("4h".into()),
            deadline: Some(t + Duration::minutes(34) + Duration::hours(4)),
        },
    );

    // H-0007 and its parallel part H-0008: in development.
    let t = ago(0, 1, 10);
    r.create(
        7,
        "Rate limit the public API",
        t,
        Some("Rate limit the public API per key, and document the headers."),
        None,
    );
    r.create(
        8,
        "Rate limit: document the headers",
        t + Duration::minutes(1),
        None,
        Some(7),
    );
    r.at(
        7,
        "hall",
        t + Duration::minutes(1),
        Body::Planned {
            parallel: true,
            children: vec![ItemId(8)],
            reason: Some("The docs don't depend on the limiter.".into()),
        },
    );
    r.usage(
        7,
        t + Duration::minutes(1),
        "front",
        "intake",
        "haiku",
        6_000,
        0.01,
    );
    r.talk(
        7,
        t + Duration::minutes(3),
        "Token bucket per API key, 600 requests a minute, 429 with Retry-After",
        "Token bucket middleware in progress, 5 tests so far.",
    );
    r.work(
        7,
        t + Duration::minutes(35),
        "src/api/limit.rs",
        "pub struct Bucket { tokens: u32 }\n",
        "feat: token bucket per API key",
    );
    r.usage(
        7,
        t + Duration::minutes(36),
        "workshop",
        "dev",
        "opus",
        520_000,
        4.2,
    );
    r.work(
        8,
        t + Duration::minutes(20),
        "docs/api/limits.md",
        "# Rate limits\n",
        "docs: rate limit headers",
    );

    // H-0009: fixing after review round 1.
    let t = ago(0, 3, 20);
    r.create(9, "Rebuild the cart on the pricing engine", t, None, None);
    r.talk(
        9,
        t + Duration::minutes(2),
        "Move cart totals onto the new pricing engine",
        "Cart totals now come from pricing::quote, 22 tests.",
    );
    let s = r.work(
        9,
        t + Duration::minutes(80),
        "src/cart/totals.rs",
        "pub fn totals(cart: &Cart) -> Quote { todo!() }\n",
        "feat: cart totals from the pricing engine",
    );
    r.usage(
        9,
        t + Duration::minutes(81),
        "workshop",
        "dev",
        "opus",
        1_250_000,
        10.2,
    );
    r.ready(9, t + Duration::minutes(85), &s, 1);
    r.checks(9, t + Duration::minutes(89), &s, 1, true);
    r.verdict(9, ago(0, 0, 12), &s, 1, false, vec![
        finding("F1", 1, Lens::Correctness, Severity::Major, "src/cart/totals.rs", 57, "Discounts apply twice when a coupon and a sale overlap", "A 10% coupon on an item already on sale is applied to the sale price and again to the subtotal.", "Apply item-level discounts once, before order-level ones."),
        finding("F2", 1, Lens::Operability, Severity::Minor, "src/cart/totals.rs", 12, "No metric when the pricing engine falls back", "On a pricing timeout the cart silently uses list prices.", "Count fallbacks and log the cart ID."),
    ], vec![], "The move is clean; overlapping discounts are applied twice.");

    // H-0010: escalated after three rejections.
    let t = ago(0, 5, 0);
    r.create(10, "Migrate order IDs to UUIDs", t, None, None);
    r.talk(
        10,
        t + Duration::minutes(2),
        "Migrate order IDs from bigint to UUIDv7",
        "Migration and backfill written, 8 tests.",
    );
    let mut s = r.work(
        10,
        t + Duration::minutes(60),
        "migrations/0042_order_uuid.sql",
        "ALTER TABLE orders ADD COLUMN uuid uuid;\n",
        "feat: order UUID migration",
    );
    r.usage(
        10,
        t + Duration::minutes(61),
        "workshop",
        "dev",
        "opus",
        700_000,
        5.6,
    );
    r.ready(10, t + Duration::minutes(65), &s, 1);
    for round in 1..=3u32 {
        let base = t + Duration::minutes(65 + 40 * round as i64);
        r.checks(10, base, &s, round, true);
        let f = if round == 1 {
            vec![finding(
                "F1",
                1,
                Lens::DataMigrations,
                Severity::Blocker,
                "migrations/0042_order_uuid.sql",
                9,
                "The migration can't be rolled back",
                "The down migration drops the bigint column after the backfill, so a rollback loses every order reference.",
                "Keep both columns until the switch is verified, and make down restore the bigint IDs.",
            )]
        } else {
            vec![]
        };
        let p = if round == 1 {
            vec![]
        } else {
            vec![prior("F1", PriorStatus::Open)]
        };
        r.verdict(
            10,
            base + Duration::minutes(12),
            &s,
            round,
            round == 3,
            f,
            p,
            "The down migration still loses the bigint IDs.",
        );
        if round < 3 {
            s = r.fixed(
                10,
                base + Duration::minutes(30),
                "migrations/0042_order_uuid.sql",
                &format!("-- attempt {round}\nALTER TABLE orders ADD COLUMN uuid uuid;\n"),
                &["F1"],
                vec![],
            );
        }
    }

    // H-0011: held at its per-item budget.
    let t = ago(0, 4, 0);
    r.create(11, "Cache product images at the edge", t, None, None);
    r.talk(
        11,
        t + Duration::minutes(2),
        "Serve product images through the CDN with immutable cache headers",
        "Signed CDN URLs and cache headers, 7 tests.",
    );
    let s = r.work(
        11,
        t + Duration::minutes(40),
        "src/media/cdn.rs",
        "pub fn url(img: &Image) -> Url { todo!() }\n",
        "feat: product images through the CDN",
    );
    r.usage(
        11,
        t + Duration::minutes(41),
        "workshop",
        "dev",
        "opus",
        600_000,
        4.8,
    );
    r.ready(11, t + Duration::minutes(45), &s, 1);
    r.checks(11, t + Duration::minutes(48), &s, 1, true);
    r.verdict(
        11,
        t + Duration::minutes(62),
        &s,
        1,
        false,
        vec![finding(
            "F1",
            1,
            Lens::SecurityTenancy,
            Severity::Major,
            "src/media/cdn.rs",
            31,
            "Signed URLs never expire",
            "A leaked URL to an unpublished product image stays valid forever.",
            "Sign with a 24h expiry.",
        )],
        vec![],
        "Cache headers are right; signed URLs don't expire.",
    );
    r.fixed(
        11,
        t + Duration::minutes(80),
        "src/media/cdn.rs",
        "pub fn url(img: &Image, ttl: Duration) -> Url { todo!() }\n",
        &["F1"],
        vec![],
    );
    r.at(
        11,
        "hall",
        t + Duration::minutes(81),
        Body::Held {
            kind: HoldKind::Budget,
            reason: "reached its 800k token cap (812k)".into(),
        },
    );

    // H-0012: in development (the animation sends it to review).
    let t = ago(0, 0, 50);
    r.create(12, "Search autocomplete", t, None, None);
    r.talk(
        12,
        t + Duration::minutes(1),
        "Add autocomplete to the product search box, prefix match, top 8",
        "Prefix index and endpoint with 12 tests.",
    );
    r.work(
        12,
        t + Duration::minutes(30),
        "src/search/suggest.rs",
        "pub fn suggest(prefix: &str) -> Vec<String> { vec![] }\n",
        "feat: search autocomplete",
    );
    r.usage(
        12,
        t + Duration::minutes(31),
        "workshop",
        "dev",
        "sonnet",
        410_000,
        2.2,
    );

    for line in [
        "watchman: 12 items, nothing stuck",
        "assayer: running gates for H-0005 round 2",
        "assayer: cargo test --all-targets passed in 63s",
        "assayer: briefed from the Library: 4 learnings",
        "assayer: reviewing H-0005 round 2 with opus (incremental since 3f2a91c)",
        "fixer: H-0009 round 1: addressing F1, F2 with sonnet",
    ] {
        r.hall.live(line);
    }

    // H-0013 and H-0014: shipped, in the merge queue behind H-0003.
    let t = ago(0, 5, 0);
    r.create(13, "Gift card balance endpoint", t, None, None);
    let s = r.work(
        13,
        t + Duration::minutes(30),
        "src/billing/export.rs",
        "pub fn balance() {}\n",
        "feat: gift card balance endpoint",
    );
    r.usage(
        13,
        t + Duration::minutes(31),
        "workshop",
        "dev",
        "opus",
        260_000,
        2.1,
    );
    r.ready(13, t + Duration::minutes(33), &s, 1);
    r.checks(13, t + Duration::minutes(36), &s, 1, true);
    r.verdict(
        13,
        t + Duration::minutes(50),
        &s,
        1,
        false,
        vec![],
        vec![],
        "Balance reads are tenant-scoped and tested.",
    );
    r.ship(13, t + Duration::minutes(51), &s, 1, 190);
    r.at(
        13,
        "shipper",
        t + Duration::minutes(62),
        Body::PrChecks {
            pr_url: "https://github.com/acme/storefront/pull/190".into(),
            passed: true,
            failing: vec![],
            ended: None,
            not_started: None,
        },
    );
    let t = ago(0, 3, 0);
    r.create(14, "Order status webhooks", t, None, None);
    let s = r.work(
        14,
        t + Duration::minutes(30),
        "src/orders/hooks.rs",
        "pub fn notify() {}\n",
        "feat: order status webhooks",
    );
    r.usage(
        14,
        t + Duration::minutes(31),
        "workshop",
        "dev",
        "opus",
        310_000,
        2.4,
    );
    r.ready(14, t + Duration::minutes(33), &s, 1);
    r.checks(14, t + Duration::minutes(36), &s, 1, true);
    r.verdict(
        14,
        t + Duration::minutes(52),
        &s,
        1,
        false,
        vec![],
        vec![],
        "Webhooks are signed and retried; tests cover both.",
    );
    r.ship(14, t + Duration::minutes(53), &s, 1, 191);

    // The merger is on, and this is the plan of its last pass (the demo has
    // no real merger to make one).
    hall_cli(&r.path, &["config", "set", "merge.mode", "auto"]);
    let pr = |n: u32| format!("https://github.com/acme/storefront/pull/{n}");
    let plan = serde_json::json!({
        "last": Utc::now(),
        "items": {},
        "queue": [
            {"position": 1, "item": "H-0003", "title": "Tenant check in the invoice export", "pr_url": pr(189), "base": "main",
             "status": "ready", "why": "ready: hallmarked at 4c1e9a2, mergeable, up to date"},
            {"position": 2, "item": "H-0013", "title": "Gift card balance endpoint", "pr_url": pr(190), "base": "main",
             "status": "waits_for", "why": "ready; first waits for #189 (src/billing/export.rs)", "waits_for": [pr(189)]},
            {"position": 3, "item": "H-0014", "title": "Order status webhooks", "pr_url": pr(191), "base": "main",
             "status": "ci_pending", "why": "CI hasn't passed at the shipped head yet"}
        ]
    });
    std::fs::write(r.hall.root.join("merger.json"), plan.to_string()).unwrap();

    // A second, quieter repo, so the repo switcher has something to show.
    let b = new_repo(root, "billing-api");
    let t = ago(1, 2, 0);
    b.create(1, "Proration for mid-cycle plan changes", t, None, None);
    let s = b.work(
        1,
        t + Duration::minutes(40),
        "src/proration.rs",
        "pub fn prorate() {}\n",
        "feat: proration",
    );
    b.usage(
        1,
        t + Duration::minutes(41),
        "workshop",
        "dev",
        "opus",
        500_000,
        4.0,
    );
    b.ready(1, t + Duration::minutes(45), &s, 1);
    b.checks(1, t + Duration::minutes(48), &s, 1, true);
    b.verdict(
        1,
        t + Duration::minutes(60),
        &s,
        1,
        false,
        vec![],
        vec![],
        "Proration matches the spec table.",
    );
    b.ship(1, t + Duration::minutes(61), &s, 1, 77);
    let t = ago(0, 0, 30);
    b.create(2, "Dunning emails after failed payments", t, None, None);
    b.work(
        2,
        t + Duration::minutes(20),
        "src/dunning.rs",
        "pub fn schedule() {}\n",
        "feat: dunning schedule",
    );

    heartbeat(Some("reviewing H-0005 round 2"));
}

fn heartbeat(current: Option<&str>) {
    let now = Utc::now();
    let mut s = state::load().unwrap_or_else(|| DaemonState {
        pid: 48213,
        started_at: Some(now - Duration::hours(6)),
        wanted: true,
        ..Default::default()
    });
    s.heartbeat = Some(now);
    if let Some(c) = current {
        s.current = Some(c.into());
    }
    state::save(&s).unwrap();
}

/// Plays the daemon: each step waits, then appends what the loop would.
fn animate(root: &Path) {
    // Hold the daemon's single-instance lock, as a running loop would.
    let _lock = state::try_instance_lock().unwrap();
    let r = Repo::open(&root.join("storefront"));
    let tick = |secs: u64, current: Option<&str>| {
        for _ in 0..secs * 2 {
            heartbeat(None);
            nap(Std::from_millis(500));
        }
        if current.is_some() {
            heartbeat(current);
        }
    };
    let head = |id: u32| git(&r.wt(id), &["rev-parse", "HEAD"]);

    tick(4, None);
    // H-0012: the agent says done; nothing to confirm, so it goes to checks.
    let s12 = head(12);
    r.now(
        12,
        "agent",
        Body::ProposedReady {
            summary: "Prefix index and endpoint, 12 tests.".into(),
            why: None,
            how: None,
            tested: None,
            revert: None,
        },
    );
    r.now(
        12,
        "agent",
        Body::Ready {
            sha: s12.clone(),
            round: 1,
            note: Some("nothing left to confirm".into()),
        },
    );
    r.queue(12, &s12, 1, Utc::now());
    r.hall.live("assayer: running gates for H-0012 round 1");
    tick(3, Some("running gates for H-0012 round 1"));

    // H-0005: approved, stamped, shipped.
    let s5 = head(5);
    r.verdict(
        5,
        Utc::now(),
        &s5,
        2,
        false,
        vec![],
        vec![
            prior("F1", PriorStatus::Fixed),
            prior("F2", PriorStatus::Fixed),
        ],
        "F1 and F2 are fixed: deliveries carry an idempotency key, and the cap is tested.",
    );
    r.hall
        .live("assayer: H-0005 approved in round 2; hallmark struck on its commit");
    tick(3, Some("shipping H-0005"));
    r.ship(5, Utc::now(), &s5, 2, 193);
    r.hall
        .live("shipper: rebased H-0005 onto origin/main; opened PR #193");
    tick(3, None);

    // H-0012: checks pass, review starts.
    r.checks(12, Utc::now(), &s12, 1, true);
    r.hall
        .live("assayer: cargo test --all-targets passed in 58s");
    r.hall.live("assayer: reviewing H-0012 round 1 with opus");
    tick(3, Some("reviewing H-0012 round 1"));

    // H-0009: the fixer commits, checks run again.
    let s9 = r.fixed(
        9,
        Utc::now(),
        "src/cart/totals.rs",
        "pub fn totals(cart: &Cart) -> Quote { /* item discounts first */ todo!() }\n",
        &["F1", "F2"],
        vec![],
    );
    r.hall
        .live("fixer: H-0009 fixed F1, F2; back to checks, round 2");
    tick(3, None);
    r.checks(9, Utc::now(), &s9, 2, true);
    r.hall
        .live("assayer: H-0009 round 2 gates passed; queued for review");
    tick(3, None);

    // H-0007: the agent asks you something (the bell rings).
    let qid = r.hall.next_question_id().unwrap();
    r.now(
        7,
        "agent",
        Body::Question {
            qid,
            text: "Limit per API key or per IP for anonymous calls?".into(),
            options: vec!["key".into(), "ip".into()],
            default: Some("ip".into()),
            wait: Some("2h".into()),
            deadline: Some(Utc::now() + Duration::hours(2)),
        },
    );
    r.hall
        .live("hall: H-0007 asks you a question; notified desktop and phone");
    tick(4, None);

    // H-0012: review asks for changes.
    r.verdict(
        12,
        Utc::now(),
        &s12,
        1,
        false,
        vec![finding(
            "F1",
            1,
            Lens::Correctness,
            Severity::Major,
            "src/search/suggest.rs",
            23,
            "Suggestions leak unpublished products",
            "A prefix that matches a draft product returns its name.",
            "Filter the index by published status.",
        )],
        vec![],
        "Prefix matching is fast; drafts leak into suggestions.",
    );
    r.hall
        .live("assayer: H-0012 round 1: changes requested (1 major)");
    tick(120, None);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = PathBuf::from(&args[2]);
    match args[1].as_str() {
        "seed" => seed(&root),
        "animate" => animate(&root),
        "idle" => {
            let _lock = state::try_instance_lock().unwrap();
            for _ in 0..600 {
                heartbeat(None);
                nap(Std::from_millis(500));
            }
        }
        other => panic!("unknown command {other}"),
    }
}
