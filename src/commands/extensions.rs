//! `hall extensions`: skill packs by other authors, with their credits, and
//! the roles that follow them in this repo (ADR 0038). `enable` and
//! `disable` write `[extensions]` in the main checkout's `.hallmark.toml`
//! through the same code as `hall config set`.

use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::{ExtensionsAction, ExtensionsArgs};
use crate::config::{self, Extensions};
use crate::error::{ErrorCode, HallError, Result};
use crate::extensions::{self, Extension, Role};
use crate::output::{Render, Response};
use crate::paths;
use crate::repo::Repo;
use crate::settings::Scope;

#[derive(Debug, Serialize, JsonSchema)]
pub struct ExtensionsOut {
    /// list, enable, disable or show.
    pub action: String,
    /// The repository's `.hallmark.toml`, when in an initialized repository.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_file: Option<String>,
    /// enable and disable: whether the file changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed: Option<bool>,
    /// list: every extension. The others: the one named.
    pub extensions: Vec<ExtensionView>,
    /// show with a skill: the skill as Hallmark's agents read it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill: Option<SkillView>,
    /// show without a skill: the extension's license, in full.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license_text: Option<String>,
    /// enable and disable: what else to know, such as when the reviewer
    /// starts following the change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// An extension and its credits.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ExtensionView {
    /// The name `.hallmark.toml` and the commands use.
    pub id: String,
    pub name: String,
    pub author: String,
    pub source: String,
    /// An SPDX identifier.
    pub license: String,
    /// The upstream commit its text was copied from, unchanged.
    pub commit: String,
    /// The credit, in one line.
    pub credit: String,
    /// Each role it serves.
    pub roles: Vec<RoleView>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RoleView {
    pub role: Role,
    /// Who plays the role.
    pub who: String,
    /// The skills the role follows.
    pub skills: Vec<String>,
    /// Turned on in this repository.
    pub on: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SkillView {
    pub extension: String,
    pub name: String,
    /// From the skill's frontmatter.
    pub description: String,
    /// The roles that follow it.
    pub roles: Vec<Role>,
    /// SKILL.md without its frontmatter, then the files it links to.
    pub text: String,
}

impl Render for ExtensionsOut {
    fn render(&self) -> String {
        if let Some(s) = &self.skill {
            let e = &self.extensions[0];
            return format!(
                "`{}` from {}, {}\n\n{}\n",
                s.name,
                e.name,
                e.credit,
                s.text.trim()
            );
        }
        let mut s = String::new();
        for e in &self.extensions {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&format!("{}  {}\n  {}\n", e.id, e.name, e.credit));
            let width = e
                .roles
                .iter()
                .map(|r| r.role.as_str().len())
                .max()
                .unwrap_or(0);
            for r in &e.roles {
                s.push_str(&format!(
                    "  {:3}  {:width$}  {}\n",
                    if r.on { "on" } else { "off" },
                    r.role.as_str(),
                    r.skills.join(", ")
                ));
            }
        }
        if self.changed == Some(false) {
            s.push_str("(unchanged)\n");
        }
        if let Some(n) = &self.note {
            s.push_str(&format!("\n{n}\n"));
        }
        if let Some(l) = &self.license_text {
            s.push_str(&format!("\n{}\n", l.trim()));
        }
        s
    }
}

fn view(e: &Extension, cfg: Option<&Extensions>) -> ExtensionView {
    ExtensionView {
        id: e.id.into(),
        name: e.name.into(),
        author: e.author.into(),
        source: e.source.into(),
        license: e.license.into(),
        commit: e.commit.into(),
        credit: e.credit(),
        roles: e
            .roles()
            .into_iter()
            .map(|r| RoleView {
                role: r,
                who: r.who().into(),
                skills: e
                    .skills_for(r)
                    .iter()
                    .map(|(s, _)| s.name.to_string())
                    .collect(),
                on: cfg.is_some_and(|c| c.roles(e.id).contains(&r)),
            })
            .collect(),
    }
}

/// The repo, when the command runs in an initialized one.
fn repo_in(cwd: &Path) -> Option<Repo> {
    Repo::discover(cwd)
        .ok()
        .filter(|r| r.main_root.join(config::FILE_NAME).is_file())
}

fn out(action: &str, repo: Option<&Repo>, extensions: Vec<ExtensionView>) -> ExtensionsOut {
    ExtensionsOut {
        action: action.into(),
        repo_file: repo.map(|r| paths::display(&r.main_root.join(config::FILE_NAME))),
        changed: None,
        extensions,
        skill: None,
        license_text: None,
        note: None,
    }
}

fn unknown(name: &str) -> HallError {
    HallError::usage(format!(
        "no extension named {name} (there is {})",
        extensions::names().join(", ")
    ))
    .next(["hall extensions"])
}

fn named(name: &str) -> Result<&'static Extension> {
    extensions::find(name).ok_or_else(|| unknown(name))
}

