//! The Gazette (ADR 0053): one newspaper page per finished item, written for
//! a person rather than an agent.
//!
//! - Code fills in every fact from the journal (`page`): the verdicts round
//!   by round, the checks, the findings and how each one ended, the
//!   timeline, the cost by role, the PR and the learnings.
//! - The librarian's run adds the prose (`Prose`): the headline, the story,
//!   each finding in plain words and, when the change has a flow worth
//!   showing, a diagram outline that `html` lays out as SVG. It is saved
//!   next to the page, so a reprint needs no model run. Without it the page
//!   uses the item's own words.
//! - Files the dev agent attaches (`hall attach`) become plates.
//! - The page is one self-contained HTML file in the item's journal
//!   directory: system fonts, no scripts, no network, images inlined.

pub mod html;
pub mod page;

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, HallError, Result};
use crate::event::{Body, Event};
use crate::item::{Item, ItemId, State};
use crate::repo::Repo;

/// The largest file `hall attach` copies (pages inline images).
pub const MAX_ATTACHMENT: u64 = 5 * 1024 * 1024;

/// What the librarian writes for the page (`gazette` in
/// `plugin/schemas/learnings.json`), read leniently.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prose {
    /// Two or three words over the headline: what part of the product.
    pub kicker: String,
    pub headline: String,
    /// One sentence under the headline.
    pub deck: String,
    /// Paragraphs: what changed, why, and what a person will notice.
    pub story: Vec<String>,
    /// One line per review round, in plain words.
    pub rounds: Vec<RoundLine>,
    /// Each finding in plain words.
    pub findings: Vec<FindingProse>,
    /// Empty when the change has no flow worth drawing.
    pub diagram: Diagram,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoundLine {
    pub round: u32,
    pub line: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FindingProse {
    pub id: String,
    pub headline: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Diagram {
    pub caption: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Node {
    pub id: String,
    pub label: String,
    pub note: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub label: String,
}

/// The page's file name in the item's journal directory.
pub const PAGE: &str = "gazette.html";

/// The page: `items/<id>/gazette.html` in the journal.
pub fn page_path(repo: &Repo, id: ItemId) -> PathBuf {
    repo.hall.item_dir(id).join(PAGE)
}

/// The librarian's prose for the page, kept for reprints.
pub fn prose_path(repo: &Repo, id: ItemId) -> PathBuf {
    repo.hall.item_dir(id).join("gazette.json")
}

/// Where `hall attach` keeps its copies.
pub fn attachments_dir(repo: &Repo, id: ItemId) -> PathBuf {
    repo.hall.item_dir(id).join("attachments")
}

/// Whether an item in this state gets a page: it has finished, or waits
/// for you after an escalation.
pub fn printable(item: &Item) -> bool {
    matches!(
        item.state,
        State::Closed | State::Escalated | State::Dropped
    )
}

/// The prose the librarian wrote, if any.
pub fn prose(repo: &Repo, id: ItemId) -> Option<Prose> {
    let text = std::fs::read_to_string(prose_path(repo, id)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save_prose(repo: &Repo, id: ItemId, prose: &Prose) -> Result<()> {
    let path = prose_path(repo, id);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(prose)?)?;
    Ok(())
}

/// A page that is missing, printed from fewer events than `item` has now,
/// or printed by another version of hall. The page says both in its head.
pub fn stale(repo: &Repo, item: &Item) -> bool {
    use std::io::Read;
    let mut head = Vec::new();
    let read = std::fs::File::open(page_path(repo, item.id))
        .and_then(|f| f.take(1024).read_to_end(&mut head));
    if read.is_err() {
        return true;
    }
    let head = String::from_utf8_lossy(&head);
    !head.contains(&html::generator()) || !head.contains(&html::events_meta(item.events as usize))
}

/// A printed page.
#[derive(Debug, Clone)]
pub struct Printed {
    pub path: PathBuf,
    pub html: String,
    /// Whether the librarian's prose is on it (else the item's own words).
    pub prose: bool,
}

/// Prints the page from the journal and the saved prose.
pub fn print(repo: &Repo, id: ItemId) -> Result<Printed> {
    let events = repo.hall.read_item(id)?;
    let item = crate::event::fold(id, &events)?;
    if !printable(&item) {
        return Err(HallError::new(
            ErrorCode::WrongState,
            format!(
                "{id} is {}; the Gazette prints an item once it is closed, escalated or dropped",
                item.state
            ),
        )
        .next([format!("hall status {id}"), format!("hall log {id}")]));
    }
    let prose = prose(repo, id);
    let page = page::build(repo, &item, &events, prose.as_ref());
    let html = html::render(&page);
    let path = page_path(repo, id);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    write_atomic(&path, &html)?;
    Ok(Printed {
        path,
        html,
        prose: prose.is_some(),
    })
}

/// Prints the page again when the item moved on since (its PR merged, CI
/// reported). Returns whether it printed.
pub fn refresh(repo: &Repo, item: &Item) -> Result<bool> {
    if !printable(item) || !page_path(repo, item.id).exists() || !stale(repo, item) {
        return Ok(false);
    }
    print(repo, item.id)?;
    Ok(true)
}

/// Reprints every page whose item moved on since it was printed. No model
/// run: the prose is already saved. Returns the items reprinted.
pub fn refresh_all(repo: &Repo) -> Result<Vec<ItemId>> {
    let mut done = Vec::new();
    for i in repo.index()?.items(true)? {
        if refresh(repo, &i)? {
            done.push(i.id);
        }
    }
    Ok(done)
}

fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let tmp = path.with_extension("html.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// One edition, for `hall gazette --list`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Edition {
    pub id: String,
    pub title: String,
    pub state: State,
    pub path: String,
    /// When the item reached its state.
    pub since: DateTime<Utc>,
}

/// Every item with a printed page, newest first.
pub fn editions(repo: &Repo) -> Result<Vec<Edition>> {
    let mut out: Vec<Edition> = repo
        .index()?
        .items(true)?
        .into_iter()
        .filter_map(|i| {
            let path = page_path(repo, i.id);
            path.exists().then(|| Edition {
                id: i.id.to_string(),
                title: i.title.clone(),
                state: i.state,
                path: crate::paths::display(&path),
                since: i.state_since,
            })
        })
        .collect();
    out.sort_by(|a, b| b.since.cmp(&a.since).then(b.id.cmp(&a.id)));
    Ok(out)
}

/// What `hall attach` kept.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Attachment {
    /// `attachments/1-mockup.png` under the item's journal directory, or the URL.
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    /// `image`, `file` or `link`.
    pub kind: String,
}

/// The kind of an attachment, from its path.
pub fn kind_of(path: &str) -> &'static str {
    if is_url(path) {
        return "link";
    }
    match image_type(path) {
        Some(_) => "image",
        None => "file",
    }
}

pub fn is_url(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("http://")
}

/// The media type of an image the page can inline.
pub fn image_type(path: &str) -> Option<&'static str> {
    let ext = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => return None,
    })
}

