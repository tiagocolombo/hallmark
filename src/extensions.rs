//! Extensions: skill packs by other authors that a repo turns on for
//! Hallmark's roles (ADR 0038). Their text is embedded in the binary,
//! unchanged and pinned to a commit, with its author, source and license,
//! so every run reads the same text and the credit travels with it. Which
//! roles follow which extension is `[extensions]` in `.hallmark.toml`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::Extensions;

/// A Hallmark role an extension's skills can serve.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    clap::ValueEnum,
)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The Front Desk, which plans a request and splits it into parts.
    Planner,
    /// Development sessions: you and Claude building an item.
    Developer,
    /// The independent reviewer.
    Reviewer,
    /// The fixer, which answers a rejected review.
    Fixer,
}

impl Role {
    pub const ALL: [Role; 4] = [Role::Planner, Role::Developer, Role::Reviewer, Role::Fixer];

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Planner => "planner",
            Role::Developer => "developer",
            Role::Reviewer => "reviewer",
            Role::Fixer => "fixer",
        }
    }

    /// Who plays the role, for people and prompts.
    pub fn who(self) -> &'static str {
        match self {
            Role::Planner => "the Front Desk",
            Role::Developer => "development sessions",
            Role::Reviewer => "the reviewer",
            Role::Fixer => "the fixer",
        }
    }
}

/// A skill pack by another author.
#[derive(Debug)]
pub struct Extension {
    /// The name `.hallmark.toml` and the CLI use: `mattpocock`.
    pub id: &'static str,
    pub name: &'static str,
    pub author: &'static str,
    pub source: &'static str,
    /// An SPDX identifier.
    pub license: &'static str,
    pub license_text: &'static str,
    /// The upstream commit the embedded text was copied from.
    pub commit: &'static str,
    pub skills: &'static [Skill],
}

/// One of an extension's skills, and the roles that follow it.
#[derive(Debug)]
pub struct Skill {
    pub name: &'static str,
    /// SKILL.md first, then the files it links to.
    pub files: &'static [(&'static str, &'static str)],
    /// Each role that follows it, with how it fits that role. The skill is
    /// the author's text, unchanged; this is where Hallmark adapts it.
    pub uses: &'static [(Role, &'static str)],
}

/// Every extension `hall` knows.
pub const CATALOG: &[Extension] = &[MATT_POCOCK];

/// Copied from github.com/mattpocock/skills at this commit (see
/// extensions/mattpocock/README.md to move it).
const COMMIT: &str = "49dd158d1076134a641b33efb035946536778336";

