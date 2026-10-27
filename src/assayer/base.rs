//! What the reviewer reads from the base branch, never from the branch under
//! review (ADR 0012, ADR 0018): the review settings in `.hallmark.toml`, the
//! rule files, `.hallmark/review.md` and its sensitive paths.

use std::path::Path;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::config::{Config, Lenses};
use crate::findings::Lens;
use crate::git;
use crate::repo::Repo;

/// The review notes file, read from the base branch.
pub const REVIEW_NOTES: &str = ".hallmark/review.md";

/// A rule file's text is cut after this many characters.
const RULE_MAX: usize = 24_000;

/// The base branch at one commit, and what the review reads from it.
#[derive(Debug, Clone)]
pub struct Base {
    /// The branch name (`main`).
    pub name: String,
    /// The commit the rules were read from.
    pub sha: String,
    /// The base's `.hallmark.toml`, else the main checkout's.
    pub config: Config,
    /// True when `config` came from the base branch.
    pub config_from_base: bool,
}

impl Base {
    /// Resolves `name` in the main checkout and reads its settings.
    pub fn resolve(repo: &Repo, name: &str) -> crate::error::Result<Base> {
        let sha = git::run(
            &repo.main_root,
            &["rev-parse", "--verify", "-q", &format!("{name}^{{commit}}")],
        )
        .map_err(|_| {
            crate::error::HallError::new(
                crate::error::ErrorCode::CommandFailed,
                format!("the base branch {name} does not resolve to a commit"),
            )
            .next(["hall doctor", "check `base` in .hallmark.toml"])
        })?;
        Ok(Base::at(repo, name, &sha))
    }

    /// The base at a known commit, with the settings read from it.
    pub fn at(repo: &Repo, name: &str, sha: &str) -> Base {
        let (config, config_from_base) = match show(&repo.main_root, sha, crate::config::FILE_NAME)
            .and_then(|t| Config::parse(&t).ok())
        {
            Some(c) => (c, true),
            None => (repo.config.clone(), false),
        };
        Base {
            name: name.to_string(),
            sha: sha.to_string(),
            config,
            config_from_base,
        }
    }

    /// The lenses this repo applies, in the canonical order.
    pub fn lenses(&self) -> Vec<Lens> {
        lenses(&self.config.lenses)
    }
}

/// `git show <rev>:<path>`, or None when the file isn't there.
pub fn show(dir: &Path, rev: &str, path: &str) -> Option<String> {
    let path = path.trim_start_matches("./").replace('\\', "/");
    git::run(dir, &["show", &format!("{rev}:{path}")]).ok()
}

/// "all", or the listed lenses that exist. An empty or unreadable list
/// means all of them: a typo must not switch the review off.
pub fn lenses(l: &Lenses) -> Vec<Lens> {
    match l {
        Lenses::All(_) => Lens::ALL.to_vec(),
        Lenses::List(names) => {
            let mut v: Vec<Lens> = names.iter().filter_map(|n| Lens::parse(n)).collect();
            v.sort();
            v.dedup();
            if v.is_empty() { Lens::ALL.to_vec() } else { v }
        }
    }
}

/// One rule file (or one section of it) as read from the base.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    /// As configured: `CLAUDE.md` or `docs/rules.md#testing`.
    pub source: String,
    pub text: String,
}

/// The configured `rules` entries that exist on the base, each cut to
/// its `#anchor` section when one is given.
pub fn rules(repo: &Repo, base: &Base) -> Vec<Rule> {
    let mut out = Vec::new();
    for entry in &base.config.rules {
        let (path, anchor) = match entry.split_once('#') {
            Some((p, a)) => (p, Some(a)),
            None => (entry.as_str(), None),
        };
        let Some(text) = show(&repo.main_root, &base.sha, path) else {
            continue;
        };
        let text = match anchor {
            Some(a) => match section(&text, a) {
                Some(s) => s,
                None => format!("(no section #{a} in {path}; the whole file follows)\n\n{text}"),
            },
            None => text,
        };
        if text.trim().is_empty() {
            continue;
        }
        out.push(Rule {
            source: entry.clone(),
            text: cut(&text, RULE_MAX),
        });
    }
    out
}

