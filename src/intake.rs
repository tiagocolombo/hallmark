//! The Front Desk's plan (workstream H, ADR 0024): can a request be split
//! into parallel parts? A small Haiku run answers with a schema-checked plan.
//!
//! Splitting is an optimisation, never a requirement, so intake **fails
//! open**: a missing `claude`, a blocked budget, a failed run or a plan that
//! doesn't hold up (fewer than two parts, too many, overlapping paths) all
//! mean "one item", with the reason in the reply.

use std::path::Path;

use globset::{GlobBuilder, GlobMatcher};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::Role;
use crate::output::clip;
use crate::repo::Repo;
use crate::runner::{self, Metered};

/// One independent part of a split request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Part {
    pub title: String,
    /// A self-contained request for whoever builds this part.
    pub request: String,
    /// Repository-relative files, directories or globs it touches.
    #[serde(default)]
    pub paths: Vec<String>,
}

/// What the model returned (`plugin/schemas/plan.json`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawPlan {
    #[serde(default)]
    pub parallel: bool,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub parts: Vec<Part>,
}

/// The Front Desk's decision, after Hallmark checked it.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Plan {
    /// True when an intake run was made (even one that failed).
    pub ran: bool,
    /// True when the request is built as parallel parts.
    pub parallel: bool,
    /// Why it was split, or why it stays one item.
    pub reason: String,
    /// The parts, when parallel.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<Part>,
}

impl Plan {
    fn single(ran: bool, reason: impl Into<String>) -> Plan {
        Plan {
            ran,
            parallel: false,
            reason: reason.into(),
            parts: Vec::new(),
        }
    }
}

/// A plan and the raw `claude -p` output it came from, kept in the item's
/// `runs/` directory once the item exists.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub plan: Plan,
    pub raw: Option<String>,
}

/// True when a model run can start at all: the fake runner, or a `claude`
/// binary that exists. Without one, `hall new` doesn't wait for a run that
/// can only fail.
pub fn runner_available() -> bool {
    if std::env::var("HALL_RUNNER").is_ok_and(|v| v.starts_with("fake:")) {
        return true;
    }
    let bin = crate::config::Tool::Claude.command();
    let p = Path::new(&bin);
    if p.is_absolute() || p.components().count() > 1 {
        p.is_file()
    } else {
        crate::gh::which(&bin).is_some()
    }
}

/// Asks the Front Desk for a plan. Never fails: anything that goes wrong
/// means one item, and the reason says why.
pub fn plan(repo: &Repo, request: &str) -> Outcome {
    let skipped = |reason: String| Outcome {
        plan: Plan::single(false, reason),
        raw: None,
    };
    if !runner_available() {
        return skipped("the Front Desk did not run: claude was not found".into());
    }
    let max = max_parts(repo);
    let mut prompt = prompt(request, &repo_map(repo), max);
    // Extensions turned on for the planner (ADR 0038).
    if let Some(x) =
        crate::extensions::prompt(&repo.config.extensions, crate::extensions::Role::Planner)
    {
        prompt.push_str(&format!("\n{x}"));
    }
    let mut req = match runner::request(repo, Role::Intake, Some("intake"), prompt, &repo.main_root)
    {
        Ok(r) => r,
        Err(e) => return skipped(format!("the Front Desk could not start: {}", e.message)),
    };
    req.json_schema = Some(crate::plugin::file("schemas/plan.json").into());
    req.allowed_tools = ["Read", "Grep", "Glob", "StructuredOutput"]
        .map(String::from)
        .to_vec();
    let runner = runner::from_env();
    match runner::run_metered(repo, runner.as_ref(), None, &req) {
        Ok(Metered::Done(o)) => {
            let plan = match o
                .structured()
                .map(|v| serde_json::from_value::<RawPlan>(v.clone()))
            {
                Some(Ok(raw)) => validate(raw, max),
                Some(Err(e)) => Plan::single(true, format!("the plan was unreadable ({e})")),
                None => Plan::single(true, "the Front Desk returned no plan"),
            };
            Outcome {
                plan,
                raw: Some(o.raw),
            }
        }
        Ok(Metered::Failed {
            outcome, failure, ..
        }) => Outcome {
            plan: Plan::single(
                true,
                format!("the Front Desk run failed: {}", failure.describe()),
            ),
            raw: Some(outcome.raw).filter(|r| !r.trim().is_empty()),
        },
        Ok(Metered::Blocked(stop)) => {
            skipped(format!("the Front Desk did not run: {}", stop.reason))
        }
        Err(e) => skipped(format!("the Front Desk could not run: {}", e.message)),
    }
}

