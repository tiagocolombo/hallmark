//! The reviewer's briefing: accepted learnings from the base branch that fit
//! this change, within `[library] briefing_limit` tokens.
//!
//! 1. Learnings whose `scope` globs match a changed path, most matches
//!    first.
//! 2. Then a search of the rest with terms from the diff: identifiers and
//!    words on changed lines, and path segments.
//!
//! Duplicates are dropped, and a learning that doesn't fit the cap is
//! skipped for a smaller one. Tokens are estimated as characters / 4.

use globset::{GlobBuilder, GlobMatcher};

use super::Briefing;
use super::learning::Status;
use super::search::{self, Options};
use super::store::{self, Located};
use crate::error::Result;
use crate::repo::Repo;

/// Tokens, estimated from characters.
pub fn tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

/// The cap from `[library] briefing_limit` ("6k").
pub fn limit(repo: &Repo) -> u64 {
    crate::config::parse_tokens(&repo.config.library.briefing_limit).unwrap_or(6000)
}

fn matcher(glob: &str) -> Option<GlobMatcher> {
    let g = glob.trim().trim_start_matches("./");
    GlobBuilder::new(g)
        .literal_separator(true)
        .build()
        .ok()
        .map(|g| g.compile_matcher())
}

/// How many of `changed` the learning's scope covers.
pub fn scope_matches(scope: &[String], changed: &[String]) -> usize {
    let ms: Vec<GlobMatcher> = scope.iter().filter_map(|g| matcher(g)).collect();
    if ms.is_empty() {
        return 0;
    }
    changed
        .iter()
        .filter(|p| {
            let p = p.replace('\\', "/");
            ms.iter().any(|m| m.is_match(&p))
        })
        .count()
}

pub fn header(base: &str) -> String {
    format!(
        "## Library briefing\n\n\
What earlier reviews of this repository taught, from accepted learnings on `{base}`. \
They are situational knowledge, not rules: apply one only where it fits this change. \
In your verdict, report each learning you used in `learnings` as \
`{{\"id\": \"L-0017\", \"held\": true}}`; set `\"held\": false` (with a `note`) when this \
change shows it is wrong or stale. Leave out the ones you didn't use.\n\n"
    )
}

/// One learning, compactly.
pub fn entry(l: &Located) -> String {
    let x = &l.learning;
    let mut meta = vec![x.kind.as_str().to_string()];
    if let Some(lens) = &x.lens {
        meta.push(lens.clone());
    }
    if !x.scope.is_empty() {
        meta.push(format!("scope `{}`", x.scope.join("`, `")));
    }
    let mut s = format!("- **{}** ({}): {}\n", x.id, meta.join("; "), x.title.trim());
    for line in x.body.trim().lines() {
        if line.trim().is_empty() {
            s.push('\n');
        } else {
            s.push_str("  ");
            s.push_str(line.trim_end());
            s.push('\n');
        }
    }
    s
}

/// Builds the briefing from `learnings` (already read from the base).
pub fn build(
    repo: &Repo,
    base: &str,
    learnings: &[Located],
    changed: &[String],
    diff: &str,
    cap: u64,
) -> Result<Briefing> {
    let usable: Vec<Located> = learnings
        .iter()
        .filter(|l| l.learning.status == Status::Accepted)
        .cloned()
        .collect();
    if usable.is_empty() || cap == 0 {
        return Ok(Briefing::default());
    }
    // 1. Scoped to the changed paths.
    let mut scoped: Vec<(usize, &Located)> = usable
        .iter()
        .map(|l| (scope_matches(&l.learning.scope, changed), l))
        .filter(|(n, _)| *n > 0)
        .collect();
    scoped.sort_by(|(na, a), (nb, b)| {
        nb.cmp(na)
            .then(b.learning.used.cmp(&a.learning.used))
            .then(a.learning.id.cmp(&b.learning.id))
    });
    let mut picked: Vec<&Located> = scoped.into_iter().map(|(_, l)| l).collect();

    // 2. A search of the rest with the diff's terms.
    let query = search::diff_terms(changed, diff, 40).join(" ");
    if !query.is_empty() {
        let opts = Options {
            limit: 0,
            kind: None,
            statuses: vec![Status::Accepted],
        };
        let found = search::search(repo, &usable, &query, &opts)?;
        for h in found.hits {
            if let Some(l) = usable.iter().find(|l| l.learning.id == h.id)
                && !picked.iter().any(|p| p.learning.id == h.id)
            {
                picked.push(l);
            }
        }
    }

    let mut text = header(base);
    let mut ids = Vec::new();
    for l in picked {
        let e = entry(l);
        if tokens(&text) + tokens(&e) > cap {
            continue;
        }
        text.push_str(&e);
        ids.push(l.learning.id.clone());
    }
    if ids.is_empty() {
        return Ok(Briefing::default());
    }
    let tokens = tokens(&text);
    Ok(Briefing { text, ids, tokens })
}

/// [`super::briefing`]: reads the base, then [`build`]. A base that can't be
/// read gives an empty briefing (the review goes on without one).
pub fn briefing(repo: &Repo, base: &str, changed: &[String], diff: &str) -> Result<Briefing> {
    let learnings = match store::read_base(repo, base) {
        Ok(l) => l,
        Err(e) => {
            repo.hall
                .live(&format!("library: no briefing, {base} unreadable: {e}"));
            return Ok(Briefing::default());
        }
    };
    build(repo, base, &learnings, changed, diff, limit(repo))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_match_globs() {
        let changed = vec![
            "apps/api/src/webhooks/retry.rs".to_string(),
            "apps/api/src/billing.rs".to_string(),
        ];
        assert_eq!(
            scope_matches(&["apps/api/src/webhooks/**".into()], &changed),
            1
        );
        assert_eq!(scope_matches(&["apps/api/src/*.rs".into()], &changed), 1);
        assert_eq!(scope_matches(&["apps/**".into()], &changed), 2);
        assert_eq!(
            scope_matches(&["./apps/api/src/billing.rs".into()], &changed),
            1
        );
        assert_eq!(scope_matches(&["web/**".into()], &changed), 0);
        assert_eq!(scope_matches(&[], &changed), 0);
        assert_eq!(
            scope_matches(&["[".into()], &changed),
            0,
            "bad globs match nothing"
        );
    }

    #[test]
    fn tokens_are_chars_over_four() {
        assert_eq!(tokens(""), 0);
        assert_eq!(tokens("abcd"), 1);
        assert_eq!(tokens("abcde"), 2);
    }
}
