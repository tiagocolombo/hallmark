//! Usage from `claude -p` results (the `result` line of a stream-json run)
//! and from Claude Code session transcripts.
//!
//! Step 0 findings this module relies on (docs/spikes.md):
//! - `modelUsage` is the source of truth for headless runs. Top-level
//!   `usage` is all zeros for local commands and only holds the last
//!   iteration in multi-turn runs.
//! - A run made with `--json-schema` can end `subtype: success` without a
//!   `structured_output`; that is a failed run.
//! - Transcripts repeat `message.usage` on every content block of one
//!   response, so lines are deduplicated by `requestId`.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde::Deserialize;

use crate::error::{ErrorCode, HallError, Result};
use crate::event::Usage;

/// List prices in USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
}

impl Price {
    /// Cache writes: 1.25x input for the 5-minute TTL, 2x for 1 hour.
    pub fn cost(
        &self,
        input: u64,
        output: u64,
        cache_read: u64,
        write_5m: u64,
        write_1h: u64,
    ) -> f64 {
        let m = 1_000_000.0;
        (input as f64 * self.input
            + output as f64 * self.output
            + cache_read as f64 * self.cache_read
            + write_5m as f64 * self.input * 1.25
            + write_1h as f64 * self.input * 2.0)
            / m
    }
}

/// Anthropic first-party list prices (cached 2026-10-06). Prefix match on
/// the model ID; the most specific prefix wins.
const PRICES: &[(&str, Price)] = &[
    (
        "claude-fable-5-1",
        Price {
            input: 10.0,
            output: 50.0,
            cache_read: 0.25,
        },
    ),
    (
        "claude-mythos-5-1",
        Price {
            input: 10.0,
            output: 50.0,
            cache_read: 0.25,
        },
    ),
    (
        "claude-fable-5",
        Price {
            input: 10.0,
            output: 50.0,
            cache_read: 1.0,
        },
    ),
    (
        "claude-mythos-5",
        Price {
            input: 10.0,
            output: 50.0,
            cache_read: 1.0,
        },
    ),
    (
        "claude-opus-5-5",
        Price {
            input: 4.0,
            output: 20.0,
            cache_read: 0.20,
        },
    ),
    (
        "claude-opus-5",
        Price {
            input: 5.0,
            output: 25.0,
            cache_read: 0.50,
        },
    ),
    (
        "claude-opus-4",
        Price {
            input: 5.0,
            output: 25.0,
            cache_read: 0.50,
        },
    ),
    (
        "claude-sonnet-5-5",
        Price {
            input: 2.0,
            output: 10.0,
            cache_read: 0.20,
        },
    ),
    (
        "claude-sonnet-5",
        Price {
            input: 2.0,
            output: 10.0,
            cache_read: 0.20,
        },
    ),
    (
        "claude-sonnet-4",
        Price {
            input: 3.0,
            output: 15.0,
            cache_read: 0.30,
        },
    ),
    (
        "claude-haiku-5-5",
        Price {
            input: 0.10,
            output: 0.50,
            cache_read: 0.01,
        },
    ),
    (
        "claude-haiku-4",
        Price {
            input: 1.0,
            output: 5.0,
            cache_read: 0.10,
        },
    ),
];

pub fn price(model: &str) -> Option<Price> {
    let m = model.trim().to_ascii_lowercase();
    let m = m.strip_prefix("anthropic.").unwrap_or(&m);
    PRICES
        .iter()
        .filter(|(prefix, _)| m.starts_with(prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, p)| *p)
}

/// "$0.004", "$0.43", "$12.80": small runs keep a significant digit.
pub fn fmt_usd(c: f64) -> String {
    if c > 0.0 && c < 0.01 {
        format!("${c:.3}")
    } else {
        format!("${c:.2}")
    }
}

pub fn fmt_tokens(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.2}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.0}k", n as f64 / 1e3),
        n => n.to_string(),
    }
}

// --- claude -p ------------------------------------------------------------

