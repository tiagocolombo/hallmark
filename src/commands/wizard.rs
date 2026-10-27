//! The setup wizard. It first offers to skip, for the defaults; otherwise it
//! asks about every setting of the files it covers (`.hallmark.toml`, the
//! user config), section by section, each with its default, and returns a
//! [`Plan`]. Nothing is written here: `hall setup` and `hall init` apply the
//! plan through the same code as `hall config set`, once you confirm it.
//!
//! The only thing in `hall` that prompts (ADR 0033, ADR 0055). It runs on a
//! terminal only: for `hall init` and the first `hall setup`, and whenever
//! `hall setup --interactive` asks. Everything it sets can be set with
//! `hall config`.

use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::Path;

use serde_json::Value;

use super::config::Entry;
use crate::detect;
use crate::error::{ErrorCode, HallError, Result};
use crate::output::Mode;
use crate::settings::{self, Kind, Scope};

/// One answer that changes a setting.
#[derive(Debug, Clone)]
pub struct Change {
    pub scope: Scope,
    pub key: String,
    /// `None` removes the key, so its default applies.
    pub value: Option<toml_edit::Value>,
}

impl Change {
    pub fn shown(&self) -> String {
        match &self.value {
            Some(v) => format!("{} = {}", self.key, v.to_string().trim()),
            None => format!("{} = (default)", self.key),
        }
    }
}

/// What to do about the phone channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phone {
    /// Leave it as it is, or as the answers set it.
    Keep,
    /// Create a private ntfy topic (`hall notify test`).
    Ntfy,
    /// No phone notifications.
    Off,
}

#[derive(Debug)]
pub struct Plan {
    pub changes: Vec<Change>,
    /// Run the daemon, starting at login.
    pub daemon: bool,
    pub phone: Phone,
    /// The person chose "skip for now": the defaults, nothing asked.
    pub skipped: bool,
}

/// What the wizard starts from.
#[derive(Debug, Default)]
pub struct Context {
    /// Every key's current value (see [`super::config::snapshot`]).
    pub entries: Vec<Entry>,
    pub in_repo: bool,
    /// The shell the repo's toolchain comes from, if one was detected.
    pub wrapper: Option<(&'static str, &'static str)>,
    /// The text each answer is applied to: `.hallmark.toml` (or what
    /// `hall init` will write) and the user config. An answer that doesn't
    /// apply to it is refused when it is given.
    pub repo_text: String,
    pub user_text: String,
}

/// Whether the wizard asks. `--interactive` always does; otherwise only the
/// first time, on a terminal, and never with `--defaults` (ADR 0055).
pub fn asks(interactive: bool, defaults: bool, first: bool, can_ask: bool) -> bool {
    interactive || (first && !defaults && can_ask)
}

/// The first run: there is no user config yet, or the repo has no
/// `.hallmark.toml`.
pub fn first_run(user_config_exists: bool, repo_has_toml: Option<bool>) -> bool {
    !user_config_exists || repo_has_toml == Some(false)
}

/// The current settings for a run in `cwd`. Before `hall init` has written
/// `.hallmark.toml`, the base branch and gates are what it will detect.
pub fn context(cwd: &Path) -> Context {
    let mut entries = super::config::snapshot(cwd);
    let repo = crate::repo::Repo::discover(cwd).ok();
    let user_text = std::fs::read_to_string(crate::config::UserConfig::path()).unwrap_or_default();
    let mut repo_text = String::new();
    if let Some(r) = &repo
        && let Ok(t) = std::fs::read_to_string(r.toplevel.join(crate::config::FILE_NAME))
    {
        repo_text = t;
    }
    if let Some(r) = &repo
        && !r.main_root.join(crate::config::FILE_NAME).is_file()
    {
        repo_text = crate::config::render_initial(
            &crate::git::default_branch(&r.main_root),
            &detect::gates(&r.toplevel),
            Default::default(),
        );
        for e in entries.iter_mut().filter(|e| e.scope == Scope::Repo) {
            match e.key.as_str() {
                "base" => e.value = crate::git::default_branch(&r.main_root).into(),
                "gates" => e.value = serde_json::json!(detect::gates(&r.toplevel)),
                _ => {}
            }
        }
    }
    Context {
        entries,
        in_repo: repo.is_some(),
        wrapper: repo.and_then(|r| detect::env_wrapper(&r.toplevel)),
        repo_text,
        user_text,
    }
}

/// Which questions a run asks, and the command that asks them again.
#[derive(Debug, Clone, Copy)]
pub struct Ask {
    /// The repository's settings (`.hallmark.toml`), when in a repo.
    pub repo: bool,
    /// Your settings (tools, notifications), the daemon and the phone topic.
    pub user: bool,
    /// The command that runs this wizard again, for `next`.
    pub again: &'static str,
    /// The command that asks after a skip. Not `again` for `hall init`:
    /// the skip wrote `.hallmark.toml`, so `hall init` asks nothing.
    pub later: &'static str,
}

impl Ask {
    /// `hall setup`: everything.
    pub const SETUP: Ask = Ask {
        repo: true,
        user: true,
        again: "hall setup --interactive",
        later: "hall setup --interactive",
    };
    /// `hall init`: this repository only.
    pub const INIT: Ask = Ask {
        repo: true,
        user: false,
        again: "hall init",
        later: "hall setup --interactive",
    };
}

/// Whether the wizard may ask: text output, a terminal on stdin and stderr,
/// and not one of Hallmark's own headless runs. Everywhere else, the
/// defaults are written without a question (ADR 0055).
pub fn can_ask(mode: Mode) -> bool {
    use std::io::IsTerminal;
    mode != Mode::Json
        && std::env::var_os("HALL_HEADLESS").is_none()
        && std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal()
}

