//! Reading shell commands for the gate: which parts commit, which push,
//! which write git notes, and in which directory. This is a best-effort
//! parser for the commands agents write (`cd x && git commit -m "…"`,
//! `git -C x push`), not a shell. What it can't read but may push, it reports
//! as a [`Suspect`]: the strict push guard denies those inside an item's
//! worktree (`[capture] push_guard`).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpKind {
    Commit,
    /// `git push`, `git send-pack`, `git http-push`, `git subtree push`,
    /// `gh pr create|merge`, a `gh api` write to pulls, refs, contents or
    /// merges, or the configured ship command.
    Push,
    /// `git notes add|append|edit|copy|remove|merge|prune`, or
    /// `git update-ref` on a notes ref. The hallmark is a note.
    NoteWrite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    pub kind: OpKind,
    /// The directory the command runs in.
    pub dir: PathBuf,
    pub text: String,
    /// The words after the git subcommand (for a push: its remote and
    /// refspecs), or after the program for other commands.
    pub args: Vec<String>,
}

/// A command the parser can't read that may push or write a note: a script
/// (`make push`), a git subcommand it doesn't know (an alias), a `gh` command
/// that changes GitHub, a command word computed when it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suspect {
    /// The directory the command runs in.
    pub dir: PathBuf,
    pub text: String,
    /// Why it is suspect, in a few words.
    pub why: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    pub ops: Vec<Op>,
    pub suspects: Vec<Suspect>,
}

/// What a scan knows besides the command.
pub struct Context<'a> {
    /// The ship command's word prefixes (`Config::ship_prefixes`).
    pub ship: &'a [Vec<String>],
    /// `[capture] push_allow` as word prefixes: never suspects.
    pub allow: &'a [Vec<String>],
    /// Resolves a git alias in a directory (`git config alias.<name>`).
    pub alias: &'a dyn Fn(&Path, &str) -> Option<String>,
}

fn no_alias(_: &Path, _: &str) -> Option<String> {
    None
}

/// How deep `sh -c`, `eval`, substitutions and aliases are followed.
const MAX_DEPTH: u8 = 4;

/// Words that, in a command the parser can't read, may mean it pushes.
const TRIGGERS: &[&str] = &["push", "notes", "gh", "release", "publish"];
/// The same, matched anywhere in a word.
const TRIGGER_PARTS: &[&str] = &["send-pack", "receive-pack", "alias.", "api.github.com"];

/// Programs whose arguments are data, never commands.
const DATA: &[&str] = &[
    "echo",
    "printf",
    "cat",
    "head",
    "tail",
    "less",
    "more",
    "wc",
    "ls",
    "tree",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ag",
    "sed",
    "jq",
    "yq",
    "sort",
    "uniq",
    "cut",
    "tr",
    "diff",
    "cmp",
    "comm",
    "paste",
    "column",
    "nl",
    "fold",
    "fmt",
    "file",
    "stat",
    "du",
    "df",
    "which",
    "type",
    "whereis",
    "basename",
    "dirname",
    "realpath",
    "readlink",
    "pwd",
    "true",
    "false",
    "test",
    "[",
    "sleep",
    "date",
    "tee",
    "touch",
    "mkdir",
    "rmdir",
    "rm",
    "cp",
    "mv",
    "ln",
    "chmod",
    "xxd",
    "od",
    "hexdump",
    "base64",
    "sha256sum",
    "shasum",
    "md5sum",
    "printenv",
    "export",
    "unset",
    "set",
    "read",
    "wait",
    "exit",
    "return",
    "find",
    "fi",
    "done",
    "esac",
];

/// Shell keywords that start a command without being it.
const KEYWORDS: &[&str] = &["if", "then", "else", "elif", "do", "while", "until", "!"];

/// git's own commands. Any other subcommand is an alias or an external
/// `git-<name>`, which the parser can't read.
const GIT_BUILTINS: &[&str] = &[
    "add",
    "am",
    "annotate",
    "apply",
    "archive",
    "backfill",
    "bisect",
    "blame",
    "branch",
    "bugreport",
    "bundle",
    "cat-file",
    "check-attr",
    "check-ignore",
    "check-mailmap",
    "check-ref-format",
    "checkout",
    "checkout-index",
    "cherry",
    "cherry-pick",
    "citool",
    "clean",
    "clone",
    "column",
    "commit",
    "commit-graph",
    "commit-tree",
    "config",
    "count-objects",
    "credential",
    "describe",
    "diagnose",
    "diff",
    "diff-files",
    "diff-index",
    "diff-tree",
    "difftool",
    "fast-export",
    "fast-import",
    "fetch",
    "fetch-pack",
    "filter-branch",
    "fmt-merge-msg",
    "for-each-ref",
    "for-each-repo",
    "format-patch",
    "fsck",
    "gc",
    "get-tar-commit-id",
    "grep",
    "gui",
    "hash-object",
    "help",
    "hook",
    "http-push",
    "index-pack",
    "init",
    "instaweb",
    "interpret-trailers",
    "log",
    "ls-files",
    "ls-remote",
    "ls-tree",
    "maintenance",
    "merge",
    "merge-base",
    "merge-file",
    "merge-index",
    "merge-tree",
    "mergetool",
    "mktag",
    "mktree",
    "multi-pack-index",
    "mv",
    "name-rev",
    "notes",
    "pack-objects",
    "pack-refs",
    "prune",
    "prune-packed",
    "pull",
    "push",
    "range-diff",
    "read-tree",
    "rebase",
    "reflog",
    "refs",
    "remote",
    "repack",
    "replace",
    "replay",
    "request-pull",
    "rerere",
    "reset",
    "restore",
    "rev-list",
    "rev-parse",
    "revert",
    "rm",
    "send-email",
    "send-pack",
    "shortlog",
    "show",
    "show-branch",
    "show-index",
    "show-ref",
    "sparse-checkout",
    "stash",
    "status",
    "stripspace",
    "submodule",
    "subtree",
    "switch",
    "symbolic-ref",
    "tag",
    "unpack-file",
    "unpack-objects",
    "update-index",
    "update-ref",
    "update-server-info",
    "var",
    "verify-commit",
    "verify-pack",
    "verify-tag",
    "version",
    "whatchanged",
    "worktree",
    "write-tree",
];

