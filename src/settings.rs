//! The settings `hall config` and `hall setup --interactive` read and write:
//! every leaf key of `.hallmark.toml` ([`Config`]) and of the user config
//! ([`UserConfig`]). The keys are found by walking the types' JSON Schemas,
//! so the list can't drift from what Hallmark reads. One key is changed with
//! toml_edit, keeping the file's comments and layout, and the whole file
//! must still parse afterwards.

use schemars::{JsonSchema, schema_for};
use serde::Serialize;
use serde_json::Value;

use crate::config::{Config, UserConfig, parse_duration, parse_tokens};

/// Which file a key lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// The repository's `.hallmark.toml`, in its main checkout.
    Repo,
    /// `<user data dir>/hallmark/config.toml`.
    User,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Repo => "repo",
            Scope::User => "user",
        }
    }
}

/// What a key holds, and so how a value typed for it is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    String,
    Bool,
    Integer,
    Number,
    /// A list of strings: several values, or one TOML array (`'["a","b"]'`).
    List,
    /// One string, or a list of strings (`lenses`).
    StringOrList,
    /// One of `values`.
    Choice,
}

/// A settable key.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Key {
    /// Dotted, as in `budget.review.max_usd`.
    pub key: String,
    pub kind: Kind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    /// The allowed values of a choice.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
}

/// Every leaf key of a scope's file, sorted.
pub fn keys(scope: Scope) -> Vec<Key> {
    let schema = match scope {
        Scope::Repo => serde_json::to_value(schema_for!(Config)),
        Scope::User => serde_json::to_value(schema_for!(UserConfig)),
    }
    .unwrap_or_default();
    let defs = schema.get("$defs").cloned().unwrap_or(Value::Null);
    let mut out = Vec::new();
    walk(&schema, &defs, "", None, &mut out);
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// The key named `name` in `scope`.
pub fn find(scope: Scope, name: &str) -> Option<Key> {
    keys(scope).into_iter().find(|k| k.key == name)
}

/// Follows `$ref`, and unwraps an `Option` (`anyOf [X, null]`).
fn resolve<'a>(node: &'a Value, defs: &'a Value) -> &'a Value {
    if let Some(name) = node
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.strip_prefix("#/$defs/"))
        && let Some(d) = defs.get(name)
    {
        return resolve(d, defs);
    }
    if let Some(any) = node.get("anyOf").and_then(Value::as_array) {
        let some: Vec<&Value> = any.iter().filter(|v| type_of(v) != Some("null")).collect();
        if some.len() == 1 {
            return resolve(some[0], defs);
        }
    }
    node
}

/// A schema's `type`, skipping `null` in `["number", "null"]`.
fn type_of(node: &Value) -> Option<&str> {
    match node.get("type")? {
        Value::String(s) => Some(s),
        Value::Array(a) => a.iter().filter_map(Value::as_str).find(|t| *t != "null"),
        _ => None,
    }
}

fn walk(node: &Value, defs: &Value, prefix: &str, help: Option<&str>, out: &mut Vec<Key>) {
    let r = resolve(node, defs);
    if let Some(props) = r.get("properties").and_then(Value::as_object) {
        for (name, p) in props {
            let key = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}.{name}")
            };
            let help = p.get("description").and_then(Value::as_str);
            walk(p, defs, &key, help, out);
        }
        return;
    }
    let help = help
        .or_else(|| r.get("description").and_then(Value::as_str))
        .map(|h| h.split_whitespace().collect::<Vec<_>>().join(" "));
    let choices: Vec<String> = r
        .get("oneOf")
        .or_else(|| r.get("enum"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.get("const").or(Some(v)).and_then(Value::as_str))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let kind = if !choices.is_empty() {
        Kind::Choice
    } else if r.get("anyOf").is_some() {
        Kind::StringOrList
    } else {
        match type_of(r) {
            Some("boolean") => Kind::Bool,
            Some("integer") => Kind::Integer,
            Some("number") => Kind::Number,
            Some("array") => Kind::List,
            _ => Kind::String,
        }
    };
    out.push(Key {
        key: prefix.to_string(),
        kind,
        help,
        values: choices,
    });
}