/// The repository's settings in the order they are asked, with the question
/// for each; "" asks with the key's own description. Keys the schema has and
/// this list doesn't (a new setting) are asked at the end, so every key is
/// offered.
const REPO_SECTIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "This repository: branches and gates (.hallmark.toml)",
        &[
            ("base", "Base branch: what reviews compare against"),
            ("gates", ""),
            (
                "gates_no_cache",
                "Gates never served from the checks cache, so they always run",
            ),
            (
                "gates_timeout",
                "Each gate is killed after this, such as 20m",
            ),
            ("rules", "The repo's rules files, which the reviewer reads"),
        ],
    ),
    (
        "Worktrees and shipping",
        &[
            ("worktree", ""),
            ("branch", ""),
            ("ship", ""),
            ("update", ""),
            ("review_comment", ""),
        ],
    ),
    (
        "The loop",
        &[
            ("fixer", ""),
            ("claude_args", ""),
            ("dev_commands", ""),
            (
                "max_rejects",
                "Rejected reviews before the item is escalated to you",
            ),
            ("max_check_failures", ""),
            ("auto_ready", ""),
            ("stuck_after", ""),
            ("lenses", ""),
            ("parallel.checks", "Checks run at once"),
            ("parallel.review", "Reviews run at once"),
            ("parallel.fix", "Fixes run at once"),
            ("parallel.ship", "Ships run at once"),
        ],
    ),
    (
        "Capture: edits outside an item",
        &[
            (
                "capture.mode",
                "When Claude edits code outside an item (gate: deny and explain · ask: ask you · off)",
            ),
            ("capture.free", ""),
            ("capture.push_guard", ""),
            ("capture.push_allow", ""),
        ],
    ),
    (
        "Merging",
        &[
            ("merge.mode", ""),
            ("merge.command", ""),
            ("merge.require_ci", ""),
            ("merge.update", ""),
            ("merge.lit", ""),
        ],
    ),
    (
        "The Front Desk",
        &[
            (
                "intake.split",
                "Split a request into parallel parts when it can (auto), or never (off)",
            ),
            ("intake.max_parts", ""),
        ],
    ),
    (
        "Models and budget",
        &[
            ("budget.review_model", "Model for reviews"),
            ("budget.fix_model", "Model for fixes"),
            ("budget.intake_model", "Model that plans requests"),
            ("budget.librarian_model", "Model that distils learnings"),
            (
                "budget.daily",
                "Daily cap for the whole loop: tokens (4M) or dollars ($15), or none",
            ),
            (
                "budget.per_item",
                "Cap per item: tokens (800k) or dollars ($5), or none",
            ),
        ],
    ),
];

/// The per-role budget tables, asked after the section above.
const ROLES: &[(&str, &str)] = &[
    ("review", "reviews"),
    ("fix", "fixes"),
    ("intake", "the Front Desk"),
    ("librarian", "the librarian"),
];

/// Sections asked after the budget.
const REPO_SECTIONS_AFTER: &[(&str, &[(&str, &str)])] = &[
    (
        "The Library",
        &[
            (
                "library.search",
                "How learnings are searched: builtin, or qmd",
            ),
            ("library.collection", ""),
            ("library.dir", ""),
            ("library.briefing_limit", ""),
            ("library.auto_accept", ""),
            ("library.retire_after", ""),
        ],
    ),
    (
        "The sandbox: what a headless run may reach",
        &[
            (
                "sandbox.mode",
                "Confine headless runs (auto · required: fail where there is no sandbox · off)",
            ),
            ("sandbox.allow_write", ""),
            ("sandbox.deny_read", ""),
            ("sandbox.domains", ""),
            ("sandbox.unix_sockets", ""),
            ("sandbox.local_binding", ""),
            ("sandbox.remove_env", ""),
        ],
    ),
];

/// The inbox kinds routed by `notify.route`.
const ROUTES: &[&str] = &[
    "question",
    "escalated",
    "stuck",
    "ready",
    "budget",
    "session_waiting",
    "learnings",
];

fn cancelled(again: &str) -> HallError {
    HallError::new(ErrorCode::Usage, "setup cancelled; nothing was changed")
        .next([again, "hall config list"])
}

