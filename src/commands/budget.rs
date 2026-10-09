//! `hall budget` (workstream D): the caps at four levels and today's spend.

use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::BudgetArgs;
use crate::error::{HallError, Result};
use crate::output::{Render, Response};

#[derive(Debug, Serialize, JsonSchema)]
pub struct BudgetOut {
    pub caps: Vec<Cap>,
    /// Settings written by --set.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub set: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Cap {
    /// run, item, room, daily.
    pub level: String,
    /// The key in .hallmark.toml, such as budget.review.daily.
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<String>,
    /// Spent today (tokens), where it applies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used: Option<u64>,
}

impl Render for BudgetOut {
    fn render(&self) -> String {
        format!("{} caps\n", self.caps.len())
    }
}

pub fn budget(_cwd: &Path, _a: BudgetArgs) -> Result<Response> {
    Err(HallError::not_implemented("hall budget"))
}
