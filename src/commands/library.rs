//! `hall library search|review|retire|distill` (workstream E).

use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::LibraryArgs;
use crate::error::{HallError, Result};
use crate::output::{Render, Response};

#[derive(Debug, Serialize, JsonSchema)]
pub struct LibraryOut {
    /// search, review, retire, distill.
    pub action: String,
    pub results: Vec<serde_json::Value>,
}

impl Render for LibraryOut {
    fn render(&self) -> String {
        format!("{} results\n", self.results.len())
    }
}

pub fn library(_cwd: &Path, _a: LibraryArgs) -> Result<Response> {
    Err(HallError::not_implemented("hall library"))
}
