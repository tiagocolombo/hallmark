//! `hall fixed` (workstream C): the fixer records which findings its commit
//! fixes, disputes others, and resubmits the item.

use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;

use super::ItemView;
use crate::cli::FixedArgs;
use crate::error::{HallError, Result};
use crate::findings::Dispute;
use crate::output::{Render, Response};

#[derive(Debug, Serialize, JsonSchema)]
pub struct FixedOut {
    #[serde(flatten)]
    pub item: ItemView,
    pub sha: String,
    pub fixes: Vec<String>,
    pub disputes: Vec<Dispute>,
    /// False when this exact submission was already recorded.
    pub changed: bool,
}

impl Render for FixedOut {
    fn render(&self) -> String {
        format!(
            "{} resubmitted at {}\n",
            self.item.id,
            crate::event::short(&self.sha)
        )
    }
}

pub fn fixed(_cwd: &Path, _a: FixedArgs) -> Result<Response> {
    Err(HallError::not_implemented("hall fixed"))
}
