//! Conventional commits (ADR 0035): `type(scope)!: subject`.
//!
//! PRs are usually squash-merged, so the PR title becomes the commit on the
//! base branch that release tooling reads to pick the next version. An
//! item's title is written for people ("Retries with jitter"), so the
//! shipper prefixes it with the type its commits carry. A branch whose
//! commits aren't conventional keeps its title unchanged.

/// The types, most significant first: a branch with a `feat` and a `fix`
/// ships as a `feat`.
const TYPES: &[&str] = &[
    "feat", "fix", "perf", "refactor", "revert", "build", "test", "ci", "docs", "style", "chore",
];

/// A parsed commit message: its type and whether it breaks compatibility.
#[derive(Debug, Clone, PartialEq)]
pub struct Kind {
    pub ty: &'static str,
    pub breaking: bool,
}

/// The type of a commit message, when its subject is conventional. A
/// breaking change is a `!` before the colon or a `BREAKING CHANGE:` footer.
pub fn parse(message: &str) -> Option<Kind> {
    let subject = message.lines().next()?.trim();
    let (head, rest) = subject.split_once(':')?;
    if !rest.starts_with(' ') || rest.trim().is_empty() {
        return None;
    }
    let (head, bang) = match head.strip_suffix('!') {
        Some(h) => (h, true),
        None => (head, false),
    };
    let ty = match head.split_once('(') {
        Some((ty, scope)) if scope.ends_with(')') && scope.len() > 1 => ty,
        Some(_) => return None,
        None => head,
    };
    let ty = TYPES.iter().find(|t| **t == ty)?;
    let footer = message.lines().skip(1).any(|l| {
        let l = l.trim_start();
        l.starts_with("BREAKING CHANGE:") || l.starts_with("BREAKING-CHANGE:")
    });
    Some(Kind {
        ty,
        breaking: bang || footer,
    })
}

/// The kind a set of commits ships as: the most significant type among
/// them, breaking if any of them is. None when none is conventional.
pub fn combined<'a>(messages: impl IntoIterator<Item = &'a str>) -> Option<Kind> {
    let kinds: Vec<Kind> = messages.into_iter().filter_map(parse).collect();
    let rank = |k: &Kind| TYPES.iter().position(|t| *t == k.ty).unwrap_or(TYPES.len());
    let top = kinds.iter().min_by_key(|k| rank(k))?;
    Some(Kind {
        ty: top.ty,
        breaking: kinds.iter().any(|k| k.breaking),
    })
}

/// The PR title for an item: its own when it is already conventional,
/// else prefixed with the kind of its commits, else unchanged.
pub fn pr_title<'a>(title: &str, messages: impl IntoIterator<Item = &'a str>) -> String {
    let title = title.trim();
    if parse(title).is_some() {
        return title.to_string();
    }
    match combined(messages) {
        Some(k) => format!("{}{}: {title}", k.ty, if k.breaking { "!" } else { "" }),
        None => title.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_types_scopes_and_breaking_changes() {
        let k = |ty, breaking| Some(Kind { ty, breaking });
        assert_eq!(parse("feat: retries"), k("feat", false));
        assert_eq!(parse("fix(test): run it"), k("fix", false));
        assert_eq!(parse("refactor(gate)!: split"), k("refactor", true));
        assert_eq!(
            parse("feat: new store\n\nBREAKING CHANGE: the journal moved"),
            k("feat", true)
        );
        for not in [
            "Merge pull request #4 from x/y",
            "Configurable setup: hall config",
            "feat:no space",
            "feat: ",
            "feature: retries",
            "fix(): empty scope",
            "",
        ] {
            assert_eq!(parse(not), None, "{not:?}");
        }
    }

    #[test]
    fn the_most_significant_type_wins() {
        let k = combined(["chore: bump", "fix: a bug", "feat: a thing", "docs: say so"]).unwrap();
        assert_eq!((k.ty, k.breaking), ("feat", false));
        let k = combined(["test: cover it", "fix(x)!: change the reply"]).unwrap();
        assert_eq!((k.ty, k.breaking), ("fix", true));
        assert_eq!(combined(["WIP", "more"]), None);
    }

    #[test]
    fn titles_take_the_commits_type() {
        assert_eq!(
            pr_title("Retries with jitter", ["fix: jitter", "feat: retries"]),
            "feat: Retries with jitter"
        );
        assert_eq!(
            pr_title("New journal", ["feat!: move the journal"]),
            "feat!: New journal"
        );
        // Already conventional, or nothing to go on: unchanged.
        assert_eq!(pr_title("fix: login", ["feat: x"]), "fix: login");
        assert_eq!(pr_title("Login", ["tweak", "more"]), "Login");
    }
}