/// The `type`s of stream-json lines that are events, not the result, plus
/// the runner's own `hall_run_end`. Every run prints them, so they mention
/// models and rate limits (`rate_limit_event` with `"status":"allowed"`)
/// whatever happened.
pub const STREAM_EVENTS: &[&str] = &[
    "system",
    "assistant",
    "user",
    "rate_limit_event",
    "stream_event",
    crate::trace::END,
];

/// The result printed by `claude -p`: the last `result` line of
/// `--output-format stream-json --verbose` (ADR 0039), or the one object of
/// `--output-format json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PrintResult {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub subtype: Option<String>,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub structured_output: Option<serde_json::Value>,
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
    #[serde(default)]
    pub num_turns: Option<u32>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default, rename = "modelUsage")]
    pub model_usage: BTreeMap<String, ModelUsage>,
    #[serde(default)]
    pub permission_denials: Vec<serde_json::Value>,
    /// `completed`, or why the run ended (`api_error`, …).
    #[serde(default)]
    pub terminal_reason: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default, rename = "costUSD")]
    pub cost_usd: f64,
}

impl PrintResult {
    pub fn parse(text: &str) -> Result<PrintResult> {
        if let Ok(r) = serde_json::from_str(text) {
            return Ok(r);
        }
        // stream-json prints a line per event, the result last of Claude's;
        // a run file then ends with the runner's `hall_run_end` (ADR 0039).
        // Take the last `result` line, else the last JSON line that is no
        // stream event (older output, such as an API error's).
        let objects: Vec<serde_json::Value> = text
            .lines()
            .filter(|l| l.trim_start().starts_with('{'))
            .filter_map(|l| serde_json::from_str(l.trim()).ok())
            .collect();
        let kind = |v: &serde_json::Value| {
            v.get("type")
                .and_then(serde_json::Value::as_str)
                .map(String::from)
        };
        let line = objects
            .iter()
            .rev()
            .find(|v| kind(v).as_deref() == Some("result"))
            .or_else(|| {
                objects
                    .iter()
                    .rev()
                    .find(|v| !kind(v).is_some_and(|k| STREAM_EVENTS.contains(&k.as_str())))
            })
            .ok_or_else(|| {
                HallError::new(
                    ErrorCode::CommandFailed,
                    format!(
                        "unreadable claude -p output: no result in {}",
                        crate::output::clip(text.trim(), 200)
                    ),
                )
            })?;
        serde_json::from_value(line.clone()).map_err(|e| {
            HallError::new(
                ErrorCode::CommandFailed,
                format!("unreadable claude -p output: {e}"),
            )
        })
    }

    /// One usage record per model, from `modelUsage`.
    pub fn usage(&self, room: &str, role: &str) -> Vec<Usage> {
        let mut out: Vec<Usage> = self
            .model_usage
            .iter()
            .map(|(model, u)| Usage {
                source: "headless".into(),
                room: room.into(),
                role: role.into(),
                model: model.clone(),
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
                cache_read_tokens: u.cache_read_input_tokens,
                cache_write_tokens: u.cache_creation_input_tokens,
                cost_usd: u.cost_usd,
                requests: 0,
                turns: None,
                duration_ms: None,
            })
            .collect();
        if let Some(first) = out.first_mut() {
            first.turns = self.num_turns;
            first.duration_ms = self.duration_ms;
        }
        out
    }

    /// The schema-checked output. Missing means the run failed, even when
    /// it reports success.
    pub fn structured(&self) -> Result<&serde_json::Value> {
        if self.is_error {
            return Err(HallError::new(
                ErrorCode::CommandFailed,
                format!(
                    "claude -p failed: {}",
                    self.subtype.as_deref().unwrap_or("error")
                ),
            ));
        }
        self.structured_output.as_ref().ok_or_else(|| {
            HallError::new(
                ErrorCode::CommandFailed,
                "claude -p returned no structured_output; the result did not match the schema",
            )
        })
    }
}

// --- transcripts ----------------------------------------------------------

