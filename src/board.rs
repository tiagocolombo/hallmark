//! The board (ADR 0068): every open item, as the Product Manager and the
//! Lead see it when they plan a request. What each item is for (its request
//! with added scope, and its criteria), where it is, and what it has
//! changed so far, with line counts from its merge base.

use schemars::JsonSchema;
use serde::Serialize;

use crate::item::{ItemId, State};
use crate::output::clip;
use crate::repo::Repo;

/// At most this many items are put on the board, newest last.
const MAX_ITEMS: usize = 25;
/// At most this many changed paths per item.
const MAX_PATHS: usize = 40;

/// One changed file and the lines it added and removed.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Changed {
    pub path: String,
    pub lines: u64,
}

/// One open item on the board.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Card {
    pub id: ItemId,
    pub title: String,
    pub state: State,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<ItemId>,
    /// The request with any scope added to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub criteria: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    /// What its branch changed since its merge base with its base.
    pub changed: Vec<Changed>,
    /// Lines added plus removed, over every changed file.
    pub lines: u64,
}

/// The open items: everything not closed or dropped. A journal or a diff
/// that can't be read leaves that part of the card empty; the board itself
/// never fails a plan.
pub fn open(repo: &Repo) -> Vec<Card> {
    let Ok(items) = repo.index().and_then(|ix| ix.items(false)) else {
        return Vec::new();
    };
    let skip = items.len().saturating_sub(MAX_ITEMS);
    let mut cards: Vec<Card> = items
        .into_iter()
        .filter(|i| !i.state.is_terminal())
        .skip(skip)
        .map(|i| {
            let events = repo.hall.read_item(i.id).unwrap_or_default();
            let (changed, lines) = diff(repo, &i.base, &i.branch);
            Card {
                id: i.id,
                title: i.title,
                state: i.state,
                parent: i.parent,
                request: crate::event::request_of(&events),
                criteria: crate::event::criteria_of(&events),
                summary: i.summary,
                pr_url: i.pr_url,
                changed,
                lines,
            }
        })
        .collect();
    add_children_lines(&mut cards);
    cards
}

/// A parent's own branch is empty until its parts merge, but its review
/// will cover them: count each open part's lines in the parent's card.
fn add_children_lines(cards: &mut [Card]) {
    let parts: Vec<(ItemId, u64, Vec<Changed>)> = cards
        .iter()
        .filter_map(|c| c.parent.map(|p| (p, c.lines, c.changed.clone())))
        .collect();
    for c in cards.iter_mut() {
        let mine: Vec<&(ItemId, u64, Vec<Changed>)> =
            parts.iter().filter(|(p, _, _)| *p == c.id).collect();
        if mine.is_empty() {
            continue;
        }
        // The parts' paths count too: the security-mix check and the
        // prompt see what the parent's review will cover.
        for (_, lines, changed) in mine {
            c.lines += lines;
            c.changed.extend(changed.iter().cloned());
        }
        c.changed
            .sort_by(|a, b| b.lines.cmp(&a.lines).then(a.path.cmp(&b.path)));
        c.changed.truncate(MAX_PATHS);
    }
}

/// `git diff --numstat <base>...<branch>`: the files the branch changed
/// since it left its base. A binary file counts as one line.
fn diff(repo: &Repo, base: &str, branch: &str) -> (Vec<Changed>, u64) {
    let root = &repo.main_root;
    let from = [format!("origin/{base}"), base.to_string()]
        .into_iter()
        .find(|r| crate::git::rev(root, r).is_some());
    let Some(from) = from else {
        return (Vec::new(), 0);
    };
    let out = crate::git::run(
        root,
        &[
            "diff",
            "--numstat",
            "--no-renames",
            &format!("{from}...{branch}"),
        ],
    )
    .unwrap_or_default();
    let mut changed = parse_numstat(&out);
    let lines = changed.iter().map(|c| c.lines).sum();
    changed.sort_by(|a, b| b.lines.cmp(&a.lines).then(a.path.cmp(&b.path)));
    changed.truncate(MAX_PATHS);
    (changed, lines)
}

/// `12\t3\tsrc/a.rs` lines; `-\t-\tlogo.png` (binary) counts as one line.
pub fn parse_numstat(out: &str) -> Vec<Changed> {
    out.lines()
        .filter_map(|l| {
            let mut f = l.splitn(3, '\t');
            let (a, d, path) = (f.next()?, f.next()?, f.next()?);
            let n = |s: &str| s.parse::<u64>().ok();
            let lines = match (n(a), n(d)) {
                (Some(a), Some(d)) => a + d,
                _ => 1,
            };
            Some(Changed {
                path: path.to_string(),
                lines,
            })
        })
        .collect()
}