fn max_parts(repo: &Repo) -> usize {
    repo.config.intake.max_parts.max(2) as usize
}

/// The prompt: the request, the limits, and a compact map of the repo.
pub fn prompt(request: &str, map: &str, max_parts: usize) -> String {
    format!(
        "Plan this request for Hallmark's Front Desk.\n\n\
         <request>\n{}\n</request>\n\n\
         Split it into 2 to {max_parts} parts only if the parts are truly independent: \
         disjoint files, no ordering between them, each buildable and testable alone. \
         Otherwise answer parallel: false with the reason. Paths are relative to the \
         repository root, which is your working directory.\n\n\
         <repository>\n{map}\n</repository>\n",
        request.trim()
    )
}

/// Top-level entries with file counts, the first tracked files, and the
/// headings of the base branch's CLAUDE.md / AGENTS.md.
pub fn repo_map(repo: &Repo) -> String {
    let root = &repo.main_root;
    let files: Vec<String> = crate::git::run(root, &["ls-files"])
        .map(|s| s.lines().map(String::from).collect())
        .unwrap_or_default();
    let mut s = String::new();
    let mut top: Vec<(String, usize)> = Vec::new();
    for f in &files {
        let (name, dir) = match f.split_once('/') {
            Some((d, _)) => (format!("{d}/"), true),
            None => (f.clone(), false),
        };
        match top.iter_mut().find(|(n, _)| *n == name) {
            Some((_, c)) => *c += 1,
            None => top.push((name, usize::from(dir))),
        }
    }
    s.push_str("Top level:\n");
    for (name, count) in top.iter().take(60) {
        if name.ends_with('/') {
            s.push_str(&format!("  {name} ({count} files)\n"));
        } else {
            s.push_str(&format!("  {name}\n"));
        }
    }
    const LIST: usize = 150;
    s.push_str(&format!(
        "\nTracked files ({} in all{}):\n",
        files.len(),
        if files.len() > LIST {
            format!(", first {LIST}")
        } else {
            String::new()
        }
    ));
    for f in files.iter().take(LIST) {
        s.push_str(&format!("  {}\n", clip(f, 120)));
    }
    for doc in ["CLAUDE.md", "AGENTS.md"] {
        let text = crate::git::run(root, &["show", &format!("{}:{doc}", repo.config.base)])
            .ok()
            .or_else(|| std::fs::read_to_string(root.join(doc)).ok());
        let Some(text) = text else { continue };
        let heads: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with('#'))
            .take(40)
            .collect();
        if !heads.is_empty() {
            s.push_str(&format!("\n{doc} headings:\n"));
            for h in heads {
                s.push_str(&format!("  {}\n", clip(h, 100)));
            }
        }
    }
    clip(&s, 12_000)
}