/// `git notes` subcommands that write.
const NOTE_WRITES: &[&str] = &["add", "append", "edit", "copy", "remove", "merge", "prune"];

/// Splits on `&&`, `||`, `;`, `|`, `&` and newlines, outside quotes and
/// command substitutions. Heredoc bodies are not commands, so they're left
/// out.
pub fn split_segments(cmd: &str) -> Vec<String> {
    lex(cmd).into_iter().map(|s| s.text).collect()
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
    let ctx = Context {
        ship,
        allow: &[],
        alias: &no_alias,
    };
    inspect(cmd, cwd, &ctx).ops
}

/// The gated operations in a Bash command, and the parts the parser can't
/// read that may push.
pub fn inspect(cmd: &str, cwd: &Path, ctx: &Context) -> Scan {
    let mut s = Scanner {
        ctx,
        out: Scan::default(),
    };
    let mut dir = cwd.to_path_buf();
    s.walk(cmd, &mut dir, 0);
    s.out
}

// --- Lexing -----------------------------------------------------------------

/// One simple command of a command line, with the heredoc bodies it reads
/// and the command substitutions inside it.
#[derive(Debug, Default)]
struct Segment {
    text: String,
    heredocs: Vec<String>,
    subs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Ctx {
    Single,
    Double,
    /// `$( … )`, with how many plain parentheses are open inside it.
    Subst(u32),
    Tick,
}

struct Heredoc {
    delim: String,
    /// `<<-`: leading tabs are stripped.
    strip: bool,
    /// The segment it belongs to.
    seg: usize,
    /// Inside a substitution: the body stays in its text, for the inner scan.
    inline: bool,
}

struct Lexer {
    chars: Vec<char>,
    i: usize,
    out: Vec<Segment>,
    seg: Segment,
    stack: Vec<Ctx>,
    /// The outermost substitution being read: its stack depth and text.
    sub: Option<(usize, String)>,
    pending: Vec<Heredoc>,
}

fn lex(cmd: &str) -> Vec<Segment> {
    let mut l = Lexer {
        chars: cmd.chars().collect(),
        i: 0,
        out: Vec::new(),
        seg: Segment::default(),
        stack: Vec::new(),
        sub: None,
        pending: Vec::new(),
    };
    l.run();
    l.out
}

impl Lexer {
    fn at(&self, k: usize) -> Option<char> {
        self.chars.get(k).copied()
    }

    fn push(&mut self, c: char) {
        self.seg.text.push(c);
        if let Some((_, s)) = &mut self.sub {
            s.push(c);
        }
    }

    /// Copies the next `n` characters.
    fn take(&mut self, n: usize) {
        for _ in 0..n {
            if let Some(c) = self.at(self.i) {
                self.push(c);
                self.i += 1;
            }
        }
    }

    fn open(&mut self, ctx: Ctx) {
        self.stack.push(ctx);
        if self.sub.is_none() && matches!(ctx, Ctx::Subst(_) | Ctx::Tick) {
            self.sub = Some((self.stack.len(), String::new()));
        }
    }

    fn close(&mut self) {
        let depth = self.stack.len();
        self.stack.pop();
        if self.sub.as_ref().is_some_and(|(d, _)| *d == depth)
            && let Some((_, s)) = self.sub.take()
        {
            self.seg.subs.push(s);
        }
    }

    fn split(&mut self) {
        let mut seg = std::mem::take(&mut self.seg);
        seg.text = seg.text.trim().to_string();
        if !seg.text.is_empty() {
            self.out.push(seg);
        }
    }

    fn word_start(&self) -> bool {
        self.i == 0
            || self
                .at(self.i - 1)
                .is_some_and(|p| p.is_whitespace() || ";|&()".contains(p))
    }