pub fn extensions(cwd: &Path, a: ExtensionsArgs) -> Result<Response> {
    match a.action.unwrap_or(ExtensionsAction::List) {
        ExtensionsAction::List => list(cwd),
        ExtensionsAction::Enable { name, roles } => change(cwd, &name, &roles, true),
        ExtensionsAction::Disable { name, roles } => change(cwd, &name, &roles, false),
        ExtensionsAction::Show { name, skill } => show(cwd, &name, skill.as_deref()),
    }
}

fn list(cwd: &Path) -> Result<Response> {
    let repo = repo_in(cwd);
    let cfg = repo.as_ref().map(|r| &r.config.extensions);
    let views = extensions::CATALOG.iter().map(|e| view(e, cfg)).collect();
    let mut next: Vec<String> = Vec::new();
    for e in extensions::CATALOG {
        if repo.is_some() {
            next.push(format!("hall extensions enable {}", e.id));
        }
        next.push(format!("hall extensions show {}", e.id));
    }
    if repo.is_none() {
        next.insert(0, "hall init".into());
    }
    Ok(Response::new(out("list", repo.as_ref(), views), next))
}

fn show(cwd: &Path, name: &str, skill: Option<&str>) -> Result<Response> {
    let e = named(name)?;
    let repo = repo_in(cwd);
    let mut o = out(
        "show",
        repo.as_ref(),
        vec![view(e, repo.as_ref().map(|r| &r.config.extensions))],
    );
    let skills = || -> Vec<String> { e.skills.iter().map(|s| s.name.to_string()).collect() };
    match skill {
        Some(s) => {
            let sk = e.skill(s).ok_or_else(|| {
                HallError::usage(format!(
                    "{} has no skill {s} (it has {})",
                    e.id,
                    skills().join(", ")
                ))
                .next([format!("hall extensions show {}", e.id)])
            })?;
            o.skill = Some(SkillView {
                extension: e.id.into(),
                name: sk.name.into(),
                description: sk.description(),
                roles: sk.uses.iter().map(|(r, _)| *r).collect(),
                text: sk.text(),
            });
        }
        None => o.license_text = Some(e.license_text.into()),
    }
    let mut next = vec!["hall extensions".to_string()];
    if skill.is_none() {
        next.extend(
            skills()
                .into_iter()
                .map(|s| format!("hall extensions show {} {s}", e.id)),
        );
    }
    Ok(Response::new(o, next))
}