/// Reads the values typed for `key` as a TOML value of its kind. A list
/// takes several values, or one TOML array; everything else takes one.
pub fn parse(key: &Key, input: &[String]) -> Result<toml_edit::Value, String> {
    let one = || match input {
        [v] => Ok(v.trim()),
        _ => Err(format!("{} takes one value", key.key)),
    };
    let list = || -> Result<toml_edit::Value, String> {
        if let [v] = input
            && v.trim_start().starts_with('[')
        {
            let doc: toml_edit::DocumentMut = format!("v = {v}")
                .parse()
                .map_err(|_| format!("{v} is not a TOML list, such as [\"a\", \"b\"]"))?;
            let arr = doc["v"]
                .as_array()
                .filter(|a| a.iter().all(|x| x.is_str()))
                .ok_or_else(|| format!("{} is a list of strings", key.key))?;
            return Ok(toml_edit::Value::Array(arr.clone()));
        }
        Ok(toml_edit::Value::Array(input.iter().collect()))
    };
    let v = match key.kind {
        Kind::String => one()?.into(),
        Kind::Bool => match one()?.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "y" => true.into(),
            "false" | "no" | "off" | "n" => false.into(),
            other => return Err(format!("{}: {other} is not true or false", key.key)),
        },
        Kind::Integer => {
            let v = one()?;
            v.parse::<i64>()
                .ok()
                .filter(|n| *n >= 0)
                .ok_or_else(|| format!("{}: {v} is not a whole number", key.key))?
                .into()
        }
        Kind::Number => {
            let v = one()?;
            v.trim_start_matches('$')
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite() && *n >= 0.0)
                .ok_or_else(|| format!("{}: {v} is not a number", key.key))?
                .into()
        }
        Kind::Choice => {
            let v = one()?;
            if !key.values.iter().any(|c| c == v) {
                return Err(format!(
                    "{}: {v} is not one of {}",
                    key.key,
                    key.values.join(", ")
                ));
            }
            v.into()
        }
        Kind::List => list()?,
        Kind::StringOrList => match input {
            // A lone lens is a one-item list: the string form means "all".
            [v] if key.key == "lenses" && !v.trim_start().starts_with('[') => {
                if v.trim() == "all" {
                    "all".into()
                } else {
                    toml_edit::Value::Array([v.trim()].into_iter().collect())
                }
            }
            [v] if !v.trim_start().starts_with('[') => v.trim().into(),
            _ => list()?,
        },
    };
    check_units(&key.key, &v)?;
    Ok(v)
}

/// Keys whose strings carry units: durations, token counts and caps.
fn check_units(key: &str, v: &toml_edit::Value) -> Result<(), String> {
    let Some(s) = v.as_str() else {
        return Ok(());
    };
    let last = key.rsplit('.').next().unwrap_or(key);
    let duration = matches!(
        key,
        "stuck_after" | "notify.remind" | "library.retire_after"
    ) || (key.starts_with("budget.") && last == "timeout");
    let cap = key.starts_with("budget.") && matches!(last, "daily" | "per_item");
    if duration && parse_duration(s).is_none() {
        return Err(format!(
            "{key}: {s} is not a duration; use 90s, 30m, 4h or 7d"
        ));
    }
    if cap && crate::budget::Cap::parse(s).is_none() {
        return Err(format!(
            "{key}: {s} is not a cap; use tokens (800k, 4M) or dollars ($15)"
        ));
    }
    if key == "library.briefing_limit" && parse_tokens(s).is_none() {
        return Err(format!("{key}: {s} is not a token count, such as 6k"));
    }
    Ok(())
}

