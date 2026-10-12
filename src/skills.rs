//! Repo skills: a repository extends or replaces Hallmark's own skills with
//! `.hallmark/skills/<name>/SKILL.md` (ADR 0037). They are read from the
//! base branch, never from the branch at hand, so a branch can't rewrite
//! how it is reviewed or fixed (ADR 0012, ADR 0018).

use std::path::Path;

use crate::assayer::base::{cut, show, strip_comments};

/// Where a repo keeps its skills.
pub const DIR: &str = ".hallmark/skills";

/// The skills a repo may extend or replace, and who follows each.
pub const KNOWN: [(&str, &str); 3] = [
    ("principal-review", "the reviewer"),
    ("hallmark-fix", "the fixer"),
    ("hallmark", "development sessions"),
];

/// A repo skill's text is cut after this many characters.
const MAX: usize = 24_000;

/// How a repo skill combines with Hallmark's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Added after Hallmark's skill (the default).
    Extend,
    /// Followed instead of Hallmark's skill (`mode: replace`).
    Replace,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Extend => "extends",
            Mode::Replace => "replaces",
        }
    }
}

/// One repo skill, as read from the base.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoSkill {
    pub name: String,
    /// Repo-relative: `.hallmark/skills/<name>/SKILL.md`.
    pub path: String,
    pub mode: Mode,
    /// The text without its frontmatter.
    pub body: String,
}

/// `.hallmark/skills/<name>/SKILL.md`.
pub fn path(name: &str) -> String {
    format!("{DIR}/{name}/SKILL.md")
}

/// The repo's `name` skill at commit `rev`, if it has one with any text.
pub fn read(dir: &Path, rev: &str, name: &str) -> Option<RepoSkill> {
    parse(name, &show(dir, rev, &path(name))?)
}

/// Parses a SKILL.md. `mode: replace` in the frontmatter replaces; any
/// other value, or none, extends: a typo must not drop Hallmark's method.
/// None when there is no text besides frontmatter and comments.
pub fn parse(name: &str, text: &str) -> Option<RepoSkill> {
    let (front, body) = split(text);
    let replace = front.lines().any(|l| {
        l.split_once(':').is_some_and(|(k, v)| {
            k.trim() == "mode"
                && v.trim()
                    .trim_matches(['"', '\''])
                    .eq_ignore_ascii_case("replace")
        })
    });
    if strip_comments(body).trim().is_empty() {
        return None;
    }
    Some(RepoSkill {
        name: name.to_string(),
        path: path(name),
        mode: if replace { Mode::Replace } else { Mode::Extend },
        body: cut(body.trim(), MAX),
    })
}

/// A skill's body without its frontmatter.
pub fn body(text: &str) -> &str {
    split(text).1
}

/// (frontmatter, body). No frontmatter: ("", the whole text).
fn split(text: &str) -> (&str, &str) {
    let t = text.strip_prefix('\u{feff}').unwrap_or(text);
    t.strip_prefix("---")
        .filter(|r| r.starts_with('\n') || r.starts_with("\r\n"))
        .and_then(|r| r.split_once("\n---"))
        .map(|(front, rest)| {
            // Skip the rest of the closing `---` line.
            let rest = rest.split_once('\n').map(|(_, b)| b).unwrap_or("");
            (front, rest)
        })
        .unwrap_or(("", t))
}

/// The repo skill as a section of a prompt or a session's context.
/// `origin` says where it was read: `main at 1a2b3c4`.
pub fn section(s: &RepoSkill, origin: &str) -> String {
    match s.mode {
        Mode::Extend => format!(
            "## This repository's additions to the `{n}` skill\n\n\
             From `{p}` on {origin}. Follow them together with the skill; where they are \
             more specific, they win.\n\n{b}",
            n = s.name,
            p = s.path,
            b = s.body
        ),
        Mode::Replace => format!(
            "## This repository's `{n}` skill\n\n\
             This repository replaces Hallmark's `{n}` skill with its own, from `{p}` on \
             {origin}. Follow it instead.\n\n{b}",
            n = s.name,
            p = s.path,
            b = s.body
        ),
    }
}

/// `builtin` (a skill's body) as this repo uses it: unchanged, followed by
/// the repo's additions, or the repo's own skill instead.
pub fn compose(builtin: &str, repo: Option<&RepoSkill>, origin: &str) -> String {
    match repo {
        None => format!("{}\n", builtin.trim()),
        Some(s) if s.mode == Mode::Extend => {
            format!("{}\n\n{}\n", builtin.trim(), section(s, origin))
        }
        Some(s) => format!("{}\n", section(s, origin)),
    }
}

/// What a repo has under `.hallmark/skills/` at `rev`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Listing {
    /// Known skills with text, and how each combines.
    pub found: Vec<(String, Mode)>,
    /// Entries Hallmark won't use, each with the reason.
    pub problems: Vec<String>,
}