/// The board as the planners' prompt shows it.
pub fn render(cards: &[Card]) -> String {
    if cards.is_empty() {
        return "No open items.\n".into();
    }
    let mut s = String::new();
    for c in cards {
        s.push_str(&format!(
            "- {} [{}] \"{}\"{}\n",
            c.id,
            c.state.as_str(),
            clip(&c.title, 80),
            c.parent
                .map(|p| format!(" (a parallel part of {p})"))
                .unwrap_or_default()
        ));
        if let Some(r) = &c.request {
            s.push_str(&format!("  request: {}\n", one_line(r, 500)));
        }
        for k in &c.criteria {
            s.push_str(&format!("  criterion: {}\n", one_line(k, 160)));
        }
        if let Some(sm) = &c.summary {
            s.push_str(&format!("  summary: {}\n", one_line(sm, 300)));
        }
        if let Some(pr) = &c.pr_url {
            s.push_str(&format!("  pr: {pr}\n"));
        }
        if c.changed.is_empty() {
            s.push_str("  changed: nothing yet\n");
        } else {
            let list: Vec<String> = c
                .changed
                .iter()
                .map(|x| format!("{} ({})", x.path, x.lines))
                .collect();
            s.push_str(&format!(
                "  changed: {} lines in {} files: {}\n",
                c.lines,
                c.changed.len(),
                list.join(", ")
            ));
        }
    }
    s
}

fn one_line(s: &str, n: usize) -> String {
    clip(&s.split_whitespace().collect::<Vec<_>>().join(" "), n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_counts_lines_and_binaries() {
        let c = parse_numstat("12\t3\tsrc/a.rs\n-\t-\tlogo.png\n0\t7\tdocs/x.md\nnoise\n");
        assert_eq!(
            c,
            vec![
                Changed {
                    path: "src/a.rs".into(),
                    lines: 15
                },
                Changed {
                    path: "logo.png".into(),
                    lines: 1
                },
                Changed {
                    path: "docs/x.md".into(),
                    lines: 7
                },
            ]
        );
    }

    #[test]
    fn a_parent_counts_its_open_parts_lines() {
        let mk = |id: &str, parent: Option<&str>, lines: u64| Card {
            id: id.parse().unwrap(),
            title: "t".into(),
            state: State::Development,
            parent: parent.map(|p| p.parse().unwrap()),
            request: None,
            criteria: Vec::new(),
            summary: None,
            pr_url: None,
            changed: if lines == 0 {
                Vec::new()
            } else {
                vec![Changed {
                    path: format!("part{lines}.rs"),
                    lines,
                }]
            },
            lines,
        };
        let mut cards = vec![
            mk("H-0041", None, 0),
            mk("H-0042", Some("H-0041"), 300),
            mk("H-0043", Some("H-0041"), 200),
        ];
        add_children_lines(&mut cards);
        assert_eq!(cards[0].lines, 500);
        assert_eq!(cards[1].lines, 300);
        // The parent shows its parts' paths, largest first.
        let paths: Vec<&str> = cards[0].changed.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, ["part300.rs", "part200.rs"]);
        assert!(render(&cards[..1]).contains("changed: 500 lines in 2 files"));
    }

    #[test]
    fn the_board_renders_each_card() {
        let card = Card {
            id: "H-0042".parse().unwrap(),
            title: "Retry webhooks".into(),
            state: State::Development,
            parent: None,
            request: Some("Retry failed\nwebhook deliveries".into()),
            criteria: vec!["stops after 6 attempts".into()],
            summary: None,
            pr_url: None,
            changed: vec![Changed {
                path: "src/hooks.rs".into(),
                lines: 40,
            }],
            lines: 40,
        };
        let s = render(&[card]);
        assert!(
            s.contains("- H-0042 [development] \"Retry webhooks\""),
            "{s}"
        );
        assert!(
            s.contains("request: Retry failed webhook deliveries"),
            "{s}"
        );
        assert!(s.contains("criterion: stops after 6 attempts"), "{s}");
        assert!(
            s.contains("changed: 40 lines in 1 files: src/hooks.rs (40)"),
            "{s}"
        );
        assert_eq!(render(&[]), "No open items.\n");
    }
}
