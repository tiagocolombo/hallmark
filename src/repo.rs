//! Finding the repository, its journal and the item you're standing in.

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::{ErrorCode, HallError, Result};
use crate::git;
use crate::index::Index;
use crate::item::{Item, ItemId};
use crate::journal::Hall;
use crate::paths;

#[derive(Debug, Clone)]
pub struct Repo {
    pub cwd: PathBuf,
    /// The root of the worktree containing `cwd`.
    pub toplevel: PathBuf,
    pub common_dir: PathBuf,
    /// The main worktree's root.
    pub main_root: PathBuf,
    /// The repository's directory name, used for `{repo}`.
    pub name: String,
    pub hall: Hall,
    pub config: Config,
    pub config_path: Option<PathBuf>,
}

impl Repo {
    pub fn discover(cwd: &Path) -> Result<Repo> {
        let cwd = paths::normalize(cwd);
        let toplevel = git::toplevel(&cwd).map_err(|_| {
            HallError::new(
                ErrorCode::NotARepo,
                format!("{} is not inside a git repository", paths::display(&cwd)),
            )
            .next(["git init", "cd <your repository>"])
        })?;
        let common_dir = git::common_dir(&cwd)?;
        let main_root = git::worktrees(&cwd)?
            .into_iter()
            .next()
            .map(|w| w.path)
            .unwrap_or_else(|| toplevel.clone());
        let name = main_root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "repo".into());
        // The main checkout's settings win, so an item can't loosen its own
        // gate by editing .hallmark.toml in its worktree.
        let (config, config_path) = Config::load(&[&main_root, &toplevel])?;
        Ok(Repo {
            hall: Hall::new(&common_dir),
            cwd,
            toplevel,
            common_dir,
            main_root,
            name,
            config,
            config_path,
        })
    }

    pub fn initialized(&self) -> bool {
        self.hall.exists()
    }

    pub fn require_init(&self) -> Result<()> {
        if self.initialized() {
            Ok(())
        } else {
            Err(HallError::new(
                ErrorCode::NotInitialized,
                format!("{} doesn't use Hallmark yet", self.name),
            )
            .next(["hall init"]))
        }
    }

    pub fn index(&self) -> Result<Index> {
        Index::synced(&self.hall)
    }

    /// The live item whose worktree contains `dir`. The most specific
    /// worktree wins, so an item adopted in the main checkout doesn't claim
    /// its sibling worktrees.
    pub fn item_at(&self, items: &[Item], dir: &Path) -> Option<Item> {
        let dir = paths::normalize(dir);
        items
            .iter()
            .filter(|i| !i.state.is_terminal())
            .filter(|i| paths::is_within(&dir, &paths::normalize(Path::new(&i.worktree))))
            .max_by_key(|i| i.worktree.len())
            .cloned()
    }

    /// The item for an explicit ID, or the one this directory belongs to.
    pub fn resolve(&self, id: Option<&str>) -> Result<Item> {
        self.require_init()?;
        if let Some(id) = id {
            let id: ItemId = id.parse()?;
            return self.hall.load_item(id);
        }
        let ix = self.index()?;
        let items = ix.items(false)?;
        if let Some(item) = self.item_at(&items, &self.cwd) {
            // Fold from the journal: the index may trail a concurrent write.
            return self.hall.load_item(item.id);
        }
        if let Some(branch) = git::current_branch(&self.cwd)
            && let Some(item) = items.iter().find(|i| i.branch == branch)
        {
            return self.hall.load_item(item.id);
        }
        Err(HallError::new(
            ErrorCode::NoItem,
            "no item ID given, and this directory is not an item's worktree",
        )
        .next([
            "hall status",
            "hall new --title \"<short title>\"",
            "hall adopt",
        ]))
    }

    /// The item in this directory, if any, without erroring.
    pub fn current_item(&self) -> Option<Item> {
        if !self.initialized() {
            return None;
        }
        let items = self.index().ok()?.items(false).ok()?;
        self.item_at(&items, &self.cwd)
    }

    /// True when `p` is inside this repository's main checkout or one of
    /// its worktrees.
    pub fn contains(&self, p: &Path) -> bool {
        if paths::is_within(p, &self.main_root) || paths::is_within(p, &self.toplevel) {
            return !paths::is_within(p, &self.common_dir);
        }
        git::worktrees(&self.main_root)
            .map(|ws| ws.iter().any(|w| paths::is_within(p, &w.path)))
            .unwrap_or(false)
    }

    /// `p` relative to the checkout (main, or a worktree) that holds it,
    /// with `/` separators, for matching repo-relative globs.
    pub fn relative(&self, p: &Path) -> Option<String> {
        let mut roots = vec![self.toplevel.clone(), self.main_root.clone()];
        if let Ok(ws) = git::worktrees(&self.main_root) {
            roots.extend(ws.into_iter().map(|w| w.path));
        }
        // The deepest root wins: worktrees can sit inside the main checkout.
        roots.sort_by_key(|r| std::cmp::Reverse(r.components().count()));
        let p = paths::normalize(p);
        roots.iter().find_map(|r| {
            let rel = p.strip_prefix(paths::normalize(r)).ok()?;
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            (!parts.is_empty()).then(|| parts.join("/"))
        })
    }
}

/// Who is acting: `you` on a terminal, `agent` inside Claude Code.
/// `HALL_ACTOR` overrides it (the daemon sets assayer, fixer, shipper).
pub fn actor() -> String {
    if let Ok(a) = std::env::var("HALL_ACTOR")
        && !a.trim().is_empty()
    {
        return a;
    }
    if std::env::var_os("CLAUDECODE").is_some() {
        "agent".into()
    } else {
        "you".into()
    }
}

/// Where per-user state lives: the repo registry and the extracted plugin.
pub fn user_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("HALL_HOME") {
        return PathBuf::from(p);
    }
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("hallmark")
}
