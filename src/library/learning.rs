//! A learning: a Markdown file with frontmatter in `.hallmark/knowledge/`.
//!
//! ```text
//! ---
//! id: L-0017
//! kind: rule      # rule · false-positive · map · decision
//! scope: ["apps/api/src/webhooks/**"]
//! lens: correctness
//! source: [H-0042/F1]
//! status: accepted
//! used: 4         # reviews it informed
//! last_used: 2026-10-09
//! ---
//! # A retry must check that the endpoint still exists
//!
//! A deleted endpoint keeps its queued deliveries until the job notices.
//! ```
//!
//! The parser is tolerant: comments, quoted or bare list items, YAML block
//! lists, unknown keys (kept as they are), and a title that is either a `#`
//! heading or the first paragraph. The writer is canonical, so a file the
//! librarian touches comes out in one shape.

use std::fmt;

use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The four kinds of learning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// From a finding that turned out right.
    Rule,
    /// What not to flag, and why: withdrawn findings, upheld disputes.
    FalsePositive,
    /// What a module does, so reviewers stop re-exploring it.
    Map,
    /// Your ruling on an escalation, with your reasons.
    Decision,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Rule, Kind::FalsePositive, Kind::Map, Kind::Decision];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Rule => "rule",
            Kind::FalsePositive => "false-positive",
            Kind::Map => "map",
            Kind::Decision => "decision",
        }
    }

    /// Accepts `false-positive`, `false_positive`, `False positive`, ….
    pub fn parse(s: &str) -> Option<Kind> {
        let k: String = s
            .trim()
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        Kind::ALL
            .into_iter()
            .find(|kind| kind.as_str().replace('-', "") == k)
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a learning is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Waiting for your approval (`hall library review`).
    Proposed,
    /// Used in briefings once it is on the base branch.
    Accepted,
    /// Kept for its history; never used.
    Retired,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Proposed => "proposed",
            Status::Accepted => "accepted",
            Status::Retired => "retired",
        }
    }

    pub fn parse(s: &str) -> Option<Status> {
        match s.trim().to_ascii_lowercase().as_str() {
            "proposed" => Some(Status::Proposed),
            "accepted" => Some(Status::Accepted),
            "retired" => Some(Status::Retired),
            _ => None,
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `L-0017`, numbered per repository.
pub fn format_id(n: u32) -> String {
    format!("L-{n:04}")
}

/// Accepts `L-0017`, `l-17`, `L17` and `17`.
pub fn parse_id(s: &str) -> Option<u32> {
    let t = s.trim();
    let digits = t
        .strip_prefix("L-")
        .or_else(|| t.strip_prefix("l-"))
        .or_else(|| t.strip_prefix('L'))
        .or_else(|| t.strip_prefix('l'))
        .unwrap_or(t);
    digits.parse::<u32>().ok().filter(|n| *n > 0)
}

/// The canonical form of an ID as typed, or `None`.
pub fn normalize_id(s: &str) -> Option<String> {
    parse_id(s).map(format_id)
}

/// The ID a file name starts with: `L-0017-retry-checks-endpoint.md`.
pub fn id_from_file_name(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("L-")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|n| *n > 0)
}

/// `L-0017-<slug>.md`.
pub fn file_name(id: &str, title: &str) -> String {
    format!("{id}-{}.md", crate::commands::slug(title))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Learning {
    /// `L-0017`.
    pub id: String,
    pub kind: Kind,
    /// Globs of the paths it applies to; empty for anywhere.
    pub scope: Vec<String>,
    /// The review lens it informs (correctness, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lens: Option<String>,
    /// Where it came from: `H-0042/F1`, `H-0042`.
    pub source: Vec<String>,
    pub status: Status,
    /// Reviews it informed.
    pub used: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<NaiveDate>,
    pub title: String,
    pub body: String,
    /// Frontmatter keys Hallmark doesn't know, kept verbatim.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(skip)]
    pub extra: Vec<(String, String)>,
}

/// Why a file is not a learning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Learning {
    /// Parses a learning file. `fallback_id` (from the file name) is used
    /// when the frontmatter has no `id`.
    pub fn parse(text: &str, fallback_id: Option<&str>) -> Result<Learning, ParseError> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut lines = text.lines();
        let first = lines.next().unwrap_or("");
        if first.trim_end() != "---" {
            return Err(ParseError(
                "no frontmatter (the file must start with ---)".into(),
            ));
        }
        let mut front: Vec<&str> = Vec::new();
        let mut closed = false;
        for l in lines.by_ref() {
            if l.trim_end() == "---" {
                closed = true;
                break;
            }
            front.push(l);
        }
        if !closed {
            return Err(ParseError("the frontmatter is not closed with ---".into()));
        }
        let rest: Vec<&str> = lines.collect();
        let fields = parse_frontmatter(&front);

        let mut id = None;
        let mut kind = None;
        let mut scope = Vec::new();
        let mut lens = None;
        let mut source = Vec::new();
        let mut status = None;
        let mut used = 0;
        let mut last_used = None;
        let mut extra = Vec::new();
        for (key, value) in fields {
            match key.as_str() {
                "id" => id = normalize_id(&value.scalar()),
                "kind" => {
                    kind =
                        Some(Kind::parse(&value.scalar()).ok_or_else(|| {
                            ParseError(format!("unknown kind `{}`", value.scalar()))
                        })?)
                }
                "scope" => scope = value.list(),
                "lens" => lens = Some(value.scalar()).filter(|s| !s.is_empty()),
                "source" => source = value.list(),
                "status" => {
                    status = Some(Status::parse(&value.scalar()).ok_or_else(|| {
                        ParseError(format!("unknown status `{}`", value.scalar()))
                    })?)
                }
                "used" => used = value.scalar().parse().unwrap_or(0),
                "last_used" => last_used = parse_date(&value.scalar()),
                _ => extra.push((key, value.raw)),
            }
        }
        let id = id
            .or_else(|| fallback_id.and_then(normalize_id))
            .ok_or_else(|| ParseError("no id".into()))?;
        let kind = kind.ok_or_else(|| ParseError("no kind".into()))?;
        let (title, body) = split_title(&rest);
        if title.is_empty() {
            return Err(ParseError("no title".into()));
        }
        Ok(Learning {
            id,
            kind,
            scope,
            lens,
            source,
            // A file without a status has not been approved.
            status: status.unwrap_or(Status::Proposed),
            used,
            last_used,
            title,
            body,
            extra,
        })
    }

    /// The canonical file text.
    pub fn render(&self) -> String {
        let mut s = String::from("---\n");
        s.push_str(&format!("id: {}\n", self.id));
        s.push_str(&format!("kind: {}\n", self.kind));
        s.push_str(&format!("scope: {}\n", inline_list(&self.scope)));
        if let Some(l) = &self.lens {
            s.push_str(&format!("lens: {}\n", scalar(l)));
        }
        s.push_str(&format!("source: {}\n", inline_list(&self.source)));
        s.push_str(&format!("status: {}\n", self.status));
        s.push_str(&format!("used: {}\n", self.used));
        if let Some(d) = self.last_used {
            s.push_str(&format!("last_used: {}\n", d.format("%Y-%m-%d")));
        }
        for (k, v) in &self.extra {
            if v.contains('\n') {
                s.push_str(&format!("{k}:\n{v}\n"));
            } else {
                s.push_str(&format!("{k}: {v}\n"));
            }
        }
        s.push_str("---\n");
        s.push_str(&format!("# {}\n", one_line(&self.title)));
        let body = self.body.trim();
        if !body.is_empty() {
            s.push('\n');
            s.push_str(body);
            s.push('\n');
        }
        s
    }

    /// The file name for a new learning.
    pub fn file_name(&self) -> String {
        file_name(&self.id, &self.title)
    }

    /// Searchable scope text.
    pub fn scope_text(&self) -> String {
        self.scope.join(" ")
    }
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    let t = s.trim();
    NaiveDate::parse_from_str(t.get(..10).unwrap_or(t), "%Y-%m-%d").ok()
}

