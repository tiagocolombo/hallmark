//! jest and vitest: each failing test with its message and expect diff (the
//! first 30 lines), and the `Tests:` / `Test Files` summary.

use super::{Filter, Limits, capped, is_program, words};

pub struct Jest;

impl Filter for Jest {
    fn name(&self) -> &'static str {
        "jest"
    }

    /// `jest`, `vitest`, and `npm|pnpm|yarn [run] test`, which usually run
    /// one of them (output that isn't theirs goes to the generic filter).
    fn applies(&self, cmd: &[String]) -> bool {
        let words = words(cmd);
        words.iter().enumerate().any(|(i, w)| {
            if is_program(w, "jest") || is_program(w, "vitest") {
                return true;
            }
            let runner = ["npm", "pnpm", "yarn"].iter().any(|r| is_program(w, r));
            let next = |k: usize| words.get(i + k).map(String::as_str);
            runner
                && (next(1) == Some("test") || (next(1) == Some("run") && next(2) == Some("test")))
        })
    }

    fn keep(&self, lines: &[&str], _limits: &Limits) -> Option<Vec<String>> {
        let mut out: Vec<String> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        let mut block: Option<Vec<&str>> = None;
        let mut recognised = false;
        for &line in lines {
            let t = line.trim();
            let starts_block = t.starts_with("● ") || (t.starts_with("FAIL ") && t.contains(" > "));
            let ends_block = starts_block
                || t.starts_with('⎯')
                || t.starts_with("FAIL ")
                || t.starts_with("PASS ")
                || is_summary(t)
                || t.starts_with("Summary of all failing tests");
            if ends_block && let Some(b) = block.take() {
                close(&b, &mut seen, &mut out);
            }
            if starts_block {
                recognised = true;
                // The console output jest groups under a failing file.
                if t != "● Console" {
                    block = Some(vec![line]);
                }
            } else if t.starts_with("FAIL ") {
                recognised = true;
                if !out.iter().any(|l| l == line) {
                    out.push(line.to_string());
                }
            } else if is_summary(t) {
                recognised = true;
                out.push(line.to_string());
            } else if let Some(b) = block.as_mut() {
                b.push(line);
            }
        }
        if let Some(b) = block.take() {
            close(&b, &mut seen, &mut out);
        }
        recognised.then_some(out)
    }
}

/// Keeps a failure block once: no blank lines, the first stack frame only,
/// at most 30 lines.
fn close(block: &[&str], seen: &mut Vec<String>, out: &mut Vec<String>) {
    let head = block[0].trim().to_string();
    if seen.contains(&head) {
        return;
    }
    seen.push(head);
    let mut kept = Vec::new();
    let mut frames = 0;
    for line in block {
        if line.trim().is_empty() {
            continue;
        }
        if line.trim_start().starts_with("at ") {
            frames += 1;
            if frames > 1 {
                continue;
            }
        }
        kept.push(line.to_string());
    }
    out.extend(capped(&kept, 30));
}

/// jest's `Test Suites:`, `Tests:`, `Snapshots:`; vitest's `Test Files`,
/// `Tests`, `Errors`.
fn is_summary(t: &str) -> bool {
    [
        "Test Suites:",
        "Tests:",
        "Snapshots:",
        "Test Files ",
        "Tests ",
        "Errors ",
    ]
    .iter()
    .any(|p| t.starts_with(p))
}