#[derive(Debug, Default)]
pub struct TranscriptUsage {
    pub records: Vec<Usage>,
    /// Bytes consumed (complete lines only).
    pub consumed: u64,
    pub last_request: Option<String>,
}

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(rename = "requestId", default)]
    request_id: Option<String>,
    #[serde(default)]
    message: Option<Msg>,
}

#[derive(Deserialize)]
struct Msg {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<RawUsage>,
}

#[derive(Deserialize, Default)]
struct RawUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_creation: Option<CacheCreation>,
}

#[derive(Deserialize, Default)]
struct CacheCreation {
    #[serde(default)]
    ephemeral_5m_input_tokens: u64,
    #[serde(default)]
    ephemeral_1h_input_tokens: u64,
}

#[derive(Default)]
struct Acc {
    input: u64,
    output: u64,
    read: u64,
    w5: u64,
    w1h: u64,
    requests: u32,
}

/// Sums the usage of assistant messages after `offset`, one count per
/// request. `seen` is the last request counted before, which may straddle
/// the offset.
pub fn transcript_usage(path: &Path, offset: u64, seen: Option<&str>) -> Result<TranscriptUsage> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let start = if offset > len { 0 } else { offset };
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    let end = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let text = String::from_utf8_lossy(&buf[..end]);

    let mut counted: HashSet<String> = HashSet::new();
    if let Some(s) = seen {
        counted.insert(s.to_string());
    }
    let mut by_model: BTreeMap<String, Acc> = BTreeMap::new();
    let mut last = seen.map(str::to_string);
    for l in text.lines() {
        let Ok(line) = serde_json::from_str::<Line>(l) else {
            continue;
        };
        if line.kind != "assistant" {
            continue;
        }
        let Some(msg) = line.message else { continue };
        let Some(u) = msg.usage else { continue };
        let model = msg.model.unwrap_or_else(|| "unknown".into());
        if model.starts_with('<') {
            continue; // <synthetic> messages cost nothing
        }
        let key = line.request_id.or(msg.id);
        if let Some(k) = &key {
            if !counted.insert(k.clone()) {
                continue;
            }
            last = Some(k.clone());
        }
        let (w5, w1h) = match &u.cache_creation {
            Some(c) if c.ephemeral_5m_input_tokens + c.ephemeral_1h_input_tokens > 0 => {
                (c.ephemeral_5m_input_tokens, c.ephemeral_1h_input_tokens)
            }
            _ => (u.cache_creation_input_tokens, 0),
        };
        let a = by_model.entry(model).or_default();
        a.input += u.input_tokens;
        a.output += u.output_tokens;
        a.read += u.cache_read_input_tokens;
        a.w5 += w5;
        a.w1h += w1h;
        a.requests += 1;
    }
    let records = by_model
        .into_iter()
        .map(|(model, a)| Usage {
            source: "transcript".into(),
            room: "workshop".into(),
            role: "dev".into(),
            cost_usd: price(&model).map_or(0.0, |p| p.cost(a.input, a.output, a.read, a.w5, a.w1h)),
            model,
            input_tokens: a.input,
            output_tokens: a.output,
            cache_read_tokens: a.read,
            cache_write_tokens: a.w5 + a.w1h,
            requests: a.requests,
            turns: None,
            duration_ms: None,
        })
        .collect();
    Ok(TranscriptUsage {
        records,
        consumed: start + end as u64,
        last_request: last,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(p: &str) -> String {
        std::fs::read_to_string(format!("{}/tests/fixtures/{p}", env!("CARGO_MANIFEST_DIR")))
            .unwrap()
    }

    #[test]
    fn prices_pick_the_most_specific_prefix() {
        assert_eq!(price("claude-opus-5-5").unwrap().input, 4.0);
        assert_eq!(price("claude-opus-5").unwrap().input, 5.0);
        assert_eq!(price("claude-opus-4-8").unwrap().input, 5.0);
        assert_eq!(price("claude-sonnet-4-6").unwrap().input, 3.0);
        assert_eq!(price("claude-haiku-5-5").unwrap().cache_read, 0.01);
        assert_eq!(
            price("anthropic.claude-fable-5-1").unwrap().cache_read,
            0.25
        );
        assert!(price("gpt-5").is_none());
    }

    #[test]
    fn haiku_price_matches_a_real_run() {
        // tests/fixtures/claude/print_agent.json: 2 in, 18 out, 3743 written (1h).
        let p = price("claude-haiku-5-5").unwrap();
        let cost = p.cost(2, 18, 0, 0, 3743);
        assert!((cost - 0.0007578).abs() < 1e-9, "{cost}");
        // print_result.json: 2 in, 37 out, 8504 read, 21349 written (1h).
        let cost = p.cost(2, 37, 8504, 0, 21349);
        assert!((cost - 0.00437354).abs() < 1e-9, "{cost}");
    }

    #[test]
    fn reads_model_usage_not_top_level_usage() {
        // /code-review reports zeros at the top level.
        let r = PrintResult::parse(&fixture("claude/print_code_review.json")).unwrap();
        let u = r.usage("assay", "reviewer");
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].model, "claude-haiku-5-5");
        assert_eq!(u[0].output_tokens, 1649);
        assert!(u[0].cost_usd > 0.0);
    }

    #[test]
    fn structured_output_is_required() {
        let ok = PrintResult::parse(&fixture("claude/print_structured.json")).unwrap();
        assert_eq!(ok.structured().unwrap()["verdict"], "changes");
        let bad = PrintResult::parse(&fixture("claude/print_schema_mismatch.json")).unwrap();
        assert!(!bad.is_error);
        assert_eq!(bad.structured().unwrap_err().code, ErrorCode::CommandFailed);
    }

    #[test]
    fn a_stream_parses_as_its_result_line_never_the_end_line() {
        // A run file: stream-json, then the runner's `hall_run_end`, which
        // is also the last JSON line (ADR 0039).
        let end = crate::trace::RunEnd::new(3_100, Some(0), None).line();
        let text = format!("{}{end}", fixture("claude/stream.jsonl"));
        let r = PrintResult::parse(&text).unwrap();
        assert_eq!(
            r.session_id.as_deref(),
            Some("91aedf44-f4cd-4efa-8e74-768d46a9dc75")
        );
        assert_eq!(r.subtype.as_deref(), Some("success"));
        assert_eq!(r.num_turns, Some(3));
        assert_eq!(r.usage("assay", "reviewer")[0].output_tokens, 341);
        // A stream that died before its result has none: not the end line,
        // not an assistant line.
        let cut: String = fixture("claude/stream.jsonl")
            .lines()
            .filter(|l| !l.contains("\"type\":\"result\""))
            .map(|l| format!("{l}\n"))
            .collect();
        assert!(PrintResult::parse(&format!("{cut}{end}")).is_err());
    }

    #[test]
    fn resume_keeps_the_session() {
        let first = PrintResult::parse(&fixture("claude/print_result.json")).unwrap();
        let resumed = PrintResult::parse(&fixture("claude/print_resume.json")).unwrap();
        assert_eq!(first.session_id, resumed.session_id);
        assert_eq!(resumed.result.as_deref(), Some("KIWI"));
    }

    #[test]
    fn transcript_usage_counts_each_request_once() {
        let path = format!(
            "{}/tests/fixtures/transcript/session.jsonl",
            env!("CARGO_MANIFEST_DIR")
        );
        let u = transcript_usage(Path::new(&path), 0, None).unwrap();
        assert_eq!(u.records.len(), 1);
        let r = &u.records[0];
        assert_eq!(r.model, "claude-haiku-5-5");
        assert_eq!(r.requests, 3, "6 assistant lines, 3 requests");
        assert_eq!(r.input_tokens, 6);
        assert_eq!(r.cache_read_tokens, 11125 + 30000 + 36979);
        assert!(r.cost_usd > 0.0);
        // Reading again from the end finds nothing new.
        let again =
            transcript_usage(Path::new(&path), u.consumed, u.last_request.as_deref()).unwrap();
        assert!(again.records.is_empty());
    }
}
