//! `hall setup --interactive`: the setup wizard. It asks about the settings
//! people change most (base branch, gates and the shell they run in, how
//! work is shipped, models, budget, tools, notifications), offers every
//! other key after that, and returns a [`Plan`]. Nothing is written here:
//! `hall setup` applies the plan through the same code as `hall config set`,
//! once you confirm it.
//!
//! The only command that prompts (ADR 0033). It runs only on a terminal and
//! only when asked; everything it sets can be set with `hall config`.

use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::Path;

use serde_json::Value;

use super::config::Entry;
use crate::detect;
use crate::error::{ErrorCode, HallError, Result};
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
}

/// What the wizard starts from.
#[derive(Debug, Default)]
pub struct Context {
    /// Every key's current value (see [`super::config::snapshot`]).
    pub entries: Vec<Entry>,
    pub in_repo: bool,
    /// The shell the repo's toolchain comes from, if one was detected.
    pub wrapper: Option<(&'static str, &'static str)>,
}

/// The current settings for a run in `cwd`. Before `hall init` has written
/// `.hallmark.toml`, the base branch and gates are what it will detect.
pub fn context(cwd: &Path) -> Context {
    let mut entries = super::config::snapshot(cwd);
    let repo = crate::repo::Repo::discover(cwd).ok();
    if let Some(r) = &repo
        && !r.main_root.join(crate::config::FILE_NAME).is_file()
    {
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
    }
}

fn cancelled() -> HallError {
    HallError::new(ErrorCode::Usage, "setup cancelled; nothing was changed")
        .next(["hall setup --interactive", "hall config list"])
}

/// Asks the questions on `out`, reading answers from `input`.
pub fn run<R: BufRead, W: Write>(ctx: &Context, input: R, out: W) -> Result<Plan> {
    let mut w = Wizard {
        ctx,
        input,
        out,
        changes: Vec::new(),
        asked: HashSet::new(),
    };
    w.say(
        "Hallmark setup. Enter keeps the value in [brackets], '-' goes back to the \
         default, '?' explains a setting. Nothing is written until you confirm.",
    )?;
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

    if ctx.in_repo {
        w.say("\n── This repository (.hallmark.toml) ──")?;
        w.ask(
            Scope::Repo,
            "base",
            Some("Base branch: what reviews compare against"),
        )?;
        w.gates()?;
        w.ask(
            Scope::Repo,
            "capture.mode",
            Some("When Claude edits code outside an item (gate: deny and explain · ask: ask you · off)"),
        )?;
        let shipping = w.yes(
            "\nChange how item worktrees and branches are made, and how work is shipped? \
             (now: git worktree, then git push and gh pr create)",
            false,
        )?;
        if shipping {
            for k in ["worktree", "branch", "ship", "update"] {
                w.ask(Scope::Repo, k, None)?;
            }
        }
        w.say("\n── Models and budget ──")?;
        for (k, label) in [
            ("budget.review_model", "Model for reviews"),
            ("budget.fix_model", "Model for fixes"),
            ("budget.intake_model", "Model that plans requests"),
            ("budget.librarian_model", "Model that distils learnings"),
        ] {
            w.ask(Scope::Repo, k, Some(label))?;
        }
        w.ask(
            Scope::Repo,
            "budget.daily",
            Some(
                "Daily cap for the whole loop: tokens (4M) or dollars ($15), or none; \
                 '-' for the default (4M)",
            ),
        )?;
        w.ask(
            Scope::Repo,
            "budget.per_item",
            Some(
                "Cap per item: tokens (800k) or dollars ($5), or none; '-' for the default (800k)",
            ),
        )?;
        w.say("\n── Extensions: skills by other authors ──")?;
        for e in crate::extensions::CATALOG {
            w.extension(e)?;
        }
    }

    w.say("\n── Your tools (your user config) ──")?;
    w.tool("claude")?;
    w.tool("gh")?;
    let qmd = w.current(Scope::Repo, "library.search") == "qmd";
    if qmd {
        w.tool("qmd")?;
    }

    w.say("\n── Notifications ──")?;
    w.ask(
        notify_scope,
        "notify.desktop",
        Some("Desktop notifications"),
    )?;
    let phone = w.phone(notify_scope)?;
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
    if phone != Phone::Off {
        w.ask(
            notify_scope,
            "notify.quiet_hours",
            Some("Quiet hours for the phone, such as 22:00-07:30"),
        )?;
    }
    w.ask(
        notify_scope,
        "notify.slack",
        Some("A Slack incoming-webhook URL (optional)"),
    )?;

    // Every other key, for those who want them. Notification keys only
    // where they take effect.
    let rest: Vec<(Scope, String)> = [Scope::Repo, Scope::User]
        .into_iter()
        .filter(|s| *s == Scope::User || ctx.in_repo)
        .flat_map(|s| settings::keys(s).into_iter().map(move |k| (s, k.key)))
        .filter(|(s, k)| !k.starts_with("notify.") || *s == notify_scope)
        .filter(|sk| !w.asked.contains(sk))
        .collect();
    if !rest.is_empty()
        && w.yes(
            &format!("\nGo through the other {} settings one by one?", rest.len()),
            false,
        )?
    {
        for (s, k) in &rest {
            w.ask(*s, k, None)?;
        }
    }

    let daemon = w.yes(
        "\nRun the review loop in the background, starting at login?",
        true,
    )?;

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
            return Err(cancelled());
        }
    }
    Ok(Plan {
        changes: w.changes,
        daemon,
        phone,
    })
}

