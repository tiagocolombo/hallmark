//! Condensing command output: lossless on disk, lossy in context (ADR 0046).
//!
//! A command's full output belongs in a log on disk; what an agent reads is
//! a condensed view of it that keeps what a fixer acts on: the exit code,
//! every failing test or error with its file:line, the first message block
//! of each failure, the tool's own summary line, and distinct warnings once
//! with a count. Progress, spinners, ANSI, "Compiling" lines, passing tests,
//! repeated lines, download logs and timing go. [`footer`] says how to read
//! the rest.
//!
//! The rules, in [`condense`]:
//!
//! - **Evidence is never condensed.** `git diff`, `git show` and `git log`
//!   pass through untouched ([`is_evidence`]). The list is hard-wired; it can
//!   grow, never shrink.
//! - **Small output is untouched.** At most [`Limits::threshold_bytes`] and
//!   [`Limits::threshold_lines`], it passes verbatim, ANSI and all.
//! - **Deterministic.** A [`Filter`] is a pure function over text, picked by
//!   the command line ([`pick`]), pinned by fixture logs and snapshots in
//!   `tests/condense.rs`. A family filter that doesn't recognise the output
//!   falls back to the generic one, and a result that isn't smaller than the
//!   input is replaced by the input.
//! - **Measured honestly.** [`Condensed`] counts the bytes and lines kept and
//!   dropped; tokens saved are an estimate (bytes/4) and say so.
//!
//! This is a library: nothing calls it yet. `hall run` and the gates use it
//! later.
//!
//! Credit: the shape of the filters (one per tool family, picked from the
//! command line, falling back to a generic head and tail) and the idea of
//! measuring the gain come from rtk (<https://github.com/rtk-ai/rtk>). Nothing
//! of rtk is embedded or required.

pub mod cargo;
pub mod generic;
pub mod go;
pub mod jest;
pub mod pytest;

use schemars::JsonSchema;
use serde::Serialize;
use std::path::Path;

/// When output is small enough to pass verbatim, and how much of it the
/// generic filter keeps. No settings yet: `[condense]` comes with `hall run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Limits {
    /// Output of at most this many bytes (and lines) passes verbatim.
    pub threshold_bytes: usize,
    /// Output of at most this many lines (and bytes) passes verbatim.
    pub threshold_lines: usize,
    /// Lines the generic filter keeps from the start.
    pub head: usize,
    /// Lines the generic filter keeps from the end.
    pub tail: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            threshold_bytes: 2048,
            threshold_lines: 60,
            head: 20,
            tail: 40,
        }
    }
}

/// Lines and bytes of some text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Counts {
    pub lines: usize,
    pub bytes: usize,
}

impl Counts {
    pub fn of(text: &str) -> Counts {
        Counts {
            lines: text.lines().count(),
            bytes: text.len(),
        }
    }
}

/// What an agent reads instead of the raw output, and what that saved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Condensed {
    /// The condensed text, or the raw output when it passed verbatim.
    pub text: String,
    /// Lines and bytes of `text`.
    pub kept: Counts,
    /// Lines and bytes of the raw output that `text` leaves out.
    pub dropped: Counts,
}

impl Condensed {
    /// The raw output, untouched.
    pub fn verbatim(raw: &str) -> Condensed {
        Condensed {
            text: raw.to_string(),
            kept: Counts::of(raw),
            dropped: Counts::default(),
        }
    }

    /// Kept lines, then `exit code: N`; the raw output instead when that
    /// isn't smaller.
    fn from_lines(raw: &str, mut lines: Vec<String>, exit: i32) -> Condensed {
        lines.push(format!("exit code: {exit}"));
        let mut text = lines.join("\n");
        text.push('\n');
        if text.len() >= raw.len() {
            return Condensed::verbatim(raw);
        }
        let kept = Counts::of(&text);
        let all = Counts::of(raw);
        Condensed {
            dropped: Counts {
                lines: all.lines.saturating_sub(kept.lines),
                bytes: all.bytes - kept.bytes,
            },
            kept,
            text,
        }
    }

