//! Cargo: `cargo test`, `cargo build|check|clippy` and `cargo fmt --check`.

use super::{Filter, Limits, capped, is_program, subcommands, words};

/// `cargo test`: each failing test's result line, its panic message and
/// first repo frame, doc-test failures, the `test result: FAILED` lines, the
/// errors cargo ends with, and the passed count over every target. Compiler
/// errors and warnings are kept as [`Build`] keeps them.
pub struct Test;

/// `cargo build`, `check` and `clippy`: each distinct error and warning once
/// with its file:line, its label and first help or note, a count per file,
/// and cargo's totals.
pub struct Build;

/// `cargo fmt --check` and `rustfmt --check`: the files that differ and the
/// first hunk of each.
pub struct Fmt;

impl Filter for Test {
    fn name(&self) -> &'static str {
        "cargo-test"
    }

    fn applies(&self, cmd: &[String]) -> bool {
        subcommands(&words(cmd), "cargo")
            .iter()
            .any(|s| matches!(*s, "test" | "t"))
    }

    fn keep(&self, lines: &[&str], _limits: &Limits) -> Option<Vec<String>> {
        test(lines)
    }
}

impl Filter for Build {
    fn name(&self) -> &'static str {
        "cargo-build"
    }

    fn applies(&self, cmd: &[String]) -> bool {
        subcommands(&words(cmd), "cargo")
            .iter()
            .any(|s| matches!(*s, "build" | "b" | "check" | "c" | "clippy"))
    }

    fn keep(&self, lines: &[&str], _limits: &Limits) -> Option<Vec<String>> {
        let mut diags = Diagnostics::default();
        let mut rest = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            if let Some(next) = diags.take(lines, i) {
                i = next;
                continue;
            }
            let line = lines[i];
            i += 1;
            if is_summary(line) {
                push_once(&mut rest, line);
            } else if is_error(line) {
                let (block, next) = indented(lines, i);
                rest.push(line.to_string());
                rest.extend(block);
                i = next;
            }
        }
        if diags.is_empty() && rest.is_empty() {
            return None;
        }
        let mut out = diags.render();
        out.extend(rest);
        Some(out)
    }
}

impl Filter for Fmt {
    fn name(&self) -> &'static str {
        "cargo-fmt"
    }

    fn applies(&self, cmd: &[String]) -> bool {
        let words = words(cmd);
        subcommands(&words, "cargo").contains(&"fmt")
            || words.iter().any(|w| is_program(w, "rustfmt"))
    }

    fn keep(&self, lines: &[&str], _limits: &Limits) -> Option<Vec<String>> {
        // (file, first hunk, further hunks), in the order rustfmt prints them.
        let mut files: Vec<(String, Vec<String>, usize)> = Vec::new();
        let mut other = Vec::new();
        let mut in_first = false;
        for line in lines {
            if let Some(file) = diff_file(line) {
                match files.iter_mut().find(|(f, ..)| *f == file) {
                    Some((_, _, more)) => {
                        *more += 1;
                        in_first = false;
                    }
                    None => {
                        files.push((file, vec![line.to_string()], 0));
                        in_first = true;
                    }
                }
            } else if in_first {
                if let Some((_, hunk, _)) = files.last_mut() {
                    hunk.push(line.to_string());
                }
            } else if line.starts_with("error") || line.starts_with("warning") {
                other.push(line.to_string());
            }
        }
        if files.is_empty() {
            return None;
        }
        let mut out = Vec::new();
        for (file, hunk, more) in &files {
            out.extend(capped(hunk, 30));
            if *more > 0 {
                out.push(format!("... {} in {file}", plural(*more, "more hunk")));
            }
        }
        out.extend(other);
        out.push(format!("{} differ", plural(files.len(), "file")));
        Some(out)
    }
}

/// `Diff in /path/src/lib.rs:12:` (or the older `... at line 12:`).
fn diff_file(line: &str) -> Option<String> {
    let rest = line.strip_prefix("Diff in ")?.strip_suffix(':')?;
    let file = match rest.find(" at line ") {
        Some(i) => &rest[..i],
        None => rest
            .trim_end_matches(|c: char| c.is_ascii_digit())
            .trim_end_matches(':'),
    };
    Some(file.to_string())
}

