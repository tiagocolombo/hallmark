//! The daemon's workers (ADR 0014). Each one is a `tick`: look at one
//! repository, do at most one piece of work, and say what happened. The
//! daemon calls them in [`ORDER`]; tests call them through
//! `hall daemon run --once`.

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::error::Result;
use crate::repo::Repo;
use crate::runner::Runner;

/// What a worker gets.
pub struct WorkerCtx<'a> {
    pub repo: &'a Repo,
    pub runner: &'a dyn Runner,
    /// The lease holder name: `daemon:<pid>:<worker>.<slot>`, one per slot.
    pub holder: String,
}

/// What a tick did.
#[derive(Debug, Clone, PartialEq, Serialize, schemars::JsonSchema)]
#[serde(tag = "progress", rename_all = "snake_case")]
pub enum Progress {
    /// Nothing to do.
    Idle,
    /// Did something; the text says what.
    Worked { what: String },
    /// Stop the daemon until `until` (a rate limit, the daily budget).
    Pause {
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        until: Option<DateTime<Utc>>,
    },
}

impl Progress {
    pub fn worked(what: impl Into<String>) -> Progress {
        Progress::Worked { what: what.into() }
    }
}

pub type Tick = fn(&WorkerCtx) -> Result<Progress>;

/// The workers, in the order a daemon pass runs them.
pub const ORDER: &[(&str, Tick)] = &[
    ("watchman", crate::watchman::tick),
    ("resume", crate::resume::tick),
    ("assayer", crate::assayer::tick),
    ("fixer", crate::fixer::tick),
    ("shipper", crate::shipper::tick),
    ("ci", crate::ci::tick),
    ("librarian", crate::library::librarian_tick),
    ("notify", crate::notify::tick),
];

pub fn names() -> Vec<&'static str> {
    ORDER.iter().map(|(n, _)| *n).collect()
}