    fn run(&mut self) {
        while let Some(c) = self.at(self.i) {
            let next = self.at(self.i + 1);
            match self.stack.last().copied() {
                Some(Ctx::Single) => {
                    self.take(1);
                    if c == '\'' {
                        self.close();
                    }
                }
                Some(Ctx::Tick) => match c {
                    '\\' => self.take(2),
                    '`' => {
                        self.close();
                        self.take(1);
                    }
                    _ => self.take(1),
                },
                Some(Ctx::Double) => match c {
                    '\\' => self.take(2),
                    '"' => {
                        self.take(1);
                        self.close();
                    }
                    '$' if next == Some('(') => {
                        self.take(2);
                        self.open(Ctx::Subst(0));
                    }
                    '`' => {
                        self.take(1);
                        self.open(Ctx::Tick);
                    }
                    _ => self.take(1),
                },
                Some(Ctx::Subst(_)) => self.code(c, next, false),
                None => self.code(c, next, true),
            }
        }
        // Unbalanced quotes or parentheses: what was read still counts.
        if let Some((_, s)) = self.sub.take() {
            self.seg.subs.push(s);
        }
        self.split();
    }

    /// A character outside quotes: at the top level, or in a substitution.
    fn code(&mut self, c: char, next: Option<char>, top: bool) {
        match c {
            '\\' => self.take(2),
            '\'' => {
                self.take(1);
                self.open(Ctx::Single);
            }
            '"' => {
                self.take(1);
                self.open(Ctx::Double);
            }
            '`' => {
                self.take(1);
                self.open(Ctx::Tick);
            }
            '$' | '<' | '>' if next == Some('(') => {
                self.take(2);
                self.open(Ctx::Subst(0));
            }
            '(' if !top => {
                if let Some(Ctx::Subst(n)) = self.stack.last_mut() {
                    *n += 1;
                }
                self.take(1);
            }
            ')' if !top => {
                match self.stack.last_mut() {
                    Some(Ctx::Subst(n)) if *n > 0 => *n -= 1,
                    _ => self.close(),
                }
                self.take(1);
            }
            '<' if next == Some('<') && self.at(self.i + 2) == Some('<') => self.take(3),
            '<' if next == Some('<') => self.heredoc(!top),
            '#' if self.word_start() => {
                while self.at(self.i).is_some_and(|c| c != '\n') {
                    self.i += 1;
                }
            }
            '\n' if !top => {
                self.take(1);
                self.bodies();
            }
            '&' if self.i > 0 && matches!(self.at(self.i - 1), Some('>' | '<')) => self.take(1),
            '&' if next == Some('>') => self.take(1),
            '|' if self.i > 0 && self.at(self.i - 1) == Some('>') => self.take(1),
            ';' | '\n' | '|' | '&' if top => {
                self.split();
                self.i += 1;
                if c != '\n' && matches!(self.at(self.i), Some('&' | '|')) {
                    self.i += 1;
                }
                if c == '\n' {
                    self.bodies();
                }
            }
            _ => self.take(1),
        }
    }

    /// `<<DELIM`, `<<-DELIM`, `<<'DELIM'`: the body starts on the next line.
    fn heredoc(&mut self, inline: bool) {
        self.take(2);
        let strip = self.at(self.i) == Some('-');
        if strip {
            self.take(1);
        }
        while matches!(self.at(self.i), Some(' ' | '\t')) {
            self.take(1);
        }
        let mut delim = String::new();
        while let Some(c) = self.at(self.i) {
            match c {
                '\'' | '"' => {
                    self.take(1);
                    while let Some(d) = self.at(self.i) {
                        self.take(1);
                        if d == c {
                            break;
                        }
                        delim.push(d);
                    }
                }
                '\\' => {
                    self.take(1);
                    if let Some(d) = self.at(self.i) {
                        delim.push(d);
                        self.take(1);
                    }
                }
                _ if c.is_whitespace() || ";|&<>()".contains(c) => break,
                _ => {
                    delim.push(c);
                    self.take(1);
                }
            }
        }
        if !delim.is_empty() {
            self.pending.push(Heredoc {
                delim,
                strip,
                seg: self.out.len(),
                inline,
            });
        }
    }

