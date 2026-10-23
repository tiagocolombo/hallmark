//! `hall gazette`: an item's newspaper page, and `hall attach`: files and
//! links for it (ADR 0053).

use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::{AttachArgs, GazetteArgs};
use crate::error::Result;
use crate::gazette::{self, Attachment, Edition};
use crate::output::{Render, Response};
use crate::paths;
use crate::repo::Repo;

#[derive(Debug, Serialize, JsonSchema)]
pub struct GazetteOut {
    pub id: String,
    /// The page: `items/<id>/gazette.html` in the journal.
    pub path: String,
    /// Printed by this call: the page was missing, or older than the item's
    /// journal or than this hall.
    pub printed: bool,
    /// Whether the librarian's prose is on it; otherwise it uses the item's
    /// own words until the librarian runs.
    pub prose: bool,
    /// Files and links attached with `hall attach`.
    pub attachments: usize,
    /// Opened in the browser (`--open`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub opened: bool,
    /// The page itself (`--html`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
}

impl Render for GazetteOut {
    fn render(&self) -> String {
        let mut s = format!("{} · the Gazette\n  page  {}\n", self.id, self.path);
        if !self.prose {
            s.push_str("  in the item's own words: the librarian hasn't written it up yet\n");
        }
        if self.opened {
            s.push_str("  opened in your browser\n");
        }
        s
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GazetteList {
    /// Every item with a printed page, newest first.
    pub editions: Vec<Edition>,
}

impl Render for GazetteList {
    fn render(&self) -> String {
        if self.editions.is_empty() {
            return "No editions yet: the librarian prints one when an item finishes.\n".into();
        }
        let mut s = String::new();
        for e in &self.editions {
            s.push_str(&format!("{}  {:<10} {}\n", e.id, e.state.label(), e.title));
        }
        s
    }
}

pub fn gazette(cwd: &Path, a: GazetteArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    if a.list {
        let editions = gazette::editions(&repo)?;
        let next = editions
            .first()
            .map(|e| vec![format!("hall gazette {} --open", e.id)])
            .unwrap_or_else(|| vec!["hall history".into()]);
        return Ok(Response::new(GazetteList { editions }, next));
    }
    let item = repo.resolve(a.id.as_deref())?;
    let id = item.id;
    let path = gazette::page_path(&repo, id);
    // A page stays as it was while its item is back in the loop.
    let print = !path.exists() || gazette::printable(&item) && gazette::stale(&repo, &item);
    let (printed, html) = if print {
        let p = gazette::print(&repo, id)?;
        (true, p.html)
    } else {
        (false, std::fs::read_to_string(&path)?)
    };
    let opened = a.open && crate::tui::open(&path.to_string_lossy()).is_ok();
    let events = repo.hall.read_item(id)?;
    Ok(Response::new(
        GazetteOut {
            id: id.to_string(),
            path: paths::display(&path),
            printed,
            prose: gazette::prose(&repo, id).is_some(),
            attachments: gazette::attached(&events).len(),
            opened,
            html: a.html.then_some(html),
        },
        vec![
            format!("hall gazette {id} --open"),
            format!("hall log {id}"),
            "hall gazette --list".into(),
        ],
    ))
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AttachOut {
    pub id: String,
    #[serde(flatten)]
    pub attachment: Attachment,
    /// False when the same file or link was already attached with this
    /// caption.
    pub added: bool,
}

impl Render for AttachOut {
    fn render(&self) -> String {
        format!(
            "{} {} {}{}\n",
            self.id,
            if self.added {
                "attached"
            } else {
                "already has"
            },
            self.attachment.path,
            self.attachment
                .caption
                .as_deref()
                .map(|c| format!(": {c}"))
                .unwrap_or_default()
        )
    }
}

pub fn attach(cwd: &Path, a: AttachArgs) -> Result<Response> {
    let repo = Repo::discover(cwd)?;
    let item = repo.resolve(a.item.as_deref())?;
    let (attachment, added) = gazette::attach(&repo, &item, &a.source, a.caption.as_deref(), cwd)?;
    let id = item.id;
    Ok(Response::new(
        AttachOut {
            id: id.to_string(),
            attachment,
            added,
        },
        vec![
            format!("hall log {id} --kind attached"),
            format!("hall status {id}"),
        ],
    ))
}