/// Asks the questions on `out`, reading answers from `input`. The first
/// question offers to skip: then the plan is empty and `skipped` is set.
pub fn run<R: BufRead, W: Write>(ctx: &Context, ask: Ask, input: R, out: W) -> Result<Plan> {
    let mut w = Wizard {
        ctx,
        ask,
        input,
        out,
        changes: Vec::new(),
        asked: HashSet::new(),
    };
    let repo = ask.repo && ctx.in_repo;
    // A repo with its own [notify] ignores yours, so notification answers
    // go where they take effect.
    let notify_scope = if ctx
        .entries
        .iter()
        .any(|e| e.scope == Scope::Repo && e.key.starts_with("notify.") && e.source == "repo")
    {
        Scope::Repo
    } else {
        Scope::User
    };
    let wanted = |s: Scope, k: &str| -> bool {
        let scope_on = match s {
            Scope::Repo => repo,
            Scope::User => ask.user,
        };
        scope_on && (!k.starts_with("notify.") || s == notify_scope)
    };
    let all: Vec<(Scope, String)> = [Scope::Repo, Scope::User]
        .into_iter()
        .flat_map(|s| settings::keys(s).into_iter().map(move |k| (s, k.key)))
        .filter(|(s, k)| wanted(*s, k))
        .collect();

    w.say("Hallmark setup.")?;
    let skip = w.choose(
        "Go through the settings now?",
        &[
            format!(
                "Yes: every setting ({}), each with its default. Enter keeps a value, \
                 '-' goes back to the default, '?' explains it. Nothing is written until you confirm.",
                all.len()
            ),
            format!(
                "Skip for now: the defaults, in a basic config file. `{}` asks again, \
                 and `hall config` sets any key.",
                ask.later
            ),
        ],
        0,
    )? == 1;
    if skip {
        w.say("Skipped: the defaults apply.")?;
        return Ok(Plan {
            changes: Vec::new(),
            daemon: true,
            phone: Phone::Keep,
            skipped: true,
        });
    }

    if repo {
        for (heading, keys) in REPO_SECTIONS {
            w.section(heading, keys)?;
        }
        for (role, what) in ROLES {
            w.say(&format!("\n── Budget for {what} ──"))?;
            let keys: Vec<(String, String)> = [
                (
                    "model",
                    format!("Model for {what}; '-' to use budget.{role}_model"),
                ),
                (
                    "fallback",
                    format!(
                        "Fallback model for {what}, when the model is unavailable or overloaded"
                    ),
                ),
                (
                    "effort",
                    format!("Effort for {what}: low, medium, high or max"),
                ),
                ("max_usd", String::new()),
                ("daily", String::new()),
                ("timeout", String::new()),
            ]
            .into_iter()
            .map(|(k, label)| (format!("budget.{role}.{k}"), label))
            .collect();
            for (k, label) in &keys {
                w.ask(
                    Scope::Repo,
                    k,
                    Some(label.as_str()).filter(|l| !l.is_empty()),
                )?;
            }
        }
        w.say("\n── Extensions: skills by other authors ──")?;
        for e in crate::extensions::CATALOG {
            w.extension(e)?;
        }
        for (heading, keys) in REPO_SECTIONS_AFTER {
            w.section(heading, keys)?;
        }
    }

    if ask.user {
        w.say("\n── Your tools (your user config) ──")?;
        for t in crate::config::Tool::ALL {
            w.tool(t.name())?;
        }
    }

    if wanted(notify_scope, "notify.desktop") {
        w.say("\n── Notifications ──")?;
        w.ask(
            notify_scope,
            "notify.desktop",
            Some("Desktop notifications"),
        )?;
    }
    let phone = if wanted(notify_scope, "notify.phone") {
        w.phone(notify_scope)?
    } else {
        Phone::Keep
    };
    // Dropping the repo's phone key must not empty its [notify] table: an
    // absent table falls back to the user's phone channel.
    if phone == Phone::Off
        && notify_scope == Scope::Repo
        && w.changes
            .iter()
            .any(|c| c.scope == Scope::Repo && c.key == "notify.phone" && c.value.is_none())
        && !w
            .changes
            .iter()
            .any(|c| c.scope == Scope::Repo && c.key == "notify.desktop" && c.value.is_some())
    {
        w.changes
            .retain(|c| !(c.scope == Scope::Repo && c.key == "notify.desktop"));
        let desktop = w.current(Scope::Repo, "notify.desktop");
        if let Some(b) = desktop.as_bool() {
            let keeps = w
                .entry(Scope::Repo, "notify.desktop")
                .is_some_and(|e| e.source == "repo");
            if !keeps {
                w.changes.push(Change {
                    scope: Scope::Repo,
                    key: "notify.desktop".into(),
                    value: Some(b.into()),
                });
            }
        }
    }
    if wanted(notify_scope, "notify.quiet_hours") {
        if phone == Phone::Off {
            // Quiet hours only hold the phone.
            w.asked.insert((notify_scope, "notify.quiet_hours".into()));
        } else {
            w.ask(
                notify_scope,
                "notify.quiet_hours",
                Some("Quiet hours for the phone, such as 22:00-07:30"),
            )?;
        }
    }
    if wanted(notify_scope, "notify.slack") {
        w.ask(
            notify_scope,
            "notify.slack",
            Some("A Slack incoming-webhook URL (optional)"),
        )?;
        w.ask(notify_scope, "notify.remind", None)?;
        w.ask(
            notify_scope,
            "notify.digest",
            Some("Local time of the morning digest, such as 08:30; empty for none"),
        )?;
        for kind in ROUTES {
            let key = format!("notify.route.{kind}");
            let label = format!("Channels for `{kind}`: desktop, phone, slack, digest");
            w.ask(notify_scope, &key, Some(&label))?;
        }
    }

    // Every key the sections above don't name: a new setting, offered too.
    let rest: Vec<&(Scope, String)> = all.iter().filter(|sk| !w.asked.contains(sk)).collect();
    if !rest.is_empty() {
        w.say("\n── Other settings ──")?;
        for (s, k) in rest {
            w.ask(*s, k, None)?;
        }
    }

    let daemon = if ask.user {
        w.yes(
            "\nRun the review loop in the background, starting at login?",
            true,
        )?
    } else {
        true
    };

    if w.changes.is_empty() {
        w.say("\nNo settings to change.")?;
    } else {
        w.say("\nThese settings will be written:")?;
        let lines: Vec<String> = w
            .changes
            .iter()
            .map(|c| format!("  {:4}  {}", c.scope.as_str(), c.shown()))
            .collect();
        for l in lines {
            w.say(&l)?;
        }
        if !w.yes("Write them?", true)? {
            return Err(cancelled(ask.again));
        }
    }
    Ok(Plan {
        changes: w.changes,
        daemon,
        phone,
        skipped: false,
    })
}

struct Wizard<'a, R, W> {
    ctx: &'a Context,
    ask: Ask,
    input: R,
    out: W,
    changes: Vec<Change>,
    asked: HashSet<(Scope, String)>,
}

/// The first sentence of a key's description, without its full stop.
fn first_sentence(help: &str) -> String {
    match help.find(". ") {
        Some(i) => help[..i].to_string(),
        None => help.trim_end_matches('.').to_string(),
    }
}

