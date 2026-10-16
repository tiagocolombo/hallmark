//! Any other command: the first and last lines, with repeats collapsed.

use super::{Filter, Limits};

pub struct Generic;

impl Filter for Generic {
    fn name(&self) -> &'static str {
        "generic"
    }

    fn applies(&self, _cmd: &[String]) -> bool {
        true
    }

    fn keep(&self, lines: &[&str], limits: &Limits) -> Option<Vec<String>> {
        Some(head_and_tail(lines, limits))
    }
}

/// Runs of one line become `line [xN]` (blank runs become one blank), then
/// the first `head` and last `tail` lines are kept around `... N lines
/// omitted`.
pub fn head_and_tail(lines: &[&str], limits: &Limits) -> Vec<String> {
    let mut runs: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let n = lines[i..].iter().take_while(|l| **l == line).count();
        if n > 1 && !line.trim().is_empty() {
            runs.push(format!("{line} [x{n}]"));
        } else {
            runs.push(line.to_string());
        }
        i += n;
    }
    if runs.len() <= limits.head + limits.tail {
        return runs;
    }
    let omitted = runs.len() - limits.head - limits.tail;
    let mut out: Vec<String> = runs[..limits.head].to_vec();
    out.push(format!("... {omitted} lines omitted"));
    out.extend_from_slice(&runs[runs.len() - limits.tail..]);
    out
}