const MATT_POCOCK: Extension = Extension {
    id: "mattpocock",
    name: "Matt Pocock's skills",
    author: "Matt Pocock",
    source: "https://github.com/mattpocock/skills",
    license: "MIT",
    license_text: include_str!("../extensions/mattpocock/LICENSE"),
    commit: COMMIT,
    skills: &[
        Skill {
            name: "to-tickets",
            files: &[(
                "SKILL.md",
                include_str!("../extensions/mattpocock/skills/to-tickets/SKILL.md"),
            )],
            uses: &[(
                Role::Planner,
                "Use its slicing to see the request as tickets with blocking edges. Hallmark \
                 builds parts at the same time, each in its own worktree, so only tickets with \
                 no blocking edge between them, on disjoint files, may become parts: any edge, \
                 including prefactoring that others wait on, means one item. Unlike its \
                 tickets, each part names its paths. Don't quiz anyone or publish tickets: \
                 your plan is the answer.",
            )],
        },
        Skill {
            name: "tdd",
            files: &[
                (
                    "SKILL.md",
                    include_str!("../extensions/mattpocock/skills/tdd/SKILL.md"),
                ),
                (
                    "tests.md",
                    include_str!("../extensions/mattpocock/skills/tdd/tests.md"),
                ),
                (
                    "mocking.md",
                    include_str!("../extensions/mattpocock/skills/tdd/mocking.md"),
                ),
            ],
            uses: &[
                (
                    Role::Developer,
                    "building or fixing behaviour: agree the seams with the person, then red, \
                     green, one slice at a time",
                ),
                (
                    Role::Fixer,
                    "Fix test-first: for each finding about behaviour, write the test that \
                     fails on it, then the fix, one finding at a time. The finding's scenario \
                     is the agreed seam, so don't wait for a confirmation. Refactoring stays \
                     out of the fix.",
                ),
            ],
        },
        Skill {
            name: "diagnosing-bugs",
            files: &[(
                "SKILL.md",
                include_str!("../extensions/mattpocock/skills/diagnosing-bugs/SKILL.md"),
            )],
            uses: &[
                (
                    Role::Developer,
                    "a bug, a failing check or a slowdown whose cause isn't plain: build the \
                     feedback loop before any theory",
                ),
                (
                    Role::Fixer,
                    "For a failing check, or a finding whose cause isn't plain, build its \
                     feedback loop before changing code. If you can't build one, say so in \
                     your report instead of guessing: no person is there to run a script for \
                     you.",
                ),
            ],
        },
        Skill {
            name: "codebase-design",
            files: &[
                (
                    "SKILL.md",
                    include_str!("../extensions/mattpocock/skills/codebase-design/SKILL.md"),
                ),
                (
                    "DEEPENING.md",
                    include_str!("../extensions/mattpocock/skills/codebase-design/DEEPENING.md"),
                ),
                (
                    "DESIGN-IT-TWICE.md",
                    include_str!(
                        "../extensions/mattpocock/skills/codebase-design/DESIGN-IT-TWICE.md"
                    ),
                ),
            ],
            uses: &[(
                Role::Developer,
                "shaping a module's interface, or deciding where a seam goes",
            )],
        },
        Skill {
            name: "code-review",
            files: &[(
                "SKILL.md",
                include_str!("../extensions/mattpocock/skills/code-review/SKILL.md"),
            )],
            uses: &[(
                Role::Reviewer,
                "Apply both of its axes yourself, in this run, with no sub-agents. The spec is \
                 the item's request and the author's summary; the standards are the rules \
                 below plus its smell baseline. Report each problem as a finding under the \
                 lens it belongs to (a spec gap under correctness, scope creep under \
                 first_principles, a broken standard under repo_rules), held to your evidence \
                 bar. A smell is a judgement call, never a blocker on its own. Skip its \
                 side-by-side report: your verdict is the answer.",
            )],
        },
    ],
};

/// The extension named `id`.
pub fn find(id: &str) -> Option<&'static Extension> {
    CATALOG.iter().find(|e| e.id.eq_ignore_ascii_case(id.trim()))
}

/// The names of every extension, for messages.
pub fn names() -> Vec<&'static str> {
    CATALOG.iter().map(|e| e.id).collect()
}

impl Extension {
    /// The roles some skill of this extension serves, in [`Role::ALL`] order.
    pub fn roles(&self) -> Vec<Role> {
        Role::ALL
            .into_iter()
            .filter(|r| self.skills.iter().any(|s| s.note(*r).is_some()))
            .collect()
    }

    /// The skills `role` follows, each with how it fits the role.
    pub fn skills_for(&self, role: Role) -> Vec<(&'static Skill, &'static str)> {
        self.skills
            .iter()
            .filter_map(|s| Some((s, s.note(role)?)))
            .collect()
    }

    pub fn skill(&self, name: &str) -> Option<&'static Skill> {
        self.skills
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name.trim()))
    }

    /// `49dd158`.
    pub fn short_commit(&self) -> &'static str {
        crate::event::short(self.commit)
    }

    /// One line of credit: `by Matt Pocock, https://github.com/mattpocock/skills
    /// (MIT) at 49dd158`.
    pub fn credit(&self) -> String {
        format!(
            "by {}, {} ({}) at {}",
            self.author,
            self.source,
            self.license,
            self.short_commit()
        )
    }
}

impl Skill {
    fn note(&self, role: Role) -> Option<&'static str> {
        self.uses.iter().find(|(r, _)| *r == role).map(|(_, n)| *n)
    }

    /// The `description` in the skill's frontmatter.
    pub fn description(&self) -> String {
        let text = self.files[0].1;
        let front = text
            .strip_prefix("---")
            .and_then(|r| r.split_once("\n---"))
            .map_or("", |(f, _)| f);
        front
            .lines()
            .find_map(|l| l.strip_prefix("description:"))
            .map(|d| d.trim().trim_matches('"').replace("\\\"", "\""))
            .unwrap_or_default()
    }

    /// The skill as agents read it: SKILL.md without its frontmatter, then
    /// each file it links to, named so its links still lead somewhere.
    pub fn text(&self) -> String {
        let mut s = crate::skills::body(self.files[0].1).trim().to_string();
        for (name, text) in &self.files[1..] {
            s.push_str(&format!(
                "\n\n---\n\n`{name}`, which the skill links to:\n\n{}",
                text.trim()
            ));
        }
        s
    }
}