/// Checks a plan: 2 to `max` parts, each with a title, a request and paths,
/// and no path shared between parts. Anything else is one item.
pub fn validate(raw: RawPlan, max: usize) -> Plan {
    let reason = raw.reason.trim().to_string();
    if !raw.parallel {
        return Plan::single(
            true,
            if reason.is_empty() {
                "the Front Desk kept it as one item".into()
            } else {
                reason
            },
        );
    }
    let reject = |why: String| Plan::single(true, format!("{why}; kept as one item"));
    if raw.parts.len() < 2 {
        return reject("the plan had fewer than 2 parts".into());
    }
    if raw.parts.len() > max {
        return reject(format!(
            "the plan had {} parts, more than intake.max_parts ({max})",
            raw.parts.len()
        ));
    }
    let mut parts = Vec::new();
    for p in raw.parts {
        let title = p.title.trim().to_string();
        let request = p.request.trim().to_string();
        if title.is_empty() || request.is_empty() {
            return reject("a part had no title or request".into());
        }
        if p.paths.is_empty() {
            return reject(format!("part \"{title}\" listed no paths"));
        }
        let mut paths = Vec::new();
        for path in &p.paths {
            match clean_path(path) {
                Some(c) => paths.push(c),
                None => return reject(format!("part \"{title}\" has an unusable path \"{path}\"")),
            }
        }
        parts.push(Part {
            title,
            request,
            paths,
        });
    }
    for (i, a) in parts.iter().enumerate() {
        for b in &parts[i + 1..] {
            for pa in &a.paths {
                for pb in &b.paths {
                    if overlap(pa, pb) {
                        return reject(format!(
                            "parts \"{}\" and \"{}\" overlap ({pa} and {pb})",
                            a.title, b.title
                        ));
                    }
                }
            }
        }
    }
    Plan {
        ran: true,
        parallel: true,
        reason: if reason.is_empty() {
            "the parts touch disjoint paths".into()
        } else {
            reason
        },
        parts,
    }
}

/// `./src/api/` → `src/api`. Absolute paths and `..` are refused; `.` or
/// an empty path means the whole repository (it overlaps everything).
fn clean_path(p: &str) -> Option<String> {
    let mut s = p.trim().replace('\\', "/");
    if s.starts_with('/') || s.get(1..2) == Some(":") {
        return None;
    }
    while let Some(rest) = s.strip_prefix("./") {
        s = rest.to_string();
    }
    let s = s.trim_end_matches('/').to_string();
    if s.split('/').any(|c| c == "..") {
        return None;
    }
    Some(if s == "." { String::new() } else { s })
}

fn is_glob(component: &str) -> bool {
    component.contains(['*', '?', '[', '{'])
}

fn has_glob(p: &str) -> bool {
    p.split('/').any(is_glob)
}

/// The components before the first one with a glob character.
fn literal(p: &str) -> Vec<&str> {
    p.split('/')
        .filter(|c| !c.is_empty())
        .take_while(|c| !is_glob(c))
        .collect()
}

fn matcher(p: &str) -> Option<GlobMatcher> {
    GlobBuilder::new(p)
        .literal_separator(true)
        .build()
        .ok()
        .map(|g| g.compile_matcher())
}

/// Could two path patterns name a common file? Plain paths overlap when one
/// contains the other. A glob is compared through its literal prefix,
/// conservatively: when unsure, they overlap.
pub fn overlap(a: &str, b: &str) -> bool {
    let (la, lb) = (literal(a), literal(b));
    let nested = la.starts_with(&lb) || lb.starts_with(&la);
    if !nested {
        return false;
    }
    match (has_glob(a), has_glob(b)) {
        (false, false) => true,
        (true, true) => true,
        (true, false) => glob_reaches(a, b),
        (false, true) => glob_reaches(b, a),
    }
}