/// The title is a leading `# heading`, or else the first paragraph.
fn split_title(rest: &[&str]) -> (String, String) {
    let start = rest.iter().position(|l| !l.trim().is_empty());
    let Some(start) = start else {
        return (String::new(), String::new());
    };
    let first = rest[start].trim();
    if let Some(h) = first.strip_prefix('#') {
        let title = h.trim_start_matches('#').trim().to_string();
        let body = rest[start + 1..].join("\n").trim().to_string();
        return (title, body);
    }
    let end = rest[start..]
        .iter()
        .position(|l| l.trim().is_empty())
        .map(|p| start + p)
        .unwrap_or(rest.len());
    let title = one_line(&rest[start..end].join(" "));
    let body = rest[end..].join("\n").trim().to_string();
    (title, body)
}

// --- frontmatter --------------------------------------------------------------

/// A frontmatter value as written, with helpers to read it as a scalar or a
/// list.
#[derive(Debug, Clone, PartialEq)]
struct Value {
    /// The text after `key:` (comment removed), or the block list lines.
    raw: String,
    /// Items of a YAML block list (`- a`).
    block: Option<Vec<String>>,
}

impl Value {
    fn scalar(&self) -> String {
        unquote(self.raw.trim())
    }

    fn list(&self) -> Vec<String> {
        if let Some(b) = &self.block {
            return b.iter().map(|s| unquote(s.trim())).collect();
        }
        let t = self.raw.trim();
        if t.is_empty() || t == "[]" || t == "~" || t == "null" {
            return Vec::new();
        }
        let inner = t
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(t);
        split_items(inner)
            .into_iter()
            .map(|s| unquote(s.trim()))
            .filter(|s| !s.is_empty())
            .collect()
    }
}