    /// Tokens saved, estimated as dropped bytes / 4. An estimate: no
    /// tokenizer is run.
    pub fn tokens_saved_estimate(&self) -> usize {
        self.dropped.bytes / 4
    }

    /// One line for people and agents: bytes in and out, and the estimate,
    /// labelled as one.
    pub fn measure(&self) -> String {
        let bytes_in = self.kept.bytes + self.dropped.bytes;
        let lines_in = self.kept.lines + self.dropped.lines;
        format!(
            "condensed {bytes_in} bytes ({lines_in} lines) to {} bytes ({} lines); ~{} tokens saved (estimate: bytes/4)",
            self.kept.bytes,
            self.kept.lines,
            self.tokens_saved_estimate()
        )
    }
}

/// One family of tools: which command lines it takes, and what it keeps.
pub trait Filter: Sync {
    /// A short name, such as `cargo-test`.
    fn name(&self) -> &'static str;

    /// Whether this filter is for `cmd` (argv, or one shell string).
    fn applies(&self, cmd: &[String]) -> bool;

    /// The lines to keep from cleaned output (no ANSI, no `\r`), or `None`
    /// when the output isn't recognisably this tool's.
    fn keep(&self, lines: &[&str], limits: &Limits) -> Option<Vec<String>>;

    /// Condenses `raw`: small output passes verbatim; otherwise the output is
    /// cleaned, filtered (by the generic filter when this one doesn't know
    /// it), and ends with the exit code.
    fn condense(&self, raw: &str, exit: i32, limits: &Limits) -> Condensed {
        if is_small(raw, limits) {
            return Condensed::verbatim(raw);
        }
        let clean = clean(raw);
        let lines: Vec<&str> = clean.lines().collect();
        let kept = self
            .keep(&lines, limits)
            .unwrap_or_else(|| generic::head_and_tail(&lines, limits));
        Condensed::from_lines(raw, kept, exit)
    }
}

static FILTERS: &[&dyn Filter] = &[
    &cargo::Test,
    &cargo::Build,
    &cargo::Fmt,
    &pytest::Pytest,
    &jest::Jest,
    &go::GoTest,
];

/// The filter for `cmd`, or the generic one.
pub fn pick(cmd: &[String]) -> &'static dyn Filter {
    FILTERS
        .iter()
        .copied()
        .find(|f| f.applies(cmd))
        .unwrap_or(&generic::Generic)
}

/// Git subcommands whose output is evidence: it passes verbatim, always.
/// This list may grow; nothing may remove from it.
pub const EVIDENCE: &[&str] = &["diff", "show", "log"];

/// Whether `cmd` runs a git command whose output is evidence (see
/// [`EVIDENCE`]), anywhere in the command line.
pub fn is_evidence(cmd: &[String]) -> bool {
    let words = words(cmd);
    (0..words.len()).any(|i| {
        is_program(&words[i], "git")
            && git_subcommand(&words[i + 1..]).is_some_and(|s| EVIDENCE.contains(&s))
    })
}

/// Condenses the output of `cmd`: evidence and small output pass verbatim,
/// everything else goes through [`pick`]'s filter.
pub fn condense(cmd: &[String], raw: &str, exit: i32, limits: &Limits) -> Condensed {
    if is_evidence(cmd) {
        return Condensed::verbatim(raw);
    }
    pick(cmd).condense(raw, exit, limits)
}

/// The line under condensed output that says where the rest is.
pub fn footer(log: &Path) -> String {
    let p = log.display();
    format!(
        "full log: {p} · show a part: hall run --show {p} [--grep <re>] [--around <re> -n 40] [--tail 200]"
    )
}

fn is_small(raw: &str, limits: &Limits) -> bool {
    raw.len() <= limits.threshold_bytes && raw.lines().count() <= limits.threshold_lines
}