struct Wizard<'a, R, W> {
    ctx: &'a Context,
    input: R,
    out: W,
    changes: Vec<Change>,
    asked: HashSet<(Scope, String)>,
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
            return Err(cancelled());
        }
        Ok(s.trim().to_string())
    }

    fn yes(&mut self, q: &str, default: bool) -> Result<bool> {
        loop {
            let a = self.line(&format!("{q} [{}] ", if default { "Y/n" } else { "y/N" }))?;
            match a.to_ascii_lowercase().as_str() {
                "" => return Ok(default),
                "y" | "yes" => return Ok(true),
                "n" | "no" => return Ok(false),
                _ => self.say("Answer y or n.")?,
            }
        }
    }

    /// A numbered menu; returns the chosen index.
    fn choose(&mut self, q: &str, options: &[String], default: usize) -> Result<usize> {
        self.say(q)?;
        for (i, o) in options.iter().enumerate() {
            self.say(&format!("  {}) {o}", i + 1))?;
        }
        loop {
            let a = self.line(&format!("Choose 1-{} [{}]: ", options.len(), default + 1))?;
            if a.is_empty() {
                return Ok(default);
            }
            if let Ok(n) = a.parse::<usize>()
                && (1..=options.len()).contains(&n)
            {
                return Ok(n - 1);
            }
            if let Some(i) = options.iter().position(|o| o == &a) {
                return Ok(i);
            }
            self.say(&format!("Type a number from 1 to {}.", options.len()))?;
        }
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
                "Use {} ({}) for the {}? `hall extensions` shows what each role gets.",
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

    /// Asks for one key by its kind.
    fn ask(&mut self, scope: Scope, key: &str, label: Option<&str>) -> Result<()> {
        let Some(k) = settings::find(scope, key) else {
            return Ok(());
        };
        self.asked.insert((scope, key.to_string()));
        let cur = self.current(scope, key);
        let label = label
            .map(String::from)
            .or_else(|| k.help.clone())
            .unwrap_or_else(|| key.to_string());
        loop {
            // None: keep. Some(None): back to the default. Some(Some(v)): set.
            let answer: Option<Option<Vec<String>>> = match k.kind {
                Kind::Bool => {
                    let b =
                        self.yes(&format!("{label} ({key})?"), cur.as_bool().unwrap_or(false))?;
                    Some(Some(vec![b.to_string()]))
                }
                Kind::Choice => {
                    let d = k.values.iter().position(|v| *v == plain(&cur)).unwrap_or(0);
                    let i = self.choose(&format!("{label} ({key})"), &k.values, d)?;
                    Some(Some(vec![k.values[i].clone()]))
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
                            let help = k.help.clone().unwrap_or_else(|| "No description.".into());
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
                Some(Some(items)) => match settings::parse(&k, &items) {
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
        }
    }

    fn run_with(ctx: &Context, answers: &[&str]) -> (Result<Plan>, String) {
        let input = answers.iter().map(|a| format!("{a}\n")).collect::<String>();
        let mut out = Vec::new();
        let r = run(ctx, std::io::Cursor::new(input), &mut out);
        (r, String::from_utf8(out).unwrap())
    }

    #[test]
    fn enter_everywhere_keeps_everything_but_offers_the_nix_shell() {
        let ctx = ctx();
        // Enter to every question; the shell question defaults to the flake.
        let (plan, out) = run_with(&ctx, &[""; 40]);
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
        assert_eq!(plan.phone, Phone::Ntfy);
    }

    #[test]
    fn answers_become_changes() {
        let ctx = Context {
            wrapper: None,
            ..ctx()
        };
        let (plan, out) = run_with(
            &ctx,
            &[
                "trunk",           // base
                "make test",       // gates: new list
                "make lint",       //
                "",                // end of the list
                "1",               // no shell
                "2",               // capture: ask
                "y",               // change shipping
                "",                // worktree
                "feat/{slug}",     // branch
                "rig submit --pr", // ship
                "",                // update
                "",                // review model
                "",                // fix model
                "",                // intake model
                "",                // librarian model
                "lots",            // daily: refused
                "2M",              // daily
                "-",               // per item: the default (already)
                "y",               // Matt Pocock's skills
                "",                // claude
                "",                // gh
                "n",               // desktop
                "3",               // phone: none
                "",                // slack
                "n",               // the other settings
                "n",               // daemon
                "y",               // write
            ],
        );
        let plan = plan.unwrap();
        assert!(out.contains("lots is not a cap"), "{out}");
        let shown: Vec<String> = plan.changes.iter().map(Change::shown).collect();
        assert_eq!(
            shown,
            vec![
                r#"base = "trunk""#,
                r#"gates = ["make test", "make lint"]"#,
                r#"capture.mode = "ask""#,
                r#"branch = "feat/{slug}""#,
                r#"ship = "rig submit --pr""#,
                r#"budget.daily = "2M""#,
                r#"extensions.mattpocock = ["planner", "developer", "reviewer", "fixer"]"#,
                "notify.desktop = false",
            ]
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
        for at in 0..30 {
            let mut answers = vec![""; 40];
            answers[at] = "4";
            let (plan, out) = run_with(&ctx, &answers);
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
        let mut answers = vec!["trunk"];
        answers.extend([""; 30]);
        let (plan, _) = run_with(&ctx, &answers);
        assert_eq!(plan.unwrap().changes.len(), 2);
        let (plan, _) = run_with(&ctx, &["trunk"]);
        assert_eq!(plan.unwrap_err().code, ErrorCode::Usage);
    }
}