    /// Reads the bodies of the heredocs opened on the line just ended.
    fn bodies(&mut self) {
        for h in std::mem::take(&mut self.pending) {
            let mut body = String::new();
            while self.i < self.chars.len() {
                let start = self.i;
                while self.at(self.i).is_some_and(|c| c != '\n') {
                    self.i += 1;
                }
                let line: String = self.chars[start..self.i].iter().collect();
                if self.i < self.chars.len() {
                    self.i += 1;
                }
                if h.inline {
                    for c in line.chars().chain(std::iter::once('\n')) {
                        self.push(c);
                    }
                }
                let cmp = if h.strip {
                    line.trim_start_matches('\t')
                } else {
                    line.as_str()
                };
                if cmp.trim_end_matches('\r') == h.delim {
                    break;
                }
                body.push_str(&line);
                body.push('\n');
            }
            if !h.inline {
                match self.out.get_mut(h.seg) {
                    Some(s) => s.heredocs.push(body),
                    None => self.seg.heredocs.push(body),
                }
            }
        }
    }
}

// --- Scanning ---------------------------------------------------------------

struct Scanner<'a, 'b> {
    ctx: &'a Context<'b>,
    out: Scan,
}

/// `prefix` starts `w`, comparing the program by name.
fn starts_with(prefix: &[String], w: &[String]) -> bool {
    !prefix.is_empty()
        && prefix.len() <= w.len()
        && prefix.iter().zip(w).enumerate().all(|(i, (p, x))| {
            if i == 0 {
                program(x) == program(p)
            } else {
                x == p
            }
        })
}

fn is_assignment(word: &str) -> bool {
    word.contains('=') && !word.starts_with('-') && !word.starts_with('=')
}

/// The first trigger in `text`, if any.
fn trigger(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    if let Some(t) = TRIGGER_PARTS.iter().find(|t| lower.contains(**t)) {
        return Some(t);
    }
    lower
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .find_map(|t| TRIGGERS.iter().find(|x| **x == t).copied())
}

/// How many leading words a wrapper takes (itself, its options and
/// operands) before the command it runs, or None if `p` is no wrapper.
fn wrapper_len(p: &str, w: &[String], dir: &mut PathBuf) -> Option<usize> {
    let valued: &[&str] = match p {
        "command" | "builtin" | "nohup" => &[],
        "time" => &["-f", "-o"],
        "exec" => &["-a"],
        "env" => &["-u", "-C", "-S", "--unset", "--chdir", "--split-string"],
        "sudo" | "doas" => &[
            "-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-U", "-T", "-R",
        ],
        "nice" => &["-n", "--adjustment"],
        "ionice" => &["-c", "-n", "-p", "-P", "-u", "--class", "--classdata"],
        "caffeinate" => &["-t", "-w"],
        "stdbuf" => &["-i", "-o", "-e", "--input", "--output", "--error"],
        "timeout" => &["-s", "-k", "--signal", "--kill-after"],
        "xargs" => &[
            "-a",
            "-d",
            "-E",
            "-I",
            "-L",
            "-n",
            "-P",
            "-s",
            "--arg-file",
            "--delimiter",
            "--max-args",
            "--max-lines",
            "--max-procs",
            "--max-chars",
        ],
        // `nix develop … --command <cmd>`, `nix shell … -c <cmd>`.
        "nix" => {
            if !matches!(w.get(1).map(String::as_str), Some("develop" | "shell")) {
                return None;
            }
            let at = w.iter().position(|x| x == "--command" || x == "-c")?;
            return Some(at + 1);
        }
        // `direnv exec <dir> <cmd>`.
        "direnv" => return (w.get(1).map(String::as_str) == Some("exec")).then_some(3),
        _ => return None,
    };
    let mut i = 1;
    while let Some(a) = w.get(i) {
        if a == "--" {
            i += 1;
            break;
        }
        if !a.starts_with('-') || a == "-" {
            break;
        }
        if p == "env"
            && (a == "-C" || a == "--chdir")
            && let Some(d) = w.get(i + 1)
        {
            *dir = crate::paths::resolve(dir, Path::new(d));
        }
        i += if valued.contains(&a.as_str()) { 2 } else { 1 };
    }
    if p == "timeout" {
        i += 1; // the duration
    }
    Some(i)
}

/// `gh` commands that only read.
fn gh_reads(v1: &str, v2: &str) -> bool {
    matches!(
        v1,
        "" | "help" | "version" | "--version" | "--help" | "-h" | "search" | "browse" | "status"
    ) || matches!(
        v2,
        "view" | "list" | "ls" | "status" | "checks" | "diff" | "watch" | "download"
    )
}

impl Scanner<'_, '_> {
    fn op(&mut self, kind: OpKind, dir: &Path, seg: &Segment, args: &[String]) {
        self.out.ops.push(Op {
            kind,
            dir: dir.to_path_buf(),
            text: seg.text.clone(),
            args: args.to_vec(),
        });
    }

    fn suspect(&mut self, dir: &Path, seg: &Segment, why: String) {
        self.out.suspects.push(Suspect {
            dir: dir.to_path_buf(),
            text: seg.text.clone(),
            why,
        });
    }

    fn walk(&mut self, cmd: &str, dir: &mut PathBuf, depth: u8) {
        for seg in lex(cmd) {
            if depth < MAX_DEPTH {
                for sub in &seg.subs {
                    let mut d = dir.clone();
                    self.walk(sub, &mut d, depth + 1);
                }
            }
            self.segment(&seg, dir, depth);
        }
    }

    fn segment(&mut self, seg: &Segment, dir: &mut PathBuf, depth: u8) {
        let mut w = words(&seg.text);
        // A subshell's parentheses and a group's braces.
        if let Some(first) = w.first_mut() {
            *first = first.trim_start_matches('(').to_string();
        }
        if let Some(last) = w.last_mut() {
            *last = last.trim_end_matches(')').to_string();
        }
        w.retain(|x| !x.is_empty() && x != "{" && x != "}");
        // Env assignments, keywords and wrappers come before the command.
        let mut xargs = false;
        while let Some(first) = w.first() {
            let p = program(first);
            if is_assignment(first) || KEYWORDS.contains(&p.as_str()) {
                w.remove(0);
                continue;
            }
            let Some(n) = wrapper_len(&p, &w, dir) else {
                break;
            };
            xargs |= p == "xargs";
            w.drain(..n.min(w.len()));
        }
        if w.is_empty() {
            return;
        }
        // `[capture] push_allow` quiets the guard, never the parser.
        let before = self.out.suspects.len();
        self.command(&w, seg, dir, depth, xargs);
        if self.ctx.allow.iter().any(|p| starts_with(p, &w)) {
            self.out.suspects.truncate(before);
        }
    }