/// Could glob `g` match `plain` or a file under it?
fn glob_reaches(g: &str, plain: &str) -> bool {
    if g.contains("**") {
        return true;
    }
    let Some(m) = matcher(g) else { return true };
    // A glob without `**` matches at a fixed depth: deeper than `plain`
    // means it may match inside it.
    m.is_match(plain) || g.split('/').count() > plain.split('/').count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(title: &str, paths: &[&str]) -> Part {
        Part {
            title: title.into(),
            request: format!("do {title}"),
            paths: paths.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn raw(parts: Vec<Part>) -> RawPlan {
        RawPlan {
            parallel: true,
            reason: "independent".into(),
            parts,
        }
    }

    #[test]
    fn overlapping_paths() {
        assert!(overlap("src/api", "src/api/users.rs"));
        assert!(overlap("src/api/users.rs", "src/api"));
        assert!(overlap("Cargo.toml", "Cargo.toml"));
        assert!(overlap("", "web"));
        assert!(overlap("**/*.rs", "web/x.rs"));
        assert!(overlap("src/**", "src/a.rs"));
        assert!(overlap("src/*/mod.rs", "src/web"));
        assert!(overlap("src/*.rs", "src/lib.rs"));
        assert!(!overlap("src/api", "src/web"));
        assert!(!overlap("src/a.rs", "src/ab.rs"));
        assert!(!overlap("src/*.rs", "src/web/x.ts"));
        assert!(!overlap("api/**", "web/**"));
        assert!(!overlap("docs/*.md", "src/**"));
    }

    #[test]
    fn cleans_paths() {
        assert_eq!(clean_path("./src/api/").as_deref(), Some("src/api"));
        assert_eq!(clean_path("web\\x.ts").as_deref(), Some("web/x.ts"));
        assert_eq!(clean_path(".").as_deref(), Some(""));
        assert_eq!(clean_path("/etc/passwd"), None);
        assert_eq!(clean_path("C:/x"), None);
        assert_eq!(clean_path("../other"), None);
    }

    #[test]
    fn a_good_plan_splits() {
        let p = validate(
            raw(vec![part("API", &["api/"]), part("Web", &["web/**"])]),
            4,
        );
        assert!(p.parallel, "{p:?}");
        assert_eq!(p.parts.len(), 2);
        assert_eq!(p.parts[0].paths, vec!["api"]);
    }

    #[test]
    fn bad_plans_stay_one_item() {
        let one = validate(raw(vec![part("API", &["api"])]), 4);
        assert!(!one.parallel && one.reason.contains("fewer than 2"));
        let many = validate(
            raw(vec![
                part("a", &["a"]),
                part("b", &["b"]),
                part("c", &["c"]),
            ]),
            2,
        );
        assert!(!many.parallel && many.reason.contains("max_parts"));
        let shared = validate(
            raw(vec![
                part("API", &["api", "Cargo.toml"]),
                part("Web", &["web", "Cargo.toml"]),
            ]),
            4,
        );
        assert!(
            !shared.parallel && shared.reason.contains("overlap"),
            "{shared:?}"
        );
        let pathless = validate(raw(vec![part("API", &["api"]), part("Web", &[])]), 4);
        assert!(!pathless.parallel && pathless.reason.contains("no paths"));
        let escape = validate(raw(vec![part("a", &["a"]), part("b", &["../b"])]), 4);
        assert!(!escape.parallel && escape.reason.contains("unusable"));
        let no = validate(
            RawPlan {
                parallel: false,
                reason: "one feature".into(),
                parts: vec![],
            },
            4,
        );
        assert!(!no.parallel && no.ran);
        assert_eq!(no.reason, "one feature");
    }

    #[test]
    fn the_prompt_carries_the_request_and_limits() {
        let p = prompt("  fix api and web  ", "Top level:\n  api/ (3 files)\n", 3);
        assert!(p.contains("<request>\nfix api and web\n</request>"));
        assert!(p.contains("2 to 3 parts"));
        assert!(p.contains("api/ (3 files)"));
    }

    #[test]
    fn the_schema_is_embedded() {
        let v: serde_json::Value =
            serde_json::from_str(crate::plugin::file("schemas/plan.json")).unwrap();
        assert_eq!(
            v["required"],
            serde_json::json!(["parallel", "parts", "reason"])
        );
        // Claude Code's --json-schema validator rejects a draft 2020-12
        // `$schema` key ("no schema with key or ref"), and the run fails.
        assert!(v.get("$schema").is_none());
        // An agent with a `tools:` list must allow StructuredOutput, or a
        // --json-schema run answers in prose and has no structured_output.
        let agent = crate::plugin::file("agents/intake.md");
        let tools = agent.lines().find(|l| l.starts_with("tools:")).unwrap();
        assert!(tools.contains("StructuredOutput"), "{tools}");
    }
}