/// Sets (`Some`) or removes (`None`) a dotted key in TOML text, creating
/// tables as needed and keeping comments, layout and an existing value's
/// comment. The result is checked to still parse as the scope's file.
pub fn edit(
    scope: Scope,
    text: &str,
    key: &str,
    value: Option<toml_edit::Value>,
) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("the file doesn't parse: {e}"))?;
    let parts: Vec<&str> = key.split('.').collect();
    let (field, tables) = parts.split_last().ok_or("empty key")?;
    // `hall init` leaves hints such as `# daily    = "4M"` at the end of
    // [budget]; a key that is now set replaces its hint.
    if value.is_some() && tables == ["budget"] {
        crate::commands::budget::drop_hint(&mut doc, field);
    }
    // Comments after the last table's keys (`hall init`'s hints at the end
    // of [budget]) are the document's trailing text: a table added now goes
    // after them, not between that table and its comments.
    let trailing = (value.is_some() && tables.first().is_some_and(|t| doc.get(t).is_none()))
        .then(|| doc.trailing().as_str().unwrap_or_default().to_string())
        .filter(|t| !t.trim().is_empty());
    if trailing.is_some() {
        doc.set_trailing("");
    }
    let mut target: &mut dyn toml_edit::TableLike = doc.as_table_mut();
    for (i, t) in tables.iter().enumerate() {
        if target.get(t).is_none() {
            if value.is_none() {
                return Ok(doc.to_string());
            }
            target.insert(t, toml_edit::table());
        }
        target = target
            .get_mut(t)
            .and_then(|i| i.as_table_like_mut())
            .ok_or_else(|| format!("{} is not a table", tables[..=i].join(".")))?;
    }
    let removing = value.is_none();
    match value {
        None => {
            target.remove(field);
        }
        Some(v) => crate::commands::budget::set_value(target, field, v),
    }
    if let Some(t) = trailing
        && let Some(table) = doc.get_mut(tables[0]).and_then(|i| i.as_table_mut())
    {
        table
            .decor_mut()
            .set_prefix(format!("{}\n\n", t.trim_end()));
    }
    // An empty [notify] still replaces the user's channels as a whole.
    if removing
        && scope == Scope::Repo
        && tables == ["notify"]
        && doc
            .get("notify")
            .and_then(|i| i.as_table_like())
            .is_some_and(|t| t.is_empty())
    {
        doc.remove("notify");
    }
    let out = doc.to_string();
    let parsed = match scope {
        Scope::Repo => Config::parse(&out).map(|_| ()),
        Scope::User => toml::from_str::<UserConfig>(&out)
            .map(|_| ())
            .map_err(Into::into),
    };
    parsed.map_err(|e| format!("{key} would make the file invalid: {}", e.message))?;
    Ok(out)
}

/// A key's value in a parsed file, or `None` when the file doesn't set it.
pub fn lookup(text: &str, key: &str) -> Option<Value> {
    let v: toml::Value = toml::from_str(text).ok()?;
    let mut cur = &v;
    for part in key.split('.') {
        cur = cur.get(part)?;
    }
    serde_json::to_value(cur).ok()
}