/// Output as a terminal would have shown it: ANSI escapes removed, CRLF made
/// LF, and of a line redrawn with `\r` (a progress bar) only its last state.
pub fn clean(raw: &str) -> String {
    let text = strip_ansi(raw);
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while !rest.is_empty() {
        let (line, nl) = match rest.find('\n') {
            Some(i) => (&rest[..i], true),
            None => (rest, false),
        };
        rest = if nl { &rest[line.len() + 1..] } else { "" };
        let line = line.strip_suffix('\r').unwrap_or(line);
        let shown = line.rsplit('\r').find(|s| !s.is_empty()).unwrap_or("");
        out.push_str(shown);
        if nl {
            out.push('\n');
        }
    }
    out
}

fn strip_ansi(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters and intermediates, then one final byte.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            // OSC (hyperlinks, titles): up to BEL or ESC \.
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            // Character set and other escapes with intermediates: ESC ( B.
            Some(c) if ('\u{20}'..='\u{2f}').contains(&c) => {
                while chars
                    .peek()
                    .is_some_and(|c| ('\u{20}'..='\u{2f}').contains(c))
                {
                    chars.next();
                }
                chars.next();
            }
            _ => {}
        }
    }
    out
}

/// The command line as words. A whole command line in one argument (the
/// only one, or a shell's `-c` or `/C`) is split as a shell would; other
/// arguments stay whole (`C:\Program Files\Git\cmd\git.exe`).
fn words(cmd: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, arg) in cmd.iter().enumerate() {
        let script = cmd.len() == 1
            || i > 0 && matches!(cmd[i - 1].as_str(), "-c" | "/C" | "/c" | "-Command");
        if script && arg.contains(char::is_whitespace) {
            match shlex::split(arg) {
                Some(ws) => out.extend(ws),
                None => out.extend(arg.split_whitespace().map(String::from)),
            }
        } else {
            out.push(arg.clone());
        }
    }
    out
}

/// Whether `word` runs `name`: `cargo`, `/usr/bin/cargo`, `cargo.exe`.
fn is_program(word: &str, name: &str) -> bool {
    let base = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let base = base
        .strip_suffix(".exe")
        .or_else(|| base.strip_suffix(".EXE"))
        .or_else(|| base.strip_suffix(".cmd"))
        .unwrap_or(base);
    base == name
}

/// The first word after `program` in `words` that isn't an option.
fn subcommand<'a>(words: &'a [String], program: &str) -> Option<&'a str> {
    let i = words.iter().position(|w| is_program(w, program))?;
    let mut rest = words[i + 1..].iter();
    while let Some(w) = rest.next() {
        if matches!(w.as_str(), "--color" | "--config" | "-Z" | "-C") {
            rest.next();
        } else if !w.starts_with('-') && !w.starts_with('+') {
            return Some(w);
        }
    }
    None
}

/// Every subcommand of `program` in `words` (`cargo build && cargo test`).
fn subcommands<'a>(words: &'a [String], program: &str) -> Vec<&'a str> {
    (0..words.len())
        .filter(|&i| is_program(&words[i], program))
        .filter_map(|i| subcommand(&words[i..], program))
        .collect()
}

/// Git's subcommand after its global options (`git -C dir --no-pager log`).
fn git_subcommand(after: &[String]) -> Option<&str> {
    let mut rest = after.iter();
    while let Some(w) = rest.next() {
        match w.as_str() {
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" => {
                rest.next();
            }
            w if w.starts_with('-') => {}
            w => return Some(w),
        }
    }
    None
}

/// Keeps at most `max` lines of `block`, then says how many it left out.
fn capped(block: &[String], max: usize) -> Vec<String> {
    if block.len() <= max {
        return block.to_vec();
    }
    let mut out = block[..max].to_vec();
    out.push(format!("... {} more lines", block.len() - max));
    out
}