/// `.hallmark/review.md` on the base, if any.
pub fn review_notes(repo: &Repo, base: &Base) -> Option<String> {
    show(&repo.main_root, &base.sha, REVIEW_NOTES)
        .filter(|t| !strip_comments(t).trim().is_empty())
        .map(|t| cut(&t, RULE_MAX))
}

/// Cuts `text` to `max` characters on a line boundary, saying so.
pub fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let end = text[..end].rfind('\n').unwrap_or(end);
    format!(
        "{}\n\n(cut: {} more characters)\n",
        &text[..end],
        text.len() - end
    )
}

/// A GitHub-style heading slug: lowercase, spaces to dashes, punctuation
/// dropped.
pub fn slug(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                Some(c)
            } else if c.is_whitespace() {
                Some('-')
            } else {
                None
            }
        })
        .collect()
}

/// A Markdown heading: (level, text).
fn heading(line: &str) -> Option<(usize, &str)> {
    let t = line.trim_end();
    let level = t.chars().take_while(|c| *c == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &t[level..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some((level, rest.trim().trim_end_matches('#').trim()))
}

/// The section under the heading whose slug (or text) matches `anchor`,
/// up to the next heading of the same or a higher level.
pub fn section(text: &str, anchor: &str) -> Option<String> {
    let want = slug(anchor);
    let mut out: Option<(usize, Vec<&str>)> = None;
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        let h = if fenced { None } else { heading(line) };
        if let Some((level, lines)) = out.as_mut() {
            if let Some((l, _)) = h
                && l <= *level
            {
                break;
            }
            lines.push(line);
        } else if let Some((level, t)) = h
            && slug(t) == want
        {
            out = Some((level, vec![line]));
        }
    }
    out.map(|(_, lines)| lines.join("\n") + "\n")
}

/// Drops `<!-- … -->` comments (the stub's examples live in them).
pub fn strip_comments(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The globs listed under "## Sensitive paths" in the review notes. Each
/// list item's first `code span`, else its first word, is a glob.
pub fn sensitive_globs(notes: &str) -> Vec<String> {
    section_globs(notes, "sensitive paths")
}

/// The globs listed under "## Paths to skip" in the review notes: changes
/// there are left out of the review (ADR 0058).
pub fn skip_globs(notes: &str) -> Vec<String> {
    section_globs(notes, "paths to skip")
}

fn section_globs(notes: &str, heading: &str) -> Vec<String> {
    let Some(sec) = section(&strip_comments(notes), heading) else {
        return Vec::new();
    };
    sec.lines()
        .skip(1)
        .filter_map(|l| {
            let t = l.trim();
            let t = t
                .strip_prefix("- ")
                .or_else(|| t.strip_prefix("* "))
                .or_else(|| t.strip_prefix("+ "))
                .unwrap_or(t)
                .trim();
            if t.is_empty() || t.starts_with('#') {
                return None;
            }
            let g = match t.find('`') {
                Some(a) => {
                    let r = &t[a + 1..];
                    r[..r.find('`')?].trim()
                }
                None => t.split_whitespace().next()?,
            };
            (!g.is_empty()).then(|| g.to_string())
        })
        .collect()
}

/// Matches repo-relative paths the way `.gitignore` does: a pattern
/// without a slash matches at any depth; a trailing slash means
/// everything under that directory.
pub fn globset(globs: &[String]) -> GlobSet {
    let mut b = GlobSetBuilder::new();
    for g in globs {
        let g = g.trim().trim_start_matches("./").trim_start_matches('/');
        let g = match g.strip_suffix('/') {
            Some(d) => format!("{d}/**"),
            None => g.to_string(),
        };
        let mut pats = vec![g.clone()];
        if !g.contains('/') {
            pats.push(format!("**/{g}"));
        }
        // A directory pattern also matches what's inside it.
        if !g.ends_with("**") {
            pats.push(format!("{g}/**"));
        }
        for p in pats {
            if let Ok(glob) = GlobBuilder::new(&p).literal_separator(true).build() {
                b.add(glob);
            }
        }
    }
    b.build().unwrap_or_else(|_| GlobSet::empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTES: &str = "# Review notes

## Sensitive paths

<!-- e.g. apps/api/src/billing/** gets the deepest review -->
- `apps/api/src/billing/**`: money moves here
- migrations/
* *.sql
secrets.toml

## Paths to skip

<!-- e.g. docs/generated/** -->
- `docs/generated/**`: built from the schema
- vendor/

## Invariants

- every query goes through the tenant-scoped client
";

    #[test]
    fn paths_to_skip_come_from_their_section() {
        let g = skip_globs(NOTES);
        assert_eq!(g, ["docs/generated/**", "vendor/"]);
        let set = globset(&g);
        assert!(set.is_match("docs/generated/schema.md"));
        assert!(set.is_match("vendor/lib/x.js"));
        assert!(!set.is_match("docs/commands.md"));
        // Skipping is never read from the sensitive section, or the reverse.
        assert!(!sensitive_globs(NOTES).contains(&"vendor/".to_string()));
        assert!(skip_globs("## Paths to skip\n\n<!-- e.g. a/** -->\n").is_empty());
        assert!(skip_globs("no section").is_empty());
    }

    #[test]
    fn sensitive_paths_come_from_their_section() {
        let g = sensitive_globs(NOTES);
        assert_eq!(
            g,
            [
                "apps/api/src/billing/**",
                "migrations/",
                "*.sql",
                "secrets.toml"
            ]
        );
        let set = globset(&g);
        assert!(set.is_match("apps/api/src/billing/invoice.ts"));
        assert!(set.is_match("migrations/0003_add.rs"));
        assert!(set.is_match("db/schema/v2.sql"));
        assert!(set.is_match("config/secrets.toml"));
        assert!(!set.is_match("apps/api/src/users.ts"));
        assert!(!set.is_match("README.md"));
        // The stub's commented example is not a glob.
        assert!(sensitive_globs("## Sensitive paths\n\n<!-- e.g. a/** -->\n").is_empty());
        assert!(sensitive_globs("no section").is_empty());
    }

    #[test]
    fn anchors_take_one_section() {
        let md = "# Title\nintro\n## Testing\nrun tests\n### Fixtures\nuse them\n```\n# not a heading\n```\n## Style\nshort\n";
        let s = section(md, "testing").unwrap();
        assert!(s.starts_with("## Testing"));
        assert!(s.contains("use them"));
        assert!(s.contains("# not a heading"));
        assert!(!s.contains("short"));
        assert_eq!(section(md, "Style").unwrap(), "## Style\nshort\n");
        assert!(section(md, "missing").is_none());
        assert_eq!(slug("Data & migrations!"), "data--migrations");
    }

    #[test]
    fn lens_lists_fall_back_to_all() {
        assert_eq!(lenses(&Lenses::All("all".into())).len(), 7);
        assert_eq!(
            lenses(&Lenses::List(vec!["security".into(), "correctness".into()])),
            [Lens::Correctness, Lens::SecurityTenancy]
        );
        assert_eq!(lenses(&Lenses::List(vec!["vibes".into()])).len(), 7);
    }

    #[test]
    fn long_text_is_cut_on_a_line() {
        let t = "line\n".repeat(100);
        let c = cut(&t, 52);
        assert!(c.starts_with("line\nline"));
        assert!(c.contains("(cut: "));
        assert_eq!(cut("short", 52), "short");
    }
}
