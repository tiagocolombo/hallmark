//! `go test`: each `--- FAIL` with its output (from `-v` or not, a panic's
//! frames in the repo), build errors, and the FAIL or ok line per package.

use super::{Filter, Limits, capped, subcommands, words};

pub struct GoTest;

impl Filter for GoTest {
    fn name(&self) -> &'static str {
        "go-test"
    }

    fn applies(&self, cmd: &[String]) -> bool {
        subcommands(&words(cmd), "go").contains(&"test")
    }

    fn keep(&self, lines: &[&str], _limits: &Limits) -> Option<Vec<String>> {
        let mut out: Vec<String> = Vec::new();
        let mut recognised = false;
        // Output under `=== RUN` (with -v), by test, until its result.
        let mut output: Vec<(String, Vec<String>)> = Vec::new();
        let mut current: Option<String> = None;
        let mut block: Option<Block> = None;
        for &line in lines {
            let t = line.trim_start();
            let marker = t.starts_with("=== ")
                || t.starts_with("--- ")
                || line.starts_with("FAIL")
                || line.starts_with("ok  ")
                || line.starts_with("PASS")
                || line.starts_with("?   ")
                || line.starts_with("# ");
            if !marker {
                if let Some(b) = block.as_mut() {
                    b.add(line);
                } else if is_go_error(line) {
                    recognised = true;
                    out.push(line.to_string());
                } else if let Some(name) = &current {
                    match output.iter_mut().find(|(n, _)| n == name) {
                        Some((_, o)) => o.push(line.to_string()),
                        None => output.push((name.clone(), vec![line.to_string()])),
                    }
                }
                continue;
            }
            if let Some(b) = block.take() {
                out.extend(capped(&b.kept, 30));
            }
            if let Some(name) = ["=== RUN", "=== CONT", "=== NAME"]
                .iter()
                .find_map(|p| t.strip_prefix(p))
            {
                current = Some(name.trim().to_string());
            } else if let Some(rest) = t.strip_prefix("--- FAIL: ") {
                recognised = true;
                current = None;
                let name = rest.split(" (").next().unwrap_or(rest);
                let mut b = Block {
                    kept: vec![line.to_string()],
                    ..Block::default()
                };
                if let Some(i) = output.iter().position(|(n, _)| n == name) {
                    b.kept.extend(output.remove(i).1);
                }
                block = Some(b);
            } else if t.starts_with("--- ") {
                current = None;
            } else if line.starts_with("FAIL\t")
                || line.starts_with("ok  \t")
                || line.starts_with("# ")
            {
                recognised = true;
                current = None;
                out.push(line.to_string());
            }
        }
        if let Some(b) = block.take() {
            out.extend(capped(&b.kept, 30));
        }
        recognised.then_some(out)
    }
}

/// One failure's lines; in a goroutine trace, frames in the standard library
/// and the test runner are left out.
#[derive(Default)]
struct Block {
    kept: Vec<String>,
    in_trace: bool,
    skip_file: bool,
}

impl Block {
    fn add(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        if line.starts_with("goroutine ") {
            self.in_trace = true;
        } else if self.in_trace {
            if line.starts_with('\t') {
                if self.skip_file {
                    return;
                }
            } else {
                self.skip_file = [
                    "testing.",
                    "runtime.",
                    "panic(",
                    "created by testing.",
                    "created by runtime.",
                ]
                .iter()
                .any(|p| line.starts_with(p));
                if self.skip_file {
                    return;
                }
            }
        }
        self.kept.push(line.to_string());
    }
}

/// A compiler or vet error: `orders/orders_test.go:12:9: undefined: X`.
fn is_go_error(line: &str) -> bool {
    !line.starts_with(char::is_whitespace)
        && line
            .split_once(".go:")
            .is_some_and(|(_, rest)| rest.starts_with(|c: char| c.is_ascii_digit()))
}
