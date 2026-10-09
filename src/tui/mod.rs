//! The terminal app (workstream F, Stage 3): `hall` with no arguments on a
//! terminal. It only reads the journal and the index.

use std::path::Path;

use crate::error::Result;
use crate::output::Response;

/// Opens the app. Until it lands, prints the status.
pub fn run(cwd: &Path) -> Result<Response> {
    crate::commands::status::status(cwd, None, false, true)
}
