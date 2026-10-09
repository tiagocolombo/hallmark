//! Reading shell commands for the gate: which parts commit, which push, and
//! in which directory. This is a best-effort parser for the commands agents
//! write (`cd x && git commit -m "…"`, `git -C x push`), not a shell.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpKind {
    Commit,
    /// `git push`, `gh pr create`, or the configured ship command.
    Push,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    pub kind: OpKind,
    /// The directory the command runs in.
    pub dir: PathBuf,
    pub text: String,
}

/// Splits on `&&`, `||`, `;`, `|`, `&` and newlines, outside quotes.
pub fn split_segments(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = cmd.chars().peekable();
    let (mut single, mut double) = (false, false);
    while let Some(c) = chars.next() {
        match c {
            '\\' if !single => {
                cur.push(c);
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            '\'' if !double => {
                single = !single;
                cur.push(c);
            }
            '"' if !single => {
                double = !double;
                cur.push(c);
            }
            ';' | '\n' | '|' | '&' if !single && !double => {
                if matches!(chars.peek(), Some('&') | Some('|')) {
                    chars.next();
                }
                if !cur.trim().is_empty() {
                    out.push(cur.trim().to_string());
                }
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// Shell words, falling back to whitespace when quotes don't balance.
pub fn words(segment: &str) -> Vec<String> {
    shlex::split(segment)
        .unwrap_or_else(|| segment.split_whitespace().map(str::to_string).collect())
}

fn program(word: &str) -> String {
    let base = word.rsplit(['/', '\\']).next().unwrap_or(word);
    base.strip_suffix(".exe")
        .unwrap_or(base)
        .to_ascii_lowercase()
}

/// The gated operations in a Bash command, given the starting directory and
/// the ship command's word prefixes.
pub fn scan(cmd: &str, cwd: &Path, ship: &[Vec<String>]) -> Vec<Op> {
    let mut ops = Vec::new();
    let mut dir = cwd.to_path_buf();
    scan_into(cmd, &mut dir, ship, &mut ops, 0);
    ops
}

fn scan_into(cmd: &str, dir: &mut PathBuf, ship: &[Vec<String>], ops: &mut Vec<Op>, depth: u8) {
    for seg in split_segments(cmd) {
        let mut w = words(&seg);
        // Strip a leading subshell paren, env assignments and wrappers.
        if let Some(first) = w.first_mut() {
            *first = first.trim_start_matches('(').to_string();
        }
        if let Some(last) = w.last_mut() {
            *last = last.trim_end_matches(')').to_string();
        }
        w.retain(|x| !x.is_empty());
        while let Some(first) = w.first() {
            let p = program(first);
            let assignment =
                first.contains('=') && !first.starts_with('-') && !first.starts_with('=');
            let wrapper = matches!(
                p.as_str(),
                "env" | "command" | "time" | "nohup" | "exec" | "sudo"
            );
            if assignment || wrapper {
                w.remove(0);
            } else {
                break;
            }
        }
        let Some(first) = w.first() else { continue };
        let prog = program(first);

        if prog == "cd" || prog == "pushd" {
            if let Some(target) = w.get(1) {
                *dir = crate::paths::resolve(dir, Path::new(target));
            }
            continue;
        }
        if matches!(prog.as_str(), "sh" | "bash" | "zsh") && depth < 3 {
            if let Some(i) = w.iter().position(|x| x == "-c" || x == "-lc")
                && let Some(inner) = w.get(i + 1)
            {
                let mut d = dir.clone();
                scan_into(inner, &mut d, ship, ops, depth + 1);
            }
            continue;
        }
        if prog == "git" {
            let mut i = 1;
            let mut gdir = dir.clone();
            while i < w.len() {
                let a = &w[i];
                if a == "-C" {
                    if let Some(d) = w.get(i + 1) {
                        gdir = crate::paths::resolve(&gdir, Path::new(d));
                    }
                    i += 2;
                } else if a == "-c" || a == "--git-dir" || a == "--work-tree" || a == "--namespace"
                {
                    i += 2;
                } else if a.starts_with('-') {
                    i += 1;
                } else {
                    break;
                }
            }
            match w.get(i).map(String::as_str) {
                Some("commit") => ops.push(Op {
                    kind: OpKind::Commit,
                    dir: gdir,
                    text: seg.clone(),
                }),
                Some("push") => ops.push(Op {
                    kind: OpKind::Push,
                    dir: gdir,
                    text: seg.clone(),
                }),
                _ => {}
            }
            continue;
        }
        if prog == "gh"
            && w.get(1).map(String::as_str) == Some("pr")
            && w.get(2).map(String::as_str) == Some("create")
        {
            ops.push(Op {
                kind: OpKind::Push,
                dir: dir.clone(),
                text: seg.clone(),
            });
            continue;
        }
        for prefix in ship {
            let matches = prefix.len() <= w.len()
                && prefix.iter().zip(&w).enumerate().all(|(i, (p, x))| {
                    if i == 0 {
                        program(x) == program(p)
                    } else {
                        x == p
                    }
                });
            if matches {
                ops.push(Op {
                    kind: OpKind::Push,
                    dir: dir.clone(),
                    text: seg.clone(),
                });
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(cmd: &str) -> Vec<(OpKind, String)> {
        let ship = vec![vec!["dev".to_string(), "submit".to_string()]];
        scan(cmd, Path::new("/repo"), &ship)
            .into_iter()
            .map(|o| (o.kind, crate::paths::display(&o.dir)))
            .collect()
    }

    #[test]
    fn finds_commits_and_pushes() {
        assert_eq!(kinds("git status"), vec![]);
        assert_eq!(
            kinds("git commit -m 'a && b'"),
            vec![(OpKind::Commit, "/repo".into())]
        );
        assert_eq!(
            kinds("git add . && git commit -m \"x; y\" && git push -u origin hm/H-1"),
            vec![
                (OpKind::Commit, "/repo".into()),
                (OpKind::Push, "/repo".into())
            ]
        );
        assert_eq!(
            kinds("gh pr create --fill"),
            vec![(OpKind::Push, "/repo".into())]
        );
        assert_eq!(kinds("gh pr view"), vec![]);
        assert_eq!(
            kinds("dev submit --pr"),
            vec![(OpKind::Push, "/repo".into())]
        );
        assert_eq!(kinds("dev test"), vec![]);
    }

    #[test]
    fn follows_directories() {
        assert_eq!(
            kinds("cd ../wt && git commit -m x"),
            vec![(OpKind::Commit, "/wt".into())]
        );
        assert_eq!(
            kinds("git -C /other push"),
            vec![(OpKind::Push, "/other".into())]
        );
        assert_eq!(
            kinds("git -c core.pager=cat --no-pager commit -am x"),
            vec![(OpKind::Commit, "/repo".into())]
        );
    }

    #[test]
    fn sees_through_wrappers() {
        assert_eq!(
            kinds("GIT_TRACE=1 git push"),
            vec![(OpKind::Push, "/repo".into())]
        );
        assert_eq!(
            kinds("env FOO=1 /usr/bin/git push"),
            vec![(OpKind::Push, "/repo".into())]
        );
        assert_eq!(
            kinds("bash -c 'git push origin main'"),
            vec![(OpKind::Push, "/repo".into())]
        );
        assert_eq!(kinds("(git push)"), vec![(OpKind::Push, "/repo".into())]);
        assert_eq!(kinds("echo 'git push'"), vec![]);
        assert_eq!(kinds("git log --grep push"), vec![]);
    }
}