/// Attaches `source` (a file, copied into the journal, or a URL) to `item`.
/// Attaching the same file or URL again with the same caption changes
/// nothing.
pub fn attach(
    repo: &Repo,
    item: &Item,
    source: &str,
    caption: Option<&str>,
    cwd: &Path,
) -> Result<(Attachment, bool)> {
    let caption = caption.map(str::trim).filter(|c| !c.is_empty());
    let events = repo.hall.read_item(item.id)?;
    let earlier = attached(&events);
    let path = if is_url(source) {
        source.to_string()
    } else {
        let file = crate::paths::resolve(cwd, Path::new(source));
        let meta = std::fs::metadata(&file).map_err(|_| {
            HallError::usage(format!("{source} is not a file"))
                .next(["hall attach <file|url> --caption \"<what it shows>\""])
        })?;
        if !meta.is_file() {
            return Err(HallError::usage(format!("{source} is not a file")));
        }
        if meta.len() > MAX_ATTACHMENT {
            return Err(HallError::usage(format!(
                "{source} is {} MB; attachments are at most {} MB",
                meta.len() / (1024 * 1024),
                MAX_ATTACHMENT / (1024 * 1024)
            )));
        }
        let bytes = std::fs::read(&file)?;
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .map(safe_name)
            .unwrap_or_else(|| "file".into());
        let dir = attachments_dir(repo, item.id);
        // The same bytes under the same name are the same attachment.
        if let Some(same) = earlier.iter().find(|(p, _)| {
            p.ends_with(&format!("-{name}"))
                && std::fs::read(repo.hall.item_dir(item.id).join(p)).ok() == Some(bytes.clone())
        }) {
            same.0.clone()
        } else {
            std::fs::create_dir_all(&dir)?;
            let n = earlier.iter().filter(|(p, _)| !is_url(p)).count() + 1;
            let rel = format!("attachments/{n}-{name}");
            std::fs::write(repo.hall.item_dir(item.id).join(&rel), &bytes)?;
            rel
        }
    };
    let caption = caption.map(String::from);
    let a = Attachment {
        kind: kind_of(&path).into(),
        path: path.clone(),
        caption: caption.clone(),
    };
    if earlier.iter().any(|(p, c)| *p == path && *c == caption) {
        return Ok((a, false));
    }
    let e = Event::new(
        crate::repo::actor(),
        Some(item.id),
        Body::Attached { path, caption },
    );
    repo.hall.append_item(item.id, &e)?;
    Ok((a, true))
}

/// Each attachment's latest caption, in the order first attached.
pub fn attached(events: &[Event]) -> Vec<(String, Option<String>)> {
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    for e in events {
        if let Body::Attached { path, caption } = &e.body {
            match out.iter_mut().find(|(p, _)| p == path) {
                Some(x) => x.1 = caption.clone(),
                None => out.push((path.clone(), caption.clone())),
            }
        }
    }
    out
}

/// A file name that is safe on every OS: letters, digits, `.`, `-`, `_`.
fn safe_name(n: &str) -> String {
    let s: String = n
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches(|c| c == '.' || c == '-').to_string();
    if s.is_empty() { "file".into() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(kind_of("https://claude.ai/artifact/x"), "link");
        assert_eq!(kind_of("attachments/1-a.PNG"), "image");
        assert_eq!(kind_of("attachments/2-mock.html"), "file");
        assert_eq!(image_type("x.svg"), Some("image/svg+xml"));
    }

    #[test]
    fn names_are_safe_everywhere() {
        assert_eq!(safe_name("My Mockup (v2).png"), "My-Mockup--v2-.png");
        assert_eq!(safe_name("..."), "file");
        assert_eq!(safe_name("a\\b:c.svg"), "a-b-c.svg");
    }

    #[test]
    fn prose_is_read_leniently() {
        let p: Prose = serde_json::from_str(r#"{"headline":"h","extra":1}"#).unwrap();
        assert_eq!(p.headline, "h");
        assert!(p.diagram.nodes.is_empty());
    }
}