impl Listing {
    pub fn is_empty(&self) -> bool {
        self.found.is_empty() && self.problems.is_empty()
    }
}

/// Lists `.hallmark/skills/` at `rev`.
pub fn list(dir: &Path, rev: &str) -> Listing {
    let mut out = Listing::default();
    let Ok(names) = crate::git::run(dir, &["ls-tree", "--name-only", rev, &format!("{DIR}/")])
    else {
        return out;
    };
    for entry in names.lines() {
        let name = entry.trim_start_matches(DIR).trim_start_matches('/');
        if name.is_empty() {
            continue;
        }
        if !KNOWN.iter().any(|(k, _)| *k == name) {
            let known: Vec<&str> = KNOWN.iter().map(|(k, _)| *k).collect();
            out.problems.push(format!(
                "`{name}` isn't a skill Hallmark uses (use {})",
                known.join(", ")
            ));
            continue;
        }
        match read(dir, rev, name) {
            Some(s) => out.found.push((s.name, s.mode)),
            None => out
                .problems
                .push(format!("`{name}` has no SKILL.md with any text")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extends_by_default_and_replaces_only_when_asked() {
        let s = parse("principal-review", "Flag N+1 queries.\n").unwrap();
        assert_eq!(s.mode, Mode::Extend);
        assert_eq!(s.body, "Flag N+1 queries.");
        assert_eq!(s.path, ".hallmark/skills/principal-review/SKILL.md");

        let s = parse(
            "hallmark-fix",
            "---\nname: x\nmode: replace\n---\n\nOurs.\n",
        )
        .unwrap();
        assert_eq!(s.mode, Mode::Replace);
        assert_eq!(s.body, "Ours.");

        let quoted = parse("hallmark", "---\nmode: \"Replace\"\n---\nOurs.").unwrap();
        assert_eq!(quoted.mode, Mode::Replace);
    }

    #[test]
    fn a_typo_in_the_mode_extends() {
        let s = parse("hallmark", "---\nmode: replcae\n---\nOurs.").unwrap();
        assert_eq!(s.mode, Mode::Extend);
        let s = parse("hallmark", "---\nmodes: replace\n---\nOurs.").unwrap();
        assert_eq!(s.mode, Mode::Extend);
    }

    #[test]
    fn crlf_and_a_bom_parse_like_lf() {
        let s = parse(
            "hallmark",
            "\u{feff}---\r\nmode: replace\r\n---\r\nOurs.\r\n",
        )
        .unwrap();
        assert_eq!(s.mode, Mode::Replace);
        assert_eq!(s.body, "Ours.");
    }

    #[test]
    fn frontmatter_or_comments_alone_are_no_skill() {
        assert_eq!(parse("hallmark", "---\nmode: replace\n---\n\n"), None);
        assert_eq!(parse("hallmark", "<!-- add notes here -->\n"), None);
        assert_eq!(parse("hallmark", ""), None);
    }

    #[test]
    fn a_rule_line_is_not_frontmatter() {
        // `---` must open the file on a line of its own.
        let s = parse("hallmark", "---- not frontmatter\n---\nmode: replace").unwrap();
        assert_eq!(s.mode, Mode::Extend);
        assert!(s.body.starts_with("---- not frontmatter"));
    }

    #[test]
    fn a_long_skill_is_cut() {
        let long = "x\n".repeat(MAX);
        let s = parse("hallmark", &long).unwrap();
        assert!(s.body.len() < long.len());
        assert!(s.body.contains("(cut: "), "says it was cut");
    }

    #[test]
    fn compose_keeps_extends_or_replaces() {
        assert_eq!(compose("# Method\n", None, "main at 1a2b3c4"), "# Method\n");

        let ext = parse("principal-review", "Flag N+1 queries.").unwrap();
        let c = compose("# Method\n", Some(&ext), "main at 1a2b3c4");
        assert!(
            c.starts_with("# Method\n\n## This repository's additions"),
            "{c}"
        );
        assert!(c.contains("`.hallmark/skills/principal-review/SKILL.md` on main at 1a2b3c4"));
        assert!(c.trim_end().ends_with("Flag N+1 queries."));

        let rep = parse("principal-review", "---\nmode: replace\n---\nOurs.").unwrap();
        let c = compose("# Method\n", Some(&rep), "main at 1a2b3c4");
        assert!(!c.contains("# Method"), "{c}");
        assert!(c.contains("replaces Hallmark's `principal-review` skill"));
        assert!(c.trim_end().ends_with("Ours."));
    }

    #[test]
    fn body_strips_the_frontmatter_of_a_builtin_skill() {
        let b = body(crate::plugin::file("skills/principal-review/SKILL.md"));
        assert!(b.trim_start().starts_with("# Principal review"), "{b}");
        assert!(!b.contains("description:"));
    }
}