fn parse_frontmatter(lines: &[&str]) -> Vec<(String, Value)> {
    let mut out: Vec<(String, Value)> = Vec::new();
    for line in lines {
        let stripped = strip_comment(line);
        if stripped.trim().is_empty() {
            continue;
        }
        let indented = line.starts_with([' ', '\t']);
        let t = stripped.trim();
        if indented || t.starts_with("- ") || t == "-" {
            // A block list item (or a continuation) of the last key.
            if let Some((_, v)) = out.last_mut() {
                if let Some(item) = t.strip_prefix('-') {
                    v.block
                        .get_or_insert_with(Vec::new)
                        .push(item.trim().into());
                }
                if !v.raw.is_empty() {
                    v.raw.push('\n');
                }
                v.raw.push_str(line.trim_end());
            }
            continue;
        }
        let Some((k, v)) = t.split_once(':') else {
            continue;
        };
        out.push((
            k.trim().to_ascii_lowercase(),
            Value {
                raw: v.trim().to_string(),
                block: None,
            },
        ));
    }
    // A key with a block list keeps only its items as raw text.
    for (_, v) in out.iter_mut() {
        if let Some(b) = &v.block
            && v.raw
                .lines()
                .next()
                .is_some_and(|l| l.trim_start().starts_with('-'))
        {
            v.raw = b
                .iter()
                .map(|i| format!("  - {i}"))
                .collect::<Vec<_>>()
                .join("\n");
        }
    }
    out
}

/// Drops a `# comment`: a `#` at the start or after whitespace, outside
/// quotes.
fn strip_comment(line: &str) -> String {
    let mut out = String::new();
    let mut quote: Option<char> = None;
    let mut prev_space = true;
    for c in line.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                } else if c == '#' && prev_space {
                    break;
                }
            }
        }
        prev_space = c.is_whitespace();
        out.push(c);
    }
    out.trim_end().to_string()
}

/// Splits `a, "b, c", {x,y}` on commas outside quotes and braces.
fn split_items(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut depth = 0i32;
    let mut escaped = false;
    for c in s.chars() {
        if let Some(q) = quote {
            cur.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' && q == '"' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                cur.push(c);
            }
            '{' | '[' => {
                depth += 1;
                cur.push(c);
            }
            '}' | ']' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth <= 0 => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

fn unquote(s: &str) -> String {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        if let Ok(v) = serde_json::from_str::<String>(s) {
            return v;
        }
        return s[1..s.len() - 1].to_string();
    }
    if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        return s[1..s.len() - 1].replace("''", "'");
    }
    s.to_string()
}

/// `["a", "b"]`, always quoted so globs and IDs survive any YAML reader.
fn inline_list(items: &[String]) -> String {
    let quoted: Vec<String> = items
        .iter()
        .map(|i| serde_json::to_string(i).unwrap_or_default())
        .collect();
    format!("[{}]", quoted.join(", "))
}