    fn command(&mut self, w: &[String], seg: &Segment, dir: &mut PathBuf, depth: u8, xargs: bool) {
        let first = &w[0];
        if self.ctx.ship.iter().any(|p| starts_with(p, w)) {
            self.op(OpKind::Push, dir, seg, &w[1..]);
            return;
        }
        if first.starts_with('$') || first.starts_with('`') || first.contains("$(") {
            self.suspect(
                dir,
                seg,
                "its command word is only known when it runs".into(),
            );
            return;
        }
        match program(first).as_str() {
            "cd" | "pushd" => {
                if let Some(target) = w.get(1) {
                    *dir = crate::paths::resolve(dir, Path::new(target));
                }
            }
            "hall" => {}
            "sh" | "bash" | "zsh" | "dash" | "ksh" => self.shell(w, seg, dir, depth),
            "eval" => {
                if depth < MAX_DEPTH {
                    self.walk(&w[1..].join(" "), dir, depth + 1);
                }
            }
            "git" => self.git(w, seg, dir, depth, xargs),
            "gh" => self.gh(w, seg, dir),
            _ => self.other(w, seg, dir),
        }
    }

    /// `sh -c '…'` (any option cluster with `c`), a heredoc on stdin, or a
    /// script.
    fn shell(&mut self, w: &[String], seg: &Segment, dir: &Path, depth: u8) {
        let mut i = 1;
        let mut command = false;
        while let Some(a) = w.get(i) {
            if a == "--" || a == "-" {
                i += 1;
                break;
            }
            if a.starts_with("--") {
                i += if matches!(a.as_str(), "--rcfile" | "--init-file") {
                    2
                } else {
                    1
                };
                continue;
            }
            if (a.starts_with('-') || a.starts_with('+')) && a.len() > 1 {
                let flags = &a[1..];
                command |= a.starts_with('-') && flags.contains('c');
                i += if flags.contains('o') || flags.contains('O') {
                    2
                } else {
                    1
                };
                continue;
            }
            break;
        }
        if depth >= MAX_DEPTH {
            return;
        }
        if command {
            if let Some(inner) = w.get(i) {
                let mut d = dir.to_path_buf();
                self.walk(inner, &mut d, depth + 1);
            }
            return;
        }
        for body in &seg.heredocs {
            let mut d = dir.to_path_buf();
            self.walk(body, &mut d, depth + 1);
        }
        if i < w.len() {
            // A script: only its name and arguments can be read.
            self.other(w, seg, dir);
        } else if seg.heredocs.is_empty() {
            self.suspect(dir, seg, "it runs the commands piped into it".into());
        }
    }

    fn git(&mut self, w: &[String], seg: &Segment, dir: &Path, depth: u8, xargs: bool) {
        let mut gdir = dir.to_path_buf();
        let mut aliases: Vec<(String, String)> = Vec::new();
        let mut i = 1;
        while let Some(a) = w.get(i) {
            match a.as_str() {
                "-C" => {
                    if let Some(d) = w.get(i + 1) {
                        gdir = crate::paths::resolve(&gdir, Path::new(d));
                    }
                    i += 2;
                }
                "-c" => {
                    if let Some((k, v)) = w.get(i + 1).and_then(|kv| kv.split_once('='))
                        && let Some(name) = k.to_ascii_lowercase().strip_prefix("alias.")
                    {
                        aliases.push((name.to_string(), v.to_string()));
                    }
                    i += 2;
                }
                "--git-dir" | "--work-tree" | "--namespace" | "--config-env" | "--super-prefix" => {
                    i += 2
                }
                x if x.starts_with('-') => i += 1,
                _ => break,
            }
        }
        let Some(sub) = w.get(i) else {
            // `… | xargs git`: the subcommand comes from stdin.
            if xargs {
                self.op(OpKind::Push, &gdir, seg, &[]);
            }
            return;
        };
        let args = &w[i + 1..];
        if !GIT_BUILTINS.contains(&sub.as_str()) {
            let lower = sub.to_ascii_lowercase();
            let alias = aliases
                .iter()
                .rev()
                .find(|(n, _)| *n == lower)
                .map(|(_, v)| v.clone())
                .or_else(|| (self.ctx.alias)(&gdir, sub));
            match alias {
                Some(_) if depth >= MAX_DEPTH => {}
                Some(v) => match v.strip_prefix('!') {
                    // A shell alias: git runs it with the arguments appended.
                    Some(shell) => {
                        let line = std::iter::once(shell.to_string())
                            .chain(args.iter().cloned())
                            .collect::<Vec<_>>()
                            .join(" ");
                        let mut d = gdir.clone();
                        self.walk(&line, &mut d, depth + 1);
                    }
                    None => {
                        let mut nw = w[..i].to_vec();
                        nw.extend(words(&v));
                        nw.extend_from_slice(args);
                        self.git(&nw, seg, dir, depth + 1, xargs);
                    }
                },
                None => self.suspect(
                    &gdir,
                    seg,
                    format!("`git {sub}` isn't a git command Hallmark knows; it may be an alias"),
                ),
            }
            return;
        }
        // Commands that git subcommands run themselves.
        let mut nested: Vec<String> = Vec::new();
        match sub.as_str() {
            "commit" => self.op(OpKind::Commit, &gdir, seg, args),
            "push" | "send-pack" | "http-push" => self.op(OpKind::Push, &gdir, seg, args),
            "subtree" if args.iter().any(|a| a == "push") => {
                self.op(OpKind::Push, &gdir, seg, args)
            }
            "notes" => {
                let mut j = 0;
                while let Some(a) = args.get(j)
                    && a.starts_with('-')
                {
                    j += if a == "--ref" { 2 } else { 1 };
                }
                if args
                    .get(j)
                    .is_some_and(|s| NOTE_WRITES.contains(&s.as_str()))
                {
                    self.op(OpKind::NoteWrite, &gdir, seg, args);
                }
            }
            "update-ref"
                if args.iter().any(|a| a.contains("refs/notes"))
                    || seg.heredocs.iter().any(|b| b.contains("refs/notes")) =>
            {
                self.op(OpKind::NoteWrite, &gdir, seg, args)
            }
            "rebase" => {
                for (k, a) in args.iter().enumerate() {
                    if a == "-x" || a == "--exec" {
                        nested.extend(args.get(k + 1).cloned());
                    } else if let Some(c) = a.strip_prefix("--exec=") {
                        nested.push(c.to_string());
                    }
                }
            }
            "bisect" if args.first().map(String::as_str) == Some("run") => {
                nested.push(args[1..].join(" "));
            }
            "submodule" => {
                if let Some(k) = args.iter().position(|a| a == "foreach") {
                    let rest: Vec<&str> = args[k + 1..]
                        .iter()
                        .map(String::as_str)
                        .skip_while(|a| a.starts_with('-'))
                        .collect();
                    nested.push(rest.join(" "));
                }
            }
            "filter-branch" => {
                for (k, a) in args.iter().enumerate() {
                    if a.starts_with("--") && a.ends_with("-filter") {
                        nested.extend(args.get(k + 1).cloned());
                    }
                }
            }
            _ => {}
        }
        if depth < MAX_DEPTH {
            for c in nested {
                let mut d = gdir.clone();
                self.walk(&c, &mut d, depth + 1);
            }
        }
    }

