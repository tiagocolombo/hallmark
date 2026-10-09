//! `hall notify test` and `hall statusline` (workstream D).

use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::NotifyArgs;
use crate::error::{HallError, Result};
use crate::output::{Render, Response};

#[derive(Debug, Serialize, JsonSchema)]
pub struct NotifyOut {
    pub sent: Vec<Sent>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Sent {
    pub channel: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Render for NotifyOut {
    fn render(&self) -> String {
        format!("{} channels\n", self.sent.len())
    }
}

pub fn notify(_cwd: &Path, _a: NotifyArgs) -> Result<Response> {
    Err(HallError::not_implemented("hall notify"))
}

/// The status line Claude Code shows: printed as plain text, never an error.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct StatuslineOut(pub String);

pub fn statusline(_cwd: &Path) -> Result<Response> {
    Ok(Response::raw(None))
}
