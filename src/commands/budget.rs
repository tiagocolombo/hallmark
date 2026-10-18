//! `hall budget` (workstream D): the caps at four levels and today's spend.
//! `--set key=value` edits the main checkout's `.hallmark.toml` in place,
//! keeping its comments and layout. `--grant` lets the loop go past the
//! daily cap for today without editing it (ADR 0028).

use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;

use crate::budget::{self, ItemSpend, ROLES, Spent, room_label, table_key};
use crate::cli::BudgetArgs;
use crate::config::{self, Config, parse_duration, parse_tokens};
use crate::error::{ErrorCode, HallError, Result};
use crate::output::{Render, Response};
use crate::repo::Repo;
use crate::usage::fmt_tokens;

#[derive(Debug, Serialize, JsonSchema)]
pub struct BudgetOut {
    /// The UTC date "today" means for the daily caps.
    pub day: String,
    /// Per-run settings by role: model, effort, `max_usd`, timeout.
    pub runs: Vec<RunCap>,
    /// The item, room and daily caps, with today's loop spend.
    pub caps: Vec<Cap>,
    /// The items the loop spent most on (all time), against `per_item`.
    pub top_items: Vec<TopItem>,
    /// Tokens your own Claude Code sessions used today: measured, never capped.
    pub workshop_today: u64,
    /// Settings written by --set.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub set: Vec<String>,
    /// The file --set wrote, when it changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub written: Option<String>,
    /// What --grant did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant: Option<GrantOut>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GrantOut {
    /// The UTC day the grant is for (today).
    pub day: String,
    /// Tokens on top of `daily` for that day.
    pub tokens: u64,
    /// False when today already had this grant.
    pub changed: bool,
    /// True when a daily-budget pause of the daemon was lifted.
    pub resumed: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RunCap {
    /// reviewer, fixer, intake, librarian.
    pub role: String,
    /// The `[budget.<key>]` table: review, fix, intake, librarian.
    pub key: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// `--max-budget-usd` for each run; none means no per-run cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_usd: Option<f64>,
    /// The per-run time limit, in seconds.
    pub timeout_secs: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Cap {
    /// item, room, daily.
    pub level: String,
    /// The key in .hallmark.toml, such as budget.review.daily.
    pub key: String,
    /// The room, for room caps (assay, repair, front, library).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    /// As written in the config; none means not capped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// The limit of a token cap, in weighted tokens.
    pub limit_tokens: Option<u64>,
    /// The limit of a dollar cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_usd: Option<f64>,
    /// Spent today (weighted tokens), where it applies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used: Option<u64>,
    /// For the daily cap: tokens granted on top of it today (`--grant`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub granted: Option<u64>,
    /// Spent today (dollars at list price), where it applies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_usd: Option<f64>,
    /// True when the cap stops work now.
    pub reached: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TopItem {
    #[serde(flatten)]
    pub spend: ItemSpend,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Tokens `hall retry` granted this item on top of `per_item`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub granted: Option<u64>,
    /// At or over `per_item` plus anything granted.
    pub over_cap: bool,
}

fn pct_bar(pct: f64) -> String {
    let filled = ((pct / 10.0).round() as usize).min(10);
    format!(
        "{}{} {:>3.0}%",
        "█".repeat(filled),
        "░".repeat(10 - filled),
        pct
    )
}

impl Cap {
    /// The cap as the budget checks read it.
    fn cap(&self) -> Option<budget::Cap> {
        match (self.limit_tokens, self.limit_usd) {
            (Some(t), _) => Some(budget::Cap::Tokens(t)),
            (_, Some(u)) => Some(budget::Cap::Usd(u)),
            _ => None,
        }
    }
}

fn spent_of(s: &ItemSpend) -> Spent {
    Spent {
        tokens: s.tokens,
        usd: s.cost_usd,
    }
}

/// A cap's limit fields: (weighted tokens, dollars).
fn limits(cap: Option<budget::Cap>) -> (Option<u64>, Option<f64>) {
    match cap {
        Some(budget::Cap::Tokens(t)) => (Some(t), None),
        Some(budget::Cap::Usd(u)) => (None, Some(u)),
        None => (None, None),
    }
}

impl Render for BudgetOut {
    fn render(&self) -> String {
        let mut s = format!("hall · budget · today ({} UTC)\n\n", self.day);
        s.push_str("per run      model    effort  max_usd  timeout\n");
        for r in &self.runs {
            s.push_str(&format!(
                "  {:10} {:8} {:7} {:8} {}m\n",
                r.role,
                r.model,
                r.effort.as_deref().unwrap_or("—"),
                r.max_usd
                    .map(|u| format!("${u:.2}"))
                    .unwrap_or_else(|| "—".into()),
                r.timeout_secs / 60
            ));
        }
        s.push_str("\nper day                  used     cap\n");
        for c in self.caps.iter().filter(|c| c.level != "item") {
            let label = match &c.room {
                Some(r) => room_label(r).to_string(),
                None => "daily total".into(),
            };
            let used = c.used.unwrap_or(0);
            let spent = Spent {
                tokens: used,
                usd: c.used_usd.unwrap_or(0.0),
            };
            let granted = c.granted.unwrap_or(0);
            let tail = match (c.limit.as_deref(), c.cap()) {
                (Some(l), Some(budget::Cap::Tokens(t))) if granted > 0 => format!(
                    "{l:>6}  {} (+{} granted today)",
                    pct_bar(
                        budget::Cap::Tokens(t.saturating_add(granted)).fraction(&spent) * 100.0
                    ),
                    fmt_tokens(granted)
                ),
                (Some(l), Some(cap)) => {
                    format!("{l:>6}  {}", pct_bar(cap.fraction(&spent) * 100.0))
                }
                (Some(l), None) => format!("{l:>6}  (unreadable)"),
                _ => "  not capped".into(),
            };
            s.push_str(&format!("  {label:20} {:>8}  {tail}\n", fmt_tokens(used)));
        }
        s.push_str(&format!(
            "  {:20} {:>8}    not capped (your sessions)\n",
            "Workshop",
            fmt_tokens(self.workshop_today)
        ));
        if let Some(c) = self.caps.iter().find(|c| c.level == "item") {
            s.push_str(&format!(
                "\nper item: {}\n",
                c.limit.as_deref().unwrap_or("not capped")
            ));
            for t in &self.top_items {
                let granted = t.granted.unwrap_or(0);
                let mut note = match c.cap() {
                    Some(_) if t.over_cap => "over item cap".to_string(),
                    // A grant raises a token cap only.
                    Some(budget::Cap::Tokens(l)) => pct_bar(
                        budget::Cap::Tokens(l.saturating_add(granted))
                            .fraction(&spent_of(&t.spend))
                            * 100.0,
                    ),
                    Some(cap) => pct_bar(cap.fraction(&spent_of(&t.spend)) * 100.0),
                    None => String::new(),
                };
                if granted > 0 {
                    note.push_str(&format!(" (+{} granted)", fmt_tokens(granted)));
                }
                s.push_str(&format!(
                    "  {} {:>8}  ${:<7.2} {:12} {note}\n",
                    t.spend.item,
                    fmt_tokens(t.spend.tokens),
                    t.spend.cost_usd,
                    t.state.as_deref().unwrap_or("")
                ));
            }
        }
        for line in &self.set {
            s.push_str(&format!("\nset {line}"));
        }
        if !self.set.is_empty() {
            s.push('\n');
        }
        if let Some(g) = &self.grant {
            s.push_str(&format!(
                "\n{} +{} for {} on top of the daily cap{}\n",
                if g.changed {
                    "granted"
                } else {
                    "already granted"
                },
                fmt_tokens(g.tokens),
                g.day,
                if g.resumed {
                    "; the daemon's budget pause is lifted"
                } else {
                    ""
                }
            ));
        }
        s
    }
}

pub fn budget(cwd: &Path, a: BudgetArgs) -> Result<Response> {
    let mut repo = Repo::discover(cwd)?;
    repo.require_init()?;
    let mut set = Vec::new();
    let mut written = None;
    if !a.set.is_empty() {
        let path = repo.main_root.join(config::FILE_NAME);
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let (new_text, applied) = apply_sets(&text, &a.set)?;
        set = applied;
        if new_text != text {
            std::fs::write(&path, &new_text)?;
            written = Some(crate::paths::display(&path));
        }
        repo.config = Config::parse(&new_text)?;
    }
    let day = budget::today();
    let grant = match &a.grant {
        Some(t) => Some(grant(&repo, &day, t, a.reason.as_deref())?),
        None => None,
    };
    let b = &repo.config.budget;
    let ix = repo.index()?;
    let runs = ROLES
        .iter()
        .map(|r| RunCap {
            role: r.as_str().into(),
            key: table_key(*r).into(),
            model: b.model(*r),
            effort: b.effort(*r),
            max_usd: b.max_usd(*r),
            timeout_secs: b.timeout_secs(*r),
        })
        .collect();
    let mut caps = Vec::new();
    let per_item = b.per_item.as_deref().and_then(budget::Cap::parse);
    let (limit_tokens, limit_usd) = limits(per_item);
    caps.push(Cap {
        level: "item".into(),
        key: "budget.per_item".into(),
        room: None,
        limit: b.per_item.clone(),
        limit_tokens,
        limit_usd,
        used: None,
        used_usd: None,
        granted: None,
        reached: false,
    });
    for r in ROLES {
        let limit = b.role(r).and_then(|rb| rb.daily.clone());
        let cap = limit.as_deref().and_then(budget::Cap::parse);
        let (limit_tokens, limit_usd) = limits(cap);
        let used = budget::room_spent(&ix, r.room(), &day);
        caps.push(Cap {
            level: "room".into(),
            key: format!("budget.{}.daily", table_key(r)),
            room: Some(r.room().into()),
            reached: cap.is_some_and(|c| c.reached(&used)),
            limit,
            limit_tokens,
            limit_usd,
            used: Some(used.tokens),
            used_usd: Some(used.usd),
            granted: None,
        });
    }
    let daily_cap = b.daily.as_deref().and_then(budget::Cap::parse);
    let (limit_tokens, limit_usd) = limits(daily_cap);
    let used = budget::daily_spent(&ix, &day);
    // A grant raises a token cap only (ADR 0028).
    let granted = match daily_cap {
        Some(budget::Cap::Tokens(_)) => budget::daily_granted(&ix, &day),
        _ => 0,
    };
    caps.push(Cap {
        level: "daily".into(),
        key: "budget.daily".into(),
        room: None,
        limit: b.daily.clone(),
        limit_tokens,
        limit_usd,
        used: Some(used.tokens),
        used_usd: Some(used.usd),
        granted: (granted > 0).then_some(granted),
        reached: budget::daily_stop(&ix, b, &day).is_some(),
    });
    let items = ix.items(true)?;
    let top_items: Vec<TopItem> = budget::top_items(&ix, 5)
        .into_iter()
        .map(|spend| {
            let it = items.iter().find(|i| i.id == spend.item);
            TopItem {
                granted: Some(budget::item_granted(&ix, spend.item)).filter(|g| *g > 0),
                over_cap: budget::item_allowance(&ix, b, spend.item)
                    .is_some_and(|c| c.reached(&spent_of(&spend))),
                title: it.map(|i| i.title.clone()),
                state: it.map(|i| match &i.hold {
                    Some(h) => format!("held ({})", h.kind.as_str()),
                    None => i.state.as_str().to_string(),
                }),
                spend,
            }
        })
        .collect();
    let out = BudgetOut {
        day: day.clone(),
        runs,
        caps,
        top_items,
        workshop_today: budget::workshop_tokens(&ix, &day),
        set,
        written,
        grant,
    };
    let mut next: Vec<String> = Vec::new();
    if out.caps.iter().any(|c| c.level == "daily" && c.reached) {
        next.push("hall budget --grant (go on today; the cap stays)".into());
    }
    next.extend([
        "hall budget --set daily=4M per_item=800k".into(),
        "hall usage --by room --since 24h".into(),
        "hall usage --by item".into(),
    ]);
    if out.grant.as_ref().is_some_and(|g| g.resumed) {
        next.insert(0, "hall daemon status".into());
    }
    Ok(Response::new(out, next))
}

/// `--grant`: records today's grant in the repo journal (unless today
/// already has exactly this one) and lifts the daemon's daily-budget pause.
fn grant(repo: &Repo, day: &str, tokens: &str, reason: Option<&str>) -> Result<GrantOut> {
    let b = &repo.config.budget;
    let daily = match b.daily.as_deref().and_then(budget::Cap::parse) {
        Some(budget::Cap::Tokens(t)) => t,
        // A grant is in weighted tokens; it can't raise a dollar cap.
        Some(budget::Cap::Usd(_)) => {
            return Err(HallError::new(
                ErrorCode::WrongState,
                "the daily budget is in dollars; --grant raises a token cap only: \
                 raise `[budget] daily` to go on today",
            )
            .next(["hall budget"]));
        }
        None => {
            return Err(HallError::new(
                ErrorCode::WrongState,
                "there is no daily budget to go past: `[budget] daily` is \"none\"",
            )
            .next(["hall budget"]));
        }
    };
    let tokens = if tokens.trim() == "daily" {
        daily
    } else {
        parse_tokens(tokens).ok_or_else(|| {
            HallError::usage(format!(
                "--grant {tokens}: use a token count such as 20M or 800k"
            ))
            .next(["hall budget --grant 20M"])
        })?
    };
    let ix = repo.index()?;
    let changed = budget::daily_granted(&ix, day) != tokens;
    if changed {
        let e = crate::journal::ev(
            &crate::repo::actor(),
            None,
            crate::event::Body::BudgetGranted {
                day: day.to_string(),
                tokens,
                reason: reason.map(str::to_string),
            },
        );
        repo.hall.append(&repo.hall.repo_events(), &e)?;
        repo.hall.live(&format!(
            "daily budget: +{} granted for {day} (the cap stays {})",
            fmt_tokens(tokens),
            b.daily.as_deref().unwrap_or_default()
        ));
    }
    // Lift the pause the spent budget caused, if the grant covers today's
    // spend. Another repo still over its own cap pauses the loop again on
    // its next model run.
    let ix = repo.index()?;
    let resumed = budget::daily_stop(&ix, b, day).is_none() && lift_budget_pause()?;
    Ok(GrantOut {
        day: day.to_string(),
        tokens,
        changed,
        resumed,
    })
}

/// Ends a daily-budget pause now, journaling `unpaused` where it was
/// recorded. Other pauses (a rate limit) are left alone.
fn lift_budget_pause() -> Result<bool> {
    use crate::daemon::{pause, state};
    let now = chrono::Utc::now();
    state::update(|s| -> Result<bool> {
        let is_budget = s
            .pause_at(now)
            .is_some_and(|p| p.reason.contains(budget::DAILY_SPENT));
        if !is_budget {
            return Ok(false);
        }
        if let Some(p) = s.paused.as_mut() {
            p.until = Some(now - chrono::Duration::seconds(1));
        }
        Ok(pause::settle(s, now)?.is_some())
    })?
}

/// What a `--set` key is.
enum Kind {
    Tokens,
    Usd,
    Duration,
    Text,
}

/// Resolves `key` to (table path under budget, key, kind).
fn resolve_key(key: &str) -> Option<(Option<&'static str>, &'static str, Kind)> {
    let key = key.trim().trim_start_matches("budget.");
    match key {
        "daily" => return Some((None, "daily", Kind::Tokens)),
        "per_item" => return Some((None, "per_item", Kind::Tokens)),
        "review_model" => return Some((None, "review_model", Kind::Text)),
        "fix_model" => return Some((None, "fix_model", Kind::Text)),
        "intake_model" => return Some((None, "intake_model", Kind::Text)),
        "librarian_model" => return Some((None, "librarian_model", Kind::Text)),
        _ => {}
    }
    let (table, field) = key.split_once('.')?;
    let table = table_key(budget::role_of(table)?);
    let (field, kind) = match field {
        "daily" => ("daily", Kind::Tokens),
        "max_usd" => ("max_usd", Kind::Usd),
        "timeout" => ("timeout", Kind::Duration),
        "model" => ("model", Kind::Text),
        "effort" => ("effort", Kind::Text),
        _ => return None,
    };
    Some((Some(table), field, kind))
}

const KEYS: &str = "daily, per_item, <role>.daily, <role>.max_usd, <role>.timeout, <role>.model, \
                    <role>.effort, review_model, fix_model, intake_model, librarian_model \
                    (roles: review, fix, intake, librarian)";

/// Applies `key=value` settings to `.hallmark.toml` text with toml_edit,
/// keeping comments and layout. `none` (or `off`) removes a cap, and turns
/// off a per-run `max_usd`; `daily` and `per_item` are then written `"none"`,
/// since without them the default caps apply. `default` removes any key. The
/// result must still parse as a [`Config`].
pub fn apply_sets(text: &str, sets: &[String]) -> Result<(String, Vec<String>)> {
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| {
        HallError::new(
            ErrorCode::Usage,
            format!(".hallmark.toml doesn't parse: {e}"),
        )
        .next(["hall doctor"])
    })?;
    let mut applied = Vec::new();
    for s in sets.iter().flat_map(|s| s.split_whitespace()) {
        let (k, v) = s.split_once('=').ok_or_else(|| {
            HallError::usage(format!("--set {s}: use key=value, such as daily=4M"))
        })?;
        let (table, field, kind) = resolve_key(k).ok_or_else(|| {
            HallError::usage(format!("--set {s}: unknown key {k}; keys: {KEYS}"))
                .next(["hall budget", "hall schema config"])
        })?;
        let v = v.trim().trim_matches('"');
        let word = v.to_ascii_lowercase();
        let off = matches!(word.as_str(), "none" | "off");
        // A per-run cap, `daily` and `per_item` have defaults, so turning
        // one off is written down as "none"; `default` (or nothing) removes
        // any key.
        let usd = matches!(kind, Kind::Usd);
        let defaulted = table.is_none() && matches!(field, "daily" | "per_item");
        let remove = matches!(word.as_str(), "default" | "") || (off && !usd && !defaulted);
        let value: Option<toml_edit::Value> = if off && defaulted {
            Some("none".into())
        } else if remove {
            None
        } else {
            Some(match kind {
                Kind::Usd if off => "none".into(),
                Kind::Tokens => {
                    if crate::budget::Cap::parse(v).is_none() {
                        return Err(HallError::usage(format!(
                            "--set {s}: {v} is not a cap; use tokens (800k, 4M) or dollars ($15)"
                        )));
                    }
                    v.into()
                }
                Kind::Usd => {
                    let n: f64 = v
                        .trim_start_matches('$')
                        .parse()
                        .ok()
                        .filter(|n: &f64| n.is_finite() && *n > 0.0)
                        .ok_or_else(|| {
                            HallError::usage(format!(
                                "--set {s}: {v} is not an amount in USD, such as 3 or 1.50"
                            ))
                        })?;
                    n.into()
                }
                Kind::Duration => {
                    parse_duration(v).ok_or_else(|| {
                        HallError::usage(format!(
                            "--set {s}: {v} is not a duration; use 20m, 90s or 1h"
                        ))
                    })?;
                    v.into()
                }
                Kind::Text => v.into(),
            })
        };
        let name = match table {
            Some(t) => format!("budget.{t}.{field}"),
            None => format!("budget.{field}"),
        };
        // `hall init` leaves hints such as `# daily    = "4M"` at the end
        // of [budget]; a key that is now set replaces its hint.
        if table.is_none() && value.is_some() {
            drop_hint(&mut doc, field);
        }
        let removed = if off { "none" } else { "default" };
        let budget = doc
            .entry("budget")
            .or_insert(toml_edit::table())
            .as_table_like_mut()
            .ok_or_else(|| HallError::usage("budget in .hallmark.toml is not a table"))?;
        let target: &mut dyn toml_edit::TableLike = match table {
            None => budget,
            Some(t) => {
                if budget.get(t).is_none() {
                    if value.is_none() {
                        applied.push(format!("{name} = {removed}"));
                        continue;
                    }
                    budget.insert(t, toml_edit::table());
                }
                budget
                    .get_mut(t)
                    .and_then(|i| i.as_table_like_mut())
                    .ok_or_else(|| {
                        HallError::usage(format!("budget.{t} in .hallmark.toml is not a table"))
                    })?
            }
        };
        match value {
            None => {
                target.remove(field);
                applied.push(format!("{name} = {removed}"));
            }
            Some(val) => {
                applied.push(format!("{name} = {}", val.to_string().trim()));
                set_value(target, field, val);
            }
        }
    }
    let out = doc.to_string();
    Config::parse(&out).map_err(|e| {
        HallError::new(
            ErrorCode::Usage,
            format!(
                "the new settings don't make a valid .hallmark.toml: {}",
                e.message
            ),
        )
    })?;
    Ok((out, applied))
}

/// Sets `field`, keeping an existing value's comment and spacing; an equal
/// value is left untouched.
pub(crate) fn set_value(target: &mut dyn toml_edit::TableLike, field: &str, val: toml_edit::Value) {
    match target.get_mut(field).and_then(|i| i.as_value_mut()) {
        Some(old) if old.to_string().trim() == val.to_string().trim() => {}
        Some(old) => {
            let decor = old.decor().clone();
            *old = val;
            *old.decor_mut() = decor;
        }
        None => {
            target.insert(field, toml_edit::Item::Value(val));
        }
    }
}

/// Removes commented-out hints for `field` (`# daily    = "4M"`) from the
/// end of the file, where `hall init` leaves them.
pub(crate) fn drop_hint(doc: &mut toml_edit::DocumentMut, field: &str) {
    let Some(trailing) = doc.trailing().as_str().map(String::from) else {
        return;
    };
    let is_hint = |l: &str| {
        l.trim_start()
            .strip_prefix('#')
            .and_then(|r| r.trim_start().strip_prefix(field))
            .is_some_and(|r| r.trim_start().starts_with('='))
    };
    if !trailing.lines().any(is_hint) {
        return;
    }
    let mut kept: String = trailing
        .lines()
        .filter(|l| !is_hint(l))
        .map(|l| format!("{l}\n"))
        .collect();
    if kept.trim().is_empty() {
        kept.clear();
    }
    doc.set_trailing(kept);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_keep_comments_and_are_idempotent() {
        let text = config::render_initial("main", &[], config::CaptureMode::Gate);
        let sets = vec![
            "daily=4M per_item=800k".to_string(),
            "review.max_usd=3".into(),
            "review.daily=2M".into(),
            "fix.model=sonnet".into(),
            "fixer.timeout=5m".into(),
        ];
        let (out, applied) = apply_sets(&text, &sets).unwrap();
        assert_eq!(applied.len(), 6, "{applied:?}");
        assert!(out.contains("# Hallmark settings for this repository"));
        assert!(out.contains("# gate: edits need an item"));
        let c = Config::parse(&out).unwrap();
        assert_eq!(c.budget.daily.as_deref(), Some("4M"));
        assert_eq!(c.budget.per_item.as_deref(), Some("800k"));
        assert_eq!(c.budget.max_usd(config::Role::Reviewer), Some(3.0));
        assert_eq!(c.budget.model(config::Role::Fixer), "sonnet");
        assert_eq!(c.budget.timeout_secs(config::Role::Fixer), 300);
        // init's commented hints for the keys now set are gone.
        assert!(!out.contains("# daily"), "{out}");
        assert!(!out.contains("# per_item"), "{out}");
        let (again, _) = apply_sets(&out, &sets).unwrap();
        assert_eq!(again, out, "idempotent");
        let (removed, _) = apply_sets(&out, &["daily=none".into()]).unwrap();
        assert_eq!(Config::parse(&removed).unwrap().budget.daily, None);
        // Changing a value keeps its trailing comment.
        let (c2, _) = apply_sets(
            "[budget]\ndaily = \"1M\" # whole loop\n",
            &["daily=2M".into()],
        )
        .unwrap();
        assert_eq!(c2, "[budget]\ndaily = \"2M\" # whole loop\n");
    }

    #[test]
    fn a_per_run_cap_can_be_turned_off_and_back_to_its_default() {
        let (off, applied) = apply_sets("", &["review.max_usd=none".into()]).unwrap();
        assert_eq!(applied, ["budget.review.max_usd = \"none\""]);
        let c = Config::parse(&off).unwrap();
        assert_eq!(c.budget.max_usd(config::Role::Reviewer), None);
        let (back, applied) = apply_sets(&off, &["review.max_usd=default".into()]).unwrap();
        assert_eq!(applied, ["budget.review.max_usd = default"]);
        let c = Config::parse(&back).unwrap();
        assert_eq!(c.budget.max_usd(config::Role::Reviewer), Some(6.0));
        // `off` lifts the cap too; it must not restore the default.
        let (off2, applied) = apply_sets("", &["review.max_usd=off".into()]).unwrap();
        assert_eq!(applied, ["budget.review.max_usd = \"none\""]);
        assert!(off2.contains("max_usd = \"none\""), "{off2}");
        let c = Config::parse(&off2).unwrap();
        assert_eq!(c.budget.max_usd(config::Role::Reviewer), None);
    }

    #[test]
    fn default_removes_any_key() {
        let start =
            "[budget]\ndaily = \"4M\"\n\n[budget.review]\nmodel = \"opus\"\ntimeout = \"10m\"\n";
        let (out, applied) = apply_sets(
            start,
            &[
                "daily=default".into(),
                "review.model=default".into(),
                "review.timeout=default".into(),
            ],
        )
        .unwrap();
        assert_eq!(
            applied,
            [
                "budget.daily = default",
                "budget.review.model = default",
                "budget.review.timeout = default"
            ]
        );
        assert!(!out.contains("daily"), "{out}");
        assert!(!out.contains("model"), "{out}");
        assert!(!out.contains("timeout"), "{out}");
        let c = Config::parse(&out).unwrap();
        // Unset, the daily cap is its default again.
        assert_eq!(c.budget.daily, Config::default().budget.daily);
        assert!(c.budget.daily.is_some());
    }

    #[test]
    fn bad_sets_are_usage_errors() {
        for s in [
            "daily=lots",
            "nope=1",
            "review.max_usd=-1",
            "review.timeout=5",
            "daily",
            "ship.daily=1M",
        ] {
            let e = apply_sets("", &[s.to_string()]).unwrap_err();
            assert_eq!(e.code, ErrorCode::Usage, "{s}");
        }
    }

    #[test]
    fn creates_tables_in_an_empty_file() {
        let (out, _) = apply_sets("", &["intake.daily=100k".into()]).unwrap();
        let c = Config::parse(&out).unwrap();
        assert_eq!(
            c.budget
                .role(config::Role::Intake)
                .unwrap()
                .daily
                .as_deref(),
            Some("100k")
        );
    }

    #[test]
    fn bars() {
        let tokens = |used| {
            budget::Cap::Tokens(100).fraction(&Spent {
                tokens: used,
                usd: 0.0,
            })
        };
        assert_eq!(pct_bar(tokens(61) * 100.0), "██████░░░░  61%");
        assert_eq!(pct_bar(tokens(300) * 100.0), "██████████ 300%");
        // A dollar cap reads dollars, whatever the tokens say.
        let usd = budget::Cap::Usd(15.0).fraction(&Spent {
            tokens: 6_410_000,
            usd: 7.5,
        });
        assert_eq!(pct_bar(usd * 100.0), "█████░░░░░  50%");
    }
}