    fn gh(&mut self, w: &[String], seg: &Segment, dir: &Path) {
        let v1 = w.get(1).map(String::as_str).unwrap_or("");
        let v2 = w.get(2).map(String::as_str).unwrap_or("");
        match (v1, v2) {
            ("pr", "create" | "merge") => self.op(OpKind::Push, dir, seg, &w[1..]),
            ("api", _) => {
                let api = GhApi::parse(&w[2..]);
                let write = match &api.method {
                    Some(m) => !matches!(m.to_ascii_uppercase().as_str(), "GET" | "HEAD"),
                    None => api.fields,
                };
                if write
                    && ["/pulls", "/git/", "/merges", "/contents/"]
                        .iter()
                        .any(|p| api.path.contains(p))
                {
                    self.op(OpKind::Push, dir, seg, &w[1..]);
                } else if write || api.path.trim_start_matches('/') == "graphql" {
                    self.suspect(dir, seg, "`gh api` may change GitHub".into());
                }
            }
            _ if gh_reads(v1, v2) => {}
            _ => self.suspect(dir, seg, format!("`gh {v1} {v2}` may change GitHub")),
        }
    }

    /// Any other program: what it runs can't be read, so its words decide.
    fn other(&mut self, w: &[String], seg: &Segment, dir: &Path) {
        let prog = program(&w[0]);
        let execs = |a: &String| a.starts_with("-exec") || a.starts_with("-ok");
        if DATA.contains(&prog.as_str()) && !(prog == "find" && w.iter().any(execs)) {
            return;
        }
        let words: Vec<&str> = if prog == "cargo" {
            // `cargo test push_guard` is fine; `cargo publish` and
            // `cargo release` are not, and `cargo run` runs anything.
            let rest: Vec<&str> = w[1..]
                .iter()
                .map(String::as_str)
                .filter(|a| !a.starts_with('-') && !a.starts_with('+'))
                .collect();
            let mut out: Vec<&str> = rest.first().copied().into_iter().collect();
            if rest.first() == Some(&"run")
                && let Some(k) = w.iter().position(|a| a == "--")
            {
                out.extend(w[k + 1..].iter().map(String::as_str));
            }
            out
        } else {
            w.iter()
                .map(String::as_str)
                .filter(|a| !a.starts_with('-'))
                .collect()
        };
        let hit = words
            .into_iter()
            .chain(seg.heredocs.iter().map(String::as_str))
            .find_map(trigger);
        if let Some(t) = hit {
            self.suspect(dir, seg, format!("it runs something that mentions `{t}`"));
        }
    }
}

/// The parts of `gh api` the gate reads: the method, whether it sends
/// fields (which makes it a POST), and the path.
struct GhApi {
    method: Option<String>,
    fields: bool,
    path: String,
}

impl GhApi {
    fn parse(a: &[String]) -> GhApi {
        let mut api = GhApi {
            method: None,
            fields: false,
            path: String::new(),
        };
        let mut k = 0;
        while let Some(x) = a.get(k) {
            let x = x.as_str();
            k += 1;
            match x {
                "-X" | "--method" => {
                    api.method = a.get(k).cloned();
                    k += 1;
                }
                "-f" | "-F" | "--field" | "--raw-field" | "--input" => {
                    api.fields = true;
                    k += 1;
                }
                "-H" | "--header" | "-q" | "--jq" | "-t" | "--template" | "--cache" | "-p"
                | "--preview" | "--hostname" => k += 1,
                _ if x.starts_with("--method=") => api.method = Some(x[9..].to_string()),
                _ if x.starts_with("-X") => api.method = Some(x[2..].to_string()),
                _ if x.starts_with("--field=")
                    || x.starts_with("--raw-field=")
                    || x.starts_with("--input=")
                    || x.starts_with("-f")
                    || x.starts_with("-F") =>
                {
                    api.fields = true
                }
                _ if x.starts_with('-') => {}
                _ if api.path.is_empty() => api.path = x.to_string(),
                _ => {}
            }
        }
        api
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
        // Real directories, so the paths resolve the same way on Windows.
        let tmp = tempfile::tempdir().unwrap();
        let base = crate::paths::normalize(tmp.path());
        let repo = base.join("repo");
        let at = |cmd: &str| -> Vec<(OpKind, PathBuf)> {
            scan(cmd, &repo, &[])
                .into_iter()
                .map(|o| (o.kind, o.dir))
                .collect()
        };
        assert_eq!(
            at("cd ../wt && git commit -m x"),
            vec![(OpKind::Commit, base.join("wt"))]
        );
        let other = base.join("other");
        assert_eq!(
            at(&format!("git -C '{}' push", other.display())),
            vec![(OpKind::Push, other)]
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

    /// What the gate sees in `cmd`: the parsed operations, and whether the
    /// strict push guard would hold the command back.
    fn verdict(cmd: &str) -> (Vec<OpKind>, bool) {
        verdict_with(cmd, &|_: &Path, _: &str| None, &[])
    }

    fn verdict_with(
        cmd: &str,
        alias: &dyn Fn(&Path, &str) -> Option<String>,
        allow: &[Vec<String>],
    ) -> (Vec<OpKind>, bool) {
        let ship = vec![vec!["dev".to_string(), "submit".to_string()]];
        let ctx = Context {
            ship: &ship,
            allow,
            alias,
        };
        let s = inspect(cmd, Path::new("/repo"), &ctx);
        (
            s.ops.into_iter().map(|o| o.kind).collect(),
            !s.suspects.is_empty(),
        )
    }

    use OpKind::{NoteWrite, Push};

    /// Appendix A of the v0.2 roadmap: the shapes the parser caught.
    const DENIED_BEFORE: &[(&str, OpKind)] = &[
        ("git push", Push),
        ("git push origin abc123:main", Push),
        ("GIT_DIR=.git git push", Push),
        ("command git push", Push),
        ("sh -c 'git push'", Push),
        ("gh pr create --fill", Push),
        ("git  push", Push),
        ("git\tpush", Push),
    ];

    /// Appendix A: the shapes that passed. `Some` is what the parser now
    /// finds; `None` is held back by the strict guard only.
    const PASSED_BEFORE: &[(&str, Option<OpKind>)] = &[
        ("git -c alias.p=push p", Some(Push)),
        ("git p", None),
        ("bash -ec 'git push'", Some(Push)),
        ("bash -xc 'git push'", Some(Push)),
        ("eval 'git push'", Some(Push)),
        ("echo push | xargs git", Some(Push)),
        ("$(echo git) push", None),
        ("timeout 10 git push", Some(Push)),
        ("nice git push", Some(Push)),
        ("make push", None),
        ("npm run release", None),
        ("./push.sh", None),
        (
            "python3 -c 'import subprocess; subprocess.run([\"git\",\"push\"])'",
            None,
        ),
        ("gh api repos/o/r/pulls -f head=x", Some(Push)),
        ("gh pr merge 7", Some(Push)),
        ("git notes --ref hallmark add -f -m x HEAD", Some(NoteWrite)),
        ("git subtree push --prefix=x origin main", Some(Push)),
        ("git send-pack origin main", Some(Push)),
    ];

    #[test]
    fn appendix_a_shapes_are_all_denied() {
        for (cmd, kind) in DENIED_BEFORE {
            assert_eq!(verdict(cmd), (vec![kind.clone()], false), "{cmd}");
        }
        for (cmd, kind) in PASSED_BEFORE {
            let (ops, guard) = verdict(cmd);
            match kind {
                Some(k) => assert_eq!(ops, vec![k.clone()], "{cmd}"),
                None => assert!(ops.is_empty() && guard, "{cmd}: {ops:?} {guard}"),
            }
        }
    }

    #[test]
    fn everyday_commands_pass_the_strict_guard() {
        for cmd in [
            "git status",
            "git log --oneline -5",
            "git log --grep push",
            "git diff HEAD~1 -- src/push.rs",
            "git stash push -m wip",
            "git stash pop",
            "git checkout -b fix-push-gate",
            "git notes show",
            "git notes --ref hallmark show HEAD",
            "git notes list",
            "git notes",
            "git fetch origin && git rebase origin/main",
            "git config --get alias.p",
            "cat > notes.txt <<'EOF'\ngit push origin main\nmake push\nEOF",
            "hall new --title \"push guard\"",
            "hall propose-ready H-0001 --summary \"push and notes\" --why x --how y",
            "cd ../wt && hall status",
            "cargo build --release",
            "cargo test",
            "cargo test push_guard",
            "cargo test --all-targets 2>&1 | tail -20",
            "nix develop --command cargo fmt --check",
            "nix develop --command cargo clippy --all-targets -- -D warnings",
            "nix develop --command cargo test --all-targets",
            "INSTA_UPDATE=always nix develop --command cargo test",
            "grep -rn push src/ | head",
            "rg 'git notes' docs",
            "sed -n 1,40p src/gh.rs",
            "echo \"$(date) push\"",
            "ls -la && pwd",
            "gh pr view 7",
            "gh pr checks 7",
            "gh issue view 212 --json body",
            "gh run list",
            "gh api repos/o/r/pulls",
            "gh api -X GET repos/o/r/pulls -f state=open",
        ] {
            assert_eq!(verdict(cmd), (vec![], false), "{cmd}");
        }
        // A commit is still a commit, whatever its message says.
        for cmd in [
            "git commit -am 'make push work'",
            "git commit -m \"$(cat <<'EOF'\nfix: the push gate's notes\n\nIt's \"strict\" now.\nEOF\n)\"",
        ] {
            assert_eq!(verdict(cmd), (vec![OpKind::Commit], false), "{cmd}");
        }
    }

    #[test]
    fn widened_parser_shapes() {
        for (cmd, kind) in [
            ("bash -e -c 'git push'", Push),
            ("zsh -lc 'cd x && git push'", Push),
            ("timeout -s KILL 10 git push", Push),
            ("nice -n 5 git push", Push),
            ("ionice -c 2 -n 7 git push", Push),
            ("caffeinate -i git push", Push),
            ("stdbuf -oL git push", Push),
            ("xargs -n 1 git push < remotes", Push),
            ("eval git push", Push),
            ("nix develop --command git push", Push),
            ("env -i PATH=/bin git push", Push),
            ("for r in a b; do git push $r; done", Push),
            ("if true; then git push; fi", Push),
            ("{ git push; }", Push),
            ("x=$(git push)", Push),
            ("echo `git push`", Push),
            ("git -c alias.x='!git push' x", Push),
            ("git http-push https://x main", Push),
            (
                "gh api -X PATCH repos/o/r/git/refs/heads/main -f sha=abc",
                Push,
            ),
            ("gh api --method PUT repos/o/r/pulls/7/merge", Push),
            ("gh pr create --base main", Push),
            ("git rebase -x 'git push' main", Push),
            ("git submodule foreach --recursive 'git push'", Push),
            ("git notes add -m x", NoteWrite),
            ("git notes --ref=refs/notes/hallmark append -m x", NoteWrite),
            ("git notes edit HEAD", NoteWrite),
            ("git notes copy a b", NoteWrite),
            ("git notes remove HEAD", NoteWrite),
            ("git notes merge other", NoteWrite),
            ("git update-ref refs/notes/hallmark abc", NoteWrite),
        ] {
            assert_eq!(verdict(cmd).0, vec![kind], "{cmd}");
        }
        // The push's arguments ride along, for refspec checks later (G3).
        let ctx = Context {
            ship: &[],
            allow: &[],
            alias: &|_: &Path, _: &str| None,
        };
        let s = inspect("git push origin abc123:main", Path::new("/repo"), &ctx);
        assert_eq!(s.ops[0].args, vec!["origin", "abc123:main"]);
    }

    #[test]
    fn the_strict_guard_holds_back_what_it_cannot_read() {
        for cmd in [
            "gh pr comment 7 --body x",
            "gh release create v1",
            "gh api graphql -f query=x",
            "gh repo sync",
            "git lfs push origin main",
            "$GIT push",
            "bash ./push.sh",
            "python3 <<'EOF'\nimport os\nos.system('git push')\nEOF",
            "cargo publish",
            "npx semantic-release",
            "make -C tools release",
            "awk 'BEGIN { system(\"gh pr merge 7\") }'",
        ] {
            let (ops, guard) = verdict(cmd);
            assert!(ops.is_empty() && guard, "{cmd}: {ops:?} {guard}");
        }
    }

    #[test]
    fn aliases_and_the_allow_list() {
        let alias = |_: &Path, name: &str| match name {
            "p" => Some("push --force".to_string()),
            "st" => Some("status -sb".to_string()),
            "pp" => Some("!git fetch && git push".to_string()),
            _ => None,
        };
        assert_eq!(verdict_with("git p", &alias, &[]), (vec![Push], false));
        assert_eq!(verdict_with("git st", &alias, &[]), (vec![], false));
        assert_eq!(verdict_with("git pp", &alias, &[]), (vec![Push], false));
        let allow = vec![vec!["make".to_string(), "release".to_string()]];
        assert_eq!(
            verdict_with("make release", &alias, &allow),
            (vec![], false)
        );
        assert_eq!(
            verdict_with("make release-notes", &alias, &allow),
            (vec![], true)
        );
        // The allow list only quiets the guard; a parsed push stays a push.
        let allow = vec![vec!["git".to_string(), "push".to_string()]];
        assert_eq!(verdict_with("git push", &alias, &allow).0, vec![Push]);
    }
}
