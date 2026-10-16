//! The condenser (ADR 0046): command output is condensed for an agent's
//! context, never evidence, never small output, and never at the cost of a
//! failure. The fixtures under `tests/fixtures/output/` are real runs (the
//! cargo ones) or the tools' real formats (pytest, jest, vitest, go).

use hallmark::condense::{self, Condensed, Limits};
use insta::assert_snapshot;
use std::path::Path;

fn log(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("output")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn argv(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

/// A fixture, the command that produced it, its exit code, and everything a
/// fixer acts on: every failing test or error, with file:line where the
/// tool gives one.
struct Case {
    file: &'static str,
    cmd: &'static str,
    exit: i32,
    filter: &'static str,
    keep: &'static [&'static str],
}

const CASES: &[Case] = &[
    Case {
        file: "cargo-test.log",
        cmd: "cargo test --no-fail-fast",
        exit: 101,
        filter: "cargo-test",
        keep: &[
            "test tests::parse_rejects_garbage ... FAILED",
            "test tests::sizes_are_sorted ... FAILED",
            "test tests::lookup_finds_nut ... FAILED",
            "test tests::add_carries ... FAILED",
            "test catalog_totals ... FAILED",
            "test src/lib.rs - add (line 5) ... FAILED",
            "Error: \"parsed garbage as 12\"",
            "panicked at src/lib.rs:56:9:",
            "sizes not sorted: [3, 1, 2]",
            "panicked at src/lib.rs:40:30:",
            "called `Option::unwrap()` on a `None` value",
            "panicked at src/lib.rs:35:9:",
            "assertion `left == right` failed: two plus two",
            "panicked at tests/catalog.rs:11:5:",
            "panicked at src/lib.rs:5:1:",
            "test result: FAILED. 3000 passed; 4 failed; 1 ignored",
            "test result: FAILED. 2 passed; 1 failed",
            "test result: FAILED. 1 passed; 1 failed",
            "error: 3 targets failed:",
            "warning: unused variable: `unused`",
            "--> src/lib.rs:18:9",
            "3003 passed",
            "exit code: 101",
        ],
    },
    Case {
        file: "cargo-clippy-errors.log",
        cmd: "cargo clippy",
        exit: 101,
        filter: "cargo-build",
        keep: &[
            "error[E0425]: cannot find value `drivn` in this scope",
            "--> src/ratio.rs:15:30",
            "help: a local variable with a similar name exists",
            "error[E0425]: cannot find value `missing` in this scope",
            "--> src/teeth.rs:13:36",
            "error[E0308]: mismatched types",
            "--> src/ratio.rs:9:18",
            "expected `u32`, found `f64`",
            "error[E0599]: no method named `len` found for type `usize` in the current scope",
            "--> src/teeth.rs:8:11",
            "warning: unused import: `std::fmt::Write`",
            "--> src/ratio.rs:2:5",
            "--> src/ratio.rs:1:5",
            "--> src/teeth.rs:1:5",
            "error: could not compile `gears` (lib) due to 4 previous errors; 3 warnings emitted",
        ],
    },
    Case {
        file: "cargo-clippy-deny.log",
        cmd: "cargo clippy --all-targets -- -D warnings",
        exit: 101,
        filter: "cargo-build",
        keep: &[
            "error: unused import: `std::collections::HashMap`",
            "--> src/ratio.rs:1:5",
            "error: unused variable: `scale`",
            "--> src/ratio.rs:4:9",
            "error: unused variable: `spare`",
            "--> src/teeth.rs:10:9",
            "error: unneeded `return` statement",
            "--> src/ratio.rs:9:5",
            "--> src/teeth.rs:6:5",
            "error: length comparison to zero",
            "--> src/ratio.rs:14:8",
            "error: writing `&Vec` instead of `&[_]` involves a new object where a slice will do",
            "--> src/teeth.rs:1:21",
            "--> src/teeth.rs:9:23",
            "error: the loop variable `i` is only used to index `teeth`",
            "--> src/teeth.rs:3:14",
            "error: could not compile `sprockets` (lib) due to 9 previous errors",
            "error: could not compile `sprockets` (lib test) due to 9 previous errors",
        ],
    },
    Case {
        file: "cargo-fmt.log",
        cmd: "cargo fmt --check",
        exit: 1,
        filter: "cargo-fmt",
        keep: &[
            "Diff in /home/dev/src/sprockets/src/ratio.rs:1:",
            "+pub fn ratio(driver: u32, driven: u32) -> f64 {",
            "Diff in /home/dev/src/sprockets/src/teeth.rs:1:",
            "Diff in /home/dev/src/sprockets/tests/ratio.rs:1:",
            "3 files differ",
        ],
    },
    Case {
        file: "pytest.log",
        cmd: "uv run pytest",
        exit: 1,
        filter: "pytest",
        keep: &[
            "FAILED tests/test_api.py::test_create_order - assert 422 == 201",
            "FAILED tests/test_catalog.py::test_search_by_tag",
            "FAILED tests/test_pricing.py::test_discount_rounding",
            "ERROR tests/test_db.py::test_migrations_apply - KeyError: 'DATABASE_URL'",
            "E       assert 422 == 201",
            "tests/test_api.py:88: AssertionError",
            "E       AttributeError: 'list' object has no attribute 'split'",
            "inventory/catalog.py:27: AttributeError",
            "E       AssertionError: assert Decimal('16.9915') == Decimal('16.99')",
            "tests/test_pricing.py:63: AssertionError",
            "E           KeyError: 'DATABASE_URL'",
            "tests/conftest.py:14:",
            "3 failed, 209 passed, 1 skipped, 3 warnings, 1 error",
            "2 distinct warnings",
        ],
    },
    Case {
        file: "jest.log",
        cmd: "npx jest --ci",
        exit: 1,
        filter: "jest",
        keep: &[
            "FAIL  src/cart/cart.test.ts",
            "● Cart › applies a percentage discount",
            "Expected: 16.99",
            "Received: 16.9915",
            "at Object.toBe (src/cart/cart.test.ts:42:42)",
            "● Cart › removes an item by sku",
            "+ Received  + 24",
            "FAIL  src/api/client.test.ts",
            "● api client › retries on 503",
            "Exceeded timeout of 5000 ms for a test.",
            "at it (src/api/client.test.ts:20:3)",
            "Test Suites: 2 failed, 9 passed, 11 total",
            "Tests:       3 failed, 87 passed, 90 total",
        ],
    },
    Case {
        file: "vitest.log",
        cmd: "pnpm vitest run",
        exit: 1,
        filter: "jest",
        keep: &[
            "FAIL  src/cart/cart.test.ts > Cart > applies a percentage discount",
            "AssertionError: expected 16.9915 to be 16.99 // Object.is equality",
            "❯ src/cart/cart.test.ts:42:42",
            "FAIL  src/cart/cart.test.ts > Cart > removes an item by sku",
            "❯ src/cart/cart.test.ts:57:24",
            "Test Files  1 failed | 10 passed (11)",
            "Tests  2 failed | 88 passed (90)",
        ],
    },
    Case {
        file: "go-test.log",
        cmd: "go test -v ./...",
        exit: 1,
        filter: "go-test",
        keep: &[
            "--- FAIL: TestParseQty (0.00s)",
            "qty_test.go:21: ParseQty(\"2x\") = 0, <nil>; want an error",
            "--- FAIL: TestRound (0.00s)",
            "--- FAIL: TestRound/half_even (0.00s)",
            "round_test.go:34: Round(2.5) = 3, want 2",
            "--- FAIL: TestLookup (0.00s)",
            "panic: runtime error: index out of range [3] with length 3",
            "/home/dev/src/inventory/catalog/catalog.go:41",
            "FAIL\texample.com/inventory/pricing",
            "FAIL\texample.com/inventory/catalog",
            "orders/orders_test.go:12:9: undefined: NewLedger",
            "orders/orders_test.go:27:15: invalid operation",
            "FAIL\texample.com/inventory/orders [build failed]",
            "ok  \texample.com/inventory/users",
            "ok  \texample.com/inventory/warehouse",
        ],
    },
    Case {
        file: "generic.log",
        cmd: "./scripts/migrate.sh staging",
        exit: 1,
        filter: "generic",
        keep: &[
            "waiting for postgres on db.staging.internal:5432 ... [x45]",
            "lines omitted",
            "ERROR:  could not create unique index \"items_sku_key\"",
            "DETAIL:  Key (sku)=(A-100) is duplicated.",
            "error: migration 0071_add_sku_index failed; rolled back",
            "1 of 71 migrations failed",
        ],
    },
];

fn run(case: &Case) -> Condensed {
    condense::condense(
        &argv(case.cmd),
        &log(case.file),
        case.exit,
        &Limits::default(),
    )
}

#[test]
fn each_command_picks_its_family() {
    for case in CASES {
        assert_eq!(
            condense::pick(&argv(case.cmd)).name(),
            case.filter,
            "{}",
            case.cmd
        );
    }
    for (cmd, family) in [
        ("cargo +nightly test -p hallmark", "cargo-test"),
        ("cargo t", "cargo-test"),
        ("cargo build --release", "cargo-build"),
        ("cargo check --all-targets", "cargo-build"),
        ("rustfmt --check src/lib.rs", "cargo-fmt"),
        ("python -m pytest -x", "pytest"),
        ("npm test", "jest"),
        ("yarn vitest", "jest"),
        ("go vet ./...", "generic"),
        ("make", "generic"),
    ] {
        assert_eq!(condense::pick(&argv(cmd)).name(), family, "{cmd}");
    }
    // A command line in one string, as `sh -c` runs it, and Windows paths.
    let one = vec!["sh".into(), "-c".into(), "cd app && cargo test".into()];
    assert_eq!(condense::pick(&one).name(), "cargo-test");
    let exe = vec![r"C:\Users\dev\.cargo\bin\cargo.exe".into(), "clippy".into()];
    assert_eq!(condense::pick(&exe).name(), "cargo-build");
}

#[test]
fn every_failure_survives_condensing() {
    for case in CASES {
        let raw = log(case.file);
        let c = run(case);
        assert_ne!(c.text, raw, "{} was not condensed", case.file);
        assert!(
            c.kept.bytes < raw.len(),
            "{}: {} >= {}",
            case.file,
            c.kept.bytes,
            raw.len()
        );
        for want in case.keep {
            assert!(
                c.text.contains(want),
                "{}: lost {want:?}\n---\n{}",
                case.file,
                c.text
            );
        }
    }
}

#[test]
fn the_big_cargo_log_condenses_under_80_lines() {
    let raw = log("cargo-test.log");
    assert!(raw.lines().count() > 3000);
    let c = condense::condense(
        &argv("cargo test --no-fail-fast"),
        &raw,
        101,
        &Limits::default(),
    );
    assert!(
        c.text.lines().count() < 80,
        "{} lines:\n{}",
        c.text.lines().count(),
        c.text
    );
    assert!(!c.text.contains("... ok"));
    assert!(!c.text.contains("Compiling"));
    assert!(!c.text.contains("Running"));
    assert!(!c.text.contains("Finished"));
}

#[test]
fn counts_measure_bytes_and_lines_honestly() {
    let raw = log("cargo-test.log");
    let c = condense::condense(&argv("cargo test"), &raw, 101, &Limits::default());
    assert_eq!(c.kept.bytes, c.text.len());
    assert_eq!(c.kept.lines, c.text.lines().count());
    assert_eq!(c.kept.bytes + c.dropped.bytes, raw.len());
    assert_eq!(c.kept.lines + c.dropped.lines, raw.lines().count());
    assert_eq!(c.tokens_saved_estimate(), c.dropped.bytes / 4);
    let m = c.measure();
    assert!(m.contains(&raw.len().to_string()), "{m}");
    assert!(m.contains(&c.text.len().to_string()), "{m}");
    assert!(m.contains("estimate"), "{m}");
    assert!(m.contains("bytes/4"), "{m}");
}

#[test]
fn output_under_the_threshold_passes_verbatim() {
    let limits = Limits::default();
    let small = log("cargo-build-small.log");
    assert!(small.len() <= limits.threshold_bytes);
    assert!(small.lines().count() <= limits.threshold_lines);
    let c = condense::condense(&argv("cargo build"), &small, 101, &limits);
    assert_eq!(c.text, small);
    assert_eq!(c.dropped.bytes, 0);
    assert_eq!(c.dropped.lines, 0);
    // Even a filter called directly leaves small output alone, ANSI and all.
    let colored = "\u{1b}[32mok\u{1b}[0m\r\n";
    let f = condense::pick(&argv("go test ./..."));
    assert_eq!(f.condense(colored, 0, &limits).text, colored);
    assert_eq!(condense::condense(&[], "", 0, &limits).text, "");
}

#[test]
fn output_that_would_not_shrink_comes_back_verbatim() {
    let raw: String = (0..70)
        .map(|i| format!("{}\n", ["a", "b", "c"][i % 3]))
        .collect();
    let limits = Limits::default();
    assert!(raw.lines().count() > limits.threshold_lines);
    let c = condense::condense(&argv("make"), &raw, 1, &limits);
    assert_eq!(c.text, raw);
    assert_eq!(c.dropped, condense::Counts::default());
}

#[test]
fn the_threshold_boundaries_are_exact() {
    let limits = Limits::default();
    let cmd = argv("make");
    let lines = |n: usize| -> String { (0..n).map(|i| format!("line number {i}\n")).collect() };
    // 60 lines: verbatim. 61: condensed.
    let at = lines(60);
    assert!(at.len() < limits.threshold_bytes);
    assert_eq!(condense::condense(&cmd, &at, 0, &limits).text, at);
    let over = lines(61);
    assert!(over.len() < limits.threshold_bytes);
    let tight_lines = Limits {
        head: 2,
        tail: 2,
        ..limits
    };
    assert_ne!(condense::condense(&cmd, &over, 0, &tight_lines).text, over);
    assert_eq!(condense::condense(&cmd, &at, 0, &tight_lines).text, at);
    // Exactly 2,048 bytes in few lines: verbatim. 2,049: condensed.
    let row = "x".repeat(99) + "\n";
    let bytes = |n: usize| -> String {
        let mut s = row.repeat(20);
        s.push_str(&"y".repeat(n - s.len() - 1));
        s.push('\n');
        s
    };
    let at = bytes(limits.threshold_bytes);
    assert_eq!(at.len(), 2048);
    assert_eq!(condense::condense(&cmd, &at, 0, &limits).text, at);
    // Narrow lines so condensing shrinks it: 2,049 bytes over 3 lines is
    // too few to drop, so use the byte limit with a smaller head and tail.
    let tight = Limits {
        head: 2,
        tail: 2,
        ..limits
    };
    let many: String = (0..30).map(|i| format!("{i:0>67}\n")).collect();
    assert_eq!(many.len(), 2040);
    let over = format!("{many}12345678\n");
    assert_eq!(over.len(), 2049);
    assert_ne!(condense::condense(&cmd, &over, 0, &tight).text, over);
    let at = format!("{many}1234567\n");
    assert_eq!(at.len(), 2048);
    assert_eq!(condense::condense(&cmd, &at, 0, &tight).text, at);
}

#[test]
fn cargo_test_keeps_inline_panics_and_crashes() {
    let mut raw = String::from("running 80 tests\n");
    for i in 0..70 {
        raw.push_str(&format!("test tests::passing_number_{i} ... ok\n"));
    }
    raw.push_str("thread 'tests::boom' panicked at src/lib.rs:5:9:\n");
    raw.push_str("assertion failed: left == right\n");
    raw.push_str("test tests::boom ... FAILED\n");
    raw.push_str("thread 'tests::deep' has overflowed its stack\n");
    raw.push_str("fatal runtime error: stack overflow\n");
    raw.push_str("error: test failed, to rerun pass `--lib`\n");
    let c = condense::condense(
        &argv("cargo test -- --nocapture"),
        &raw,
        101,
        &Limits::default(),
    );
    for want in [
        "src/lib.rs:5:9",
        "assertion failed: left == right",
        "tests::boom ... FAILED",
        "tests::deep' has overflowed its stack",
        "fatal runtime error: stack overflow",
    ] {
        assert!(c.text.contains(want), "{want}\n{}", c.text);
    }
    assert!(c.text.lines().count() < 20, "{}", c.text);
}

#[test]
fn evidence_commands_pass_verbatim() {
    let big = log("cargo-test.log");
    for cmd in [
        argv("git diff"),
        argv("git diff --stat main...HEAD"),
        argv("git -C /home/dev/src show HEAD"),
        argv("git --no-pager log -p -5"),
        argv("git -c core.pager=cat log"),
        argv("/usr/bin/git show"),
        vec![r"C:\Program Files\Git\cmd\git.exe".into(), "diff".into()],
        vec!["sh".into(), "-c".into(), "git diff | cat".into()],
        vec!["git diff origin/main".into()],
        argv("bash -lc 'git diff'"),
        vec!["bash".into(), "-lc".into(), "git diff".into()],
        vec!["sh".into(), "-ec".into(), "git show HEAD".into()],
        vec!["zsh".into(), "-ic".into(), "git log".into()],
        vec!["powershell".into(), "-command".into(), "git log".into()],
        vec!["cd app;git diff".into()],
        vec!["cd app&&git log".into()],
        vec!["sh".into(), "-c".into(), "(git show)".into()],
    ] {
        assert!(condense::is_evidence(&cmd), "{cmd:?}");
        let c = condense::condense(&cmd, &big, 0, &Limits::default());
        assert_eq!(c.text, big, "{cmd:?}");
        assert_eq!(c.dropped.bytes, 0);
    }
    for cmd in ["git status", "git fetch", "cargo test", "diff a b", "git"] {
        assert!(!condense::is_evidence(&argv(cmd)), "{cmd}");
    }
}

#[test]
fn the_footer_names_the_log() {
    let p = Path::new("logs").join("H-0001").join("cargo-test.log");
    let f = condense::footer(&p);
    let shown = p.display().to_string();
    assert!(f.starts_with(&format!("full log: {shown}")), "{f}");
    assert!(f.contains(&format!("hall run --show {shown}")), "{f}");
    assert!(f.contains("[--grep <re>]"), "{f}");
    assert!(f.contains("[--around <re> -n 40]"), "{f}");
    assert!(f.contains("[--tail 200]"), "{f}");
}

#[test]
fn crlf_ansi_and_carriage_return_progress_condense_like_plain_lf() {
    for case in CASES {
        let plain = run(case).text;
        let crlf = log(case.file).replace('\n', "\r\n");
        let c = condense::condense(&argv(case.cmd), &crlf, case.exit, &Limits::default());
        assert_eq!(c.text, plain, "{} with CRLF", case.file);
    }
    let raw = log("pytest.log")
        .replace("FAILED", "\u{1b}[31mFAILED\u{1b}[0m")
        .replace(
            "collected 214 items",
            "collecting ... \rcollected 214 items",
        )
        .replace("=====", "\u{1b}[1m=====\u{1b}(B\u{1b}[m");
    let c = condense::condense(&argv("pytest"), &raw, 1, &Limits::default());
    assert_eq!(c.text, run(&CASES[4]).text);
}

#[test]
fn clean_strips_ansi_and_overwritten_progress() {
    assert_eq!(condense::clean("\u{1b}[1;31merror\u{1b}[0m: x"), "error: x");
    assert_eq!(condense::clean("\u{1b}(B\u{1b}[mplain"), "plain");
    assert_eq!(
        condense::clean("\u{1b}]8;;https://x\u{7}link\u{1b}]8;;\u{7}"),
        "link"
    );
    assert_eq!(
        condense::clean(" 10%\r 50%\r100%\r\ndone\r\n"),
        "100%\ndone\n"
    );
    assert_eq!(condense::clean("a\r\nb"), "a\nb");
}

#[test]
fn generic_keeps_head_and_tail_and_collapses_repeats() {
    let raw: String = (1..=200)
        .map(|i| format!("line {i}\n"))
        .chain(std::iter::repeat_n("same\n".to_string(), 30))
        .collect();
    let c = condense::condense(&argv("make all"), &raw, 2, &Limits::default());
    let lines: Vec<&str> = c.text.lines().collect();
    assert_eq!(lines[0], "line 1");
    assert_eq!(lines[19], "line 20");
    assert_eq!(lines[20], "... 141 lines omitted");
    assert!(c.text.contains("line 200\nsame [x30]\n"), "{}", c.text);
    assert!(c.text.ends_with("exit code: 2\n"), "{}", c.text);
}

#[test]
fn a_family_filter_falls_back_to_generic_on_output_it_does_not_know() {
    let raw: String = (1..=100).map(|i| format!("step {i}\n")).collect();
    let limits = Limits::default();
    let generic = condense::pick(&argv("make")).condense(&raw, 1, &limits);
    for cmd in [
        "cargo test",
        "cargo build",
        "cargo fmt --check",
        "pytest",
        "jest",
        "go test ./...",
    ] {
        let c = condense::pick(&argv(cmd)).condense(&raw, 1, &limits);
        assert_eq!(c.text, generic.text, "{cmd}");
    }
}

#[test]
fn condensed_fixtures() {
    for case in CASES {
        let c = run(case);
        let name = case.file.trim_end_matches(".log");
        assert_snapshot!(name, format!("{}---\n{}", c.text, c.measure()));
    }
}