/// Cargo's own status lines (right-aligned verbs), all noise here.
fn is_status(line: &str) -> bool {
    const STATUS: &[&str] = &[
        "Compiling",
        "Checking",
        "Finished",
        "Running",
        "Doc-tests",
        "Downloaded",
        "Downloading",
        "Updating",
        "Locking",
        "Adding",
        "Blocking",
        "Fresh",
        "Documenting",
        "Building",
        "Unpacking",
    ];
    let t = line.trim_start();
    line.starts_with(' ')
        && STATUS
            .iter()
            .any(|s| t.strip_prefix(s).is_some_and(|r| r.starts_with(' ')))
}

/// Cargo's totals: ``warning: `x` (lib) generated 3 warnings``, `error: could
/// not compile ...`.
fn is_summary(line: &str) -> bool {
    (line.starts_with("warning: `") && line.contains(" generated "))
        || line.starts_with("error: could not compile")
}

/// An error or warning cargo prints without a location, which a fixer
/// needs: `error: test failed, ...`, `error: linking with cc failed`.
fn is_error(line: &str) -> bool {
    line.starts_with("error") || line.starts_with("Caused by:")
}

/// The indented lines under `lines[i - 1]` (up to ten), and where the next
/// line starts.
fn indented(lines: &[&str], i: usize) -> (Vec<String>, usize) {
    let mut end = i;
    while end < lines.len()
        && lines[end].starts_with(' ')
        && !lines[end].trim().is_empty()
        && !is_status(lines[end])
    {
        end += 1;
    }
    let block: Vec<String> = lines[i..end].iter().map(|l| l.to_string()).collect();
    (capped(&block, 10), end)
}

fn push_once(out: &mut Vec<String>, line: &str) {
    if !out.iter().any(|l| l == line) {
        out.push(line.to_string());
    }
}