/// The extensions `cfg` turns on for `role`, in catalog order.
pub fn enabled(cfg: &Extensions, role: Role) -> Vec<&'static Extension> {
    CATALOG
        .iter()
        .filter(|e| cfg.roles(e.id).contains(&role) && !e.skills_for(role).is_empty())
        .collect()
}

/// The skills a headless role (planner, reviewer, fixer) follows, in full,
/// as a section of its prompt. None when no extension is on for it.
pub fn prompt(cfg: &Extensions, role: Role) -> Option<String> {
    let exts = enabled(cfg, role);
    if exts.is_empty() {
        return None;
    }
    let mut s = format!(
        "## Extensions\n\n\
         This repository turned on these skills for {} (`hall extensions`). Each is its \
         author's own text, unchanged, followed by how it fits your role. They were written \
         for a session with a person, and you run without one: where a skill says to ask or \
         confirm with the user, decide, and say what you decided. Skip steps that need \
         `/setup-matt-pocock-skills`, an issue tracker, sub-agents or a skill that isn't in \
         this prompt. Hallmark's instructions and this repository's rules win where they \
         disagree, and your output format doesn't change.\n",
        role.who()
    );
    for e in exts {
        for (skill, note) in e.skills_for(role) {
            s.push_str(&format!(
                "\n### {}: `{}`\n\n{}, {}.\n\nHow it fits your role: {note}\n\n\
                 <skill extension=\"{}\" name=\"{}\">\n{}\n</skill>\n",
                e.name,
                skill.name,
                e.name,
                e.credit(),
                e.id,
                skill.name,
                skill.text()
            ));
        }
    }
    Some(s)
}