/// A key's value in a value serialized from the effective settings, or
/// null.
pub fn pick(effective: &Value, key: &str) -> Value {
    key.split('.')
        .try_fold(effective, |cur, part| cur.get(part))
        .cloned()
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(scope: Scope, name: &str) -> Key {
        find(scope, name).unwrap_or_else(|| panic!("no key {name}"))
    }

    #[test]
    fn walks_every_leaf_with_its_kind() {
        let repo = keys(Scope::Repo);
        let kind = |k: &str| repo.iter().find(|x| x.key == k).map(|x| x.kind);
        assert_eq!(kind("base"), Some(Kind::String));
        assert_eq!(kind("gates"), Some(Kind::List));
        assert_eq!(kind("max_rejects"), Some(Kind::Integer));
        assert_eq!(kind("auto_ready"), Some(Kind::Bool));
        assert_eq!(kind("lenses"), Some(Kind::StringOrList));
        assert_eq!(kind("capture.mode"), Some(Kind::Choice));
        assert_eq!(kind("budget.review.max_usd"), Some(Kind::Number));
        assert_eq!(kind("budget.daily"), Some(Kind::String));
        assert_eq!(kind("notify.route.ready"), Some(Kind::List));
        assert_eq!(kind("parallel.checks"), Some(Kind::Integer));
        // Tables are walked into, never listed.
        assert_eq!(kind("budget"), None);
        assert_eq!(kind("notify"), None);
        let mode = key(Scope::Repo, "capture.mode");
        assert_eq!(mode.values, ["gate", "ask", "off"]);
        assert!(key(Scope::Repo, "base").help.is_some());

        let user: Vec<String> = keys(Scope::User).into_iter().map(|k| k.key).collect();
        assert!(user.contains(&"tools.gh".to_string()));
        assert!(user.contains(&"notify.phone".to_string()));
        assert!(!user.contains(&"base".to_string()));
    }

    #[test]
    fn parses_values_by_kind() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let gates = key(Scope::Repo, "gates");
        let v = parse(&gates, &s(&["cargo test", "cargo clippy"])).unwrap();
        assert_eq!(v.to_string().trim(), r#"["cargo test", "cargo clippy"]"#);
        let v = parse(&gates, &s(&[r#"["make test"]"#])).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert!(parse(&gates, &s(&["[1, 2]"])).is_err());

        let b = key(Scope::Repo, "auto_ready");
        assert_eq!(parse(&b, &s(&["no"])).unwrap().as_bool(), Some(false));
        assert!(parse(&b, &s(&["maybe"])).is_err());
        let n = key(Scope::Repo, "max_rejects");
        assert_eq!(parse(&n, &s(&["5"])).unwrap().as_integer(), Some(5));
        assert!(parse(&n, &s(&["-1"])).is_err());
        assert!(parse(&n, &s(&["1", "2"])).is_err());
        let m = key(Scope::Repo, "capture.mode");
        assert!(parse(&m, &s(&["ask"])).is_ok());
        assert!(parse(&m, &s(&["sometimes"])).is_err());
        let l = key(Scope::Repo, "lenses");
        assert!(parse(&l, &s(&["all"])).unwrap().is_str());
        assert!(parse(&l, &s(&["security", "tests"])).unwrap().is_array());

        let d = key(Scope::Repo, "budget.daily");
        assert!(parse(&d, &s(&["4M"])).is_ok());
        assert!(parse(&d, &s(&["$15"])).is_ok());
        assert!(parse(&d, &s(&["lots"])).is_err());
        let t = key(Scope::Repo, "budget.review.timeout");
        assert!(parse(&t, &s(&["20m"])).is_ok());
        assert!(parse(&t, &s(&["20"])).is_err());
    }

    #[test]
    fn edits_keep_comments_and_create_tables() {
        let text = crate::config::render_initial("main", &[], crate::config::CaptureMode::Gate);
        let out = edit(Scope::Repo, &text, "base", Some("trunk".into())).unwrap();
        assert!(out.contains(r#"base     = "trunk""#), "{out}");
        assert!(out.contains("# Hallmark settings for this repository."));
        let out = edit(Scope::Repo, &out, "budget.daily", Some("4M".into())).unwrap();
        assert!(!out.contains("# daily"), "the hint is replaced: {out}");
        let out = edit(Scope::Repo, &out, "budget.review.max_usd", Some(3.0.into())).unwrap();
        let c = Config::parse(&out).unwrap();
        assert_eq!(c.base, "trunk");
        assert_eq!(c.budget.daily.as_deref(), Some("4M"));
        assert_eq!(c.budget.review.unwrap().max_usd, Some(3.0));
        // The same value again changes nothing; unset goes back to the default.
        assert_eq!(
            edit(Scope::Repo, &out, "base", Some("trunk".into())).unwrap(),
            out
        );
        let back = edit(Scope::Repo, &out, "base", None).unwrap();
        assert_eq!(Config::parse(&back).unwrap().base, "main");
        assert_eq!(
            edit(Scope::Repo, &back, "notify.slack", None).unwrap(),
            back
        );
        // A value of the wrong type is refused with the file unchanged.
        assert!(edit(Scope::Repo, &out, "max_rejects", Some("x".into())).is_err());

        let user = edit(Scope::User, "", "tools.gh", Some("/opt/gh".into())).unwrap();
        assert_eq!(lookup(&user, "tools.gh"), Some(Value::from("/opt/gh")));
        assert_eq!(lookup(&user, "tools.qmd"), None);
    }

    #[test]
    fn a_new_table_goes_after_the_last_tables_comments() {
        // `hall init` ends [budget] with commented hints; toml_edit keeps
        // them as the document's trailing text.
        let text = crate::config::render_initial("main", &[], crate::config::CaptureMode::Gate);
        assert!(
            text.trim_end().ends_with(r#"# per_item = "800k""#),
            "{text}"
        );
        let roles = toml_edit::Value::Array(["reviewer"].into_iter().collect());
        let out = edit(Scope::Repo, &text, "extensions.mattpocock", Some(roles)).unwrap();
        let budget = out.find("[budget]").unwrap();
        let hint = out.find("# per_item").unwrap();
        let table = out.find("[extensions]").unwrap();
        assert!(budget < hint && hint < table, "{out}");
        assert!(
            out.trim_end()
                .ends_with("[extensions]\nmattpocock = [\"reviewer\"]"),
            "{out}"
        );
        // Unset leaves the hints where they were.
        let back = edit(Scope::Repo, &out, "extensions.mattpocock", None).unwrap();
        assert!(back.find("# per_item").unwrap() > back.find("[budget]").unwrap());
    }
}
