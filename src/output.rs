//! The agent contract for output: JSON when stdout is not a terminal (or with
//! `--json`), readable text on a terminal, and a `next` list in every reply.

use std::io::{IsTerminal, Write};

use schemars::JsonSchema;
use serde::Serialize;

use crate::error::HallError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Json,
    Text,
}

impl Mode {
    pub fn detect(force_json: bool) -> Mode {
        if force_json || !std::io::stdout().is_terminal() {
            Mode::Json
        } else {
            Mode::Text
        }
    }
}

/// Human-readable rendering for a terminal.
pub trait Render {
    fn render(&self) -> String;
}

/// Every successful reply: the command's data, flattened, plus `next`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Out<T> {
    #[serde(flatten)]
    pub data: T,
    /// Commands that make sense now.
    pub next: Vec<String>,
}

/// A rendered reply, ready to print.
#[derive(Debug)]
pub struct Response {
    pub value: serde_json::Value,
    pub text: String,
    /// Raw replies (hook output) are printed as-is in every mode.
    pub raw: bool,
    pub exit: i32,
}

impl Response {
    pub fn new<T: Serialize + Render>(data: T, next: Vec<String>) -> Response {
        let mut text = data.render();
        if !next.is_empty() {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str("\nnext:\n");
            for n in &next {
                text.push_str("  ");
                text.push_str(n);
                text.push('\n');
            }
        }
        let value = serde_json::to_value(Out { data, next })
            .unwrap_or_else(|e| serde_json::json!({ "error": e.to_string() }));
        Response {
            value,
            text,
            raw: false,
            exit: 0,
        }
    }

    /// Output for Claude Code hooks: printed verbatim, or nothing at all.
    pub fn raw(value: Option<serde_json::Value>) -> Response {
        Response {
            text: value.as_ref().map(|v| v.to_string()).unwrap_or_default(),
            value: value.unwrap_or(serde_json::Value::Null),
            raw: true,
            exit: 0,
        }
    }

    pub fn print(&self, mode: Mode) {
        let mut out = std::io::stdout().lock();
        if self.raw {
            if !self.value.is_null() {
                let _ = writeln!(out, "{}", self.value);
            }
            return;
        }
        match mode {
            Mode::Json => {
                let _ = writeln!(out, "{}", self.value);
            }
            Mode::Text => {
                let _ = write!(out, "{}", self.text);
                if !self.text.ends_with('\n') {
                    let _ = writeln!(out);
                }
            }
        }
    }
}

pub fn print_error(mode: Mode, err: &HallError) {
    match mode {
        Mode::Json => {
            let v = serde_json::to_string(&err.envelope()).unwrap_or_default();
            println!("{v}");
        }
        Mode::Text => {
            let mut e = std::io::stderr().lock();
            let _ = writeln!(e, "error[{}]: {}", err.code.as_str(), err.message);
            for n in &err.next {
                let _ = writeln!(e, "  try: {n}");
            }
        }
    }
}

/// Shorten text for one-line display.
pub fn clip(s: &str, max: usize) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        one_line
    } else {
        let mut t: String = one_line.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct D {
        id: &'static str,
    }
    impl Render for D {
        fn render(&self) -> String {
            format!("item {}", self.id)
        }
    }

    #[test]
    fn next_is_always_present() {
        let r = Response::new(D { id: "H-0001" }, vec![]);
        assert_eq!(r.value, serde_json::json!({"id": "H-0001", "next": []}));
    }

    #[test]
    fn clip_keeps_short_text() {
        assert_eq!(clip("a  b\nc", 10), "a b c");
        assert_eq!(clip("abcdefghijkl", 5), "abcd…");
    }
}