fn is_diagnostic_header(line: &str) -> bool {
    !is_summary(line)
        && (line.starts_with("error[")
            || line.starts_with("warning[")
            || line.starts_with("error: ")
            || line.starts_with("warning: "))
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("{n} {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// Rustc diagnostics, each distinct one once, in order.
#[derive(Default)]
struct Diagnostics {
    list: Vec<Diagnostic>,
}

struct Diagnostic {
    header: String,
    location: String,
    label: Option<String>,
    help: Option<String>,
    count: usize,
}

impl Diagnostics {
    fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Takes the diagnostic starting at `lines[i]`, if one does, and returns
    /// where the next line starts. One without a location (`error: test
    /// failed, ...`, a linker error) isn't taken: it belongs to the caller.
    fn take(&mut self, lines: &[&str], i: usize) -> Option<usize> {
        let header = lines[i];
        if !is_diagnostic_header(header) {
            return None;
        }
        let mut end = i + 1;
        while end < lines.len() && is_continuation(lines[end]) {
            end += 1;
        }
        let block = &lines[i + 1..end];
        let location = block
            .iter()
            .find_map(|l| l.trim_start().strip_prefix("--> "))
            .map(str::to_string)?;
        let label = block.iter().find_map(|l| label(l));
        let help = block.iter().find_map(|l| help(l));
        match self
            .list
            .iter_mut()
            .find(|d| d.header == header && d.location == location)
        {
            Some(d) => d.count += 1,
            None => self.list.push(Diagnostic {
                header: header.to_string(),
                location,
                label,
                help,
                count: 1,
            }),
        }
        Some(end)
    }

    /// Each diagnostic, then the count per file.
    fn render(&self) -> Vec<String> {
        let mut out = Vec::new();
        // (file, errors, warnings), in order of first appearance.
        let mut files: Vec<(String, usize, usize)> = Vec::new();
        for d in &self.list {
            if d.count > 1 {
                out.push(format!("{} [x{}]", d.header, d.count));
            } else {
                out.push(d.header.clone());
            }
            let loc = &d.location;
            out.push(format!("  --> {loc}"));
            let file = loc.split(':').next().unwrap_or(loc).to_string();
            let i = match files.iter().position(|(f, ..)| *f == file) {
                Some(i) => i,
                None => {
                    files.push((file, 0, 0));
                    files.len() - 1
                }
            };
            if d.header.starts_with("error") {
                files[i].1 += 1;
            } else {
                files[i].2 += 1;
            }
            out.extend(d.label.iter().cloned());
            out.extend(d.help.iter().cloned());
        }
        if !files.is_empty() {
            let per: Vec<String> = files
                .iter()
                .map(|(f, e, w)| {
                    let mut parts = Vec::new();
                    if *e > 0 {
                        parts.push(plural(*e, "error"));
                    }
                    if *w > 0 {
                        parts.push(plural(*w, "warning"));
                    }
                    format!("{f} {}", parts.join(", "))
                })
                .collect();
            out.push(format!("per file: {}", per.join("; ")));
        }
        out
    }
}

/// Lines inside one rustc diagnostic: the gutter, numbered source lines,
/// `help:`/`note:` subsections. A blank line or anything else ends it.
fn is_continuation(line: &str) -> bool {
    if line.trim().is_empty() || is_status(line) {
        return false;
    }
    line.starts_with(' ')
        || line.starts_with(|c: char| c.is_ascii_digit())
        || line.starts_with("help:")
        || line.starts_with("note:")
        || line.starts_with("...")
}

/// The text a caret underline carries: ``   | ^ expected `u32`, found `f64` ``.
fn label(line: &str) -> Option<String> {
    let after = line.trim_start().strip_prefix('|')?;
    let start = after.find('^')?;
    let text = after[start..].trim_start_matches('^').trim();
    if text.is_empty() {
        return None;
    }
    Some(format!("   | ^ {text}"))
}

/// The first help or note that isn't boilerplate about lint levels.
fn help(line: &str) -> Option<String> {
    let t = line.trim_start();
    let t = t.strip_prefix("= ").unwrap_or(t);
    if !(t.starts_with("help:") || t.starts_with("note:")) {
        return None;
    }
    let boilerplate = t.contains("implied by `-D warnings`")
        || t.contains("to override `-D warnings`")
        || (t.contains("#[warn(") && t.ends_with("on by default"));
    if boilerplate {
        return None;
    }
    Some(format!("   = {t}"))
}

/// Where `cargo test` output is.
#[derive(PartialEq)]
enum Section {
    /// Between failure sections: results, status lines, diagnostics.
    Outside,
    /// `---- name stdout ----` up to the next one or the name list.
    Failure,
    /// The `failures:` list of names after the sections: the result lines
    /// already carry them.
    Names,
}

fn test(lines: &[&str]) -> Option<Vec<String>> {
    let mut diags = Diagnostics::default();
    let mut out: Vec<String> = Vec::new();
    let mut recognised = false;
    let (mut passed, mut failed, mut ignored, mut targets) = (0, 0, 0, 0);
    let mut section = Section::Outside;
    let mut block: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let starts_section = line.starts_with("---- ") && line.ends_with(" ----");
        if section == Section::Failure {
            let ends = starts_section
                || line == "failures:"
                || line.starts_with("test result:")
                || is_status(line);
            if !ends {
                block.push(line);
                i += 1;
                continue;
            }
            out.extend(failure(&block));
            block.clear();
            section = Section::Outside;
        }
        if starts_section {
            recognised = true;
            out.push(line.to_string());
            section = Section::Failure;
            i += 1;
            continue;
        }
        if section == Section::Names {
            if line.starts_with("    ") {
                i += 1;
                continue;
            }
            section = Section::Outside;
        }
        if let Some(next) = diags.take(lines, i) {
            recognised = true;
            i = next;
            continue;
        }
        if is_panic(line) {
            // Printed inline (`--nocapture`), outside a stdout section.
            recognised = true;
            let mut end = i + 1;
            while end < lines.len()
                && !lines[end].trim().is_empty()
                && !lines[end].starts_with("note: ")
                && !lines[end].starts_with("stack backtrace:")
                && !lines[end].starts_with("test ")
                && !is_panic(lines[end])
            {
                end += 1;
            }
            out.extend(failure(&lines[i..end]));
            i = end;
            continue;
        } else if (line.starts_with("thread '") && line.contains("has overflowed its stack"))
            || line.starts_with("fatal runtime error:")
        {
            recognised = true;
            out.push(line.to_string());
        } else if line == "failures:" {
            if lines.get(i + 1).is_some_and(|l| l.starts_with("    ")) {
                section = Section::Names;
            }
        } else if let Some(r) = line.strip_prefix("test result: ") {
            recognised = true;
            targets += 1;
            passed += count(r, "passed");
            failed += count(r, "failed");
            ignored += count(r, "ignored");
            if !r.starts_with("ok") {
                out.push(line.to_string());
            }
        } else if line.starts_with("test ") && line.ends_with(" FAILED") {
            recognised = true;
            out.push(line.to_string());
        } else if line.starts_with("running ")
            && (line.ends_with(" tests") || line.ends_with(" test"))
        {
            recognised = true;
        } else if is_summary(line) {
            push_once(&mut out, line);
        } else if is_error(line) {
            recognised = true;
            let (block, next) = indented(lines, i + 1);
            out.push(line.to_string());
            out.extend(block);
            i = next;
            continue;
        }
        i += 1;
    }
    if section == Section::Failure {
        out.extend(failure(&block));
    }
    if !recognised {
        return None;
    }
    let mut all = diags.render();
    all.extend(out);
    if targets > 0 {
        all.push(format!(
            "total: {passed} passed; {failed} failed; {ignored} ignored in {}",
            plural(targets, "target")
        ));
    }
    Some(all)
}

/// `N word` out of `FAILED. 3000 passed; 4 failed; ...`.
fn count(result: &str, word: &str) -> usize {
    result
        .split([';', '.'])
        .filter_map(|part| part.trim().strip_suffix(word))
        .find_map(|n| n.trim().parse().ok())
        .unwrap_or(0)
}

/// What a failure section keeps: the panic line and its message (up to ten
/// lines), and the first frame in the repo when it isn't where the panic
/// was. Without a panic (a test returning `Err`, a doc test that didn't
/// compile), its first ten lines.
fn failure(block: &[&str]) -> Vec<String> {
    let Some(p) = block.iter().position(|l| is_panic(l)) else {
        let text: Vec<String> = block
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.to_string())
            .collect();
        return capped(&text, 10);
    };
    let panic = block[p];
    let mut out = vec![panic.to_string()];
    let message: Vec<String> = block[p + 1..]
        .iter()
        .take_while(|l| {
            !l.trim().is_empty() && !l.starts_with("note: ") && !l.starts_with("stack backtrace:")
        })
        .map(|l| l.to_string())
        .collect();
    out.extend(capped(&message, 10));
    let at = panic
        .rsplit(" panicked at ")
        .next()
        .unwrap_or("")
        .trim_end_matches(':');
    if let Some((frame, loc)) = first_repo_frame(&block[p..]) {
        let loc_path = loc.trim_start().trim_start_matches("at ");
        if loc_path.trim_start_matches("./") != at {
            out.push(frame.to_string());
            out.push(loc.to_string());
        }
    }
    out
}

fn is_panic(line: &str) -> bool {
    line.starts_with("thread '") && line.contains(" panicked at ")
}

/// The first backtrace frame whose source is in the repo, not the standard
/// library or a dependency: `   2: crate::f` then `      at ./src/lib.rs:5:9`.
fn first_repo_frame<'a>(block: &[&'a str]) -> Option<(&'a str, &'a str)> {
    block.windows(2).find_map(|w| {
        let at = w[1].trim_start().strip_prefix("at ")?;
        let numbered = w[0]
            .trim_start()
            .split_once(": ")
            .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        let outside = at.starts_with("/rustc/")
            || at.contains("/.cargo/registry/")
            || at.contains("/.cargo/git/")
            || at.contains("/rustlib/")
            || at.contains("\\.cargo\\registry\\");
        (numbered && !outside).then_some((w[0], w[1]))
    })
}