/// A scalar, quoted only when it would not read back as itself.
fn scalar(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./ &".contains(c))
        && !s.starts_with([' ', '-'])
        && !s.ends_with(' ');
    if plain {
        s.to_string()
    } else {
        serde_json::to_string(s).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example from docs/design.html, as written there.
    const DESIGN: &str = "---
id: L-0017
kind: rule      # rule · false-positive · map · decision
scope: [\"apps/api/src/webhooks/**\"]
lens: correctness
source: [H-0042/F1]
status: accepted
used: 4         # reviews it informed
last_used: 2026-10-09
---
A retry must check that the endpoint still exists
before each attempt. A deleted endpoint keeps its
queued deliveries until the job notices.
";

    #[test]
    fn parses_the_design_example() {
        let l = Learning::parse(DESIGN, None).unwrap();
        assert_eq!(l.id, "L-0017");
        assert_eq!(l.kind, Kind::Rule);
        assert_eq!(l.scope, vec!["apps/api/src/webhooks/**"]);
        assert_eq!(l.lens.as_deref(), Some("correctness"));
        assert_eq!(l.source, vec!["H-0042/F1"]);
        assert_eq!(l.status, Status::Accepted);
        assert_eq!(l.used, 4);
        assert_eq!(l.last_used, NaiveDate::from_ymd_opt(2026, 10, 9));
        assert!(l.title.starts_with("A retry must check"), "{}", l.title);
        assert!(l.title.ends_with("until the job notices."));
        assert!(l.extra.is_empty());
    }

    #[test]
    fn round_trips() {
        let l = Learning::parse(DESIGN, None).unwrap();
        let text = l.render();
        let back = Learning::parse(&text, None).unwrap();
        assert_eq!(back, l);
        assert_eq!(back.render(), text, "rendering is stable");

        let l = Learning {
            id: "L-0003".into(),
            kind: Kind::FalsePositive,
            scope: vec!["src/{a,b}/*.rs".into(), "docs/\"x\".md".into()],
            lens: Some("security_tenancy".into()),
            source: vec!["H-0007/F2".into(), "H-0009".into()],
            status: Status::Proposed,
            used: 0,
            last_used: None,
            title: "Don't flag `unwrap` in tests: #42".into(),
            body: "Tests may panic.\n\n- one\n- two".into(),
            extra: vec![("owner".into(), "platform".into())],
        };
        let back = Learning::parse(&l.render(), None).unwrap();
        assert_eq!(back, l);
    }

    #[test]
    fn tolerates_yaml_variants() {
        let text = "---\r\nid: '12'\r\nkind: False_Positive\r\nscope:\r\n  - src/**\r\n  - \"lib/*.rs\" # the lib\r\nsource: []\r\nweird: [1, 2]\r\n---\r\n\r\n## Title here\r\n\r\nBody.\r\n";
        let l = Learning::parse(text, None).unwrap();
        assert_eq!(l.id, "L-0012");
        assert_eq!(l.kind, Kind::FalsePositive);
        assert_eq!(l.scope, vec!["src/**", "lib/*.rs"]);
        assert_eq!(l.status, Status::Proposed, "no status means proposed");
        assert_eq!(l.title, "Title here");
        assert_eq!(l.body, "Body.");
        assert_eq!(l.extra, vec![("weird".into(), "[1, 2]".into())]);
    }

    #[test]
    fn falls_back_to_the_file_name_id() {
        let l = Learning::parse("---\nkind: map\n---\n# Billing\n", Some("L-0005")).unwrap();
        assert_eq!(l.id, "L-0005");
        assert_eq!(id_from_file_name("L-0005-billing.md"), Some(5));
        assert_eq!(id_from_file_name("README.md"), None);
    }

    #[test]
    fn rejects_what_is_not_a_learning() {
        for bad in [
            "# just markdown\n",
            "---\nid: L-1\nkind: rule\n",
            "---\nid: L-1\nkind: rumour\n---\n# x\n",
            "---\nid: L-1\nkind: rule\n---\n\n",
            "---\nkind: rule\n---\n# x\n",
        ] {
            assert!(Learning::parse(bad, None).is_err(), "{bad}");
        }
    }

    #[test]
    fn ids_and_kinds() {
        for s in ["L-0017", "l-17", "L17", "17"] {
            assert_eq!(normalize_id(s).as_deref(), Some("L-0017"));
        }
        assert_eq!(normalize_id("H-1"), None);
        assert_eq!(Kind::parse("false positive"), Some(Kind::FalsePositive));
        assert_eq!(Kind::parse("Decision"), Some(Kind::Decision));
        assert_eq!(
            file_name("L-0001", "Retry: check the endpoint!"),
            "L-0001-retry-check-the-endpoint.md"
        );
    }
}