/// A value as typed: strings without quotes.
fn plain(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A TOML value as JSON, to compare it with the current one.
fn to_json(v: &toml_edit::Value) -> Value {
    toml::from_str::<toml::Table>(&format!("v = {v}"))
        .ok()
        .and_then(|t| t.get("v").cloned())
        .and_then(|v| serde_json::to_value(v).ok())
        .unwrap_or(Value::Null)
}

impl<R: BufRead, W: Write> Wizard<'_, R, W> {
    fn say(&mut self, s: &str) -> Result<()> {
        writeln!(self.out, "{s}")?;
        Ok(())
    }

    /// One line of input, trimmed. End of input cancels the setup.
    fn line(&mut self, prompt: &str) -> Result<String> {
        write!(self.out, "{prompt}")?;
        self.out.flush()?;
        let mut s = String::new();
        if self.input.read_line(&mut s)? == 0 {
            writeln!(self.out)?;
            return Err(cancelled(self.ask.again));
        }
        Ok(s.trim().to_string())
    }

    fn yes(&mut self, q: &str, default: bool) -> Result<bool> {
        Ok(self.yes_with(q, default, None)?.unwrap_or(default))
    }

    /// Yes or no. With `help`, '?' prints it and '-' returns `None`: back
    /// to the default.
    fn yes_with(&mut self, q: &str, default: bool, help: Option<&str>) -> Result<Option<bool>> {
        loop {
            let a = self.line(&format!("{q} [{}] ", if default { "Y/n" } else { "y/N" }))?;
            match a.to_ascii_lowercase().as_str() {
                "" => return Ok(Some(default)),
                "y" | "yes" => return Ok(Some(true)),
                "n" | "no" => return Ok(Some(false)),
                "?" if help.is_some() => self.say(&format!("  {}", help.unwrap_or_default()))?,
                "-" if help.is_some() => return Ok(None),
                _ => self.say("Answer y or n.")?,
            }
        }
    }

    /// A numbered menu; returns the chosen index.
    fn choose(&mut self, q: &str, options: &[String], default: usize) -> Result<usize> {
        Ok(self
            .choose_with(q, options, default, None)?
            .unwrap_or(default))
    }

    /// A numbered menu. With `help`, '?' prints it and '-' returns `None`.
    fn choose_with(
        &mut self,
        q: &str,
        options: &[String],
        default: usize,
        help: Option<&str>,
    ) -> Result<Option<usize>> {
        self.say(q)?;
        for (i, o) in options.iter().enumerate() {
            self.say(&format!("  {}) {o}", i + 1))?;
        }
        loop {
            let a = self.line(&format!("Choose 1-{} [{}]: ", options.len(), default + 1))?;
            if a.is_empty() {
                return Ok(Some(default));
            }
            if let Some(h) = help {
                if a == "?" {
                    self.say(&format!("  {h}"))?;
                    continue;
                }
                if a == "-" {
                    return Ok(None);
                }
            }
            if let Ok(n) = a.parse::<usize>()
                && (1..=options.len()).contains(&n)
            {
                return Ok(Some(n - 1));
            }
            if let Some(i) = options.iter().position(|o| o == &a) {
                return Ok(Some(i));
            }
            self.say(&format!("Type a number from 1 to {}.", options.len()))?;
        }
    }

    /// Whether `value` applies to the scope's file on top of the answers
    /// already given.
    fn check(
        &self,
        scope: Scope,
        key: &str,
        value: &Option<toml_edit::Value>,
    ) -> std::result::Result<(), String> {
        let mut text = match scope {
            Scope::Repo => self.ctx.repo_text.clone(),
            Scope::User => self.ctx.user_text.clone(),
        };
        for c in self.changes.iter().filter(|c| c.scope == scope) {
            text = settings::edit(scope, &text, &c.key, c.value.clone())?;
        }
        settings::edit(scope, &text, key, value.clone()).map(|_| ())
    }

    fn entry(&self, scope: Scope, key: &str) -> Option<&Entry> {
        self.ctx
            .entries
            .iter()
            .find(|e| e.scope == scope && e.key == key)
    }

    fn current(&self, scope: Scope, key: &str) -> Value {
        self.entry(scope, key)
            .map(|e| e.value.clone())
            .unwrap_or(Value::Null)
    }

    /// Records an answer when it changes something.
    fn record(&mut self, scope: Scope, key: &str, value: Option<toml_edit::Value>) {
        let changes = match &value {
            Some(v) => to_json(v) != self.current(scope, key),
            // Only a key the file sets can be removed.
            None => self
                .entry(scope, key)
                .is_some_and(|e| e.source == scope.as_str()),
        };
        self.changes.retain(|c| !(c.scope == scope && c.key == key));
        if changes {
            self.changes.push(Change {
                scope,
                key: key.into(),
                value,
            });
        }
    }

    /// Asks whether to use an extension (ADR 0038): yes turns it on for
    /// every role it serves, no turns it off. A repo that has it on for
    /// some roles keeps them on Enter.
    fn extension(&mut self, e: &crate::extensions::Extension) -> Result<()> {
        let key = format!("extensions.{}", e.id);
        self.asked.insert((Scope::Repo, key.clone()));
        let on = self
            .current(Scope::Repo, &key)
            .as_array()
            .is_some_and(|a| !a.is_empty());
        let roles: Vec<&str> = e.roles().iter().map(|r| r.as_str()).collect();
        let yes = self.yes(
            &format!(
                "Use {} ({}) for the {} ({key})? `hall extensions` shows what each role gets.",
                e.name,
                e.credit(),
                roles.join(", ")
            ),
            on,
        )?;
        if yes != on {
            let value = yes.then(|| toml_edit::Value::Array(roles.iter().copied().collect()));
            self.record(Scope::Repo, &key, value);
        }
        Ok(())
    }

    /// A heading, then each of its repo keys. `gates` comes with the shell
    /// they run in.
    fn section(&mut self, heading: &str, keys: &[(&str, &str)]) -> Result<()> {
        self.say(&format!("\n── {heading} ──"))?;
        for (key, label) in keys {
            if *key == "gates" {
                self.gates()?;
            } else {
                self.ask(
                    Scope::Repo,
                    key,
                    Some(label).filter(|l| !l.is_empty()).copied(),
                )?;
            }
        }
        Ok(())
    }

    /// Asks for one key by its kind.
    fn ask(&mut self, scope: Scope, key: &str, label: Option<&str>) -> Result<()> {
        let Some(k) = settings::find(scope, key) else {
            return Ok(());
        };
        self.asked.insert((scope, key.to_string()));
        let cur = self.current(scope, key);
        // The question is the key's first sentence; '?' shows the rest.
        let label = label
            .map(String::from)
            .or_else(|| k.help.as_deref().map(first_sentence))
            .unwrap_or_else(|| key.to_string());
        let help = k.help.clone().unwrap_or_else(|| "No description.".into());
        loop {
            // None: keep. Some(None): back to the default. Some(Some(v)): set.
            let answer: Option<Option<Vec<String>>> = match k.kind {
                Kind::Bool => {
                    let b = self.yes_with(
                        &format!("{label} ({key})?"),
                        cur.as_bool().unwrap_or(false),
                        Some(help.as_str()),
                    )?;
                    Some(b.map(|b| vec![b.to_string()]))
                }
                Kind::Choice => {
                    let d = k.values.iter().position(|v| *v == plain(&cur)).unwrap_or(0);
                    let i = self.choose_with(
                        &format!("{label} ({key})"),
                        &k.values,
                        d,
                        Some(help.as_str()),
                    )?;
                    Some(i.map(|i| vec![k.values[i].clone()]))
                }
                Kind::List => self.list(&label, key, &cur)?,
                _ => {
                    let shown = if cur.is_null() {
                        "none".into()
                    } else {
                        plain(&cur)
                    };
                    let a = self.line(&format!("{label} ({key}) [{shown}]: "))?;
                    match a.as_str() {
                        "" => None,
                        "-" => Some(None),
                        "?" => {
                            self.say(&format!("  {help}"))?;
                            continue;
                        }
                        _ => Some(Some(vec![a])),
                    }
                }
            };
            match answer {
                None => return Ok(()),
                Some(None) => {
                    self.record(scope, key, None);
                    return Ok(());
                }
                Some(Some(items)) => match settings::parse(&k, &items).and_then(|v| {
                    self.check(scope, key, &Some(v.clone()))?;
                    Ok(v)
                }) {
                    Ok(v) => {
                        self.record(scope, key, Some(v));
                        return Ok(());
                    }
                    Err(e) => self.say(&format!("  {e}"))?,
                },
            }
        }
    }

    /// A list: shown one item per line, typed one per line.
    fn list(&mut self, label: &str, key: &str, cur: &Value) -> Result<Option<Option<Vec<String>>>> {
        self.say(&format!("{label} ({key}):"))?;
        let items: Vec<String> = cur
            .as_array()
            .map(|a| a.iter().map(plain).collect())
            .unwrap_or_default();
        if items.is_empty() {
            self.say("  (none)")?;
        }
        for i in &items {
            self.say(&format!("  - {i}"))?;
        }
        let first = self.line(
            "Enter keeps them, '-' resets them; or type new ones, one per line, and an empty line to end: ",
        )?;
        let mut first = first;
        loop {
            match first.as_str() {
                "" => return Ok(None),
                "-" => return Ok(Some(None)),
                "?" => {
                    let help = settings::find(Scope::Repo, key)
                        .or_else(|| settings::find(Scope::User, key))
                        .and_then(|k| k.help)
                        .unwrap_or_else(|| "No description.".into());
                    self.say(&format!("  {help}"))?;
                    first = self.line("  ")?;
                }
                _ => break,
            }
        }
        if first.starts_with('[') {
            return Ok(Some(Some(vec![first])));
        }
        let mut out = vec![first];
        loop {
            let next = self.line("  ")?;
            if next.is_empty() {
                return Ok(Some(Some(out)));
            }
            out.push(next);
        }
    }

    /// The gates, and the shell they run in.
    fn gates(&mut self) -> Result<()> {
        self.ask(
            Scope::Repo,
            "gates",
            Some("Gates: the checks every change must pass before review"),
        )?;
        let gates: Vec<Value> = self
            .changes
            .iter()
            .find(|c| c.key == "gates")
            .and_then(|c| c.value.as_ref())
            .map(to_json)
            .unwrap_or_else(|| self.current(Scope::Repo, "gates"))
            .as_array()
            .cloned()
            .unwrap_or_default();
        // A gate with its own timeout is a table: leave those as written.
        if gates.is_empty() || !gates.iter().all(Value::is_string) {
            return Ok(());
        }
        let gates: Vec<String> = gates.iter().map(plain).collect();
        let wrapped = |p: &str| gates.iter().all(|g| g.starts_with(p));
        if detect::WRAPPERS.iter().any(|(_, p)| wrapped(p)) {
            return Ok(());
        }
        // The daemon runs gates with `sh -c`, outside any shell you entered.
        let mut options: Vec<String> = vec!["No: the tools are on PATH".into()];
        options.extend(detect::WRAPPERS.iter().map(|(_, p)| format!("{p} <gate>")));
        options.push("Another command".into());
        let default = self
            .ctx
            .wrapper
            .and_then(|(n, _)| detect::WRAPPERS.iter().position(|(m, _)| *m == n))
            .map(|i| i + 1)
            .unwrap_or(0);
        let i = self.choose(
            "Run the gates inside a shell that provides the toolchain?",
            &options,
            default,
        )?;
        let prefix = if i == 0 {
            return Ok(());
        } else if i <= detect::WRAPPERS.len() {
            detect::WRAPPERS[i - 1].1.to_string()
        } else {
            let p = self.line("The command to put before each gate, such as `rig run --`: ")?;
            if p.is_empty() {
                return Ok(());
            }
            p
        };
        let new: Vec<String> = gates
            .iter()
            .map(|g| {
                if g.starts_with(&prefix) {
                    g.clone()
                } else {
                    format!("{prefix} {g}")
                }
            })
            .collect();
        let v = toml_edit::Value::Array(new.iter().collect());
        self.record(Scope::Repo, "gates", Some(v));
        Ok(())
    }

    /// Where a program lives, when it isn't simply on PATH.
    fn tool(&mut self, name: &str) -> Result<()> {
        let key = format!("tools.{name}");
        self.asked.insert((Scope::User, key.clone()));
        let (value, source) = self
            .entry(Scope::User, &key)
            .map(|e| (plain(&e.value), e.source.clone()))
            .unwrap_or_default();
        let env = crate::config::Tool::ALL
            .into_iter()
            .find(|t| t.name() == name)
            .map(|t| t.env())
            .unwrap_or_default();
        let shown = match source.as_str() {
            "env" => format!("{value}, from {env}"),
            "path" => format!("{value}, found on PATH"),
            "user" => value.clone(),
            _ => "not found".into(),
        };
        loop {
            let a = self.line(&format!("Where is {name}? [{shown}]: "))?;
            match a.as_str() {
                "" => return Ok(()),
                "-" => {
                    self.record(Scope::User, &key, None);
                    return Ok(());
                }
                _ => {}
            }
            let found = Path::new(&a).is_file() || crate::gh::which(&a).is_some();
            if found || self.yes(&format!("  {a} was not found. Use it anyway?"), false)? {
                if source != "user" || value != a {
                    self.changes.retain(|c| c.key != key);
                    self.changes.push(Change {
                        scope: Scope::User,
                        key,
                        value: Some(a.into()),
                    });
                }
                return Ok(());
            }
        }
    }

    /// The phone channel: a private ntfy topic, Telegram, or none.
    fn phone(&mut self, scope: Scope) -> Result<Phone> {
        self.asked.insert((scope, "notify.phone".into()));
        let cur = plain(&self.current(scope, "notify.phone"));
        let mut options = Vec::new();
        if !cur.is_empty() {
            options.push(format!("Keep {cur}"));
        }
        options.extend([
            "A new private ntfy.sh topic (free; get the ntfy app)".to_string(),
            "Telegram (a bot token and a chat ID)".into(),
            "None".into(),
        ]);
        let offset = usize::from(!cur.is_empty());
        let i = self.choose(
            "Phone notifications, for questions and escalations",
            &options,
            0,
        )?;
        match i.checked_sub(offset) {
            None => Ok(Phone::Keep),
            Some(0) => {
                self.record(scope, "notify.phone", None);
                Ok(Phone::Ntfy)
            }
            Some(1) => {
                let token = self.line("  Bot token: ")?;
                let chat = self.line("  Chat ID: ")?;
                if token.is_empty() || chat.is_empty() {
                    self.say("  Both are needed; the phone channel is unchanged.")?;
                    return Ok(Phone::Keep);
                }
                self.record(
                    scope,
                    "notify.phone",
                    Some(format!("telegram://{token}@{chat}").into()),
                );
                Ok(Phone::Keep)
            }
            _ => {
                self.record(scope, "notify.phone", None);
                Ok(Phone::Off)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(scope: Scope, key: &str, value: Value, source: &str) -> Entry {
        let k = settings::find(scope, key).unwrap();
        Entry {
            key: key.into(),
            scope,
            value,
            source: source.into(),
            kind: k.kind,
            help: k.help,
            values: k.values,
        }
    }

    /// A repo with defaults, cargo gates and a flake.
    fn ctx() -> Context {
        let mut entries = Vec::new();
        let c = serde_json::to_value(crate::config::Config::default()).unwrap();
        for k in settings::keys(Scope::Repo) {
            let mut v = settings::pick(&c, &k.key);
            if k.key == "gates" {
                v = serde_json::json!(["cargo test", "cargo clippy"]);
            }
            entries.push(entry(Scope::Repo, &k.key, v, "default"));
        }
        let u = serde_json::json!({ "notify": crate::config::Notify::default() });
        for k in settings::keys(Scope::User) {
            let (v, source) = if k.key == "tools.gh" {
                ("/usr/bin/gh".into(), "path")
            } else {
                (settings::pick(&u, &k.key), "default")
            };
            entries.push(entry(Scope::User, &k.key, v, source));
        }
        Context {
            entries,
            in_repo: true,
            wrapper: Some(detect::WRAPPERS[0]),
            repo_text: crate::config::render_initial(
                "main",
                &["cargo test".to_string()],
                Default::default(),
            ),
            user_text: String::new(),
        }
    }

    /// When the wizard asks: `--defaults` never; the first run on a terminal
    /// yes; a set-up machine no; `--interactive` always; no terminal no.
    #[test]
    fn asks_table() {
        // (interactive, defaults, first, can_ask) -> asks
        let table = [
            (false, true, true, true, false),
            (false, false, true, true, true),
            (false, false, false, true, false),
            (true, false, false, true, true),
            (false, false, true, false, false),
            (false, true, false, false, false),
        ];
        for (i, d, f, c, want) in table {
            assert_eq!(asks(i, d, f, c), want, "{i} {d} {f} {c}");
        }
        assert!(first_run(false, Some(true)));
        assert!(first_run(true, Some(false)));
        assert!(first_run(false, None));
        assert!(!first_run(true, Some(true)));
        assert!(!first_run(true, None));
    }

    fn ask_one(ctx: &Context, scope: Scope, key: &str, input: &str) -> (Vec<Change>, String) {
        let mut out = Vec::new();
        let mut w = Wizard {
            ctx,
            ask: Ask::INIT,
            input: std::io::Cursor::new(input.to_string()),
            out: &mut out,
            changes: Vec::new(),
            asked: HashSet::new(),
        };
        w.ask(scope, key, None).unwrap();
        let changes = w.changes;
        (changes, String::from_utf8(out).unwrap())
    }

    #[test]
    fn a_refused_answer_is_asked_again() {
        let ctx = ctx();
        let (changes, out) = ask_one(&ctx, Scope::Repo, "claude_args", "--mcp-config\n\n\n");
        assert!(out.matches("claude_args").count() >= 2, "{out}");
        assert!(
            changes.iter().all(|c| !c.shown().contains("mcp-config")),
            "{changes:?}"
        );
    }

    #[test]
    fn help_and_reset_work_on_yes_no_and_choices() {
        let ctx = ctx();
        let (_, out) = ask_one(&ctx, Scope::Repo, "merge.require_ci", "?\n\n");
        assert!(!out.contains("Answer y or n."), "{out}");
        let (_, out) = ask_one(&ctx, Scope::Repo, "capture.push_guard", "?\n\n");
        assert!(!out.contains("Type a number"), "{out}");
        let on = ctx
            .entries
            .iter()
            .find(|e| e.key == "merge.require_ci")
            .and_then(|e| e.value.as_bool())
            .unwrap_or(false);
        let (changes, _) = ask_one(
            &ctx,
            Scope::Repo,
            "merge.require_ci",
            if on { "n\n" } else { "y\n" },
        );
        assert_eq!(changes.len(), 1);
        // '-' goes back to the default without an error.
        let (_, out) = ask_one(&ctx, Scope::Repo, "merge.require_ci", "-\n");
        assert!(!out.contains("Answer y or n."), "{out}");
    }

    fn run_with(ctx: &Context, ask: Ask, answers: &[&str]) -> (Result<Plan>, String) {
        let input = answers.iter().map(|a| format!("{a}\n")).collect::<String>();
        let mut out = Vec::new();
        let r = run(ctx, ask, std::io::Cursor::new(input), &mut out);
        (r, String::from_utf8(out).unwrap())
    }

    /// Enter to every question: the walk, with every default kept.
    const ENTER: [&str; 200] = [""; 200];

    #[test]
    fn enter_everywhere_keeps_everything_but_offers_the_nix_shell() {
        let ctx = ctx();
        // Enter to every question; the shell question defaults to the flake.
        let (plan, out) = run_with(&ctx, Ask::SETUP, &ENTER);
        let plan = plan.unwrap();
        assert!(out.contains("nix develop --command <gate>"), "{out}");
        assert_eq!(plan.changes.len(), 1, "{:?}", plan.changes);
        let gates = to_json(plan.changes[0].value.as_ref().unwrap());
        assert_eq!(
            gates,
            serde_json::json!([
                "nix develop --command cargo test",
                "nix develop --command cargo clippy"
            ])
        );
        assert!(plan.daemon);
        assert!(!plan.skipped);
        assert_eq!(plan.phone, Phone::Ntfy);
    }

    #[test]
    fn skip_for_now_changes_nothing() {
        let ctx = ctx();
        let (plan, out) = run_with(&ctx, Ask::SETUP, &["2"]);
        let plan = plan.unwrap();
        assert!(out.contains("Skip for now"), "{out}");
        assert!(out.contains("hall setup --interactive"), "{out}");
        assert!(plan.skipped);
        assert!(plan.changes.is_empty());
        assert!(plan.daemon);
        assert_eq!(plan.phone, Phone::Keep);
        // After a skip, `hall init` asks nothing: point to a command that does.
        let (_, out) = run_with(&ctx, Ask::INIT, &["2"]);
        assert!(
            out.contains("`hall setup --interactive` asks again"),
            "{out}"
        );
        assert!(!out.contains("`hall init` asks again"), "{out}");
    }

    /// Every key of both files is offered, in a section, so nothing lands
    /// in "Other settings". A key added to a config type gets a place here.
    #[test]
    fn every_key_is_offered() {
        let ctx = ctx();
        let (plan, out) = run_with(&ctx, Ask::SETUP, &ENTER);
        plan.unwrap();
        let mut n = 0;
        for scope in [Scope::Repo, Scope::User] {
            for k in settings::keys(scope) {
                // notify.* is asked once, in the user's file here.
                if scope == Scope::Repo && k.key.starts_with("notify.") {
                    continue;
                }
                n += 1;
                let asked = match k.key.as_str() {
                    "notify.phone" => out.contains("Phone notifications"),
                    t if t.starts_with("tools.") => {
                        out.contains(&format!("Where is {}?", &t["tools.".len()..]))
                    }
                    _ => out.contains(&format!("({})", k.key)),
                };
                assert!(asked, "{} was not asked:\n{out}", k.key);
            }
        }
        assert!(out.contains(&format!("every setting ({n})")), "{out}");
        assert!(!out.contains("Other settings"), "{out}");
        for (_, keys) in REPO_SECTIONS.iter().chain(REPO_SECTIONS_AFTER) {
            for (k, _) in *keys {
                assert!(settings::find(Scope::Repo, k).is_some(), "{k}");
            }
        }
    }

    #[test]
    fn init_asks_only_about_the_repo() {
        let ctx = ctx();
        let (plan, out) = run_with(&ctx, Ask::INIT, &ENTER);
        let plan = plan.unwrap();
        assert!(out.contains("(base)"), "{out}");
        assert!(!out.contains("Where is claude"), "{out}");
        assert!(!out.contains("Notifications"), "{out}");
        assert!(!out.contains("starting at login"), "{out}");
        assert_eq!(plan.changes.len(), 1, "{:?}", plan.changes);
        assert!(plan.daemon);
        assert_eq!(plan.phone, Phone::Keep);
        let (plan, _) = run_with(&ctx, Ask::INIT, &[]);
        let e = plan.unwrap_err();
        assert_eq!(e.code, ErrorCode::Usage);
        assert_eq!(e.next[0], "hall init");
    }

    #[test]
    fn gates_with_a_table_are_left_as_written() {
        let mut ctx = ctx();
        for e in ctx.entries.iter_mut() {
            if e.scope == Scope::Repo && e.key == "gates" {
                e.value = serde_json::json!([
                    { "cmd": "cargo test", "timeout": "40m" },
                    "cargo clippy"
                ]);
            }
        }
        let (plan, out) = run_with(&ctx, Ask::SETUP, &ENTER);
        let plan = plan.unwrap();
        assert!(!out.contains("nix develop --command <gate>"), "{out}");
        assert!(plan.changes.is_empty(), "{:?}", plan.changes);
    }

    #[test]
    fn answers_become_changes() {
        let ctx = Context {
            wrapper: None,
            ..ctx()
        };
        let mut answers = vec![
            "1",               // go through the settings
            "trunk",           // base
            "make test",       // gates: new list
            "make lint",       //
            "",                // end of the list
            "1",               // no shell
            "",                // gates_no_cache
            "",                // gates_timeout
            "",                // rules
            "",                // worktree
            "feat/{slug}",     // branch
            "rig submit --pr", // ship
            "",                // update
            "",                // review_comment
        ];
        answers.extend([""; 12]); // the loop: fixer … parallel.ship
        answers.extend([
            "2", // capture: ask
            "",  // capture.free
            "",  // push_guard
            "",  // push_allow
        ]);
        answers.extend([""; 5]); // merging
        answers.extend([""; 2]); // the Front Desk
        answers.extend([
            "",     // review model
            "",     // fix model
            "",     // intake model
            "",     // librarian model
            "lots", // daily: refused
            "2M",   // daily
            "-",    // per item: the default (already)
        ]);
        answers.extend([""; 24]); // the four role tables
        answers.push("y"); // Matt Pocock's skills
        answers.extend([""; 6]); // the Library
        answers.extend([""; 7]); // the sandbox
        answers.extend([""; 4]); // claude, gh, qmd, cargo
        answers.extend([
            "n", // desktop
            "3", // phone: none (quiet hours not asked)
            "",  // slack
            "",  // remind
            "",  // digest
        ]);
        answers.extend([""; 7]); // the routes
        answers.extend([
            "n", // daemon
            "y", // write
        ]);
        let (plan, out) = run_with(&ctx, Ask::SETUP, &answers);
        let plan = plan.unwrap_or_else(|e| panic!("{e:?}\n{out}"));
        assert!(out.contains("lots is not a cap"), "{out}");
        let shown: Vec<String> = plan.changes.iter().map(Change::shown).collect();
        assert_eq!(
            shown,
            vec![
                r#"base = "trunk""#,
                r#"gates = ["make test", "make lint"]"#,
                r#"branch = "feat/{slug}""#,
                r#"ship = "rig submit --pr""#,
                r#"capture.mode = "ask""#,
                r#"budget.daily = "2M""#,
                r#"extensions.mattpocock = ["planner", "developer", "reviewer", "fixer"]"#,
                "notify.desktop = false",
            ],
            "{out}"
        );
        let desktop = plan.changes.iter().find(|c| c.key == "notify.desktop");
        assert_eq!(desktop.map(|c| c.scope), Some(Scope::User));
        assert!(!plan.daemon);
        assert_eq!(plan.phone, Phone::Off);
    }

    #[test]
    fn no_phone_keeps_the_repos_notify_table() {
        let mut ctx = ctx();
        for e in &mut ctx.entries {
            if e.scope == Scope::Repo && e.key == "notify.phone" {
                e.value = "telegram://t@c".into();
                e.source = "repo".into();
            }
        }
        // Find the phone question: the answer that picks "None" there.
        let mut found = false;
        for at in 0..ENTER.len() {
            let mut answers = ENTER.to_vec();
            answers[at] = "4";
            let (plan, out) = run_with(&ctx, Ask::SETUP, &answers);
            let Ok(plan) = plan else { continue };
            if plan.phone == Phone::Off && out.contains("Phone notifications") {
                found = true;
                assert!(
                    plan.changes
                        .iter()
                        .any(|c| c.key == "notify.phone" && c.value.is_none())
                );
                let d = plan.changes.iter().find(|c| c.key == "notify.desktop");
                assert_eq!(d.map(|c| c.scope), Some(Scope::Repo));
                assert!(d.is_some_and(|c| c.value.is_some()));
                break;
            }
        }
        assert!(found);
    }

    #[test]
    fn declining_or_running_out_of_input_cancels() {
        let ctx = ctx();
        let mut answers = vec!["", "trunk"];
        answers.extend(ENTER);
        let (plan, _) = run_with(&ctx, Ask::SETUP, &answers);
        assert_eq!(plan.unwrap().changes.len(), 2);
        let (plan, _) = run_with(&ctx, Ask::SETUP, &["", "trunk"]);
        let e = plan.unwrap_err();
        assert_eq!(e.code, ErrorCode::Usage);
        assert_eq!(e.next[0], "hall setup --interactive");
    }
}