/// Turns `roles` (every role the extension serves when empty) on or off.
fn change(cwd: &Path, name: &str, roles: &[Role], on: bool) -> Result<Response> {
    let action = if on { "enable" } else { "disable" };
    // Headless runs work under these settings; they don't get to change them.
    if std::env::var_os("HALL_HEADLESS").is_some() {
        return Err(HallError::new(
            ErrorCode::GateDenied,
            format!("`hall extensions {action}` doesn't run inside a Hallmark agent run"),
        )
        .next(["hall extensions"]));
    }
    let e = named(name)?;
    let serves = e.roles();
    if let Some(r) = roles.iter().find(|r| !serves.contains(r)) {
        let names: Vec<&str> = serves.iter().map(|r| r.as_str()).collect();
        return Err(HallError::usage(format!(
            "{} has no skill for the {} (it serves {})",
            e.id,
            r.as_str(),
            names.join(", ")
        ))
        .next(["hall extensions".to_string()]));
    }
    let repo = Repo::discover(cwd)?;
    repo.require_init()?;
    let picked: &[Role] = if roles.is_empty() { &serves } else { roles };
    let before = repo.config.extensions.roles(e.id).to_vec();
    let after: Vec<Role> = Role::ALL
        .into_iter()
        .filter(|r| {
            if picked.contains(r) {
                on
            } else {
                before.contains(r)
            }
        })
        .collect();
    let path = repo.main_root.join(config::FILE_NAME);
    let key = format!("extensions.{}", e.id);
    let value = (!after.is_empty())
        .then(|| toml_edit::Value::Array(after.iter().map(|r| r.as_str()).collect()));
    let changed = super::config::write(Scope::Repo, &path, &key, value)?;

    // Re-read, so the reply shows what Hallmark now uses.
    let repo = Repo::discover(cwd)?;
    let mut o = out(
        action,
        Some(&repo),
        vec![view(e, Some(&repo.config.extensions))],
    );
    o.changed = Some(changed);
    o.note = reviewer_note(&repo, e, &after);
    let mut next = vec![
        "hall extensions".to_string(),
        format!("hall extensions show {}", e.id),
    ];
    if o.note.is_some() {
        next.push("hall doctor".into());
    }
    Ok(Response::new(o, next))
}

/// The reviewer reads its settings from the base branch (ADR 0012): says
/// so when the base doesn't have this change for it yet.
fn reviewer_note(repo: &Repo, e: &Extension, after: &[Role]) -> Option<String> {
    let base = crate::assayer::base::Base::resolve(repo, &repo.config.base).ok()?;
    if !base.config_from_base {
        return None;
    }
    let want = after.contains(&Role::Reviewer);
    if base.config.extensions.roles(e.id).contains(&Role::Reviewer) == want {
        return None;
    }
    Some(format!(
        "The reviewer reads {} from {}, so it {} {} once this change is committed there. \
         The other roles use it now.",
        config::FILE_NAME,
        base.name,
        if want { "follows" } else { "stops following" },
        e.name
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_shows_each_role_on_or_off_with_the_credit() {
        let e = extensions::find("mattpocock").unwrap();
        let cfg = Extensions {
            mattpocock: vec![Role::Reviewer],
        };
        let mut o = out("enable", None, vec![view(e, Some(&cfg))]);
        o.changed = Some(false);
        o.note = Some("NOTE".into());
        assert_eq!(
            o.render(),
            "mattpocock  Matt Pocock's skills\n  \
             by Matt Pocock, https://github.com/mattpocock/skills (MIT) at 49dd158\n  \
             off  planner    to-tickets\n  \
             off  developer  tdd, diagnosing-bugs, codebase-design\n  \
             on   reviewer   code-review\n  \
             off  fixer      tdd, diagnosing-bugs\n\
             (unchanged)\n\nNOTE\n"
        );
    }

    #[test]
    fn text_of_a_skill_leads_with_its_credit() {
        let e = extensions::find("mattpocock").unwrap();
        let sk = e.skill("tdd").unwrap();
        let mut o = out("show", None, vec![view(e, None)]);
        o.skill = Some(SkillView {
            extension: e.id.into(),
            name: sk.name.into(),
            description: sk.description(),
            roles: vec![],
            text: sk.text(),
        });
        let t = o.render();
        assert!(
            t.starts_with(
                "`tdd` from Matt Pocock's skills, by Matt Pocock, \
                 https://github.com/mattpocock/skills (MIT) at 49dd158\n\n# Test-Driven Development"
            ),
            "{t}"
        );
    }
}
