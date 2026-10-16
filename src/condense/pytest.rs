//! pytest: the FAILED and ERROR lines, the assertion block of each failure,
//! the short test summary and the final count; the warnings summary becomes
//! one count.

use super::{Filter, Limits, is_program, words};

pub struct Pytest;

impl Filter for Pytest {
    fn name(&self) -> &'static str {
        "pytest"
    }

    fn applies(&self, cmd: &[String]) -> bool {
        words(cmd)
            .iter()
            .any(|w| is_program(w, "pytest") || is_program(w, "py.test"))
    }

    fn keep(&self, lines: &[&str], _limits: &Limits) -> Option<Vec<String>> {
        let mut out = Vec::new();
        let mut recognised = false;
        let mut part = Part::Other;
        let mut failure = Failure::default();
        let mut warnings: Vec<&str> = Vec::new();
        for &line in lines {
            if let Some(title) = section(line) {
                recognised = true;
                out.extend(failure.take());
                if part == Part::Warnings {
                    out.extend(warning_count(&warnings));
                }
                part = match title {
                    "ERRORS" | "FAILURES" => {
                        out.push(line.to_string());
                        Part::Failures
                    }
                    "warnings summary" => Part::Warnings,
                    "short test summary info" => {
                        out.push(line.to_string());
                        Part::Summary
                    }
                    t if is_count(t) => {
                        out.push(line.to_string());
                        Part::Other
                    }
                    _ => Part::Other,
                };
                continue;
            }
            match part {
                Part::Failures => {
                    if is_test_title(line) {
                        out.extend(failure.take());
                        out.push(line.to_string());
                    } else if line.starts_with("----") && line.contains(" Captured ") {
                        failure.captured = true;
                    } else if !failure.captured {
                        failure.add(line);
                    }
                }
                Part::Warnings => {
                    // `  path:31: DeprecationWarning: ...`, under the tests
                    // it came from and over the source line (indented 4).
                    let t = line.trim();
                    let message = line.starts_with("  ") && !line.starts_with("   ");
                    if message && !warnings.contains(&t) {
                        warnings.push(t);
                    }
                }
                Part::Summary => out.push(line.to_string()),
                Part::Other => {
                    if is_count(line) || line.starts_with("!!!") || line.starts_with("Interrupted:")
                    {
                        recognised = true;
                        out.push(line.to_string());
                    }
                }
            }
        }
        out.extend(failure.take());
        if part == Part::Warnings {
            out.extend(warning_count(&warnings));
        }
        recognised.then_some(out)
    }
}

#[derive(PartialEq)]
enum Part {
    Failures,
    Warnings,
    Summary,
    Other,
}

/// One failure's assertion block: frame locations, the `>` line that failed
/// and the `E` lines under it, and the `file:line: Error` line.
#[derive(Default)]
struct Failure {
    kept: Vec<String>,
    last_source: Option<String>,
    errors: usize,
    captured: bool,
}

impl Failure {
    fn add(&mut self, line: &str) {
        if line.starts_with('>') {
            self.last_source = Some(line.to_string());
        } else if line.starts_with("E ") || line == "E" {
            if let Some(src) = self.last_source.take() {
                self.kept.push(src);
            }
            self.errors += 1;
            if self.errors <= 20 {
                self.kept.push(line.to_string());
            }
        } else if is_location(line) {
            self.kept.push(line.trim_end().to_string());
        }
    }

    fn take(&mut self) -> Vec<String> {
        let mut kept = std::mem::take(&mut self.kept);
        if self.errors > 20 {
            kept.push(format!("... {} more E lines", self.errors - 20));
        }
        *self = Failure::default();
        kept
    }
}

/// `===== FAILURES =====` and the like: the title between the rules.
fn section(line: &str) -> Option<&str> {
    let t = line.strip_prefix("===")?.trim_start_matches('=');
    let t = t.trim_end_matches('=');
    let t = t.trim();
    (!t.is_empty()).then_some(t)
}

/// `____ test_name ____`: one failure (not the `_ _ _` frame separator).
fn is_test_title(line: &str) -> bool {
    line.starts_with("___") && line.ends_with("___") && line.trim_matches('_').trim() != ""
}

/// `tests/test_api.py:88: AssertionError`, `tests/conftest.py:14: `.
fn is_location(line: &str) -> bool {
    if line.starts_with(char::is_whitespace) {
        return false;
    }
    let Some((head, _)) = line
        .split_once(": ")
        .or_else(|| line.trim_end().strip_suffix(':').map(|h| (h, "")))
    else {
        return false;
    };
    let Some((file, n)) = head.rsplit_once(':') else {
        return false;
    };
    !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) && file.ends_with(".py")
}

/// The final count: `3 failed, 209 passed, 1 skipped in 4.82s`.
fn is_count(text: &str) -> bool {
    let words = [" passed", " failed", " error", " skipped", "no tests ran"];
    text.starts_with(|c: char| c.is_ascii_digit() || c == 'n')
        && words.iter().any(|w| text.contains(w))
        && text.contains(" in ")
}

/// The warnings summary, as one line.
fn warning_count(warnings: &[&str]) -> Option<String> {
    let n = warnings.len();
    let s = if n == 1 { "" } else { "s" };
    (n > 0).then(|| format!("warnings summary: {n} distinct warning{s} omitted"))
}
