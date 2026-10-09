//! The record (workstream C): `hall history` and `hall export`.

use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;

use super::ItemView;
use crate::cli::{ExportArgs, HistoryArgs};
use crate::error::{HallError, Result};
use crate::output::{Render, Response};

#[derive(Debug, Serialize, JsonSchema)]
pub struct HistoryOut {
    pub items: Vec<HistoryRow>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct HistoryRow {
    #[serde(flatten)]
    pub item: ItemView,
    /// Rejected reviews.
    pub rejected: u32,
    /// Blocker and major findings raised, as "F2 correctness: title".
    pub reasons: Vec<String>,
}

impl Render for HistoryOut {
    fn render(&self) -> String {
        format!("{} items\n", self.items.len())
    }
}

pub fn history(_cwd: &Path, _a: HistoryArgs) -> Result<Response> {
    Err(HallError::not_implemented("hall history"))
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ExportOut {
    pub id: String,
    /// Where the Markdown was written.
    pub path: String,
    pub markdown: String,
}

impl Render for ExportOut {
    fn render(&self) -> String {
        self.markdown.clone()
    }
}

pub fn export(_cwd: &Path, _a: ExportArgs) -> Result<Response> {
    Err(HallError::not_implemented("hall export"))
}
