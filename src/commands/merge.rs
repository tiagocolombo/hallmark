//! `hall merge` (workstream H): a parallel part (child item) merges into its
//! parent's branch and closes. The parent is reviewed once, as a whole.

use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;

use super::ItemView;
use crate::cli::MergeArgs;
use crate::error::{HallError, Result};
use crate::output::{Render, Response};

#[derive(Debug, Serialize, JsonSchema)]
pub struct MergeOut {
    /// The child, now closed.
    #[serde(flatten)]
    pub item: ItemView,
    pub parent: ItemView,
    /// The merge commit on the parent's branch.
    pub sha: String,
    /// Children of the parent still open.
    pub open_children: Vec<String>,
    pub changed: bool,
}

impl Render for MergeOut {
    fn render(&self) -> String {
        format!("{} merged into {}\n", self.item.id, self.parent.id)
    }
}

pub fn merge(_cwd: &Path, _a: MergeArgs) -> Result<Response> {
    Err(HallError::not_implemented("hall merge"))
}