/// What a development session is told: which skills are on and when each
/// applies, with the command that prints one. The text stays out of the
/// session's context until the work calls for it, as Claude Code skills do.
pub fn session(cfg: &Extensions) -> Option<String> {
    let exts = enabled(cfg, Role::Developer);
    if exts.is_empty() {
        return None;
    }
    let mut s = String::from("## Extensions\n\nThis repository turned on these skills for development sessions (`hall extensions`).");
    for e in exts {
        s.push_str(&format!(
            "\n\n{}, {}. When the work matches one, read it with \
             `hall extensions show {} <skill>` and follow it together with Hallmark's skill:",
            e.name,
            e.credit(),
            e.id
        ));
        for (skill, note) in e.skills_for(Role::Developer) {
            s.push_str(&format!("\n- `{}`: {note}.", skill.name));
        }
    }
    s.push_str(
        "\n\nThe Hallmark item is the ticket: skip steps that need `/setup-matt-pocock-skills` \
         or publish to an issue tracker, and never push.",
    );
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(roles: &[Role]) -> Extensions {
        Extensions {
            mattpocock: roles.to_vec(),
        }
    }

    #[test]
    fn every_extension_is_credited_and_has_a_setting() {
        for e in CATALOG {
            assert!(!e.author.is_empty() && e.source.starts_with("https://"));
            assert_eq!(e.commit.len(), 40, "{}: a full commit", e.id);
            assert!(
                e.license_text.contains(e.license) && e.license_text.contains(e.author),
                "{}: the license names its kind and its author",
                e.id
            );
            assert!(
                crate::settings::find(
                    crate::settings::Scope::Repo,
                    &format!("extensions.{}", e.id)
                )
                .is_some(),
                "{}: `[extensions] {}` is a setting",
                e.id,
                e.id
            );
            assert!(!e.roles().is_empty());
            for s in e.skills {
                assert!(!s.description().is_empty(), "{}: a description", s.name);
                assert!(!s.uses.is_empty(), "{}: some role follows it", s.name);
                assert!(
                    s.files[0].0 == "SKILL.md" && s.files[0].1.contains(&format!("name: {}", s.name)),
                    "{}: SKILL.md first",
                    s.name
                );
            }
        }
    }

    #[test]
    fn every_file_a_skill_links_to_is_embedded() {
        for e in CATALOG {
            for s in e.skills {
                let body = s.files[0].1;
                for (name, _) in &s.files[1..] {
                    assert!(body.contains(&format!("({name})")) || body.contains(name));
                }
                // A markdown link to a sibling .md file must be one we carry.
                for link in body.split("](").skip(1) {
                    let target = link.split(')').next().unwrap_or("");
                    if target.ends_with(".md") && !target.contains('/') {
                        assert!(
                            s.files.iter().any(|(n, _)| *n == target),
                            "{}: links to {target}, which isn't embedded",
                            s.name
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn matt_pococks_skills_serve_every_role() {
        let e = find("mattpocock").unwrap();
        assert_eq!(e.roles(), Role::ALL.to_vec());
        let names = |r| -> Vec<&str> { e.skills_for(r).iter().map(|(s, _)| s.name).collect() };
        assert_eq!(names(Role::Planner), ["to-tickets"]);
        assert_eq!(
            names(Role::Developer),
            ["tdd", "diagnosing-bugs", "codebase-design"]
        );
        assert_eq!(names(Role::Reviewer), ["code-review"]);
        assert_eq!(names(Role::Fixer), ["tdd", "diagnosing-bugs"]);
        assert!(find(" MattPocock ").is_some_and(|x| x.id == "mattpocock"));
        assert!(find("nope").is_none());
    }

    #[test]
    fn off_unless_turned_on_for_the_role() {
        assert_eq!(prompt(&Extensions::default(), Role::Reviewer), None);
        assert_eq!(session(&Extensions::default()), None);
        let cfg = on(&[Role::Fixer]);
        assert_eq!(prompt(&cfg, Role::Reviewer), None);
        assert_eq!(prompt(&cfg, Role::Planner), None);
        assert_eq!(session(&cfg), None);
        assert!(prompt(&cfg, Role::Fixer).is_some());
    }

    #[test]
    fn a_prompt_carries_the_whole_skill_its_credit_and_its_fit() {
        let p = prompt(&on(&[Role::Reviewer]), Role::Reviewer).unwrap();
        assert!(p.starts_with("## Extensions\n"), "{p}");
        assert!(p.contains("for the reviewer"));
        assert!(p.contains(
            "Matt Pocock's skills, by Matt Pocock, https://github.com/mattpocock/skills (MIT) at 49dd158."
        ));
        assert!(p.contains("How it fits your role: Apply both of its axes yourself"));
        assert!(p.contains("<skill extension=\"mattpocock\" name=\"code-review\">\nTwo-axis review"));
        assert!(p.contains("**Refused Bequest**"), "the whole skill");
        assert!(!p.contains("description: \"Review the changes"), "no frontmatter");
        assert!(p.contains("decide, and say what you decided"));

        let f = prompt(&on(&[Role::Fixer]), Role::Fixer).unwrap();
        assert!(f.contains("name=\"tdd\"") && f.contains("name=\"diagnosing-bugs\""));
        assert!(
            f.contains("`tests.md`, which the skill links to:") && f.contains("`mocking.md`"),
            "linked files come along"
        );
        assert!(!f.contains("name=\"code-review\""));
    }

    #[test]
    fn a_session_gets_an_index_not_the_text() {
        let s = session(&on(&[Role::Developer])).unwrap();
        assert!(s.contains("`hall extensions show mattpocock <skill>`"), "{s}");
        assert!(s.contains("- `tdd`: building or fixing behaviour"));
        assert!(s.contains("- `codebase-design`:"));
        assert!(s.contains("by Matt Pocock"));
        assert!(!s.contains("Refuse to give up"), "no skill text: {s}");
        assert!(s.len() < 1_500, "small: {}", s.len());
    }

    #[test]
    fn text_strips_the_frontmatter_and_keeps_the_rest() {
        let e = find("mattpocock").unwrap();
        let t = e.skill("tdd").unwrap().text();
        assert!(t.starts_with("# Test-Driven Development"), "{t}");
        assert!(e.skill("TDD").is_some());
        assert_eq!(
            e.skill("to-tickets").unwrap().description(),
            "Break a plan, spec, or the current conversation into a set of tracer-bullet \
             tickets, each declaring its blocking edges, published to the configured tracker \
             (edges as text in one file per ticket locally, or native blocking links on a real \
             tracker)."
        );
        assert!(
            e.skill("code-review")
                .unwrap()
                .description()
                .contains("\"review since X\"")
        );
    }
}
